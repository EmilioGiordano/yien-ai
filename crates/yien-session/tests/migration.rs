//! Schema versions: the frozen v1 format, and the migration mechanism driven by a fake v0.

mod common;

use common::{basic, sample};
use serde_json::{Map, Value, json};
use yien_session::{
    CodecError, MemStore, Migration, SCHEMA_VERSION, deserialize, deserialize_with, load,
    migrate_with, validate,
};

const V0: &[u8] = include_bytes!("fixtures/workspace.v0.json");
const V1: &[u8] = include_bytes!("fixtures/workspace.v1.json");

/// The fake v0 named a pane's directory `directory` and kept the agent session flat in
/// `session` and `ended`.
fn v0_to_v1(mut doc: Value) -> Result<Value, String> {
    let projects = doc
        .get_mut("projects")
        .and_then(Value::as_array_mut)
        .ok_or("no projects")?;
    for project in projects {
        let tabs = project
            .get_mut("tabs")
            .and_then(Value::as_array_mut)
            .ok_or("no tabs")?;
        for tab in tabs {
            let panes = tab
                .get_mut("panes")
                .and_then(Value::as_array_mut)
                .ok_or("no panes")?;
            for pane in panes {
                let pane = pane.as_object_mut().ok_or("pane is not an object")?;
                migrate_pane(pane)?;
            }
        }
    }
    Ok(doc)
}

fn migrate_pane(pane: &mut Map<String, Value>) -> Result<(), String> {
    let cwd = pane.remove("directory").ok_or("pane without directory")?;
    pane.insert("cwd".into(), cwd);
    let agent = match pane.remove("session") {
        Some(session) => json!({
            "kind": pane.get("profile").cloned().unwrap_or(Value::Null),
            "session_id": session,
            "ended_explicitly": pane.remove("ended").unwrap_or(Value::Bool(false)),
        }),
        None => Value::Null,
    };
    pane.insert("agent".into(), agent);
    pane.entry("last_command").or_insert(Value::Null);
    Ok(())
}

const FAKE_TABLE: &[Migration] = &[Migration {
    from: 0,
    run: v0_to_v1,
}];

#[test]
fn rf_ses_01_the_v1_fixture_still_decodes_to_the_same_model() {
    let snapshot = deserialize(V1).unwrap();
    assert_eq!(snapshot, sample(1_000));
    assert_eq!(validate(&snapshot), Ok(()));
}

#[test]
fn rf_ses_02_a_v1_file_without_the_optional_fields_still_loads() {
    let mut doc: Value = serde_json::from_slice(V1).unwrap();
    let root = doc.as_object_mut().unwrap();
    root.remove("selected");
    for project in root["projects"].as_array_mut().unwrap() {
        let project = project.as_object_mut().unwrap();
        project.remove("setup_script");
        for tab in project["tabs"].as_array_mut().unwrap() {
            let tab = tab.as_object_mut().unwrap();
            for key in ["branch", "worktree", "archived"] {
                tab.remove(key);
            }
            for pane in tab["panes"].as_array_mut().unwrap() {
                pane.as_object_mut().unwrap().remove("name");
            }
        }
    }
    let bytes = serde_json::to_vec(&doc).unwrap();
    assert_eq!(deserialize(&bytes).unwrap(), basic(1_000));
}

#[test]
fn rf_ses_02_a_v0_file_migrates_to_the_current_model() {
    let snapshot = deserialize_with(V0, FAKE_TABLE).unwrap();
    assert_eq!(snapshot.schema_version, SCHEMA_VERSION);
    assert_eq!(snapshot, basic(1_000));
}

#[test]
fn rf_ses_02_without_a_step_an_old_file_is_an_error_not_a_guess() {
    let err = deserialize(V0).unwrap_err();
    assert!(matches!(err, CodecError::NoMigration { from: 0 }));

    let mut store = MemStore::new();
    store.insert("workspace.json", V0.to_vec());
    let report = load(&store);
    assert!(report.restored.is_none());
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn a_migration_that_refuses_the_document_is_reported() {
    let mut doc: Value = serde_json::from_slice(V0).unwrap();
    doc["projects"][0]["tabs"][0]["panes"][0]
        .as_object_mut()
        .unwrap()
        .remove("directory");
    let err = migrate_with(doc, 0, FAKE_TABLE).unwrap_err();
    assert!(matches!(err, CodecError::Migration { from: 0, .. }));
}

#[test]
fn rf_ses_02_a_newer_version_is_never_migrated() {
    let doc = json!({ "schema_version": SCHEMA_VERSION + 1 });
    let err = migrate_with(doc, SCHEMA_VERSION + 1, FAKE_TABLE).unwrap_err();
    assert!(matches!(err, CodecError::NewerSchema { found } if found == SCHEMA_VERSION + 1));
}
