//! Requirement traceability (plan §9.3): tests named after a requirement id,
//! and, when the ERS is on this machine, the R1 ids that still have none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The ERS lives outside git, in `<specs>/docs/`.
const ERS_IN_SPECS: &str = "docs/02-ers.md";

/// Names the folder holding `docs/` and `design/`, in the environment or in `.yien-local`.
const SPECS_KEY: &str = "YIEN_SPECS_DIR";

/// One test that names a requirement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TracedTest {
    pub file: String,
    pub name: String,
}

/// The requirement id a test name starts with, in ERS form.
///
/// `rf_pan_05_x` is `RF-PAN-05`; `rnf_03_x`, `rn_02_x` and `cu_05_x` are
/// `RNF-03`, `RN-02` and `CU-05`.
pub fn requirement_id(test_name: &str) -> Option<String> {
    let parts: Vec<&str> = test_name.split('_').collect();
    let is_number = |s: &&str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let is_word = |s: &&str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase());
    match parts.as_slice() {
        ["rf", area, number, ..] if is_word(area) && is_number(number) => {
            Some(format!("RF-{}-{number}", area.to_ascii_uppercase()))
        }
        [kind @ ("rnf" | "rn" | "cu"), number, ..] if is_number(number) => {
            Some(format!("{}-{number}", kind.to_ascii_uppercase()))
        }
        _ => None,
    }
}

/// Names of the functions marked `#[test]` (or `#[<runtime>::test]`) in a file.
///
/// Handles `#[x::test(...)]`, attributes spread over several lines and an
/// attribute on the same line as the `fn`.
pub fn test_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending_test = false;
    // Open brackets of an attribute that continues on the next line.
    let mut attr_depth = 0usize;
    for line in source.lines() {
        let mut rest = line.trim();
        if attr_depth > 0 {
            match close_brackets(rest, attr_depth) {
                Ok(end) => {
                    attr_depth = 0;
                    rest = rest[end..].trim_start();
                }
                Err(depth) => {
                    attr_depth = depth;
                    continue;
                }
            }
        }
        while let Some(attr) = rest.strip_prefix("#[") {
            let path: String = attr
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            if path == "test" || path.ends_with("::test") {
                pending_test = true;
            }
            match close_brackets(attr, 1) {
                Ok(end) => rest = attr[end..].trim_start(),
                Err(depth) => {
                    attr_depth = depth;
                    rest = "";
                }
            }
        }
        if rest.is_empty() || rest.starts_with("//") {
            continue;
        }
        if pending_test && let Some(at) = rest.find("fn ") {
            let name: String = rest[at + 3..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
        pending_test = false;
    }
    names
}

/// Scans `text` with `depth` brackets already open. Returns the byte offset just
/// past the bracket that closes them, or the depth still open at the end.
fn close_brackets(text: &str, mut depth: usize) -> Result<usize, usize> {
    for (i, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(i + 1);
                }
            }
            _ => {}
        }
    }
    Err(depth)
}

/// R1 ids from the ERS tables: RF rows whose release column says R1, and
/// every RNF and RN (the ERS gives them no release column).
pub fn ers_r1_ids(ers: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for line in ers.lines() {
        let Some(row) = line.trim().strip_prefix('|') else {
            continue;
        };
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        let Some(id) = cells.first().copied() else {
            continue;
        };
        let wanted = if id.starts_with("RF-") {
            cells.contains(&"R1")
        } else {
            id.starts_with("RNF-") || id.starts_with("RN-")
        };
        if wanted && is_ers_id(id) {
            ids.insert(id.to_owned());
        }
    }
    ids
}

fn is_ers_id(id: &str) -> bool {
    let mut parts = id.split('-');
    let kind = parts.next();
    let rest: Vec<&str> = parts.collect();
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let word = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_uppercase());
    match (kind, rest.as_slice()) {
        (Some("RF"), [area, n]) => word(area) && number(n),
        (Some("RNF" | "RN"), [n]) => number(n),
        _ => false,
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Every traced test under `crates/`, grouped by requirement id.
pub fn scan(root: &Path) -> anyhow::Result<BTreeMap<String, Vec<TracedTest>>> {
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files)?;
    files.sort();

    let mut by_id: BTreeMap<String, Vec<TracedTest>> = BTreeMap::new();
    for file in files {
        let source = std::fs::read_to_string(&file)?;
        let shown = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        for name in test_names(&source) {
            if let Some(id) = requirement_id(&name) {
                by_id.entry(id).or_default().push(TracedTest {
                    file: shown.clone(),
                    name,
                });
            }
        }
    }
    Ok(by_id)
}

/// The value of `key` in a `.yien-local` file: `KEY=value` lines, `#` comments,
/// optional quotes around the value. The last line wins, as when the shell
/// sources it; an empty value counts as unset.
pub fn local_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
        .map(|value| {
            let value = value.trim();
            ['"', '\'']
                .into_iter()
                .find_map(|q| value.strip_prefix(q)?.strip_suffix(q))
                .unwrap_or(value)
                .to_string()
        })
        .next_back()
        .filter(|value| !value.is_empty())
}

/// Where to look for the ERS, in order: the repo's own `docs/`, then the specs
/// folder (the environment over `.yien-local`), then the main checkout's `docs/`.
pub fn ers_candidates(
    root: &Path,
    specs_env: Option<String>,
    local_file: Option<&str>,
    main_checkout: Option<&Path>,
) -> Vec<PathBuf> {
    let specs = specs_env
        .filter(|dir| !dir.is_empty())
        .or_else(|| local_value(local_file?, SPECS_KEY));
    let mut candidates = vec![root.join(ERS_IN_SPECS)];
    candidates.extend(specs.map(|dir| Path::new(&dir).join(ERS_IN_SPECS)));
    candidates.extend(main_checkout.map(|dir| dir.join(ERS_IN_SPECS)));
    candidates
}

/// The main checkout: the parent of the git dir every worktree shares.
fn main_checkout(root: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    common.parent().map(Path::to_path_buf)
}

fn find_ers(root: &Path, explicit: Option<PathBuf>) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    let main = main_checkout(root);
    let local = main
        .as_deref()
        .and_then(|dir| std::fs::read_to_string(dir.join(".yien-local")).ok());
    ers_candidates(
        root,
        std::env::var(SPECS_KEY).ok(),
        local.as_deref(),
        main.as_deref(),
    )
    .into_iter()
    .find(|p| p.is_file())
}

pub fn run(root: &Path, ers: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let by_id = scan(root)?;
    let width = by_id.keys().map(String::len).max().unwrap_or(0).max(11);

    println!("{:width$}  test", "requirement");
    for (id, tests) in &by_id {
        for t in tests {
            println!("{id:width$}  {} ({})", t.name, t.file);
        }
    }
    let total: usize = by_id.values().map(Vec::len).sum();
    println!("\n{} requirement(s) traced by {total} test(s)", by_id.len());

    match find_ers(root, ers) {
        Some(path) => {
            let text = std::fs::read_to_string(&path)?;
            let missing: Vec<String> = ers_r1_ids(&text)
                .into_iter()
                .filter(|id| !by_id.contains_key(id))
                .collect();
            println!(
                "\nR1 ids in {} with no test yet ({}, information only):",
                path.display(),
                missing.len()
            );
            for chunk in missing.chunks(8) {
                println!("  {}", chunk.join(" "));
            }
        }
        None => println!("\nERS not found; skipping the list of untested R1 ids"),
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functional_ids_have_three_segments() {
        assert_eq!(
            requirement_id("rf_pan_05_drop_docks"),
            Some("RF-PAN-05".into())
        );
        assert_eq!(requirement_id("rf_pan_05"), Some("RF-PAN-05".into()));
        assert_eq!(requirement_id("rf_05_no_area"), None);
        assert_eq!(requirement_id("rf_pan_x5_bad"), None);
    }

    #[test]
    fn rnf_is_not_mistaken_for_rn() {
        assert_eq!(requirement_id("rnf_03_memory"), Some("RNF-03".into()));
        assert_eq!(
            requirement_id("rn_02_unknown_is_idle"),
            Some("RN-02".into())
        );
        assert_eq!(requirement_id("cu_05_attend"), Some("CU-05".into()));
    }

    #[test]
    fn ordinary_names_are_not_requirements() {
        for name in ["splits_evenly", "rfc_parser", "rn_x_y", "run_01", "rf"] {
            assert_eq!(requirement_id(name), None, "{name}");
        }
    }

    #[test]
    fn only_test_functions_are_collected() {
        let source = "
            #[cfg(test)]
            mod tests {
                fn rf_pan_01_helper() {}

                #[test]
                // a comment between attribute and fn
                fn rf_pan_01_splits() {}

                #[tokio::test]
                #[ignore]
                async fn rn_02_async() {}

                #[test]
                fn plain() {}
            }
        ";
        assert_eq!(
            test_names(source),
            vec!["rf_pan_01_splits", "rn_02_async", "plain"]
        );
    }

    #[test]
    fn runtime_test_attributes_with_arguments_count() {
        let source = "
            #[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]
            async fn rf_ses_01_restores() {}

            #[some_crate::test(timeout = 5)]
            fn rnf_09_power_loss() {}

            #[cfg_attr(test, allow(dead_code))]
            fn rf_pan_02_not_a_test() {}
        ";
        assert_eq!(
            test_names(source),
            vec!["rf_ses_01_restores", "rnf_09_power_loss"]
        );
    }

    #[test]
    fn a_test_attribute_on_the_fn_line_counts() {
        let source = "#[test] fn rf_pan_01_a() {}\n#[test] #[ignore] fn rn_01_b() {}\n";
        assert_eq!(test_names(source), vec!["rf_pan_01_a", "rn_01_b"]);
    }

    #[test]
    fn multi_line_attributes_after_test_keep_it_pending() {
        let source = "
            #[test]
            #[should_panic(
                expected = \"boom\"
            )]
            fn rf_pan_03_panics() {}

            #[tokio::test(
                flavor = \"current_thread\",
            )]
            async fn rn_04_ended() {}

            #[allow(
                dead_code
            )]
            fn rf_pan_04_helper() {}
        ";
        assert_eq!(test_names(source), vec!["rf_pan_03_panics", "rn_04_ended"]);
    }

    #[test]
    fn r1_ids_come_from_the_ers_tables() {
        let ers = "\
| ID | Requisito | Prio. | Rel. | Car. |
|---|---|---|---|---|
| RF-PAN-01 | Dividir. | E | R1 | CAR-02 |
| RF-PAN-13 | Pegar imagenes. | D | R3 | CAR-03 |
| RN-02 | Regla. |
| RNF-03 | Memoria. | Medicion. |
| CU-04 | Organizar | Desarrollador | RF-PAN-01 a RF-PAN-05 |
| CAR-02 Paneles | RF-PAN-01 |
Text mentioning RF-PAN-02 outside a table.
";
        let ids: Vec<String> = ers_r1_ids(ers).into_iter().collect();
        assert_eq!(ids, vec!["RF-PAN-01", "RN-02", "RNF-03"]);
    }

    #[test]
    fn local_values_skip_comments_and_strip_quotes() {
        let text = "\
# YIEN_SPECS_DIR=X:/commented
YIEN_SPECS_DIR_OLD=X:/other-key
  YIEN_SPECS_DIR=X:/first\r
YIEN_SPECS_DIR=\"X:/quoted dir\"\t
YIEN_BUILD_ROOT='X:/build'
YIEN_WORKTREES_DIR=
";
        assert_eq!(
            local_value(text, "YIEN_SPECS_DIR").as_deref(),
            Some("X:/quoted dir")
        );
        assert_eq!(
            local_value(text, "YIEN_BUILD_ROOT").as_deref(),
            Some("X:/build")
        );
        assert_eq!(local_value(text, "YIEN_WORKTREES_DIR"), None);
        assert_eq!(local_value(text, "YIEN_BUILD_LOCK"), None);
    }

    #[test]
    fn ers_lookup_prefers_repo_docs_then_env_then_local_file_then_main_checkout() {
        let root = Path::new("X:/wt/topic");
        let main = Path::new("X:/main");
        let local = "YIEN_SPECS_DIR=X:/from-file\n";
        let ers = |dir: &str| Path::new(dir).join("docs/02-ers.md");

        let with_env = ers_candidates(root, Some("X:/from-env".into()), Some(local), Some(main));
        assert_eq!(
            with_env,
            vec![ers("X:/wt/topic"), ers("X:/from-env"), ers("X:/main")]
        );

        let empty_env = ers_candidates(root, Some(String::new()), Some(local), Some(main));
        assert_eq!(
            empty_env,
            vec![ers("X:/wt/topic"), ers("X:/from-file"), ers("X:/main")]
        );

        let nothing = ers_candidates(root, None, None, None);
        assert_eq!(nothing, vec![ers("X:/wt/topic")]);
    }

    #[test]
    fn the_ers_is_found_through_the_specs_dir() {
        let specs = std::env::temp_dir().join(format!("yien-trace-specs-{}", std::process::id()));
        std::fs::create_dir_all(specs.join("docs")).unwrap();
        std::fs::write(specs.join(ERS_IN_SPECS), "| RF-PAN-01 | x | E | R1 |\n").unwrap();
        let empty = specs.join("empty-root");
        let local = format!("{SPECS_KEY}={}\n", specs.display());

        let found = ers_candidates(&empty, None, Some(&local), None)
            .into_iter()
            .find(|p| p.is_file());

        std::fs::remove_dir_all(&specs).unwrap();
        assert_eq!(found, Some(specs.join(ERS_IN_SPECS)));
    }

    #[test]
    fn the_real_workspace_traces_layout_tests() {
        let by_id = scan(&crate::workspace_root()).unwrap();
        let pan01 = by_id.get("RF-PAN-01").expect("RF-PAN-01 is traced");
        assert!(
            pan01
                .iter()
                .any(|t| t.file == "crates/yien-layout/src/lib.rs")
        );
    }
}
