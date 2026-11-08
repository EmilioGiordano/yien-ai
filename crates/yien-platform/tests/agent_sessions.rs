//! Past agent sessions read from fixture CLI folders, never the real ones.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns are not covered by clippy.toml's in-test allowances"
)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use yien_agent::SessionReader;
use yien_platform::agent_sessions::{CliDirs, MAX_SESSIONS, PastSession, list};
use yien_session::claude_project_dir_name;

const ROOT: &str = r"C:\Fake\proj";
const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const C: &str = "cccccccc-0000-4000-8000-000000000003";

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("yien-past-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn prompt(cwd: &str, text: &str) -> String {
    let quote = |s: &str| s.replace('\\', r"\\").replace('"', r#"\""#);
    format!(
        r#"{{"type":"user","cwd":"{}","message":{{"content":"{}"}}}}"#,
        quote(cwd),
        quote(text)
    )
}

/// Writes a session file whose last change was `ago` before now.
fn session(folder: &Path, id: &str, lines: &[String], ago: Duration) -> PathBuf {
    fs::create_dir_all(folder).unwrap();
    let path = folder.join(format!("{id}.jsonl"));
    fs::write(&path, lines.join("\n") + "\n").unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - ago)
        .unwrap();
    path
}

fn claude(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

fn ids(sessions: &[PastSession]) -> Vec<&str> {
    sessions.iter().map(|s| s.id.as_str()).collect()
}

fn hours(n: u64) -> Duration {
    Duration::from_secs(n * 3600)
}

fn read(home: &Path, root: &str) -> Vec<PastSession> {
    list(
        SessionReader::ClaudeProjects,
        &CliDirs::under_home(home),
        root,
        MAX_SESSIONS,
    )
    .unwrap()
}

#[test]
fn rf_ses_07_a_projects_sessions_are_listed_newest_first() {
    let home = scratch("newest");
    let folder = claude(&home).join(claude_project_dir_name(ROOT));
    session(&folder, A, &[prompt(ROOT, "oldest")], hours(48));
    session(
        &folder,
        B,
        &[prompt(ROOT, "newest"), prompt(ROOT, "more")],
        hours(1),
    );
    session(&folder, C, &[prompt(ROOT, "middle")], hours(3));
    // Things in the folder that are not sessions.
    fs::create_dir_all(folder.join(A)).unwrap();
    fs::create_dir_all(folder.join("memory")).unwrap();
    fs::write(folder.join("notes.txt"), "x").unwrap();
    fs::write(folder.join("not-a-session.jsonl"), prompt(ROOT, "x")).unwrap();

    let sessions = read(&home, ROOT);
    assert_eq!(ids(&sessions), [B, C, A]);
    let titles: Vec<_> = sessions
        .iter()
        .map(|s| s.summary.title.as_deref())
        .collect();
    assert_eq!(titles, [Some("newest"), Some("middle"), Some("oldest")]);
    assert_eq!(sessions[0].summary.prompts, Some(2));
    assert!(sessions[0].modified_ms > sessions[1].modified_ms);

    let capped = list(
        SessionReader::ClaudeProjects,
        &CliDirs::under_home(&home),
        ROOT,
        2,
    )
    .unwrap();
    assert_eq!(ids(&capped), [B, C]);
}

#[test]
fn rf_ses_07_another_projects_sessions_are_not_listed() {
    let home = scratch("other");
    let projects = claude(&home);
    session(
        &projects.join(claude_project_dir_name(r"C:\Fake\other")),
        A,
        &[prompt(r"C:\Fake\other", "elsewhere")],
        hours(1),
    );
    session(
        &projects.join(claude_project_dir_name(r"C:\Fake\proj-wt")),
        B,
        &[prompt(r"C:\Fake\proj-wt", "a worktree")],
        hours(1),
    );
    // `proj.x` and `proj-x` share a folder name; the session's own folder
    // tells them apart.
    let shared = projects.join(claude_project_dir_name(r"C:\Fake\proj-x"));
    session(
        &shared,
        C,
        &[prompt(r"C:\Fake\proj.x", "the dotted one")],
        hours(1),
    );
    assert!(read(&home, ROOT).is_empty());
    assert!(read(&home, r"C:\Fake\proj-x").is_empty());
    assert_eq!(ids(&read(&home, r"C:\Fake\proj.x")), [C]);
}

#[cfg(windows)]
#[test]
fn rf_ses_07_folder_names_match_without_case_on_windows() {
    let home = scratch("case");
    let folder = claude(&home).join(claude_project_dir_name(ROOT).to_lowercase());
    session(&folder, A, &[prompt(r"c:\fake\PROJ", "lower")], hours(1));
    assert_eq!(ids(&read(&home, ROOT)), [A]);
}

#[test]
fn rf_ses_07_broken_and_huge_files_still_list() {
    let home = scratch("broken");
    let folder = claude(&home).join(claude_project_dir_name(ROOT));
    session(
        &folder,
        A,
        &[
            "{not json".to_owned(),
            String::new(),
            prompt(ROOT, "after the noise"),
            r#"{"type":"user","message":{"content":"cut o"#.to_owned(),
        ],
        hours(1),
    );
    let filler = r#"{"type":"assistant","message":{"content":"filler filler filler"}}"#;
    let mut huge = vec![prompt(ROOT, "big one")];
    huge.extend(std::iter::repeat_n(filler.to_owned(), 60_000));
    session(&folder, B, &huge, hours(2));
    let mut late = vec![filler.to_owned(); 60_000];
    late.push(prompt(ROOT, "only at the end"));
    session(&folder, C, &late, hours(3));

    let sessions = read(&home, ROOT);
    assert_eq!(ids(&sessions), [A, B, C]);
    assert_eq!(
        sessions[0].summary.title.as_deref(),
        Some("after the noise")
    );
    assert_eq!(sessions[0].summary.prompts, Some(1));
    assert_eq!(sessions[1].summary.title.as_deref(), Some("big one"));
    assert_eq!(
        sessions[1].summary.prompts, None,
        "read only up to the budget"
    );
    assert_eq!(sessions[2].summary.title, None);
}

#[test]
fn rf_ses_07_missing_folders_and_other_readers_list_nothing() {
    let home = scratch("missing");
    assert!(read(&home, ROOT).is_empty());
    let none = list(
        SessionReader::ClaudeProjects,
        &CliDirs::default(),
        ROOT,
        MAX_SESSIONS,
    )
    .unwrap();
    assert!(none.is_empty());
    let folder = home.join(".codex").join("sessions");
    fs::create_dir_all(&folder).unwrap();
    let codex = list(
        SessionReader::CodexSessions,
        &CliDirs::under_home(&home),
        ROOT,
        MAX_SESSIONS,
    )
    .unwrap();
    assert!(codex.is_empty());
}

#[test]
fn rnf_14_listing_changes_nothing_in_the_cli_folder() {
    let home = scratch("untouched");
    let folder = claude(&home).join(claude_project_dir_name(ROOT));
    let path = session(&folder, A, &[prompt(ROOT, "keep me")], hours(5));
    let snapshot = |dir: &Path| {
        let mut entries: Vec<(String, Vec<u8>, SystemTime)> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| {
                let p = e.path();
                let meta = fs::metadata(&p).unwrap();
                let bytes = if meta.is_file() {
                    fs::read(&p).unwrap()
                } else {
                    Vec::new()
                };
                (p.display().to_string(), bytes, meta.modified().unwrap())
            })
            .collect();
        entries.sort();
        entries
    };
    let before = snapshot(&folder);
    assert_eq!(read(&home, ROOT).len(), 1);
    assert_eq!(snapshot(&folder), before);
    // The CLI can still append to and remove the file.
    fs::write(&path, "more").unwrap();
    fs::remove_file(&path).unwrap();
}
