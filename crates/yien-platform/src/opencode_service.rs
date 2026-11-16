//! Where OpenCode's background service registers itself, read and never
//! written (RNF-14): the file is opened for reading only, sharing read, write
//! and delete, so OpenCode is never held up.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::agent_sessions::{AGENT_HOME_ENV, open_shared};

/// The service's file under a state folder.
const UNDER_STATE: &str = "opencode/service.json";

/// The files OpenCode's service may register in, in the order to try them:
/// under `$YIEN_AGENT_HOME` when set (and only there, so a throwaway profile
/// never reaches the user's own); otherwise under `$XDG_STATE_HOME` when it
/// is set to an absolute path, then under `~/.local/state`.
///
/// ```no_run
/// for file in yien_platform::opencode_service::service_files() {
///     assert!(file.ends_with("opencode/service.json"));
/// }
/// ```
pub fn service_files() -> Vec<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    service_files_from(
        var(AGENT_HOME_ENV),
        var("XDG_STATE_HOME"),
        crate::paths::home_dir(),
    )
}

fn service_files_from(
    agent_home: Option<OsString>,
    xdg_state: Option<OsString>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let state_of = |home: &Path| home.join(".local").join("state").join(UNDER_STATE);
    if let Some(agent_home) = agent_home {
        return vec![state_of(Path::new(&agent_home))];
    }
    let xdg = xdg_state
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(UNDER_STATE));
    xdg.into_iter()
        .chain(home.as_deref().map(state_of))
        .collect()
}

/// Reads at most `cap + 1` bytes of the service file at `path`, so a caller
/// with a cap of `cap` can tell a longer file. `None` when there is none.
///
/// ```no_run
/// use yien_platform::opencode_service::{read_service_file, service_files};
///
/// for file in service_files() {
///     if let Some(bytes) = read_service_file(&file, 16 * 1024)? {
///         println!("{} bytes", bytes.len());
///     }
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn read_service_file(path: &Path, cap: usize) -> io::Result<Option<Vec<u8>>> {
    let file = match open_shared(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rnf_14_a_throwaway_agent_home_is_the_only_place_looked_at() {
        let files = service_files_from(
            Some("C:/fake-home".into()),
            Some("C:/state".into()),
            Some(PathBuf::from("C:/Users/me")),
        );
        assert_eq!(
            files,
            vec![PathBuf::from(
                "C:/fake-home/.local/state/opencode/service.json"
            )]
        );
    }

    #[test]
    fn xdg_state_comes_first_then_the_home_folder() {
        // "C:/state" is relative on Unix, so the absolute paths follow the host.
        let abs = |p: &str| {
            PathBuf::from(if cfg!(windows) {
                format!("C:{p}")
            } else {
                p.to_owned()
            })
        };
        let home = Some(abs("/Users/me"));
        let both = service_files_from(None, Some(abs("/state").into()), home.clone());
        assert_eq!(
            both,
            vec![
                abs("/state/opencode/service.json"),
                abs("/Users/me/.local/state/opencode/service.json"),
            ]
        );
        // A relative XDG_STATE_HOME is ignored, as the XDG rules say.
        let relative = service_files_from(None, Some("state".into()), home);
        assert_eq!(
            relative,
            vec![abs("/Users/me/.local/state/opencode/service.json")]
        );
        assert!(service_files_from(None, None, None).is_empty());
    }

    #[test]
    fn a_service_file_is_read_up_to_one_byte_past_the_cap() {
        let dir = std::env::temp_dir().join(format!("yien-oc-service-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("service.json");
        assert_eq!(read_service_file(&file, 8).unwrap(), None);
        std::fs::write(&file, b"0123456789").unwrap();
        assert_eq!(
            read_service_file(&file, 100).unwrap().as_deref(),
            Some(&b"0123456789"[..])
        );
        assert_eq!(
            read_service_file(&file, 4).unwrap().as_deref(),
            Some(&b"01234"[..])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
