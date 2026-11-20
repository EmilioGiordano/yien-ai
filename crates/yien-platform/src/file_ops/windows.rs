//! The Windows calls behind the Files view's actions: a move that never
//! replaces, the Recycle Bin, and Explorer with the entry selected.

#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    SHFILEOPSTRUCTW, SHFileOperationW,
};
use windows::core::PCWSTR;
use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Moves `from` to `to`; an existing `to` fails with `AlreadyExists`
/// (`ERROR_ALREADY_EXISTS`), in one step.
pub(super) fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = wide(from);
    let to = wide(to);
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call;
    // flags 0 has no MOVEFILE_REPLACE_EXISTING, so a target is never replaced.
    let moved = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) };
    if moved == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `path` as the shell takes it: without the `\\?\` prefix `canonicalize`
/// adds, which `SHFileOperationW` and Explorer do not read.
fn shell_path(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

/// Moves `path` to the Recycle Bin. The shell shows no progress and asks
/// nothing, except when the item is too big for the bin and would be
/// deleted for good: then it asks first (`FOF_WANTNUKEWARNING`).
pub(super) fn move_to_recycle_bin(path: &Path) -> io::Result<()> {
    if path.to_str().is_none() {
        return Err(io::Error::other("its name is not valid Unicode"));
    }
    let mut from: Vec<u16> = shell_path(path).as_os_str().encode_wide().collect();
    // The list of paths ends with two NULs.
    from.extend([0, 0]);
    let flags = FOF_ALLOWUNDO.0
        | FOF_NOCONFIRMATION.0
        | FOF_SILENT.0
        | FOF_NOERRORUI.0
        | FOF_WANTNUKEWARNING.0;
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: flags as u16,
        ..Default::default()
    };
    let code = with_com(|| {
        // SAFETY: `op` is fully initialised, `pFrom` points at a
        // double-NUL-terminated list that outlives the call, and no other
        // field holds a pointer.
        unsafe { SHFileOperationW(&mut op) }
    });
    if code != 0 {
        return Err(io::Error::other(format!(
            "the shell answered {code:#x}; nothing was moved"
        )));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err(io::Error::other("it was cancelled"));
    }
    Ok(())
}

/// Runs `call` with COM set up on this thread, as the shell's file
/// operations expect.
fn with_com<T>(call: impl FnOnce() -> T) -> T {
    // SAFETY: plain call on the caller's thread; the matching CoUninitialize
    // runs below when it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let result = call();
    if com.is_ok() {
        // SAFETY: balances the successful CoInitializeEx above, same thread.
        unsafe { CoUninitialize() };
    }
    result
}

/// Opens Explorer on the folder holding `path` with `path` selected.
pub(super) fn select_in_explorer(path: &Path) -> io::Result<()> {
    let target = shell_path(path);
    let text = target
        .to_str()
        .ok_or_else(|| io::Error::other("its name is not valid Unicode"))?;
    let explorer = windows_dir()?.join("explorer.exe");
    // Explorer reads `/select,` and the quoted path as one argument; a
    // Windows path cannot hold `"`.
    let mut child = std::process::Command::new(explorer)
        .raw_arg(format!("/select,\"{text}\""))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Explorer exits at once, with 1 even when it worked.
    let _ = child.wait();
    Ok(())
}

fn windows_dir() -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 260];
    loop {
        // SAFETY: the buffer is valid for writes of its whole length.
        let len = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if len < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..len])));
        }
        // Too small: `len` is the size it needs, its NUL included.
        buffer.resize(len, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::shell_path;
    use std::path::Path;

    #[test]
    fn the_shell_gets_paths_without_the_verbatim_prefix() {
        assert_eq!(
            shell_path(Path::new(r"\\?\C:\Code\a b.txt")),
            Path::new(r"C:\Code\a b.txt")
        );
        assert_eq!(
            shell_path(Path::new(r"\\?\UNC\server\share\x")),
            Path::new(r"\\server\share\x")
        );
        assert_eq!(shell_path(Path::new(r"C:\x")), Path::new(r"C:\x"));
    }
}
