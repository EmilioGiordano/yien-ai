//! The production session store on real files.

use std::fs;
use std::path::PathBuf;

use yien_platform::state_store::FileStore;
use yien_session::{CURRENT_FILE, SessionSnapshot, Store, load, save};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("yien-state-store-{name}"));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn rf_ses_01_writes_replace_the_file_and_leave_no_temp() {
    let dir = scratch("write");
    let mut store = FileStore::new(dir.join("state"));
    assert_eq!(store.read("a.json").unwrap(), None);
    store.write_atomic("a.json", b"one").unwrap();
    store.write_atomic("a.json", b"two").unwrap();
    assert_eq!(store.read("a.json").unwrap().as_deref(), Some(&b"two"[..]));
    assert!(!dir.join("state").join("a.json.tmp").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rename_replaces_an_existing_target() {
    let dir = scratch("rename");
    let mut store = FileStore::new(&dir);
    store.write_atomic("a", b"new").unwrap();
    store.write_atomic("b", b"old").unwrap();
    store.rename("a", "b").unwrap();
    assert_eq!(store.read("a").unwrap(), None);
    assert_eq!(store.read("b").unwrap().as_deref(), Some(&b"new"[..]));
    assert!(store.rename("missing", "b").is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rnf_14_names_outside_the_directory_are_refused() {
    let mut store = FileStore::new(scratch("names"));
    for name in ["", ".", "..", "../x", "a/b", "a\\b", "C:x"] {
        assert!(store.write_atomic(name, b"x").is_err(), "{name}");
        assert!(store.read(name).is_err(), "{name}");
    }
    assert!(store.rename("a", "../b").is_err());
}

#[test]
fn rf_ses_06_generations_rotate_through_the_real_files() {
    let dir = scratch("generations");
    let mut store = FileStore::new(&dir);
    for minute in 0..7u64 {
        save(
            &mut store,
            &SessionSnapshot::new(minute * 60_000, Vec::new()),
        )
        .unwrap();
    }
    let restored = load(&store).restored.unwrap();
    assert_eq!(restored.generation, 0);
    assert_eq!(restored.snapshot.saved_at, 6 * 60_000);
    assert!(dir.join(CURRENT_FILE).exists());
    assert!(dir.join("workspace.4.json").exists());
    assert!(!dir.join("workspace.5.json").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rf_ses_10_files_are_listed_removed_and_read_within_a_limit() {
    let dir = scratch("list");
    let mut store = FileStore::new(&dir);
    assert!(store.list().unwrap().is_empty());
    store.remove("missing").unwrap();
    store.write_atomic("2.vt", b"two").unwrap();
    store.write_atomic("1.vt", b"one!").unwrap();
    fs::create_dir_all(dir.join("sub")).unwrap();
    assert_eq!(store.list().unwrap(), vec!["1.vt", "2.vt"]);
    assert_eq!(
        store.read_at_most("1.vt", 4).unwrap().as_deref(),
        Some(&b"one!"[..])
    );
    assert_eq!(store.read_at_most("1.vt", 3).unwrap(), None);
    assert_eq!(store.read_at_most("3.vt", 3).unwrap(), None);
    store.remove("1.vt").unwrap();
    assert_eq!(store.list().unwrap(), vec!["2.vt"]);
    assert!(store.remove("../2.vt").is_err());
    assert!(store.read_at_most("a/b", 9).is_err());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rnf_14_a_leftover_temp_file_is_replaced_not_reused() {
    let dir = scratch("leftover");
    let mut store = FileStore::new(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.json.tmp"),
        b"stale and much longer than the new bytes",
    )
    .unwrap();
    store.write_atomic("a.json", b"new").unwrap();
    assert_eq!(store.read("a.json").unwrap().as_deref(), Some(&b"new"[..]));
    assert!(!dir.join("a.json.tmp").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn rnf_14_a_link_planted_as_the_temp_file_is_not_followed() {
    let dir = scratch("link");
    let outside = scratch("link-outside");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let victim = outside.join("victim");
    fs::write(&victim, b"keep").unwrap();
    std::os::unix::fs::symlink(&victim, dir.join("a.json.tmp")).unwrap();
    FileStore::new(&dir).write_atomic("a.json", b"new").unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&outside);
}
