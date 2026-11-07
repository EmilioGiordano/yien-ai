//! JSON encoding of the snapshot, with `schema_version` and migrations (ADR-10, plan §7.2).

use serde_json::Value;

use crate::model::SessionSnapshot;

/// The schema this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// A step that turns a document of schema `from` into one of schema `from + 1`.
///
/// ```
/// use yien_session::Migration;
///
/// fn rename_field(mut doc: serde_json::Value) -> Result<serde_json::Value, String> {
///     let old = doc.get("old").cloned().ok_or("no old field")?;
///     doc["new"] = old;
///     Ok(doc)
/// }
/// let step = Migration { from: 0, run: rename_field };
/// assert_eq!(step.from, 0);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// The schema the step reads.
    pub from: u32,
    /// The step; an `Err` explains why the document cannot be migrated.
    pub run: fn(Value) -> Result<Value, String>,
}

/// The migrations this build knows. Each incompatible schema change adds one, with an
/// example file of the old version under `tests/fixtures/`.
pub const MIGRATIONS: &[Migration] = &[];

/// Why bytes could not become a snapshot.
///
/// ```
/// let err = yien_session::deserialize(b"{}").unwrap_err();
/// assert!(matches!(err, yien_session::CodecError::NoVersion));
/// ```
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The bytes are not JSON, or the JSON does not fit the schema.
    #[error("invalid snapshot JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// `schema_version` is missing or not an unsigned 32-bit integer.
    #[error("missing or invalid schema_version")]
    NoVersion,
    /// The document comes from a newer YIEN; it must not be overwritten.
    #[error("schema version {found} is newer than the supported {SCHEMA_VERSION}")]
    NewerSchema {
        /// The version found.
        found: u32,
    },
    /// No migration starts at this version.
    #[error("no migration from schema version {from}")]
    NoMigration {
        /// The version with no step out of it.
        from: u32,
    },
    /// A migration step refused the document.
    #[error("migration from schema version {from} failed: {reason}")]
    Migration {
        /// The version the failing step reads.
        from: u32,
        /// What the step reported.
        reason: String,
    },
}

/// The `schema_version` of a parsed document, if it has a valid one.
///
/// ```
/// let doc = serde_json::json!({ "schema_version": 3 });
/// assert_eq!(yien_session::schema_version_of(&doc), Some(3));
/// ```
pub fn schema_version_of(doc: &Value) -> Option<u32> {
    doc.get("schema_version")?
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
}

/// Encodes a snapshot as pretty JSON.
///
/// ```
/// let snapshot = yien_session::SessionSnapshot::new(7, Vec::new());
/// let bytes = yien_session::serialize(&snapshot).unwrap();
/// assert_eq!(yien_session::deserialize(&bytes).unwrap(), snapshot);
/// ```
pub fn serialize(snapshot: &SessionSnapshot) -> Result<Vec<u8>, CodecError> {
    Ok(serde_json::to_vec_pretty(snapshot)?)
}

/// Decodes a snapshot, migrating older schemas with [`MIGRATIONS`]. It does not validate.
///
/// ```
/// let err = yien_session::deserialize(br#"{"schema_version": 99}"#).unwrap_err();
/// assert!(matches!(err, yien_session::CodecError::NewerSchema { found: 99 }));
/// ```
pub fn deserialize(bytes: &[u8]) -> Result<SessionSnapshot, CodecError> {
    deserialize_with(bytes, MIGRATIONS)
}

/// [`deserialize`] with an explicit migration table.
///
/// ```
/// let bytes = br#"{"schema_version": 1, "saved_at": 0, "projects": []}"#;
/// let snapshot = yien_session::deserialize_with(bytes, &[]).unwrap();
/// assert!(snapshot.projects.is_empty());
/// ```
pub fn deserialize_with(
    bytes: &[u8],
    migrations: &[Migration],
) -> Result<SessionSnapshot, CodecError> {
    let doc: Value = serde_json::from_slice(bytes)?;
    let from = schema_version_of(&doc).ok_or(CodecError::NoVersion)?;
    migrate_with(doc, from, migrations)
}

/// Brings a document of schema `from` to [`SCHEMA_VERSION`] with [`MIGRATIONS`].
///
/// ```
/// let doc = serde_json::json!({ "schema_version": 1, "saved_at": 0, "projects": [] });
/// assert!(yien_session::migrate(doc, 1).is_ok());
/// ```
pub fn migrate(doc: Value, from: u32) -> Result<SessionSnapshot, CodecError> {
    migrate_with(doc, from, MIGRATIONS)
}

/// [`migrate`] with an explicit migration table, applied one version at a time.
///
/// ```
/// let doc = serde_json::json!({ "schema_version": 0 });
/// let err = yien_session::migrate_with(doc, 0, &[]).unwrap_err();
/// assert!(matches!(err, yien_session::CodecError::NoMigration { from: 0 }));
/// ```
pub fn migrate_with(
    mut doc: Value,
    from: u32,
    migrations: &[Migration],
) -> Result<SessionSnapshot, CodecError> {
    if from > SCHEMA_VERSION {
        return Err(CodecError::NewerSchema { found: from });
    }
    let mut version = from;
    while version < SCHEMA_VERSION {
        let step = migrations
            .iter()
            .find(|m| m.from == version)
            .ok_or(CodecError::NoMigration { from: version })?;
        doc = (step.run)(doc).map_err(|reason| CodecError::Migration {
            from: version,
            reason,
        })?;
        version += 1;
    }
    if let Value::Object(map) = &mut doc {
        map.insert("schema_version".into(), SCHEMA_VERSION.into());
    }
    Ok(serde_json::from_value(doc)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_ses_01_bad_bytes_are_errors_not_panics() {
        for bytes in [
            &b""[..],
            b"not json",
            b"[]",
            b"{\"schema_version\": \"1\"}",
            b"{\"schema_version\": -1}",
            b"{\"schema_version\": 4294967296}",
            b"{\"schema_version\": 1}",
            b"{\"schema_version\": 1, \"saved_at\": 0, \"projects\": [{}]}",
        ] {
            assert!(
                deserialize(bytes).is_err(),
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn a_failing_step_names_its_version() {
        fn refuse(_: Value) -> Result<Value, String> {
            Err("no".into())
        }
        let table = [Migration {
            from: 0,
            run: refuse,
        }];
        let err = migrate_with(serde_json::json!({}), 0, &table).unwrap_err();
        assert!(matches!(err, CodecError::Migration { from: 0, .. }));
    }

    #[test]
    fn the_current_version_needs_no_migration() {
        let doc = serde_json::json!({ "schema_version": 1, "saved_at": 9, "projects": [] });
        assert_eq!(migrate(doc, 1).unwrap().saved_at, 9);
    }
}
