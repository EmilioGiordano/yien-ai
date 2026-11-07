//! One YIEN per data dir (RF-SES-14): a second start on the same data dir
//! asks the running one to show its window and exits before it touches the
//! saved session. Different data dirs (`YIEN_DATA_DIR`) run side by side.
//!
//! On Windows the claim is a named mutex and the request goes through a
//! named pipe, both named after [`instance_key`]. Elsewhere a lock file in
//! the data dir keeps the claim, and the running instance is not told.
//!
//! ```no_run
//! use yien_platform::single_instance::{self, Claim};
//!
//! let dir = yien_platform::paths::data_dir()?;
//! match single_instance::claim(&dir)? {
//!     Claim::First(instance) => {
//!         instance.serve(|request| println!("show, activate: {}", request.activate));
//!         // ... run the app while `instance` lives ...
//!     }
//!     Claim::Running { delivered } => println!("already running, told: {delivered}"),
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::path::Path;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
use windows as imp;

#[cfg(not(windows))]
mod imp {
    use std::fs::{File, OpenOptions, TryLockError};
    use std::path::Path;

    use super::{Claim, InstanceError, ShowRequest};

    pub(super) struct Guard {
        _lock: File,
    }

    pub(super) fn claim(dir: &Path, _key: &str) -> Result<Claim, InstanceError> {
        let os = |call: &'static str| {
            move |e: std::io::Error| InstanceError {
                call,
                code: e.raw_os_error().unwrap_or(0),
            }
        };
        std::fs::create_dir_all(dir).map_err(os("create_dir_all"))?;
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("instance.lock"))
            .map_err(os("open"))?;
        match file.try_lock() {
            Ok(()) => Ok(Claim::First(super::Instance {
                guard: Guard { _lock: file },
            })),
            Err(TryLockError::WouldBlock) => Ok(Claim::Running { delivered: false }),
            Err(TryLockError::Error(e)) => Err(os("flock")(e)),
        }
    }

    impl Guard {
        pub(super) fn serve(self, _on_show: Box<dyn Fn(ShowRequest) + Send>) -> Self {
            self
        }
    }

    pub(super) fn show_window(_hwnd: isize, _request: ShowRequest) {}
}

/// What [`claim`] found.
pub enum Claim {
    /// No other YIEN runs on this data dir. Keep the [`Instance`] for as long
    /// as the app runs: dropping it gives the claim up.
    First(Instance),
    /// Another YIEN runs on this data dir. `delivered` says whether it got
    /// the request to show its window.
    Running {
        /// The running instance received the request.
        delivered: bool,
    },
}

/// This process's claim on its data dir.
pub struct Instance {
    guard: imp::Guard,
}

impl Instance {
    /// Calls `on_show`, on a thread of its own, each time a later start on
    /// the same data dir asks for the window. The thread lives until the
    /// process ends.
    pub fn serve(self, on_show: impl Fn(ShowRequest) + Send + 'static) -> Self {
        Self {
            guard: self.guard.serve(Box::new(on_show)),
        }
    }
}

/// A later start asking the running instance for its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowRequest {
    /// The later start let this process take the foreground. False when it
    /// could not, or when it was itself started not to take the foreground:
    /// the window then only flashes.
    pub activate: bool,
}

/// An OS call refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{call} failed with OS error {code}")]
pub struct InstanceError {
    /// The failing call.
    pub call: &'static str,
    /// The OS error code.
    pub code: i32,
}

/// Claims `data_dir` for this process, or asks the YIEN that holds it to
/// show its window.
pub fn claim(data_dir: &Path) -> Result<Claim, InstanceError> {
    let absolute = std::path::absolute(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    imp::claim(&absolute, &instance_key(&absolute))
}

/// Brings this process's window (a Win32 `HWND`) forward for `request`:
/// to the front when it may take the foreground, otherwise flashing its
/// taskbar button. Does nothing outside Windows.
pub fn show_window(hwnd: isize, request: ShowRequest) {
    imp::show_window(hwnd, request);
}

/// The name both starts derive from the data dir: 16 hex digits of a stable
/// hash (FNV-1a), so the same folder written another way (case, separators,
/// a trailing separator, the `\\?\` prefix) gives the same name.
///
/// ```
/// use std::path::Path;
/// use yien_platform::single_instance::instance_key;
///
/// let key = instance_key(Path::new("D:/yien/data"));
/// assert_eq!(key.len(), 16);
/// assert_ne!(key, instance_key(Path::new("D:/yien/other")));
/// ```
pub fn instance_key(data_dir: &Path) -> String {
    let normal = normalize(&data_dir.to_string_lossy(), cfg!(windows));
    format!("{:016x}", fnv1a(normal.as_bytes()))
}

/// The data dir in one spelling. Windows paths ignore case and take either
/// separator.
fn normalize(path: &str, windows: bool) -> String {
    if !windows {
        let trimmed = path.trim_end_matches('/');
        return if trimmed.is_empty() { path } else { trimmed }.to_owned();
    }
    let path = path.replace('/', "\\");
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    path.trim_end_matches('\\').to_lowercase()
}

/// 64-bit FNV-1a; unlike `DefaultHasher`, the same in every Rust release.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_ses_14_one_folder_written_many_ways_is_one_key_on_windows() {
        let same = [
            r"C:\Users\Me\AppData\Local\YIEN",
            r"c:\users\me\appdata\local\yien\",
            "C:/Users/Me/AppData/Local/YIEN/",
            r"\\?\C:\Users\Me\AppData\Local\YIEN",
        ];
        let keys: Vec<String> = same.iter().map(|p| normalize(p, true)).collect();
        assert!(keys.windows(2).all(|w| w[0] == w[1]), "{keys:?}");
    }

    #[test]
    fn rf_ses_14_different_data_dirs_get_different_keys() {
        let a = instance_key(Path::new("D:/yien-compiled/slot-1/run/data-a"));
        let b = instance_key(Path::new("D:/yien-compiled/slot-1/run/data-b"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn unix_paths_keep_their_case_and_root() {
        assert_ne!(
            normalize("/home/Me/yien", false),
            normalize("/home/me/yien", false)
        );
        assert_eq!(normalize("/home/me/yien/", false), "/home/me/yien");
        assert_eq!(normalize("/", false), "/");
    }

    #[test]
    fn the_hash_is_the_published_fnv1a() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
