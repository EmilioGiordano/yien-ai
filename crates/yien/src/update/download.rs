//! Fetching the releases list and an installer, checking the installer
//! against its published SHA-256, and starting it after YIEN has quit.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

use super::checksum::{CHECKSUM_LIMIT, Hashing, expected_digest, file_digest};
use super::release::{LIST_LIMIT, RELEASES_URL, Release, newest_after, releases, setup_name};
use super::schedule::Saved;

/// The most bytes of an installer downloaded.
pub(crate) const SETUP_LIMIT: u64 = 512 << 20;

/// Gets a URL into a sink; the network in the app, canned answers in tests.
pub(crate) trait Fetch: Send + Sync {
    fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, String>;
}

/// The network, through the platform's HTTPS client.
pub(crate) struct Web {
    /// Allows `http://127.0.0.1`, for the local server tests only.
    pub(crate) loopback: bool,
}

impl Fetch for Web {
    fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, String> {
        let options = yien_platform::http::Options {
            allow_loopback_http: self.loopback,
            timeout: Duration::from_secs(30),
            ..yien_platform::http::Options::new(user_agent(), limit)
        }
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
        yien_platform::http::get(url, &options, sink).map_err(|e| e.to_string())
    }
}

/// The only thing YIEN says about itself.
pub(crate) fn user_agent() -> String {
    format!("YIEN/{}", env!("CARGO_PKG_VERSION"))
}

/// Asks for the releases list; the state to keep, with the newest release
/// above `current`, if any.
pub(crate) fn check(fetch: &dyn Fetch, current: &Version, now: u64) -> Result<Saved, String> {
    let mut body = Vec::new();
    fetch.fetch(RELEASES_URL, LIST_LIMIT, &mut body)?;
    let list = releases(&body).map_err(|e| format!("the releases list did not read: {e}"))?;
    Ok(Saved {
        checked_at: Some(now),
        newest: newest_after(list, current),
    })
}

/// An installer on disk whose SHA-256 matched the published one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Downloaded {
    pub(crate) path: PathBuf,
    pub(crate) digest: String,
}

/// Downloads `release`'s installer into `dir`, which only YIEN uses, and
/// checks it against the release's checksum file. A file that does not
/// match is deleted, never kept or started.
pub(crate) fn download(
    fetch: &dyn Fetch,
    release: &Release,
    dir: &Path,
) -> Result<Downloaded, String> {
    let name = &release.setup.name;
    if *name != setup_name(&release.version) || Version::parse(&release.version).is_err() {
        return Err("the release does not name a YIEN installer".into());
    }
    fs::create_dir_all(dir).map_err(|e| format!("the download folder could not be made: {e}"))?;
    clear(dir);
    let mut sidecar = Vec::new();
    fetch
        .fetch(&release.checksum.url, CHECKSUM_LIMIT, &mut sidecar)
        .map_err(|e| format!("the checksum could not be downloaded: {e}"))?;
    let expected =
        expected_digest(&String::from_utf8_lossy(&sidecar), name).map_err(|e| e.to_string())?;
    let part = dir.join(format!("{name}.part"));
    let file = File::create(&part).map_err(|e| format!("the installer could not be saved: {e}"))?;
    let mut sink = Hashing::new(BufWriter::new(file));
    let fetched = fetch.fetch(&release.setup.url, SETUP_LIMIT, &mut sink);
    let (digest, writer) = sink.finish();
    let saved = fetched.and_then(|_| {
        let file = writer.into_inner().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    });
    if let Err(error) = saved {
        let _ = fs::remove_file(&part);
        return Err(format!("the installer could not be downloaded: {error}"));
    }
    if digest != expected {
        let _ = fs::remove_file(&part);
        return Err(
            "the download did not match its published SHA-256 checksum, so it was deleted".into(),
        );
    }
    let path = dir.join(name);
    fs::rename(&part, &path).map_err(|e| format!("the installer could not be saved: {e}"))?;
    Ok(Downloaded { path, digest })
}

/// Removes earlier downloads; only plain files, never a folder or a link.
fn clear(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The installer's command line: no wizard pages but its progress window,
/// no message boxes, no reboot, a log next to it, and `/YIENUPDATE=1`,
/// which makes it wait for this YIEN to exit and start YIEN again at the
/// end (`installer/yien.iss`). The `/LOG` path is quoted inside the
/// argument, as Inno Setup reads it.
pub(crate) fn installer_args(log: &Path) -> Vec<OsString> {
    let mut log_arg = OsString::from("/LOG=\"");
    log_arg.push(log.as_os_str());
    log_arg.push("\"");
    vec![
        "/SILENT".into(),
        "/SUPPRESSMSGBOXES".into(),
        "/NORESTART".into(),
        "/SP-".into(),
        "/YIENUPDATE=1".into(),
        log_arg,
    ]
}

/// Starts the installer once more checked against its digest, as the user
/// itself, never elevated: a plain process start, which fails rather than
/// asks when a program wants elevation.
pub(crate) fn launch(installer: &Downloaded) -> Result<(), String> {
    let digest = file_digest(&installer.path).map_err(|e| e.to_string())?;
    if digest != installer.digest {
        return Err("the installer changed after it was checked".into());
    }
    let log = installer.path.with_extension("log");
    let mut command = std::process::Command::new(&installer.path);
    command
        .current_dir(installer.path.parent().unwrap_or(Path::new(".")))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        for arg in installer_args(&log) {
            command.raw_arg(arg);
        }
    }
    #[cfg(not(windows))]
    command.args(installer_args(&log));
    command.spawn().map(drop).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::update::release::tests::entry;
    use crate::update::release::{Asset, DOWNLOAD_PREFIX, checksum_name};

    /// Canned answers by URL; records what was asked.
    struct Canned {
        answers: HashMap<String, Result<Vec<u8>, String>>,
        asked: Mutex<Vec<String>>,
    }

    impl Fetch for Canned {
        fn fetch(&self, url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, String> {
            self.asked.lock().unwrap().push(url.to_owned());
            let body = self
                .answers
                .get(url)
                .cloned()
                .unwrap_or(Err("404".into()))?;
            if body.len() as u64 > limit {
                return Err("too long".into());
            }
            sink.write_all(&body).map_err(|e| e.to_string())?;
            Ok(body.len() as u64)
        }
    }

    fn canned(answers: Vec<(&str, Result<Vec<u8>, String>)>) -> Canned {
        Canned {
            answers: answers
                .into_iter()
                .map(|(url, body)| (url.to_owned(), body))
                .collect(),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn release(version: &str) -> Release {
        let base = format!("{DOWNLOAD_PREFIX}v{version}/");
        Release {
            version: version.into(),
            notes: String::new(),
            page: String::new(),
            setup: Asset {
                name: setup_name(version),
                url: format!("{base}{}", setup_name(version)),
            },
            checksum: Asset {
                name: checksum_name(version),
                url: format!("{base}{}", checksum_name(version)),
            },
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yien-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    const PAYLOAD: &[u8] = b"pretend installer";

    fn sha(bytes: &[u8]) -> String {
        let mut h = Hashing::new(Vec::new());
        h.write_all(bytes).unwrap();
        h.finish().0
    }

    #[test]
    fn rnf_21_a_check_keeps_the_newest_release_and_the_time() {
        let json = serde_json::to_vec(&serde_json::Value::Array(vec![
            entry("v0.1.0", false, false),
            entry("v0.3.0", true, false),
            entry("v0.2.0", false, true),
        ]))
        .unwrap();
        let web = canned(vec![(RELEASES_URL, Ok(json))]);
        let saved = check(&web, &Version::parse("0.1.0").unwrap(), 42).unwrap();
        assert_eq!(saved.checked_at, Some(42));
        assert_eq!(saved.newest.unwrap().version, "0.2.0");
        assert_eq!(*web.asked.lock().unwrap(), [RELEASES_URL]);
        let latest = check(&web, &Version::parse("0.2.0").unwrap(), 43).unwrap();
        assert_eq!(latest.newest, None, "up to date");
    }

    #[test]
    fn a_failed_check_is_an_error_not_an_empty_answer() {
        let offline = canned(vec![]);
        assert!(check(&offline, &Version::new(0, 1, 0), 1).is_err());
        let limited = canned(vec![(
            RELEASES_URL,
            Ok(b"{\"message\":\"API rate limit\"}".to_vec()),
        )]);
        assert!(check(&limited, &Version::new(0, 1, 0), 1).is_err());
    }

    #[test]
    fn rnf_21_a_matching_download_is_kept_and_older_ones_removed() {
        let r = release("0.2.0");
        let sidecar = format!("{}  {}\n", sha(PAYLOAD), r.setup.name);
        let web = canned(vec![
            (&r.checksum.url, Ok(sidecar.into_bytes())),
            (&r.setup.url, Ok(PAYLOAD.to_vec())),
        ]);
        let dir = temp("ok");
        fs::create_dir_all(dir.join("keep-folder")).unwrap();
        fs::write(dir.join("yien-0.1.9-x86_64-setup.exe"), b"old").unwrap();
        let got = download(&web, &r, &dir).unwrap();
        assert_eq!(got.path, dir.join("yien-0.2.0-x86_64-setup.exe"));
        assert_eq!(got.digest, sha(PAYLOAD));
        assert_eq!(fs::read(&got.path).unwrap(), PAYLOAD);
        assert!(!dir.join("yien-0.1.9-x86_64-setup.exe").exists());
        assert!(dir.join("keep-folder").is_dir(), "folders are left alone");
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_mismatching_download_is_refused_and_deleted() {
        let r = release("0.2.0");
        let sidecar = format!("{}  {}\n", sha(b"something else"), r.setup.name);
        let web = canned(vec![
            (&r.checksum.url, Ok(sidecar.into_bytes())),
            (&r.setup.url, Ok(PAYLOAD.to_vec())),
        ]);
        let dir = temp("mismatch");
        let error = download(&web, &r, &dir).unwrap_err();
        assert!(
            error.contains("did not match its published SHA-256"),
            "{error}"
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "nothing kept");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_checksum_for_another_file_or_a_failed_download_installs_nothing() {
        let r = release("0.2.0");
        let other = format!("{}  yien-0.1.0-x86_64-setup.exe\n", sha(PAYLOAD));
        let web = canned(vec![
            (&r.checksum.url, Ok(other.into_bytes())),
            (&r.setup.url, Ok(PAYLOAD.to_vec())),
        ]);
        let dir = temp("other");
        assert!(download(&web, &r, &dir).unwrap_err().contains("names"));
        assert_eq!(
            *web.asked.lock().unwrap(),
            std::slice::from_ref(&r.checksum.url)
        );

        let sidecar = format!("{}  {}\n", sha(PAYLOAD), r.setup.name);
        let cut = canned(vec![
            (&r.checksum.url, Ok(sidecar.into_bytes())),
            (&r.setup.url, Err("connection reset".into())),
        ]);
        assert!(
            download(&cut, &r, &dir)
                .unwrap_err()
                .contains("connection reset")
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "no partial file");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_release_that_names_another_file_is_refused() {
        let mut r = release("0.2.0");
        r.setup.name = "..\\..\\evil.exe".into();
        let dir = temp("evil");
        assert!(download(&canned(vec![]), &r, &dir).is_err());
        assert!(!dir.exists(), "nothing written");
    }

    #[test]
    fn the_installer_runs_silently_with_the_update_flag_and_never_elevates() {
        let args = installer_args(Path::new(
            r"C:\Users\A B\AppData\Local\YIEN\state\updates\x.log",
        ));
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "/SILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/SP-",
                "/YIENUPDATE=1",
                r#"/LOG="C:\Users\A B\AppData\Local\YIEN\state\updates\x.log""#,
            ]
        );
        assert!(
            !args.iter().any(|a| a.contains("VERYSILENT")),
            "progress stays visible"
        );
    }

    #[test]
    fn a_changed_installer_is_not_started() {
        let dir = temp("changed");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("yien-0.2.0-x86_64-setup.exe");
        fs::write(&path, b"tampered").unwrap();
        let error = launch(&Downloaded {
            path,
            digest: sha(PAYLOAD),
        })
        .unwrap_err();
        assert!(error.contains("changed"), "{error}");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Download and verify through the real HTTP client, against a local
    /// server; never GitHub.
    #[cfg(windows)]
    #[test]
    fn rnf_21_download_and_verify_through_a_local_server() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let mut r = release("0.2.0");
        r.checksum.url = format!("{base}/{}", r.checksum.name);
        r.setup.url = format!("{base}/{}", r.setup.name);
        let sidecar = format!("{}  {}\n", sha(PAYLOAD), r.setup.name).into_bytes();
        let server = std::thread::spawn(move || {
            for body in [sidecar, PAYLOAD.to_vec()] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap() == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let dir = temp("local-server");
        let got = download(&Web { loopback: true }, &r, &dir).unwrap();
        assert_eq!(fs::read(&got.path).unwrap(), PAYLOAD);
        server.join().unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
