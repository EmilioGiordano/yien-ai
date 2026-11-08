//! `ShellExecuteExW`: the file's default app, Notepad, or the configured
//! editor. Every program starts from an absolute path, so the shell never
//! looks for it in the current folder first.

#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_NO_ASSOCIATION, WIN32_ERROR};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::UI::Shell::{
    SE_ERR_NOASSOC, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use super::{batch_safe, command_params, is_batch, quote_path};

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Why a launch failed.
enum Failure {
    NoAssociation,
    Other(String),
}

/// A program to start: its absolute path, its command line and the folder
/// it starts in (`None`: ours).
struct Launch {
    file: PathBuf,
    params: String,
    directory: Option<PathBuf>,
}

impl Launch {
    fn run(&self) -> Result<(), Failure> {
        execute(
            self.file.as_os_str(),
            Some(&self.params),
            self.directory.as_deref(),
        )
    }
}

/// Runs on the caller's own thread, which it initialises for COM as
/// `ShellExecuteExW` asks (some handlers are COM objects).
pub(super) fn open(path: &Path, command: Option<&[String]>) -> Result<(), String> {
    let editor = match command {
        Some([program, args @ ..]) => Some(editor(program, args, path)?),
        _ => None,
    };
    with_com(|| match &editor {
        Some(launch) => launch.run(),
        None => match execute(path.as_os_str(), None, None) {
            // No app is set for .toml files: Notepad ships with Windows.
            Err(Failure::NoAssociation) => notepad(path).map_err(Failure::Other)?.run(),
            other => other,
        },
    })
}

/// Opens `path` in the system's own Notepad.
pub(super) fn open_in_notepad(path: &Path) -> Result<(), String> {
    let launch = notepad(path)?;
    with_com(|| launch.run())
}

fn with_com(launch: impl FnOnce() -> Result<(), Failure>) -> Result<(), String> {
    // SAFETY: plain call on a thread this module owns; the matching
    // CoUninitialize runs below when it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let result = launch();
    if com.is_ok() {
        // SAFETY: pairs the successful CoInitializeEx above, same thread.
        unsafe { CoUninitialize() };
    }
    result.map_err(|failure| match failure {
        Failure::NoAssociation => "no app is set to open it".to_owned(),
        Failure::Other(why) => why,
    })
}

/// The editor, found on `PATH` by [`yien_which::resolve`].
fn editor(program: &str, args: &[String], path: &Path) -> Result<Launch, String> {
    let path_var = std::env::var_os("PATH");
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| yien_which::DEFAULT_PATHEXT.into());
    editor_launch(program, args, path, path_var.as_deref(), &pathext)
}

fn editor_launch(
    program: &str,
    args: &[String],
    path: &Path,
    path_var: Option<&OsStr>,
    pathext: &str,
) -> Result<Launch, String> {
    let file = yien_which::resolve(OsStr::new(program), path_var, pathext, |p| p.is_file())
        .map_err(|e| match e {
            yien_which::WhichError::Relative => format!(
                "the editor {program} is a relative path, which would resolve against \
                 YIEN's own folder; set its full path in config.toml"
            ),
            _ => format!(
                "the editor {program} was not found on PATH (the current folder is not \
                 searched); set its full path in config.toml"
            ),
        })?;
    if is_batch(&file) && !batch_safe(args, path) {
        // cmd.exe parses a .cmd or .bat's arguments again, and no quoting
        // protects `%`, `!` or `"` from it.
        return Err(format!(
            "{program} is a batch file, and its arguments or the file's path hold a \
             character cmd.exe would interpret"
        ));
    }
    Ok(Launch {
        file,
        params: command_params(args, path),
        directory: None,
    })
}

/// The system folder's notepad.exe, started in the system folder.
fn notepad(path: &Path) -> Result<Launch, String> {
    let system = system_dir()?;
    Ok(Launch {
        file: system.join("notepad.exe"),
        params: quote_path(path),
        directory: Some(system),
    })
}

fn system_dir() -> Result<PathBuf, String> {
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: the buffer is valid for writes of its whole length.
        let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if len == 0 {
            return Err("the Windows system folder could not be found".to_owned());
        }
        if len < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..len])));
        }
        // Too small: `len` is the size it needs, its NUL included.
        buffer.resize(len, 0);
    }
}

/// `SEE_MASK_NOASYNC` because the calling thread ends right after the call;
/// `SEE_MASK_FLAG_NO_UI` so a missing association comes back as an error
/// to handle, not as a dialog.
fn execute(file: &OsStr, params: Option<&str>, directory: Option<&Path>) -> Result<(), Failure> {
    let verb = wide("open");
    let file = wide(file);
    let params = params.map(wide);
    let directory = directory.map(wide);
    let text = |s: &Option<Vec<u16>>| s.as_ref().map_or(PCWSTR::null(), |s| PCWSTR(s.as_ptr()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: text(&params),
        lpDirectory: text(&directory),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` carries its own size, and every string it points to is
    // NUL-terminated and outlives the call.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Ok(()),
        Err(error) => {
            let no_assoc = info.hInstApp.0 as usize == SE_ERR_NOASSOC as usize
                || WIN32_ERROR::from_error(&error) == Some(ERROR_NO_ASSOCIATION);
            if no_assoc {
                Err(Failure::NoAssociation)
            } else {
                Err(Failure::Other(error.message()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the launch only; nothing starts.
    #[test]
    fn notepad_is_the_system_folders_and_starts_there() {
        let launch = notepad(Path::new(r"C:\p\setup.bat")).unwrap();
        let system = launch.directory.clone().unwrap();
        assert!(launch.file.is_absolute(), "{}", launch.file.display());
        assert!(system.is_absolute());
        assert_eq!(launch.file, system.join("notepad.exe"));
        assert!(launch.file.is_file());
        assert_eq!(launch.params, r#""C:\p\setup.bat""#);
    }

    /// Builds the launches only; nothing starts.
    #[test]
    fn a_bare_editor_starts_from_its_absolute_path_on_path_or_not_at_all() {
        let root = std::env::temp_dir().join(format!("yien-editor-launch-{}", std::process::id()));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("ed.exe"), "").unwrap();
        std::fs::write(bin.join("bat.cmd"), "").unwrap();
        let file = Path::new(r"C:\p\a%b.txt");
        let dirs = std::env::join_paths([PathBuf::from("."), PathBuf::new(), bin.clone()]).unwrap();
        let dirs = Some(dirs.as_os_str());
        let launch = editor_launch("ed", &["-w".into()], file, dirs, ".com;.exe").unwrap();
        assert_eq!(launch.file, bin.join("ed.exe"));
        assert!(launch.file.is_absolute());
        assert_eq!(launch.directory, None);
        assert_eq!(launch.params, r#"-w "C:\p\a%b.txt""#);
        let missing = editor_launch("nothere", &[], file, dirs, ".EXE");
        assert!(missing.is_err_and(|e| e.contains("not found on PATH")));
        // Found, but a batch file cannot take a path holding `%`.
        assert!(editor_launch("bat", &[], file, dirs, ".EXE;.CMD").is_err());
        let relative = editor_launch(r"bin\ed.exe", &[], file, dirs, ".EXE");
        assert!(relative.is_err_and(|e| e.contains("relative path")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
