//! Installs the global subscriber, so everything lives in one test: tests in a
//! binary share the process and only the first install can succeed.

use std::path::PathBuf;

use yien_platform::{LogError, init_logging_with};

fn fresh_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn adr_16_logs_go_to_a_file_at_info_and_init_runs_once() {
    let dir = fresh_dir("yien-platform-logging");
    let guard = init_logging_with(&dir.join("logs"), None).unwrap();

    tracing::info!("visible at info");
    tracing::debug!("hidden at info");

    let again = init_logging_with(&dir.join("other"), None);
    assert!(
        matches!(again, Err(LogError::AlreadyInitialized)),
        "{again:?}"
    );

    drop(guard);

    let files: Vec<_> = std::fs::read_dir(dir.join("logs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("yien.") && name.ends_with(".log"),
        "{name}"
    );

    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(text.contains("visible at info"), "{text}");
    assert!(!text.contains("hidden at info"), "{text}");
    assert!(
        !text.contains('\x1b'),
        "the file must not carry ANSI escapes"
    );
}
