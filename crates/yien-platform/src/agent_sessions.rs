//! Listing a project's past agent sessions from each CLI's own files
//! (RF-SES-07, 04 §4.6), with the reader its profile declares.
//!
//! Strictly read-only (RNF-14): folders are listed and files opened for
//! reading, sharing read, write and delete, so a CLI writing or removing a
//! session at the same time is never held up. Nothing is created, written,
//! locked or renamed.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use yien_agent::SessionReader;
use yien_proto::same_folder;
use yien_session::{
    SESSION_READ_BUDGET, SessionSummary, claude_dir_matches, is_session_id, read_claude_session,
};

/// Stands in for the user's home folder when looking up every CLI's data:
/// `<it>/.claude`, `<it>/.codex`. For tests and throwaway profiles.
pub const AGENT_HOME_ENV: &str = "YIEN_AGENT_HOME";

/// The most sessions one listing returns, newest first.
pub const MAX_SESSIONS: usize = 200;

/// Where each CLI keeps its data. `None` lists nothing for that CLI.
///
/// ```
/// use std::path::Path;
/// use yien_platform::agent_sessions::CliDirs;
///
/// let dirs = CliDirs::under_home(Path::new("C:/fake-home"));
/// assert_eq!(dirs.claude.as_deref(), Some(Path::new("C:/fake-home/.claude")));
/// assert_eq!(CliDirs::default().claude, None);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliDirs {
    /// Claude Code's folder, which holds `projects/`.
    pub claude: Option<PathBuf>,
    /// Codex's folder, which holds `sessions/`.
    pub codex: Option<PathBuf>,
}

impl CliDirs {
    /// The folders each CLI uses under `home`.
    pub fn under_home(home: &Path) -> Self {
        Self {
            claude: Some(home.join(".claude")),
            codex: Some(home.join(".codex")),
        }
    }

    /// The folders the CLIs use for this user: under `$YIEN_AGENT_HOME` when
    /// set and not empty; otherwise where each CLI's own variable
    /// (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) points, or under the home folder.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        if let Some(home) = var(AGENT_HOME_ENV) {
            return Self::under_home(Path::new(&home));
        }
        let home = crate::paths::home_dir();
        let pick = |own: &str, under: &str| {
            var(own)
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(under)))
        };
        Self {
            claude: pick("CLAUDE_CONFIG_DIR", ".claude"),
            codex: pick("CODEX_HOME", ".codex"),
        }
    }
}

/// One past session as its CLI left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastSession {
    /// The CLI's session id; always UUID-shaped.
    pub id: String,
    /// When its file last changed, in milliseconds since the Unix epoch.
    pub modified_ms: u64,
    /// What its file says. Its `Debug` leaves the title out.
    pub summary: SessionSummary,
}

/// Why a listing failed. A missing folder is not a failure: it lists none.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    /// A CLI's folder exists but could not be read.
    #[error("could not read {path}: {source}")]
    Read {
        /// The folder.
        path: String,
        /// What the OS said.
        #[source]
        source: io::Error,
    },
}

/// The past sessions of the project at `root`, newest first, at most `cap`,
/// read with `reader` from the folders in `dirs`.
///
/// ```no_run
/// use yien_agent::SessionReader;
/// use yien_platform::agent_sessions::{CliDirs, MAX_SESSIONS, list};
///
/// let sessions = list(SessionReader::ClaudeProjects, &CliDirs::from_env(), "C:/Code/yien", MAX_SESSIONS)?;
/// for s in &sessions {
///     println!("{} {}", s.id, s.modified_ms);
/// }
/// # Ok::<(), yien_platform::agent_sessions::ListError>(())
/// ```
pub fn list(
    reader: SessionReader,
    dirs: &CliDirs,
    root: &str,
    cap: usize,
) -> Result<Vec<PastSession>, ListError> {
    match reader {
        SessionReader::ClaudeProjects => match &dirs.claude {
            Some(dir) => list_claude(&dir.join("projects"), root, cap),
            None => Ok(Vec::new()),
        },
        // Not read yet: the format and the resume arguments are unverified.
        SessionReader::CodexSessions => Ok(Vec::new()),
    }
}

/// Claude Code's sessions of `root` under `projects`: every `<id>.jsonl` in
/// the folders whose name encodes `root`, newest first. Only the newest
/// `cap` files are opened; one whose first folder is another project (the
/// folder encoding is lossy) is left out.
fn list_claude(projects: &Path, root: &str, cap: usize) -> Result<Vec<PastSession>, ListError> {
    let folders = match fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ListError::Read {
                path: projects.display().to_string(),
                source,
            });
        }
    };
    let mut files: HashMap<String, (u64, PathBuf)> = HashMap::new();
    for folder in folders.flatten() {
        let is_dir = folder.file_type().is_ok_and(|t| t.is_dir());
        let matches = folder
            .file_name()
            .to_str()
            .is_some_and(|name| claude_dir_matches(name, root, cfg!(windows)));
        if !is_dir || !matches {
            continue;
        }
        let Ok(entries) = fs::read_dir(folder.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = session_id_of(&path) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let modified = meta.modified().map_or(0, unix_ms);
            let newer = files.get(&id).is_none_or(|(m, _)| modified > *m);
            if newer {
                files.insert(id, (modified, path));
            }
        }
    }
    let mut newest: Vec<(String, u64, PathBuf)> = files
        .into_iter()
        .map(|(id, (modified, path))| (id, modified, path))
        .collect();
    newest.sort_by_key(|(id, modified, _)| (Reverse(*modified), id.clone()));
    newest.truncate(cap);
    let mut sessions = Vec::with_capacity(newest.len());
    for (id, modified_ms, path) in newest {
        // A file removed or unreadable since the listing is left out.
        let Ok(summary) =
            open_shared(&path).and_then(|f| read_claude_session(f, SESSION_READ_BUDGET))
        else {
            continue;
        };
        if summary
            .cwd
            .as_deref()
            .is_some_and(|cwd| !same_folder(cwd, root))
        {
            continue;
        }
        sessions.push(PastSession {
            id,
            modified_ms,
            summary,
        });
    }
    Ok(sessions)
}

/// The session id a `<id>.jsonl` file name carries.
fn session_id_of(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    if !ext.eq_ignore_ascii_case("jsonl") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    is_session_id(stem).then(|| stem.to_owned())
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Opens `path` for reading only, letting others read, write, delete and
/// rename it meanwhile.
fn open_shared(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    options.open(path)
}
