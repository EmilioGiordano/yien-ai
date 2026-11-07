//! `FsStore` against a real directory under the system temp folder.

#![cfg(feature = "fs")]

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use common::sample;
use yien_session::{
    CURRENT_FILE, FsStore, ROTATION_INTERVAL_MS, Store, generation_name, load, save,
};

/// A fresh directory per test, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("yien-session-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn rnf_10_fs_store_saves_rotates_and_loads() {
    let dir = TempDir::new();
    let mut store = FsStore::new(dir.0.join("state"));
    save(&mut store, &sample(0)).unwrap();
    save(&mut store, &sample(ROTATION_INTERVAL_MS)).unwrap();

    assert!(dir.0.join("state").join(CURRENT_FILE).is_file());
    assert!(dir.0.join("state").join(generation_name(1)).is_file());
    assert!(!dir.0.join("state").join("workspace.json.tmp").exists());
    assert_eq!(
        load(&store).restored.unwrap().snapshot,
        sample(ROTATION_INTERVAL_MS)
    );
}

#[test]
fn fs_store_reads_missing_files_as_none_and_replaces_on_rename() {
    let dir = TempDir::new();
    let mut store = FsStore::new(&dir.0);
    assert_eq!(store.read("absent.json").unwrap(), None);
    store.write_atomic("a.json", b"first").unwrap();
    store.write_atomic("b.json", b"second").unwrap();
    store.rename("a.json", "b.json").unwrap();
    assert_eq!(store.read("a.json").unwrap(), None);
    assert_eq!(
        store.read("b.json").unwrap().as_deref(),
        Some(&b"first"[..])
    );
    let err = store.rename("a.json", "c.json").unwrap_err();
    assert_eq!(err.name, "a.json -> c.json");
}

#[test]
fn fs_store_refuses_names_that_leave_its_directory() {
    let dir = TempDir::new();
    let mut store = FsStore::new(&dir.0);
    for name in [
        "",
        ".",
        "..",
        "../x.json",
        "sub/x.json",
        "sub\\x.json",
        "C:x.json",
    ] {
        assert!(store.write_atomic(name, b"x").is_err(), "{name:?}");
        assert!(store.read(name).is_err(), "{name:?}");
    }
}
