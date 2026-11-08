//! The settings file on disk: written once with commented defaults, read
//! with a size limit, watched by polling, opened in an editor (RF-CFG-01).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
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
/// [`read`] returns; the watch ends when `on_change` returns `false`.
/// `applied` is the stamp of the version the caller already has.
///
/// ```no_run
/// use yien_platform::config_file;
///
/// let file = yien_platform::paths::config_file()?;
/// let seen = config_file::stamp(&file);
/// config_file::watch(file, seen, |text| {
///     println!("changed: {:?}", text.map(|t| t.map(|t| t.len())));
///     true
/// })?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn watch(
    path: PathBuf,
    applied: FileStamp,
    mut on_change: impl FnMut(io::Result<Option<String>>) -> bool + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("yien-config-watch".into())
        .spawn(move || {
            let start = Instant::now();
            let mut gate = ReloadGate::new(applied, SETTLE);
            loop {
                std::thread::sleep(POLL);
                if gate.observe(stamp(&path), start.elapsed()) == Reload::Apply
                    && !on_change(read(&path))
                {
                    return;
                }
            }
        })
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
}

/// Opens `path` in `editor` (a command line, the file appended as its last
/// argument), or in the OS default app for it when `editor` is `None`. Runs
/// on a thread of its own and returns at once; `done` gets the outcome.
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
        .spawn(move || done(open_now(&path, command.as_deref())));
    if let Err(e) = spawned {
        tracing::warn!("could not start the thread that opens the file: {e}");
    }
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
    fn a_bad_editor_line_fails_at_once() {
        let (tx, rx) = std::sync::mpsc::channel();
        open(PathBuf::from("x.toml"), Some("\"ed".into()), move |r| {
            let _ = tx.send(r);
        });
        assert!(matches!(rx.recv().unwrap(), Err(OpenError::BadEditor(_))));
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
