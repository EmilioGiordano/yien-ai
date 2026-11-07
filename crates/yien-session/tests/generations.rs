//! Saving and loading through a store: rotation, recovery, newer schemas and crashes.

mod common;

use common::sample;
use yien_session::{
    BACKUPS, CURRENT_FILE, DAMAGED_FILE, MemStore, ROTATION_INTERVAL_MS, SaveError, SkipReason,
    Store, StoreError, generation_name, load, preserve_newer, save, serialize,
};

const MINUTE: u64 = ROTATION_INTERVAL_MS;

fn saved_at(store: &MemStore, generation: usize) -> Option<u64> {
    let bytes = store.read(&generation_name(generation)).ok()??;
    let doc: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    doc.get("saved_at")?.as_u64()
}

#[test]
fn rf_ses_02_a_saved_snapshot_loads_back_whole() {
    let mut store = MemStore::new();
    let snapshot = sample(1_000);
    save(&mut store, &snapshot).unwrap();
    let report = load(&store);
    assert!(report.skipped.is_empty());
    let restored = report.restored.unwrap();
    assert_eq!(restored.generation, 0);
    assert_eq!(restored.snapshot, snapshot);
}

#[test]
fn rf_ses_01_an_invalid_snapshot_is_not_written() {
    let mut store = MemStore::new();
    let mut snapshot = sample(0);
    snapshot.projects[0].tabs[0].focused = yien_proto::PaneId::new(99);
    assert!(matches!(
        save(&mut store, &snapshot),
        Err(SaveError::Invalid(_))
    ));
    assert!(store.names().is_empty());
}

#[test]
fn rf_ses_06_saves_within_a_minute_replace_only_the_current_file() {
    let mut store = MemStore::new();
    assert!(!save(&mut store, &sample(0)).unwrap().rotated);
    // The first save after the initial one seeds workspace.1.json.
    assert!(save(&mut store, &sample(250)).unwrap().rotated);
    for at in [500, 750, MINUTE - 1] {
        assert!(!save(&mut store, &sample(at)).unwrap().rotated);
    }
    assert_eq!(store.names(), vec!["workspace.1.json", CURRENT_FILE]);
    assert_eq!(saved_at(&store, 0), Some(MINUTE - 1));
    assert_eq!(saved_at(&store, 1), Some(0));
}

#[test]
fn rf_ses_06_keeps_five_generations_a_minute_apart() {
    let mut store = MemStore::new();
    save(&mut store, &sample(0)).unwrap();
    for minute in 1..=10 {
        assert!(save(&mut store, &sample(minute * MINUTE)).unwrap().rotated);
    }
    let names: Vec<String> = (0..=BACKUPS).map(generation_name).collect();
    assert_eq!(store.names().len(), BACKUPS + 1);
    for name in &names {
        assert!(store.names().contains(&name.as_str()), "{name}");
    }
    for generation in 0..=BACKUPS {
        assert_eq!(
            saved_at(&store, generation),
            Some((10 - generation as u64) * MINUTE)
        );
    }
}

#[test]
fn rf_ses_06_a_clock_jump_back_still_rotates() {
    let mut store = MemStore::new();
    save(&mut store, &sample(10 * MINUTE)).unwrap();
    save(&mut store, &sample(10 * MINUTE + 1)).unwrap();
    assert!(save(&mut store, &sample(MINUTE)).unwrap().rotated);
}

#[test]
fn rf_ses_06_a_corrupt_newest_generation_falls_back_to_the_previous() {
    let mut store = MemStore::new();
    save(&mut store, &sample(0)).unwrap();
    save(&mut store, &sample(MINUTE)).unwrap();
    store.insert(
        CURRENT_FILE,
        b"{\"schema_version\": 1, \"saved_at\": tr".to_vec(),
    );

    let report = load(&store);
    let restored = report.restored.unwrap();
    assert_eq!(restored.generation, 1);
    assert_eq!(restored.snapshot, sample(0));
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].generation, 0);
    assert!(matches!(report.skipped[0].reason, SkipReason::Corrupt(_)));
}

#[test]
fn rf_ses_06_an_invalid_generation_is_skipped_with_its_problems() {
    let mut store = MemStore::new();
    save(&mut store, &sample(0)).unwrap();
    save(&mut store, &sample(MINUTE)).unwrap();
    let mut broken = sample(2 * MINUTE);
    broken.projects[0].tabs[1].panes.clear();
    store.insert(CURRENT_FILE, serialize(&broken).unwrap());

    let report = load(&store);
    assert_eq!(report.restored.unwrap().generation, 1);
    assert!(matches!(&report.skipped[0].reason, SkipReason::Invalid(p) if !p.is_empty()));
}

#[test]
fn rf_ses_06_a_damaged_current_file_is_kept_when_saving() {
    let mut store = MemStore::new();
    for minute in 0..=5 {
        save(&mut store, &sample(minute * MINUTE)).unwrap();
    }
    store.insert(CURRENT_FILE, b"garbage".to_vec());
    assert!(!save(&mut store, &sample(5 * MINUTE + 1)).unwrap().rotated);
    assert_eq!(
        store.read(DAMAGED_FILE).unwrap().as_deref(),
        Some(&b"garbage"[..])
    );
    for generation in 1..=BACKUPS {
        assert_eq!(
            saved_at(&store, generation),
            Some((5 - generation as u64) * MINUTE),
            "backup {generation} untouched"
        );
    }
    assert_eq!(
        load(&store).restored.unwrap().snapshot,
        sample(5 * MINUTE + 1)
    );
}

#[test]
fn rf_ses_06_with_every_generation_damaged_nothing_is_restored() {
    let mut store = MemStore::new();
    for generation in 0..=BACKUPS {
        store.insert(&generation_name(generation), b"{}".to_vec());
    }
    let report = load(&store);
    assert!(report.restored.is_none());
    assert_eq!(report.skipped.len(), BACKUPS + 1);
}

#[test]
fn rf_ses_06_a_leftover_temp_file_is_ignored() {
    let mut store = MemStore::new();
    save(&mut store, &sample(0)).unwrap();
    store.insert("workspace.json.tmp", b"half writ".to_vec());
    let report = load(&store);
    assert!(report.skipped.is_empty());
    assert_eq!(report.restored.unwrap().snapshot, sample(0));
}

#[test]
fn rf_ses_06_a_newer_schema_stops_loading_and_blocks_saving() {
    let mut store = MemStore::new();
    save(&mut store, &sample(0)).unwrap();
    save(&mut store, &sample(MINUTE)).unwrap();
    let newer = br#"{"schema_version": 2, "saved_at": 0, "projects": [], "extra": true}"#;
    store.insert(CURRENT_FILE, newer.to_vec());

    let report = load(&store);
    assert!(report.restored.is_none(), "older state must not be shown");
    assert_eq!(report.newer_schema(), Some((0, 2)));

    let err = save(&mut store, &sample(2 * MINUTE)).unwrap_err();
    assert!(matches!(err, SaveError::NewerSchema { found: 2, .. }));
    assert_eq!(
        store.read(CURRENT_FILE).unwrap().as_deref(),
        Some(&newer[..])
    );
}

#[test]
fn rf_ses_06_a_newer_schema_in_a_backup_also_blocks_saving() {
    let mut store = MemStore::new();
    save(&mut store, &sample(0)).unwrap();
    let newer = br#"{"schema_version": 3}"#;
    store.insert(&generation_name(BACKUPS), newer.to_vec());
    let err = save(&mut store, &sample(MINUTE)).unwrap_err();
    assert!(
        matches!(err, SaveError::NewerSchema { found: 3, ref name } if *name == generation_name(BACKUPS))
    );
}

#[test]
fn rf_ses_06_a_preserved_newer_file_survives_and_saving_resumes() {
    let mut store = MemStore::new();
    let newer = br#"{"schema_version": 2}"#;
    store.insert("workspace.v2.json", b"an earlier copy".to_vec());
    store.insert(CURRENT_FILE, newer.to_vec());

    assert_eq!(
        preserve_newer(&mut store).unwrap(),
        vec!["workspace.v2.1.json"]
    );
    assert_eq!(
        store.read("workspace.v2.1.json").unwrap().as_deref(),
        Some(&newer[..])
    );
    assert_eq!(
        store.read("workspace.v2.json").unwrap().as_deref(),
        Some(&b"an earlier copy"[..])
    );
    save(&mut store, &sample(0)).unwrap();
    assert_eq!(load(&store).restored.unwrap().snapshot, sample(0));
}

/// A crash at every write or rename of a rotating save leaves a loadable state that is
/// either the old snapshot or the new one.
#[test]
fn rnf_10_a_crash_at_any_step_of_a_save_keeps_a_valid_generation() {
    let old = sample(5 * MINUTE);
    let new = sample(6 * MINUTE);
    let mut base = MemStore::new();
    for minute in 0..=5 {
        save(&mut base, &sample(minute * MINUTE)).unwrap();
    }

    let mut steps = 0;
    loop {
        let mut store = base.clone().crash_after(steps);
        let finished = save(&mut store, &new).is_ok();
        let restored = load(&store)
            .restored
            .expect("a generation survives")
            .snapshot;
        if finished {
            assert_eq!(restored, new);
            break;
        }
        assert_eq!(restored, old, "crash after {steps} step(s)");
        steps += 1;
    }
    assert_eq!(steps, BACKUPS + 1, "four renames, then the write");
}

#[test]
fn rnf_09_every_save_lands_in_the_current_file() {
    let mut store = MemStore::new();
    for at in (0..20).map(|i| i * 250) {
        save(&mut store, &sample(at)).unwrap();
        assert_eq!(load(&store).restored.unwrap().snapshot.saved_at, at);
    }
}

/// A store whose reads of one file fail, as when another process holds it.
struct UnreadableFile {
    inner: MemStore,
    name: String,
}

impl Store for UnreadableFile {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        if name == self.name {
            return Err(StoreError::new(
                "read",
                name,
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
        }
        self.inner.read(name)
    }

    fn write_atomic(&mut self, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
        self.inner.write_atomic(name, bytes)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), StoreError> {
        self.inner.rename(from, to)
    }
}

fn filled_store() -> MemStore {
    let mut store = MemStore::new();
    for minute in 0..=5 {
        save(&mut store, &sample(minute * MINUTE)).unwrap();
    }
    store
}

#[test]
fn rnf_10_an_unreadable_backup_does_not_block_saving() {
    let mut store = UnreadableFile {
        inner: filled_store(),
        name: generation_name(3),
    };
    let saved = save(&mut store, &sample(6 * MINUTE)).unwrap();
    assert!(saved.rotated);
    assert_eq!(
        load(&store.inner).restored.unwrap().snapshot,
        sample(6 * MINUTE)
    );
    assert_eq!(
        saved_at(&store.inner, 4),
        Some(2 * MINUTE),
        "the unreadable backup moved down"
    );
}

#[test]
fn rnf_10_an_unreadable_current_file_fails_the_save() {
    let mut store = UnreadableFile {
        inner: filled_store(),
        name: CURRENT_FILE.into(),
    };
    assert!(matches!(
        save(&mut store, &sample(6 * MINUTE)),
        Err(SaveError::Store(_))
    ));
    assert_eq!(saved_at(&store.inner, 0), Some(5 * MINUTE));
}

/// The damaged-current path renames the damaged file, then writes: a crash at either step
/// still restores the newest good backup, and finishing restores the new snapshot.
#[test]
fn rnf_10_a_crash_while_setting_a_damaged_file_aside_keeps_a_valid_generation() {
    let mut base = filled_store();
    base.insert(CURRENT_FILE, b"garbage".to_vec());
    let new = sample(5 * MINUTE + 1);

    let mut steps = 0;
    loop {
        let mut store = base.clone().crash_after(steps);
        let finished = save(&mut store, &new).is_ok();
        let restored = load(&store).restored.expect("a generation survives");
        if finished {
            assert_eq!(restored.snapshot, new);
            assert_eq!(
                store.read(DAMAGED_FILE).unwrap().as_deref(),
                Some(&b"garbage"[..])
            );
            break;
        }
        assert_eq!(restored.generation, 1, "crash after {steps} step(s)");
        assert_eq!(restored.snapshot, sample(4 * MINUTE));
        steps += 1;
    }
    assert_eq!(steps, 2, "one rename, then the write");
}

#[test]
fn rf_ses_06_a_newer_backup_is_preserved_and_saving_resumes() {
    let mut store = filled_store();
    let newer = br#"{"schema_version": 2, "saved_at": 0}"#;
    store.insert(&generation_name(3), newer.to_vec());
    assert!(matches!(
        save(&mut store, &sample(6 * MINUTE)),
        Err(SaveError::NewerSchema { .. })
    ));

    assert_eq!(
        preserve_newer(&mut store).unwrap(),
        vec!["workspace.v2.json"]
    );
    assert_eq!(store.read(&generation_name(3)).unwrap(), None);
    assert_eq!(
        store.read("workspace.v2.json").unwrap().as_deref(),
        Some(&newer[..])
    );
    save(&mut store, &sample(6 * MINUTE)).unwrap();
    assert_eq!(load(&store).restored.unwrap().snapshot, sample(6 * MINUTE));
}

#[test]
fn rf_ses_06_several_newer_generations_are_all_preserved() {
    let mut store = filled_store();
    let docs: [(usize, &[u8]); 3] = [
        (0, br#"{"schema_version": 2, "n": 0}"#),
        (1, br#"{"schema_version": 3, "n": 1}"#),
        (3, br#"{"schema_version": 2, "n": 3}"#),
    ];
    for (generation, bytes) in docs {
        store.insert(&generation_name(generation), bytes.to_vec());
    }

    let moved = preserve_newer(&mut store).unwrap();
    assert_eq!(
        moved,
        vec![
            "workspace.v2.json",
            "workspace.v3.json",
            "workspace.v2.1.json"
        ]
    );
    for ((_, bytes), name) in docs.iter().zip(&moved) {
        assert_eq!(store.read(name).unwrap().as_deref(), Some(*bytes));
    }
    for generation in [0, 1, 3] {
        assert_eq!(store.read(&generation_name(generation)).unwrap(), None);
    }

    let report = load(&store);
    assert_eq!(report.newer_schema(), None);
    assert_eq!(report.restored.unwrap().snapshot, sample(3 * MINUTE));
    save(&mut store, &sample(6 * MINUTE)).unwrap();
    assert_eq!(load(&store).restored.unwrap().snapshot, sample(6 * MINUTE));
}
