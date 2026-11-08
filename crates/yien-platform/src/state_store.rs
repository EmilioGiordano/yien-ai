//! The production [`Store`] for the saved session: one directory of small files,
//! replaced atomically and flushed to disk (plan §7.1, RNF-10).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use yien_session::{Store, StoreError};

/// A [`Store`] over one directory.
///
/// A write goes to `<name>.tmp`, is flushed (`FlushFileBuffers` on Windows), then
/// replaces the target. On Windows the replace is
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`, which returns
/// only once the rename is on disk; elsewhere it is `rename` and an `fsync` of the
/// directory. The directory is created on the first write.
///
/// ```no_run
/// use yien_platform::state_store::FileStore;
/// use yien_session::{SessionSnapshot, load, save};
///
/// let mut store = FileStore::new(yien_platform::paths::state_dir()?);
/// save(&mut store, &SessionSnapshot::new(0, Vec::new()))?;
/// assert!(load(&store).restored.is_some());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// A store over `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory the files live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file's bytes when it holds at most `limit` of them; `None` when it
    /// does not exist or is bigger.
    ///
    /// ```no_run
    /// let store = yien_platform::state_store::FileStore::new("screens");
    /// assert!(store.read_at_most("1.vt", 1024)?.is_none_or(|b| b.len() <= 1024));
    /// # Ok::<(), yien_session::StoreError>(())
    /// ```
    pub fn read_at_most(&self, name: &str, limit: u64) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.path("read", name)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(StoreError::new("read", name, err)),
        };
        let mut bytes = Vec::new();
        io::Read::read_to_end(
            &mut io::Read::take(file, limit.saturating_add(1)),
            &mut bytes,
        )
        .map_err(|err| StoreError::new("read", name, err))?;
        Ok((bytes.len() as u64 <= limit).then_some(bytes))
    }

    /// Removes the file; one that does not exist is not an error.
    ///
    /// ```no_run
    /// let mut store = yien_platform::state_store::FileStore::new("screens");
    /// store.remove("1.vt")?;
    /// # Ok::<(), yien_session::StoreError>(())
    /// ```
    pub fn remove(&mut self, name: &str) -> Result<(), StoreError> {
        let path = self.path("remove", name)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(StoreError::new("remove", name, err)),
        }
    }

    /// The names of the plain files in the directory; none when it does not
    /// exist yet.
    ///
    /// ```no_run
    /// let store = yien_platform::state_store::FileStore::new("screens");
    /// for name in store.list()? {
    ///     println!("{name}");
    /// }
    /// # Ok::<(), yien_session::StoreError>(())
    /// ```
    pub fn list(&self) -> Result<Vec<String>, StoreError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(StoreError::new("list", ".", err)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| StoreError::new("list", ".", err))?;
            if entry.file_type().is_ok_and(|t| t.is_file())
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Only plain file names: nothing outside the directory is ever touched (RNF-14).
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
}

impl Store for FileStore {
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
            // A leftover temp, or a link planted in its place, is removed, never
            // followed: the write lands only in a file it creates.
            if let Err(err) = fs::remove_file(&temp)
                && err.kind() != io::ErrorKind::NotFound
            {
                return Err(err);
            }
            let mut file = File::options().write(true).create_new(true).open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            replace(&temp, &target, &self.dir)
        };
        write().map_err(|err| StoreError::new("write", name, err))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        let source = self.path("rename", from)?;
        let target = self.path("rename", to)?;
        replace(&source, &target, &self.dir)
            .map_err(|err| StoreError::new("rename", &format!("{from} -> {to}"), err))
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn replace(from: &Path, to: &Path, _dir: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both buffers are NUL-terminated UTF-16 that outlive the call.
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(from: &Path, to: &Path, dir: &Path) -> io::Result<()> {
    fs::rename(from, to)?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}
