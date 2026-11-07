//! Build tasks for the YIEN workspace, run as `cargo xtask <task>`.
//!
//! - `check-deps`: enforces the layer rule on every `crates/*/Cargo.toml`.
//! - `trace [--ers <path>]`: maps requirement ids to the tests named after them.

#![forbid(unsafe_code)]

mod check_deps;
mod trace;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <check-deps | trace [--ers <path>]>";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("xtask: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = workspace_root();
    match args.first().map(String::as_str) {
        Some("check-deps") if args.len() == 1 => check_deps::run(&root),
        Some("trace") => match &args[1..] {
            [] => trace::run(&root, None),
            [flag, path] if flag == "--ers" => trace::run(&root, Some(PathBuf::from(path))),
            _ => usage(),
        },
        _ => usage(),
    }
}

fn usage() -> anyhow::Result<ExitCode> {
    eprintln!("{USAGE}");
    Ok(ExitCode::from(2))
}

/// `cargo run` keeps the caller's working directory, so the root comes from
/// where this crate lives, not from the cwd.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The member crates under `crates/`, sorted by directory name.
fn crate_dirs(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let crates = root.join("crates");
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(&crates)? {
        let path = entry?.path();
        if path.join("Cargo.toml").is_file() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}
