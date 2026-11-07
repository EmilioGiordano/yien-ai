//! Where snapshots live: a few named files in one directory, behind a trait so the crate
//! does no I/O of its own.

use std::collections::BTreeMap;
use std::io;

/// A failed store operation.
///
/// ```
/// let err = yien_session::StoreError::new("read", "workspace.json", std::io::Error::other("busy"));
/// assert!(err.to_string().contains("workspace.json"));
/// ```
#[derive(Debug, thiserror::Error)]
#[error("{op} {name}: {source}")]
pub struct StoreError {
    /// The operation: `read`, `write` or `rename`.
    pub op: &'static str,
    /// The file it was working on.
    pub name: String,
    /// The underlying error.
    #[source]
    pub source: io::Error,
}

impl StoreError {
    /// Builds an error for `op` on the file `name`.
    pub fn new(op: &'static str, name: &str, source: io::Error) -> Self {
        Self {
            op,
            name: name.to_owned(),
            source,
        }
    }
}

/// A flat directory of small files, addressed by plain file names.
///
/// ```
/// use yien_session::{MemStore, Store};
///
/// let mut store = MemStore::new();
/// store.write_atomic("a.json", b"{}").unwrap();
/// store.rename("a.json", "b.json").unwrap();
/// assert_eq!(store.read("a.json").unwrap(), None);
/// assert_eq!(store.read("b.json").unwrap().as_deref(), Some(&b"{}"[..]));
/// ```
pub trait Store {
    /// The file's bytes, or `None` when it does not exist.
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError>;

    /// Replaces the file so a crash leaves either the old bytes or the new ones, never a mix.
    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError>;

    /// Renames `from` to `to`, atomically replacing `to` if it exists. `from` must exist.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError>;
}

/// An in-memory [`Store`] for tests, able to simulate a crash.
///
/// ```
/// use yien_session::{MemStore, Store};
///
/// let mut store = MemStore::new().crash_after(1);
/// assert!(store.write_atomic("a", b"1").is_ok());
/// assert!(store.write_atomic("b", b"2").is_err());
/// assert_eq!(store.read("b").unwrap(), None);
/// ```
#[derive(Debug, Clone, Default)]
pub struct MemStore {
    files: BTreeMap<String, Vec<u8>>,
    changes: usize,
    crash_after: Option<usize>,
}

impl MemStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lets `changes` more writes or renames succeed; every later one fails and changes
    /// nothing, as if the process had died there.
    pub fn crash_after(mut self, changes: usize) -> Self {
        self.crash_after = Some(self.changes + changes);
        self
    }

    /// Puts bytes in place directly, bypassing crash simulation (to plant fixtures).
    ///
    /// ```
    /// use yien_session::{MemStore, Store};
    /// let mut store = MemStore::new();
    /// store.insert("x", b"garbage".to_vec());
    /// assert!(store.read("x").unwrap().is_some());
    /// ```
    pub fn insert(&mut self, name: &str, bytes: Vec<u8>) {
        self.files.insert(name.to_owned(), bytes);
    }

    /// The names of every file, sorted.
    ///
    /// ```
    /// let mut store = yien_session::MemStore::new();
    /// store.insert("b", Vec::new());
    /// store.insert("a", Vec::new());
    /// assert_eq!(store.names(), vec!["a", "b"]);
    /// ```
    pub fn names(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    fn change(&mut self, op: &'static str, name: &str) -> Result<(), StoreError> {
        if self.crash_after.is_some_and(|limit| self.changes >= limit) {
            return Err(StoreError::new(
                op,
                name,
                io::Error::other("simulated crash"),
            ));
        }
        self.changes += 1;
        Ok(())
    }
}

impl Store for MemStore {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.files.get(name).cloned())
    }

    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.change("write", name)?;
        self.files.insert(name.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        if !self.files.contains_key(from) {
            return Err(StoreError::new(
                "rename",
                from,
                io::Error::from(io::ErrorKind::NotFound),
            ));
        }
        self.change("rename", from)?;
        if let Some(bytes) = self.files.remove(from) {
            self.files.insert(to.to_owned(), bytes);
        }
        Ok(())
    }
}

#[cfg(feature = "fs")]
pub use fs_store::FsStore;

#[cfg(feature = "fs")]
mod fs_store {
    use std::fs::{self, File};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};

    use super::{Store, StoreError};

    /// A [`Store`] over one directory, using only `std::fs`.
    ///
    /// Writes go to `<name>.tmp`, are flushed to disk, then renamed over the target. On
    /// Windows `std::fs::rename` replaces the target but cannot ask for write-through, so
    /// `yien-platform` should supply the final store.
    ///
    /// ```no_run
    /// use yien_session::{FsStore, Store};
    ///
    /// let mut store = FsStore::new("C:/Users/me/AppData/Local/YIEN/state");
    /// store.write_atomic("workspace.json", b"{}").unwrap();
    /// ```
    #[derive(Debug, Clone)]
    pub struct FsStore {
        dir: PathBuf,
    }

    impl FsStore {
        /// A store over `dir`, created on the first write.
        pub fn new(dir: impl Into<PathBuf>) -> Self {
            Self { dir: dir.into() }
        }

        /// The directory the files live in.
        pub fn dir(&self) -> &Path {
            &self.dir
        }

        fn path(&self, op: &'static str, name: &str) -> Result<PathBuf, StoreError> {
            let plain =
                !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':']);
            if !plain {
                return Err(StoreError::new(
                    op,
                    name,
                    io::Error::new(io::ErrorKind::InvalidInput, "not a plain file name"),
                ));
            }
            Ok(self.dir.join(name))
        }

        #[cfg(unix)]
        fn sync_dir(&self) -> io::Result<()> {
            File::open(&self.dir)?.sync_all()
        }

        #[cfg(not(unix))]
        fn sync_dir(&self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Store for FsStore {
        fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
            let path = self.path("read", name)?;
            match fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(StoreError::new("read", name, err)),
            }
        }

        fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
            let target = self.path("write", name)?;
            let temp = self.path("write", &format!("{name}.tmp"))?;
            let write = || -> io::Result<()> {
                fs::create_dir_all(&self.dir)?;
                let mut file = File::create(&temp)?;
                file.write_all(bytes)?;
                file.sync_all()?;
                drop(file);
                fs::rename(&temp, &target)?;
                self.sync_dir()
            };
            write().map_err(|err| StoreError::new("write", name, err))
        }

        fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
            let source = self.path("rename", from)?;
            let target = self.path("rename", to)?;
            fs::rename(source, target)
                .and_then(|()| self.sync_dir())
                .map_err(|err| StoreError::new("rename", from, err))
        }
    }
}
