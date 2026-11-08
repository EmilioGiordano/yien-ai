//! The settings file on disk: written once with commented defaults, read
//! with a size limit, watched by polling, opened in an editor (RF-CFG-01).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use yien_config::{MAX_FILE_BYTES, Reload, ReloadGate};

#[cfg(windows)]
mod windows;

/// How often the watcher looks at the file.
pub const POLL: Duration = Duration::from_millis(500);

/// How long a changed stamp must hold before the file is read.
const SETTLE: Duration = Duration::from_millis(400);

/// What a poll sees of the file: `None` while it does not exist.
pub type FileStamp = Option<(Option<SystemTime>, u64)>;

/// Writes `text` to `path` only if nothing is there, creating its folder.
/// Returns whether it wrote. An existing file is never touched.
///
/// ```no_run
/// let file = yien_platform::paths::config_file()?;
/// let wrote = yien_platform::config_file::create_if_absent(&file, "# defaults\n")?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn create_if_absent(path: &Path, text: &str) -> io::Result<bool> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// The file's stamp now.
pub fn stamp(path: &Path) -> FileStamp {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok(), meta.len()))
}

/// The file's text; `Ok(None)` when it does not exist. A file larger than
/// [`MAX_FILE_BYTES`] or not in UTF-8 is an error, read no further than the
/// limit.
pub fn read(path: &Path) -> io::Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the file is larger than {} KiB", MAX_FILE_BYTES / 1024),
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the file is not UTF-8"))
}

/// Watches `path` on a thread of its own, polling its stamp every [`POLL`].
/// Each change that holds for a moment calls `on_change` with what
/// [`read`] returns; the watch ends when `on_change` returns `false` or the
/// returned [`Watcher`] is stopped. A read that fails for a passing reason
/// (the file held open by an editor saving it) is retried on later polls.
/// `applied` is the stamp of the version the caller already has.
///
/// ```no_run
/// use yien_platform::config_file;
///
/// let file = yien_platform::paths::config_file()?;
/// let seen = config_file::stamp(&file);
/// let watcher = config_file::watch(file, seen, |text| {
///     println!("changed: {:?}", text.map(|t| t.map(|t| t.len())));
///     true
/// })?;
/// watcher.stop();
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn watch(
    path: PathBuf,
    applied: FileStamp,
    on_change: impl FnMut(io::Result<Option<String>>) -> bool + Send + 'static,
) -> io::Result<Watcher> {
    watch_every(path, applied, POLL, SETTLE, on_change)
}

/// The watcher thread of [`watch`].
#[derive(Debug)]
pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Watcher {
    /// Asks the thread to end at its next poll, without waiting for it.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Whether the thread has ended.
    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }
}

fn watch_every(
    path: PathBuf,
    applied: FileStamp,
    poll: Duration,
    settle: Duration,
    mut on_change: impl FnMut(io::Result<Option<String>>) -> bool + Send + 'static,
) -> io::Result<Watcher> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("yien-config-watch".into())
        .spawn(move || {
            let start = Instant::now();
            let mut gate = ReloadGate::new(applied, settle);
            while !stopped.load(Ordering::Acquire) {
                std::thread::sleep(poll);
                if stopped.load(Ordering::Acquire) {
                    return;
                }
                if gate.observe(stamp(&path), start.elapsed()) != Reload::Apply {
                    continue;
                }
                let text = read(&path);
                if let Err(e) = &text
                    && passing(e)
                {
                    gate.retry();
                    continue;
                }
                if !on_change(text) {
                    return;
                }
            }
        })?;
    Ok(Watcher { stop, thread })
}

/// A read error worth retrying rather than reporting: anything but a
/// missing file or one too large or not UTF-8.
fn passing(error: &io::Error) -> bool {
    !matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::InvalidData
    )
}

/// Why the file could not be opened in an editor.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The `editor` setting has no program, or a quote is left open.
    #[error("the editor setting {0:?} names no program")]
    BadEditor(String),
    /// The OS refused to start the program or open the file.
    #[error("could not open {path}: {reason}")]
    Failed {
        /// The file.
        path: String,
        /// What the OS said.
        reason: String,
    },
    /// No editor is set and the OS would run the file instead of showing
    /// it: a program, a script or a shortcut.
    #[error("{name} would run as a program; set `editor` in config.toml to open it")]
    WouldRun {
        /// The file's name.
        name: String,
    },
}

/// Opens `path` in `editor` (a command line, the file appended as its last
/// argument). With no editor, a file of a view-only type ([`VIEW_ONLY`]) goes
/// to the OS default app; any other file goes to Notepad on Windows and is
/// refused elsewhere, so the default app never runs a program or script.
/// On Windows a bare editor name is looked up on `PATH` alone, and Notepad
/// is the system folder's, so the current folder is never searched.
/// Runs on a thread of its own and returns at once; `done` gets the outcome.
///
/// ```no_run
/// let file = yien_platform::paths::config_file()?;
/// yien_platform::config_file::open(file, None, |result| {
///     if let Err(error) = result {
///         eprintln!("{error}");
///     }
/// });
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn open(
    path: PathBuf,
    editor: Option<String>,
    done: impl FnOnce(Result<(), OpenError>) + Send + 'static,
) {
    let command = match editor
        .as_deref()
        .map(|e| (e, yien_config::split_command(e)))
    {
        Some((line, None)) => return done(Err(OpenError::BadEditor(line.to_owned()))),
        Some((_, Some(parts))) => Some(parts),
        None => None,
    };
    let spawned = std::thread::Builder::new()
        .name("yien-open-file".into())
        .spawn(move || {
            let outcome = match command {
                Some(command) => open_now(&path, Some(&command)),
                None => open_by_type(&path),
            };
            done(outcome);
        });
    if let Err(e) = spawned {
        tracing::warn!("could not start the thread that opens the file: {e}");
    }
}

/// With no editor: the default app, Notepad or a refusal, by [`route`].
fn open_by_type(path: &Path) -> Result<(), OpenError> {
    let pathext = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_default()
    } else {
        String::new()
    };
    match route(path, &pathext, cfg!(windows), launches(path)) {
        Route::DefaultApp => open_now(path, None),
        Route::Notepad => open_in_notepad(path),
        Route::Refuse => Err(would_run(path)),
    }
}

fn would_run(path: &Path) -> OpenError {
    OpenError::WouldRun {
        name: path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

#[cfg(windows)]
fn open_in_notepad(path: &Path) -> Result<(), OpenError> {
    windows::open_in_notepad(path).map_err(|reason| failed(path, reason))
}

/// Never reached: [`route`] sends nothing to Notepad off Windows.
#[cfg(not(windows))]
fn open_in_notepad(path: &Path) -> Result<(), OpenError> {
    Err(would_run(path))
}

/// Where a file goes when no editor is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    DefaultApp,
    Notepad,
    Refuse,
}

/// File types whose default app only shows them.
const VIEW_ONLY: &[&str] = &[
    "txt", "md", "markdown", "log", "csv", "tsv", "json", "jsonc", "toml", "yaml", "yml", "xml",
    "ini", "cfg", "conf", "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "pdf", "docx", "xlsx",
    "pptx", "odt", "ods", "odp",
];

/// Only a view-only type that no runnable list names and that is not a
/// launcher goes to the default app; the rest go to Notepad on Windows and
/// are refused elsewhere. `launches` is the file system's say (an execute
/// bit, a folder).
fn route(path: &Path, pathext: &str, windows: bool, launches: bool) -> Route {
    let viewable = last_extension(path).is_some_and(|e| VIEW_ONLY.contains(&e.as_str()))
        && !runnable_extension(path, pathext)
        && !launches;
    if viewable {
        Route::DefaultApp
    } else if windows {
        Route::Notepad
    } else {
        Route::Refuse
    }
}

/// The name's last extension, lowercased, after the trailing dots and
/// spaces Windows drops.
fn last_extension(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let name = name.trim_end_matches(['.', ' ']);
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_owned())
}

/// Whether the file system marks `path` as something that runs: a folder
/// (an app bundle) or a file with an execute bit.
#[cfg(unix)]
fn launches(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path).is_ok_and(|m| m.is_dir() || m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn launches(path: &Path) -> bool {
    path.is_dir()
}

/// Extensions the Windows shell runs, or hands to something that runs
/// them, on its default verb; `PATHEXT` adds to them. A second guard
/// behind [`VIEW_ONLY`].
const RUNNABLE: &[&str] = &[
    "exe",
    "bat",
    "cmd",
    "com",
    "lnk",
    "url",
    "js",
    "jse",
    "vbs",
    "vbe",
    "wsf",
    "wsh",
    "hta",
    "msi",
    "msp",
    "scr",
    "cpl",
    "reg",
    "ps1",
    "psm1",
    "appref-ms",
    "application",
    "jar",
    "pif",
    "inf",
    "msc",
    "settingcontent-ms",
];

/// Whether `path`'s name ends in an extension the Windows shell runs:
/// [`RUNNABLE`] or one of `pathext` (`;`-separated, with dots), ignoring
/// case as Windows does, and ignoring the trailing dots and spaces Windows
/// drops from a name.
fn runnable_extension(path: &Path, pathext: &str) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let name = name.trim_end_matches(['.', ' ']);
    let listed = pathext
        .split(';')
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty());
    RUNNABLE
        .iter()
        .map(|e| (*e).to_owned())
        .chain(listed)
        .any(|ext| name.ends_with(&format!(".{ext}")))
}

fn failed(path: &Path, reason: impl ToString) -> OpenError {
    OpenError::Failed {
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(windows)]
fn open_now(path: &Path, command: Option<&[String]>) -> Result<(), OpenError> {
    windows::open(path, command).map_err(|reason| failed(path, reason))
}

#[cfg(not(windows))]
fn open_now(path: &Path, command: Option<&[String]>) -> Result<(), OpenError> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let (program, args) = match command {
        Some([program, args @ ..]) => (program.as_str(), args),
        _ => (opener, &[][..]),
    };
    let mut child = std::process::Command::new(program)
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| failed(path, format!("{program}: {e}")))?;
    // Reaps the child so it does not linger as a zombie; this thread is ours.
    let _ = child.wait();
    Ok(())
}

/// The file as the last argument of a Windows command line, always quoted
/// (a Windows path cannot hold `"`), its trailing backslashes doubled so
/// the closing quote stays a quote.
#[cfg_attr(not(windows), allow(dead_code))]
fn quote_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trailing = text.len() - text.trim_end_matches('\\').len();
    format!("\"{text}{}\"", "\\".repeat(trailing))
}

/// The editor's arguments and then the file, as the Windows command line
/// that follows the program.
#[cfg_attr(not(windows), allow(dead_code))]
fn command_params(args: &[String], path: &Path) -> String {
    let mut params: Vec<String> = args.iter().map(|a| quote_arg(a)).collect();
    params.push(quote_path(path));
    params.join(" ")
}

/// The file `program` names: an absolute path as given, or a bare name
/// looked up in each absolute `dirs` folder in order, with each `pathext`
/// extension when it has none. Relative folders, the current one among them,
/// are never searched, and a relative path with folders in it is refused.
#[cfg_attr(not(windows), allow(dead_code))]
fn resolve_program(program: &str, dirs: &[PathBuf], pathext: &str) -> Option<PathBuf> {
    let program = Path::new(program);
    let has_extension = program.extension().is_some();
    let exts: Vec<&str> = pathext.split(';').filter(|e| !e.is_empty()).collect();
    let find = |base: PathBuf| -> Option<PathBuf> {
        if has_extension || exts.is_empty() {
            return base.is_file().then_some(base);
        }
        exts.iter().find_map(|ext| {
            let mut file = base.clone().into_os_string();
            file.push(ext);
            let file = PathBuf::from(file);
            file.is_file().then_some(file)
        })
    };
    if program.is_absolute() {
        return find(program.to_path_buf()).or_else(|| Some(program.to_path_buf()));
    }
    let bare = matches!(
        program.components().collect::<Vec<_>>()[..],
        [std::path::Component::Normal(_)]
    );
    if !bare {
        return None;
    }
    dirs.iter()
        .filter(|d| d.is_absolute())
        .find_map(|d| find(d.join(program)))
}

/// Whether Windows runs `file` through cmd.exe.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_batch(file: &Path) -> bool {
    file.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Whether arguments and a path can go to a batch file: nothing cmd.exe
/// would expand or treat as syntax. The path is always quoted, and inside
/// quotes cmd.exe still expands `%` and `!` and ends the quote at `"`; the
/// editor's arguments are quoted only when needed, so they get the full set.
#[cfg_attr(not(windows), allow(dead_code))]
fn batch_safe(args: &[String], path: &Path) -> bool {
    const META: &[char] = &['%', '!', '"', '^', '&', '|', '<', '>', '(', ')'];
    const IN_QUOTES: &[char] = &['%', '!', '"'];
    args.iter().all(|a| !a.contains(META)) && !path.to_string_lossy().contains(IN_QUOTES)
}

/// One argument for a Windows command line: quoted when it holds a space, a
/// tab or a quote, with backslashes doubled before a quote (the MSVC rules
/// `CommandLineToArgvW` follows).
#[cfg_attr(not(windows), allow(dead_code))]
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("yien-config-file-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn rf_cfg_01_the_default_file_is_written_once_and_never_overwritten() {
        let dir = scratch("create");
        let file = dir.join("nested").join("config.toml");
        assert!(create_if_absent(&file, "# first\n").unwrap());
        fs::write(&file, "theme = \"washi\"\n").unwrap();
        assert!(!create_if_absent(&file, "# first\n").unwrap());
        assert_eq!(read(&file).unwrap().as_deref(), Some("theme = \"washi\"\n"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_a_missing_file_is_none_and_a_huge_one_is_refused() {
        let dir = scratch("read");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir.join("absent.toml")).unwrap(), None);
        assert_eq!(stamp(&dir.join("absent.toml")), None);
        let big = dir.join("big.toml");
        fs::write(&big, vec![b'#'; MAX_FILE_BYTES + 10]).unwrap();
        assert!(read(&big).is_err());
        let latin1 = dir.join("latin1.toml");
        fs::write(&latin1, b"theme = \"\xe9\"\n").unwrap();
        assert!(read(&latin1).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_cfg_01_the_watcher_reads_a_change_and_ends_when_told() {
        let dir = scratch("watch");
        let file = dir.join("config.toml");
        create_if_absent(
            &file,
            "theme = \"washi\"
",
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // The file is new to the watcher: its stamp differs from `None`.
        let watcher = watch_every(
            file.clone(),
            None,
            Duration::from_millis(5),
            Duration::ZERO,
            move |text| {
                let _ = tx.send(text.map_err(|e| e.kind()));
                false
            },
        )
        .unwrap();
        let text = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(
            text,
            Ok(Some(
                "theme = \"washi\"
"
                .to_owned()
            ))
        );
        // Returning false ended the thread: the sender went with it.
        assert!(rx.recv_timeout(Duration::from_secs(10)).is_err());
        assert!(watcher.thread.join().is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stopped_watcher_ends_without_reading() {
        let dir = scratch("stop");
        fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let watcher = watch_every(
            dir.join("absent.toml"),
            Some((None, 1)),
            Duration::from_millis(5),
            Duration::from_secs(3600),
            move |_| tx.send(()).is_ok(),
        )
        .unwrap();
        watcher.stop();
        assert!(watcher.thread.join().is_ok());
        assert!(rx.try_recv().is_err(), "nothing was applied");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_passing_read_errors_are_retried() {
        assert!(
            passing(&io::Error::from_raw_os_error(32)),
            "sharing violation"
        );
        assert!(passing(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(!passing(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(!passing(&io::Error::from(io::ErrorKind::InvalidData)));
    }

    #[test]
    fn a_bad_editor_line_fails_at_once() {
        let (tx, rx) = std::sync::mpsc::channel();
        open(PathBuf::from("x.toml"), Some("\"ed".into()), move |r| {
            let _ = tx.send(r);
        });
        assert!(matches!(rx.recv().unwrap(), Err(OpenError::BadEditor(_))));
    }

    #[test]
    fn the_file_argument_is_always_quoted() {
        assert_eq!(
            quote_path(Path::new(r"C:\Data\config.toml")),
            r#""C:\Data\config.toml""#
        );
        assert_eq!(quote_path(Path::new(r"C:\dir\")), r#""C:\dir\\""#);
    }

    #[test]
    fn a_bare_program_is_found_only_in_absolute_path_folders() {
        let dir = std::path::absolute(scratch("resolve")).unwrap();
        let (first, second) = (dir.join("first"), dir.join("second"));
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(second.join("ed.cmd"), "").unwrap();
        fs::write(second.join("tool.exe"), "").unwrap();
        fs::write(first.join("tool.cmd"), "").unwrap();
        let dirs = [
            PathBuf::from("."),
            PathBuf::new(),
            first.clone(),
            second.clone(),
        ];
        let ext = ".exe;.cmd";
        // PATH order first, then PATHEXT order inside a folder.
        assert_eq!(
            resolve_program("tool", &dirs, ext),
            Some(first.join("tool.cmd"))
        );
        assert_eq!(
            resolve_program("ed", &dirs, ext),
            Some(second.join("ed.cmd"))
        );
        assert_eq!(
            resolve_program("tool.exe", &dirs, ext),
            Some(second.join("tool.exe"))
        );
        assert_eq!(resolve_program("missing", &dirs, ext), None);
        // Relative folders, `.` among them, are skipped.
        assert_eq!(resolve_program("ed", &[PathBuf::from(".")], ext), None);
        let relative = Path::new("bin").join("ed");
        assert_eq!(
            resolve_program(&relative.to_string_lossy(), &dirs, ext),
            None
        );
        let absolute = second.join("ed");
        assert_eq!(
            resolve_program(&absolute.to_string_lossy(), &[], ext),
            Some(second.join("ed.cmd"))
        );
        assert!(is_batch(&second.join("ed.cmd")));
        assert!(!is_batch(&second.join("tool.exe")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn batch_editors_are_given_only_plain_arguments() {
        assert!(is_batch(Path::new("run.BAT")));
        let file = Path::new(r"C:\Data\config.toml");
        assert!(batch_safe(&["--new-window".into()], file));
        assert!(!batch_safe(&[], Path::new(r"C:\100%\config.toml")));
        assert!(!batch_safe(&["a&calc".into()], file));
    }

    #[test]
    fn a_batch_editor_takes_paths_with_characters_plain_inside_quotes() {
        assert!(batch_safe(&[], Path::new(r"C:\web\app\(auth)\page.tsx")));
        assert!(batch_safe(&[], Path::new(r"C:\Docs\Report (1).md")));
        assert!(batch_safe(&[], Path::new(r"C:\a&b\x^y.md")));
        assert!(!batch_safe(&[], Path::new(r"C:\a!b\x.md")));
        assert!(!batch_safe(&[], Path::new(r"C:\100%\x.md")));
        assert!(
            !batch_safe(&["(x)".into()], Path::new(r"C:\x.md")),
            "arguments keep the full set"
        );
    }

    #[test]
    fn rf_ctx_03_files_the_windows_shell_would_run_are_recognised() {
        let ext = ".COM;.EXE;.BAT;.CMD;.VBS;.JS;.WSF;.PY";
        for runs in [
            "setup.bat",
            "SETUP.BAT",
            ".bat",
            "README.lnk",
            "x.js",
            "x.vbs",
            "tool.exe",
            "a.ps1",
            "site.url",
            "app.appref-ms",
            "x.settingcontent-ms",
            "keys.reg",
            "run.py",
            "trailing.bat. ",
        ] {
            assert!(runnable_extension(Path::new(runs), ext), "{runs:?}");
        }
        for shows in [
            "config.toml",
            "main.rs",
            "notes.md",
            "Makefile",
            "batch.txt",
            "exe",
            "page.jsx",
        ] {
            assert!(!runnable_extension(Path::new(shows), ext), "{shows}");
        }
        assert!(
            !runnable_extension(Path::new("run.py"), ""),
            "only PATHEXT makes .py runnable"
        );
    }

    #[test]
    fn rf_ctx_03_with_no_editor_only_view_only_types_reach_the_default_app() {
        let route_of = |name: &str, windows: bool| route(Path::new(name), "", windows, false);
        for viewable in [
            "README.md",
            "config.toml",
            "NOTES.TXT",
            "data.json",
            "photo.JPG",
            "report.pdf",
            "sheet.xlsx",
            "trailing.md. ",
        ] {
            assert_eq!(route_of(viewable, true), Route::DefaultApp, "{viewable:?}");
            assert_eq!(route_of(viewable, false), Route::DefaultApp, "{viewable:?}");
        }
        for other in [
            "install.sh",
            "run.py",
            "main.rs",
            "setup.bat",
            "README.lnk",
            "page.html",
            "macro.docm",
            "Makefile",
            ".md",
            "notes.md.exe",
            "launch.desktop",
            "Tool.app",
        ] {
            assert_eq!(route_of(other, true), Route::Notepad, "{other}");
            assert_eq!(route_of(other, false), Route::Refuse, "{other}");
        }
        // The second guard: a type PATHEXT names never reaches the default
        // app, even one that is otherwise view-only.
        assert_eq!(route(Path::new("x.md"), ".MD", true, false), Route::Notepad);
        // An execute bit or a folder overrules the name.
        assert_eq!(route(Path::new("x.md"), "", false, true), Route::Refuse);
        assert_eq!(last_extension(Path::new("a.tar.GZ")).as_deref(), Some("gz"));
        assert_eq!(last_extension(Path::new(".bashrc")), None);
    }

    /// Unix only: on Windows the same file would go to Notepad, which must
    /// not open while Emilio is at the PC.
    #[cfg(unix)]
    #[test]
    fn rf_ctx_03_with_no_editor_a_script_is_refused_through_the_callback() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("refused");
        fs::create_dir_all(&dir).unwrap();
        let readme = dir.join("README.md");
        fs::write(&readme, "").unwrap();
        fs::set_permissions(&readme, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(launches(&readme), "an execute bit");
        assert!(launches(&dir), "a folder");
        for file in [dir.join("install.sh"), readme] {
            let (tx, rx) = std::sync::mpsc::channel();
            open(file.clone(), None, move |r| {
                let _ = tx.send(r);
            });
            let refused = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            assert!(
                matches!(&refused, Err(OpenError::WouldRun { .. })),
                "{file:?}: {refused:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_ctx_03_the_editor_gets_its_arguments_then_the_quoted_file() {
        assert_eq!(
            command_params(
                &["--reuse-window".into(), "a b".into()],
                Path::new(r"C:\My Code\src\main.rs")
            ),
            r#"--reuse-window "a b" "C:\My Code\src\main.rs""#
        );
        assert_eq!(
            command_params(&[], Path::new(r"C:\x\notes.txt")),
            r#""C:\x\notes.txt""#
        );
    }

    /// A fake editor that records the arguments it was started with; no
    /// window opens. Unix only: on Windows the launch goes through
    /// `ShellExecuteExW`, covered by the command line test above.
    #[cfg(unix)]
    #[test]
    fn rf_ctx_03_a_file_opens_in_the_configured_editor_as_one_argument() {
        let dir = scratch("fake-editor");
        fs::create_dir_all(dir.join("my code")).unwrap();
        let editor = dir.join("record.sh");
        let record = dir.join("args.txt");
        fs::write(
            &editor,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                record.display()
            ),
        )
        .unwrap();
        let file = dir.join("my code").join("main file.rs");
        fs::write(&file, "").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        open(
            file.clone(),
            Some(format!("/bin/sh \"{}\" --wait", editor.display())),
            move |r| {
                let _ = tx.send(r.map_err(|e| e.to_string()));
            },
        );
        rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(&record).unwrap(),
            format!("--wait\n{}\n", file.display())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_arguments_are_quoted_only_when_needed() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(r"C:\a b\c.toml"), r#""C:\a b\c.toml""#);
        assert_eq!(quote_arg(""), r#""""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
    }
}
