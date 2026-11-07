//! The saved file and its backups: atomic save, rotation, and recovery of the newest valid
//! generation (RF-SES-06, ADR-10, plan §5.2 and §7).

use serde_json::Value;

use crate::codec::{CodecError, SCHEMA_VERSION, deserialize, schema_version_of, serialize};
use crate::model::SessionSnapshot;
use crate::store::{Store, StoreError};
use crate::validate::{Problem, validate};

/// The newest generation's file name.
pub const CURRENT_FILE: &str = "workspace.json";

/// How many backups sit behind [`CURRENT_FILE`]: `workspace.1.json` (newest) to
/// `workspace.4.json`, five copies in all.
pub const BACKUPS: usize = 4;

/// Where [`save`] keeps a damaged [`CURRENT_FILE`] for diagnosis (plan §5.2), replacing any
/// earlier one.
pub const DAMAGED_FILE: &str = "workspace.damaged.json";

/// The least time between the snapshot in `workspace.1.json` and the one that pushes it
/// down, in milliseconds. Saves in between only replace [`CURRENT_FILE`].
pub const ROTATION_INTERVAL_MS: u64 = 60_000;

/// The file name of a generation: 0 is [`CURRENT_FILE`], 1 to [`BACKUPS`] the backups.
///
/// ```
/// assert_eq!(yien_session::generation_name(0), "workspace.json");
/// assert_eq!(yien_session::generation_name(2), "workspace.2.json");
/// ```
pub fn generation_name(generation: usize) -> String {
    if generation == 0 {
        CURRENT_FILE.to_owned()
    } else {
        format!("workspace.{generation}.json")
    }
}

/// Why [`save`] wrote nothing, or stopped part way (earlier generations stay loadable).
///
/// ```
/// let err = yien_session::SaveError::NewerSchema { name: "workspace.json".into(), found: 2 };
/// assert!(err.to_string().contains("newer"));
/// ```
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The snapshot breaks an invariant, so it is not written.
    #[error("the snapshot breaks {} invariant(s)", .0.len())]
    Invalid(Vec<Problem>),
    /// A generation comes from a newer YIEN; call [`preserve_newer`] before saving.
    #[error("{name} was written by a newer YIEN (schema {found}) and is not overwritten")]
    NewerSchema {
        /// The file holding it.
        name: String,
        /// Its schema version.
        found: u32,
    },
    /// The snapshot could not be encoded.
    #[error(transparent)]
    Encode(#[from] CodecError),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What [`save`] did.
///
/// ```
/// let mut store = yien_session::MemStore::new();
/// let snapshot = yien_session::SessionSnapshot::new(0, Vec::new());
/// let saved = yien_session::save(&mut store, &snapshot).unwrap();
/// assert!(!saved.rotated);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Saved {
    /// Whether the backups were rotated before writing.
    pub rotated: bool,
}

/// Writes the snapshot as [`CURRENT_FILE`], atomically.
///
/// The previous file is first pushed down to `workspace.1.json` when the snapshot is
/// [`ROTATION_INTERVAL_MS`] away from the one in `workspace.1.json`; the oldest backup then
/// drops off. A damaged previous file is moved to [`DAMAGED_FILE`] instead, for diagnosis,
/// and the backups stay as they are. Refuses to write an invalid snapshot, and refuses while any
/// generation holds a newer schema, since rotation would move or drop it. A backup the store
/// cannot read does not block the save: it is treated as not newer and rotated like the others;
/// only an unreadable [`CURRENT_FILE`] is an error.
///
/// ```
/// use yien_session::{MemStore, SessionSnapshot, load, save};
///
/// let mut store = MemStore::new();
/// save(&mut store, &SessionSnapshot::new(1, Vec::new())).unwrap();
/// assert_eq!(load(&store).restored.unwrap().snapshot.saved_at, 1);
/// ```
pub fn save(store: &mut impl Store, snapshot: &SessionSnapshot) -> Result<Saved, SaveError> {
    validate(snapshot).map_err(SaveError::Invalid)?;
    let bytes = serialize(snapshot)?;

    let mut present = [false; BACKUPS + 1];
    let mut current_healthy = false;
    let mut backup_saved_at = None;
    for (generation, slot) in present.iter_mut().enumerate() {
        let name = generation_name(generation);
        let old = match store.read(&name) {
            Ok(Some(old)) => old,
            Ok(None) => continue,
            Err(err) if generation == 0 => return Err(err.into()),
            Err(_) => {
                *slot = true;
                continue;
            }
        };
        *slot = true;
        let doc = serde_json::from_slice::<Value>(&old).ok();
        if let Some(found) = doc.as_ref().and_then(schema_version_of)
            && found > SCHEMA_VERSION
        {
            return Err(SaveError::NewerSchema { name, found });
        }
        match generation {
            0 => current_healthy = deserialize(&old).is_ok_and(|s| validate(&s).is_ok()),
            1 => backup_saved_at = doc.and_then(|d| d.get("saved_at")?.as_u64()),
            _ => {}
        }
    }

    if present[0] && !current_healthy {
        store.rename(CURRENT_FILE, DAMAGED_FILE)?;
        present[0] = false;
    }
    let due = backup_saved_at
        .is_none_or(|at: u64| at.abs_diff(snapshot.saved_at) >= ROTATION_INTERVAL_MS);
    let rotated = present[0] && due;
    if rotated {
        for generation in (1..BACKUPS).rev() {
            if present[generation] {
                store.rename(
                    &generation_name(generation),
                    &generation_name(generation + 1),
                )?;
            }
        }
        store.rename(CURRENT_FILE, &generation_name(1))?;
    }
    store.write_atomic(CURRENT_FILE, &bytes)?;
    Ok(Saved { rotated })
}

/// The generation [`load`] restored.
#[derive(Debug, Clone, PartialEq)]
pub struct Restored {
    /// Which generation: 0 is [`CURRENT_FILE`].
    pub generation: usize,
    /// Its snapshot, valid and in the current schema.
    pub snapshot: SessionSnapshot,
}

/// Why [`load`] passed over a generation.
#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// The store could not read it.
    Unreadable(String),
    /// It is not a snapshot: bad JSON, no version, wrong shape, or a failed migration.
    Corrupt(String),
    /// It decodes but breaks invariants.
    Invalid(Vec<Problem>),
    /// It comes from a newer YIEN; loading stops there.
    NewerSchema(u32),
}

/// One generation [`load`] passed over.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    /// Which generation: 0 is [`CURRENT_FILE`].
    pub generation: usize,
    /// Why.
    pub reason: SkipReason,
}

/// What [`load`] found.
///
/// ```
/// let report = yien_session::load(&yien_session::MemStore::new());
/// assert!(report.restored.is_none() && report.skipped.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadReport {
    /// The newest valid generation, if any.
    pub restored: Option<Restored>,
    /// The newer generations passed over, newest first. Missing files are not listed.
    pub skipped: Vec<Skipped>,
}

impl LoadReport {
    /// The generation and schema version of a file from a newer YIEN, if loading stopped
    /// at one. The caller warns, opens without restoring, and calls [`preserve_newer`].
    ///
    /// ```
    /// let report = yien_session::load(&yien_session::MemStore::new());
    /// assert_eq!(report.newer_schema(), None);
    /// ```
    pub fn newer_schema(&self) -> Option<(usize, u32)> {
        self.skipped.iter().find_map(|s| match s.reason {
            SkipReason::NewerSchema(found) => Some((s.generation, found)),
            _ => None,
        })
    }
}

/// Returns the newest generation that decodes and validates, reporting the ones skipped.
///
/// Stops without restoring at a file from a newer YIEN, so older state is never shown over
/// it. Never fails: a store error only skips that generation.
///
/// ```
/// use yien_session::{MemStore, load};
///
/// let mut store = MemStore::new();
/// store.insert("workspace.json", b"not json".to_vec());
/// let report = load(&store);
/// assert!(report.restored.is_none());
/// assert_eq!(report.skipped.len(), 1);
/// ```
pub fn load(store: &impl Store) -> LoadReport {
    let mut report = LoadReport::default();
    for generation in 0..=BACKUPS {
        let reason = match store.read(&generation_name(generation)) {
            Ok(None) => continue,
            Err(err) => SkipReason::Unreadable(err.to_string()),
            Ok(Some(bytes)) => match deserialize(&bytes) {
                Ok(snapshot) => match validate(&snapshot) {
                    Ok(()) => {
                        report.restored = Some(Restored {
                            generation,
                            snapshot,
                        });
                        return report;
                    }
                    Err(problems) => SkipReason::Invalid(problems),
                },
                Err(CodecError::NewerSchema { found }) => {
                    report.skipped.push(Skipped {
                        generation,
                        reason: SkipReason::NewerSchema(found),
                    });
                    return report;
                }
                Err(err) => SkipReason::Corrupt(err.to_string()),
            },
        };
        report.skipped.push(Skipped { generation, reason });
    }
    report
}

/// Renames every generation from a newer YIEN to `workspace.v<N>.json` (adding `.1`, `.2`
/// … when taken) so [`save`] can proceed without losing it. Returns the new names.
///
/// ```
/// use yien_session::{MemStore, Store, preserve_newer};
///
/// let mut store = MemStore::new();
/// store.insert("workspace.json", br#"{"schema_version": 9}"#.to_vec());
/// assert_eq!(preserve_newer(&mut store).unwrap(), vec!["workspace.v9.json"]);
/// assert_eq!(store.read("workspace.json").unwrap(), None);
/// ```
pub fn preserve_newer(store: &mut impl Store) -> Result<Vec<String>, StoreError> {
    let mut moved = Vec::new();
    for generation in 0..=BACKUPS {
        let name = generation_name(generation);
        let Some(bytes) = store.read(&name)? else {
            continue;
        };
        let doc = serde_json::from_slice::<Value>(&bytes).ok();
        let Some(found) = doc.as_ref().and_then(schema_version_of) else {
            continue;
        };
        if found <= SCHEMA_VERSION {
            continue;
        }
        let target = free_preserved_name(store, found)?;
        store.rename(&name, &target)?;
        moved.push(target);
    }
    Ok(moved)
}

fn free_preserved_name(store: &impl Store, version: u32) -> Result<String, StoreError> {
    let mut name = format!("workspace.v{version}.json");
    let mut copy = 0u32;
    while store.read(&name)?.is_some() {
        copy += 1;
        name = format!("workspace.v{version}.{copy}.json");
    }
    Ok(name)
}
