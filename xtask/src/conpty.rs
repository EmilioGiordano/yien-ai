//! Fetches Microsoft's pinned ConPTY package for Windows distributions.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

pub const VERSION: &str = "1.24.261001001";
pub const SHA256: &str = "4d6aaddc1d2385c9f5897df28f33879f699f8f2783315d5204cf3d8c3616ac5f";
pub const PACKAGE_URL: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/1.24.261001001/microsoft.windows.console.conpty.1.24.261001001.nupkg";
pub const PACKAGE_FILE: &str = "microsoft.windows.console.conpty.1.24.261001001.nupkg";

pub const DLL_ENTRY: &str = "runtimes/win-x64/native/conpty.dll";
pub const HOST_ENTRY: &str = "build/native/runtimes/x64/OpenConsole.exe";

pub const OUTPUT_FILES: [(&str, &str); 2] =
    [(DLL_ENTRY, "conpty.dll"), (HOST_ENTRY, "OpenConsole.exe")];

pub fn cache_path(target: &Path) -> PathBuf {
    target.join("conpty-cache").join(PACKAGE_FILE)
}

pub fn hash_matches(actual: &str) -> bool {
    actual.eq_ignore_ascii_case(SHA256)
}

/// Downloads, verifies, and extracts only the x64 runtime pair into a package
/// folder, keeping the NuGet archive in the slot-local cache.
pub fn fetch_and_extract(target: &Path, package_dir: &Path) -> anyhow::Result<()> {
    if !cfg!(windows) {
        bail!("fetching the Windows ConPTY package runs only on Windows");
    }
    if !package_dir.join("yien.exe").is_file() {
        bail!("ConPTY files must be extracted beside yien.exe in the package");
    }

    let archive = cache_path(target);
    let cache = archive.parent().context("ConPTY cache has no parent")?;
    std::fs::create_dir_all(cache).context("creating ConPTY package cache")?;
    let script_path = cache.join("fetch-conpty.ps1");
    std::fs::write(&script_path, include_str!("fetch_conpty.ps1"))
        .context("writing the ConPTY fetch script")?;
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script_path)
        .env("YIEN_CONPTY_ARCHIVE", &archive)
        .env("YIEN_CONPTY_SHA256", SHA256)
        .env("YIEN_CONPTY_URL", PACKAGE_URL)
        .env("YIEN_CONPTY_PACKAGE_DIR", package_dir)
        .env("YIEN_CONPTY_DLL_ENTRY", OUTPUT_FILES[0].0)
        .env("YIEN_CONPTY_HOST_ENTRY", OUTPUT_FILES[1].0)
        .output()
        .context("starting the ConPTY package fetch script")?;
    if !status.status.success() {
        bail!(
            "ConPTY package fetch or extraction failed ({}): {}",
            status.status,
            String::from_utf8_lossy(&status.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&status.stdout);
    let actual = stdout
        .lines()
        .find_map(|line| line.strip_prefix("SHA256="))
        .context("fetch script returned no package SHA-256")?;
    if !hash_matches(actual) {
        bail!("ConPTY package SHA-256 mismatch: expected {SHA256}, got {actual}");
    }
    println!("ConPTY package SHA-256 verified: {actual}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_pan_06_pinned_conpty_hash_rejects_wrong_hash() {
        assert!(hash_matches(SHA256));
        assert!(hash_matches(&SHA256.to_ascii_uppercase()));
        assert!(!hash_matches("00"));
    }

    #[test]
    fn rf_pan_06_extracts_only_the_verified_x64_package_entries() {
        assert_eq!(
            OUTPUT_FILES,
            [
                ("runtimes/win-x64/native/conpty.dll", "conpty.dll"),
                (
                    "build/native/runtimes/x64/OpenConsole.exe",
                    "OpenConsole.exe"
                ),
            ]
        );
    }
}
