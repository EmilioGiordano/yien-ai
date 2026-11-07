//! Restricts where the process loads DLLs from at run time, so that a DLL
//! planted on `PATH` or in the current directory is never loaded.
//!
//! portable-pty loads `conpty.dll` by bare name before falling back to
//! kernel32's ConPTY; without this, a `conpty.dll` in any `PATH` folder or in
//! the current directory is loaded into YIEN.
//!
//! ```no_run
//! // First thing in `main`, before any thread starts.
//! if let Err(error) = yien_platform::dll_search::restrict_dll_search() {
//!     eprintln!("{error}");
//! }
//! ```

#![allow(unsafe_code)]

/// The OS refused to change the DLL search order.
#[derive(Debug, thiserror::Error)]
#[error("cannot restrict the DLL search path: {0}")]
pub struct DllSearchError(#[source] std::io::Error);

/// Limits run-time DLL loads to the executable's folder and System32, and
/// drops the current directory from the legacy search order.
///
/// Process-wide; call it once, first thing in `main`, before other threads
/// start. A no-op outside Windows.
pub fn restrict_dll_search() -> Result<(), DllSearchError> {
    imp::restrict()
}

#[cfg(windows)]
mod imp {
    use super::DllSearchError;
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        SetDefaultDllDirectories, SetDllDirectoryW,
    };

    pub(super) fn restrict() -> Result<(), DllSearchError> {
        // SAFETY: takes plain flags and touches only the loader's own state.
        let ok = unsafe {
            SetDefaultDllDirectories(
                LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if ok == 0 {
            return Err(DllSearchError(std::io::Error::last_os_error()));
        }
        // An empty string removes the current directory; a null pointer would
        // restore the default order, current directory included.
        let empty: [u16; 1] = [0];
        // SAFETY: `empty` is a valid NUL-terminated wide string for the call.
        let ok = unsafe { SetDllDirectoryW(empty.as_ptr()) };
        if ok == 0 {
            return Err(DllSearchError(std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::DllSearchError;

    pub(super) fn restrict() -> Result<(), DllSearchError> {
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

    const PROBE_ENV: &str = "YIEN_DLL_PROBE";
    const PATH_DLL: &str = "yien_probe_on_path.dll";
    const CWD_DLL: &str = "yien_probe_in_cwd.dll";

    /// Loads `name` by bare name and unloads it again; true when it loaded.
    fn bare_load(name: &str) -> bool {
        let wide: Vec<u16> = OsStr::new(name).encode_wide().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated; the probe DLL is a renamed copy of
        // a System32 DLL, so loading it runs only that DLL's own init.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            return false;
        }
        // SAFETY: `module` was returned by the LoadLibraryW above.
        unsafe { FreeLibrary(module) };
        true
    }

    fn unique_dir(tag: &str) -> std::io::Result<PathBuf> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("yien-dll-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn plant(dir: &Path, name: &str) -> std::io::Result<()> {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let source = Path::new(&system_root).join("System32").join("version.dll");
        std::fs::copy(source, dir.join(name)).map(drop)
    }

    /// The search setting is process-wide, so the check runs in a child copy
    /// of this test binary (see [`planted_dll_child`]) and no other test here
    /// may call [`restrict_dll_search`].
    #[test]
    fn planted_dll_on_path_or_in_cwd_is_not_loaded() {
        let path_dir = unique_dir("path").unwrap();
        let cwd_dir = unique_dir("cwd").unwrap();
        plant(&path_dir, PATH_DLL).unwrap();
        plant(&cwd_dir, CWD_DLL).unwrap();
        let mut path = std::ffi::OsString::from(&path_dir);
        if let Some(old) = std::env::var_os("PATH") {
            path.push(";");
            path.push(old);
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dll_search::tests::planted_dll_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE_ENV, "1")
            .env("PATH", path)
            .current_dir(&cwd_dir)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&path_dir);
        let _ = std::fs::remove_dir_all(&cwd_dir);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "child failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Runs only when started by [`planted_dll_on_path_or_in_cwd_is_not_loaded`].
    #[test]
    fn planted_dll_child() {
        if std::env::var_os(PROBE_ENV).is_none() {
            return;
        }
        assert!(bare_load(PATH_DLL), "control: PATH DLL should load before");
        assert!(bare_load(CWD_DLL), "control: cwd DLL should load before");
        restrict_dll_search().unwrap();
        restrict_dll_search().expect("a repeated call also succeeds");
        assert!(!bare_load(PATH_DLL), "PATH DLL loaded after restricting");
        assert!(!bare_load(CWD_DLL), "cwd DLL loaded after restricting");
    }
}
