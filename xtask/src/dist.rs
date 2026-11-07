//! The portable Windows build: release binaries, README, licences and the
//! third-party notice in `<target>/dist/yien-<version>-windows-x64/`, then zipped.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, bail};
use serde_json::Value;

/// Platform label in the package name.
pub const PLATFORM: &str = "windows-x64";
/// Target whose dependency graph the third-party notice lists.
pub const TARGET_TRIPLE: &str = "x86_64-pc-windows-msvc";
/// The binary packages the portable build ships.
pub const BINARIES: [&str; 2] = ["yien", "yien-hook"];
/// The generated notice, written straight into the package folder.
pub const THIRD_PARTY: &str = "THIRD-PARTY.md";

/// PE optional-header subsystem of a GUI program.
pub const SUBSYSTEM_GUI: u16 = 2;
/// PE optional-header subsystem of a console program.
pub const SUBSYSTEM_CONSOLE: u16 = 3;

/// One file copied into the package folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub source: PathBuf,
    pub dest: &'static str,
    /// The PE subsystem the file must have, for executables.
    pub subsystem: Option<u16>,
}

/// `yien-<version>-windows-x64`.
pub fn package_name(version: &str) -> String {
    format!("yien-{version}-{PLATFORM}")
}

/// `yien-<version>-windows-x64.zip`.
pub fn zip_name(version: &str) -> String {
    format!("{}.zip", package_name(version))
}

/// Cargo's target dir: `CARGO_TARGET_DIR` (relative to `cwd` when relative)
/// or `<root>/target` when it is unset or empty.
pub fn target_dir(env_value: Option<OsString>, root: &Path, cwd: &Path) -> PathBuf {
    match env_value.filter(|v| !v.is_empty()) {
        Some(dir) => cwd.join(dir),
        None => root.join("target"),
    }
}

/// The files copied from the build and the repository, in package order.
/// `yien` must be a GUI program so a double-click opens no console;
/// `yien-hook` must stay a console program, since Claude Code pipes its stdin.
pub fn files(root: &Path, release_dir: &Path) -> Vec<Entry> {
    vec![
        Entry {
            source: release_dir.join("yien.exe"),
            dest: "yien.exe",
            subsystem: Some(SUBSYSTEM_GUI),
        },
        Entry {
            source: release_dir.join("yien-hook.exe"),
            dest: "yien-hook.exe",
            subsystem: Some(SUBSYSTEM_CONSOLE),
        },
        Entry {
            source: root.join("README.md"),
            dest: "README.md",
            subsystem: None,
        },
        Entry {
            source: root.join("LICENSE-MIT"),
            dest: "LICENSE-MIT",
            subsystem: None,
        },
        Entry {
            source: root.join("LICENSE-APACHE"),
            dest: "LICENSE-APACHE",
            subsystem: None,
        },
    ]
}

/// The `[workspace.package] version` of the root `Cargo.toml`.
pub fn workspace_version(root_cargo_toml: &str) -> anyhow::Result<String> {
    let table: toml::Table = root_cargo_toml.parse().context("root Cargo.toml")?;
    table
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .context("root Cargo.toml has no [workspace.package] version")
}

/// The subsystem field of a PE image's optional header, or `None` when the
/// bytes are not a PE image.
pub fn pe_subsystem(image: &[u8]) -> Option<u16> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(image.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(image.get(at..at + 4)?.try_into().ok()?));
    if image.get(..2)? != b"MZ" {
        return None;
    }
    let pe = usize::try_from(u32_at(0x3C)?).ok()?;
    if image.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    // COFF header is 20 bytes after the signature; Subsystem sits at offset
    // 68 of the optional header in both PE32 and PE32+.
    u16_at(pe + 4 + 20 + 68)
}

/// One third-party package in the shipped binaries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub license: String,
    pub repository: String,
}

/// The registry packages reachable from `roots` through normal (not build or
/// dev) dependencies in `cargo metadata` output, sorted by name and version.
pub fn third_party_packages(metadata: &Value, roots: &[&str]) -> anyhow::Result<Vec<Package>> {
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata has no packages")?;
    let by_id: BTreeMap<&str, &Value> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .context("cargo metadata has no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();

    let mut stack = Vec::new();
    for root in roots {
        let id = packages
            .iter()
            .find(|p| p["name"] == *root && p["source"].is_null())
            .and_then(|p| p["id"].as_str())
            .with_context(|| format!("cargo metadata has no workspace package {root}"))?;
        stack.push(id);
    }
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = nodes
            .get(id)
            .with_context(|| format!("{id} is missing from the resolve graph"))?;
        for dep in node["deps"].as_array().into_iter().flatten() {
            let normal = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null());
            if let (true, Some(dep_id)) = (normal, dep["pkg"].as_str()) {
                stack.push(dep_id);
            }
        }
    }

    let mut out = Vec::new();
    for id in seen {
        let package = by_id
            .get(id)
            .with_context(|| format!("{id} is missing from the packages"))?;
        if package["source"].is_null() {
            continue;
        }
        let text = |key: &str| package[key].as_str().unwrap_or_default().to_owned();
        out.push(Package {
            name: text("name"),
            version: text("version"),
            license: match package["license"].as_str() {
                Some(license) => license.to_owned(),
                None => "see the package".to_owned(),
            },
            repository: text("repository"),
        });
    }
    out.sort();
    Ok(out)
}

/// The text of `THIRD-PARTY.md`.
pub fn third_party_notice(packages: &[Package]) -> String {
    let mut out = String::from(
        "# Third-party software\n\n\
         The YIEN binaries include the following open-source packages. Each is used under \
         the licence shown (an `OR` expression means one of them was chosen); the full licence \
         texts and copyright notices are in each package's source, at the repository listed or \
         at `https://crates.io/crates/<name>/<version>`.\n\n\
         | Package | Version | Licence | Repository |\n\
         |---|---|---|---|\n",
    );
    for p in packages {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            p.name,
            p.version,
            p.license.replace('|', "\\|"),
            p.repository
        ));
    }
    out
}

/// Builds, assembles and zips the portable build.
pub fn run(root: &Path) -> anyhow::Result<ExitCode> {
    if !cfg!(windows) {
        bail!("dist builds the Windows portable package and runs only on Windows");
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let target = target_dir(std::env::var_os("CARGO_TARGET_DIR"), root, &cwd);
    let version = workspace_version(
        &std::fs::read_to_string(root.join("Cargo.toml")).context("reading Cargo.toml")?,
    )?;

    let mut build = Command::new(&cargo);
    build
        .current_dir(root)
        .args(["build", "--release", "--locked"]);
    for bin in BINARIES {
        build.args(["-p", bin]);
    }
    let status = build.status().context("starting cargo build")?;
    if !status.success() {
        bail!("cargo build --release failed ({status})");
    }

    let metadata = Command::new(&cargo)
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            TARGET_TRIPLE,
        ])
        .output()
        .context("starting cargo metadata")?;
    if !metadata.status.success() {
        bail!(
            "cargo metadata failed ({}): {}",
            metadata.status,
            String::from_utf8_lossy(&metadata.stderr).trim()
        );
    }
    let metadata: Value =
        serde_json::from_slice(&metadata.stdout).context("parsing cargo metadata")?;
    let notice = third_party_notice(&third_party_packages(&metadata, &BINARIES)?);

    let dist = target.join("dist");
    if dist.exists() {
        std::fs::remove_dir_all(&dist)
            .with_context(|| format!("removing {} (is a file in it open?)", dist.display()))?;
    }
    let package = dist.join(package_name(&version));
    std::fs::create_dir_all(&package).with_context(|| format!("creating {}", package.display()))?;
    for entry in files(root, &target.join("release")) {
        let dest = package.join(entry.dest);
        if let Some(expected) = entry.subsystem {
            let image = std::fs::read(&entry.source)
                .with_context(|| format!("reading {}", entry.source.display()))?;
            let found = pe_subsystem(&image);
            if found != Some(expected) {
                bail!(
                    "{} has PE subsystem {found:?}, expected {expected}",
                    entry.source.display()
                );
            }
        }
        std::fs::copy(&entry.source, &dest)
            .with_context(|| format!("copying {} to {}", entry.source.display(), dest.display()))?;
    }
    std::fs::write(package.join(THIRD_PARTY), notice)
        .with_context(|| format!("writing {THIRD_PARTY}"))?;

    let zip = dist.join(zip_name(&version));
    zip_folder(&dist, &package_name(&version), &zip)?;
    println!("dist folder: {}", package.display());
    println!("dist zip:    {}", zip.display());
    Ok(ExitCode::SUCCESS)
}

/// Zips `<dir>/<folder>` into `zip`, keeping `folder` as the archive's top
/// level. Windows' own bsdtar is named by full path: the `tar` on PATH may be
/// GNU tar from Git, which cannot write zip.
fn zip_folder(dir: &Path, folder: &str, zip: &Path) -> anyhow::Result<()> {
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let tar = Path::new(&system_root).join("System32").join("tar.exe");
    if !tar.is_file() {
        bail!(
            "{} not found; Windows 10 1803 or later ships it",
            tar.display()
        );
    }
    let status = Command::new(&tar)
        .current_dir(dir)
        .arg("-a")
        .arg("-cf")
        .arg(zip)
        .arg(folder)
        .status()
        .with_context(|| format!("starting {}", tar.display()))?;
    if !status.success() {
        bail!("zipping {} failed ({status})", zip.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_carry_version_and_platform() {
        assert_eq!(package_name("0.1.0"), "yien-0.1.0-windows-x64");
        assert_eq!(zip_name("0.1.0"), "yien-0.1.0-windows-x64.zip");
    }

    #[test]
    fn target_dir_honours_cargo_target_dir() {
        let root = Path::new("/repo");
        let cwd = Path::new("/work");
        assert_eq!(target_dir(None, root, cwd), Path::new("/repo/target"));
        assert_eq!(
            target_dir(Some("".into()), root, cwd),
            Path::new("/repo/target")
        );
        assert_eq!(
            target_dir(Some("/slots/4".into()), root, cwd),
            Path::new("/slots/4")
        );
        assert_eq!(
            target_dir(Some("rel".into()), root, cwd),
            Path::new("/work/rel")
        );
    }

    #[test]
    fn package_holds_both_binaries_readme_and_licences() {
        let entries = files(Path::new("/repo"), Path::new("/t/release"));
        let dests: Vec<&str> = entries.iter().map(|e| e.dest).collect();
        assert_eq!(
            dests,
            [
                "yien.exe",
                "yien-hook.exe",
                "README.md",
                "LICENSE-MIT",
                "LICENSE-APACHE"
            ]
        );
        assert_eq!(entries[0].source, Path::new("/t/release/yien.exe"));
        assert_eq!(entries[0].subsystem, Some(SUBSYSTEM_GUI));
        assert_eq!(entries[1].subsystem, Some(SUBSYSTEM_CONSOLE));
        assert_eq!(entries[2].source, Path::new("/repo/README.md"));
        assert!(entries[2..].iter().all(|e| e.subsystem.is_none()));
    }

    #[test]
    fn the_package_files_exist_in_the_repository() {
        let root = crate::workspace_root();
        for entry in files(&root, Path::new("unused")) {
            if entry.subsystem.is_none() {
                assert!(entry.source.is_file(), "{} missing", entry.source.display());
            }
        }
    }

    #[test]
    fn version_comes_from_the_workspace_package() {
        let toml = "[workspace]\nmembers = []\n[workspace.package]\nversion = \"1.2.3\"\n";
        assert_eq!(workspace_version(toml).unwrap(), "1.2.3");
        assert!(workspace_version("[workspace]\n").is_err());
    }

    fn pe_image(subsystem: u16) -> Vec<u8> {
        let pe = 0x80;
        let mut image = vec![0_u8; pe + 4 + 20 + 96];
        image[..2].copy_from_slice(b"MZ");
        image[0x3C..0x40].copy_from_slice(&(pe as u32).to_le_bytes());
        image[pe..pe + 4].copy_from_slice(b"PE\0\0");
        image[pe + 92..pe + 94].copy_from_slice(&subsystem.to_le_bytes());
        image
    }

    #[test]
    fn pe_subsystem_reads_the_optional_header() {
        assert_eq!(pe_subsystem(&pe_image(2)), Some(SUBSYSTEM_GUI));
        assert_eq!(pe_subsystem(&pe_image(3)), Some(SUBSYSTEM_CONSOLE));
    }

    #[test]
    fn pe_subsystem_rejects_what_is_not_a_pe_image() {
        assert_eq!(pe_subsystem(b""), None);
        assert_eq!(pe_subsystem(b"#!/bin/sh\n"), None);
        let mut bad_offset = pe_image(2);
        bad_offset[0x3C..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(pe_subsystem(&bad_offset), None);
        let mut no_signature = pe_image(2);
        no_signature[0x80] = b'X';
        assert_eq!(pe_subsystem(&no_signature), None);
        let truncated = &pe_image(2)[..0x80 + 30];
        assert_eq!(pe_subsystem(truncated), None);
    }

    fn metadata() -> Value {
        let normal = json!([{ "kind": null, "target": null }]);
        let build = json!([{ "kind": "build", "target": null }]);
        let dev = json!([{ "kind": "dev", "target": null }]);
        let reg = "registry+https://github.com/rust-lang/crates.io-index";
        json!({
            "packages": [
                { "id": "app", "name": "yien", "version": "0.0.0", "source": null, "license": "MIT OR Apache-2.0", "repository": null },
                { "id": "hook", "name": "yien-hook", "version": "0.0.0", "source": null, "license": "MIT OR Apache-2.0", "repository": null },
                { "id": "proto", "name": "yien-proto", "version": "0.0.0", "source": null, "license": "MIT OR Apache-2.0", "repository": null },
                { "id": "serde", "name": "serde", "version": "1.0.0", "source": reg, "license": "MIT OR Apache-2.0", "repository": "https://github.com/serde-rs/serde" },
                { "id": "deep", "name": "deep", "version": "0.2.0", "source": reg, "license": "Zlib", "repository": "https://example.org/deep" },
                { "id": "cc", "name": "cc", "version": "1.0.0", "source": reg, "license": "MIT", "repository": "" },
                { "id": "proptest", "name": "proptest", "version": "1.0.0", "source": reg, "license": "MIT", "repository": "" },
                { "id": "nolicence", "name": "nolicence", "version": "0.1.0", "source": reg, "license": null, "repository": null },
                { "id": "unused", "name": "unused", "version": "9.9.9", "source": reg, "license": "MIT", "repository": "" }
            ],
            "resolve": { "nodes": [
                { "id": "app", "deps": [
                    { "pkg": "proto", "dep_kinds": normal },
                    { "pkg": "cc", "dep_kinds": build },
                    { "pkg": "proptest", "dep_kinds": dev }
                ]},
                { "id": "hook", "deps": [ { "pkg": "proto", "dep_kinds": normal }, { "pkg": "nolicence", "dep_kinds": normal } ] },
                { "id": "proto", "deps": [ { "pkg": "serde", "dep_kinds": normal } ] },
                { "id": "serde", "deps": [ { "pkg": "deep", "dep_kinds": normal } ] },
                { "id": "deep", "deps": [] },
                { "id": "cc", "deps": [] },
                { "id": "proptest", "deps": [] },
                { "id": "nolicence", "deps": [] },
                { "id": "unused", "deps": [] }
            ]}
        })
    }

    #[test]
    fn third_party_lists_shipped_registry_packages_only() {
        let found = third_party_packages(&metadata(), &BINARIES).unwrap();
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["deep", "nolicence", "serde"]);
        assert_eq!(found[0].license, "Zlib");
        assert_eq!(found[1].license, "see the package");
        assert_eq!(found[1].repository, "");
    }

    #[test]
    fn third_party_fails_on_an_unknown_root() {
        assert!(third_party_packages(&metadata(), &["missing"]).is_err());
    }

    #[test]
    fn notice_has_one_row_per_package() {
        let found = third_party_packages(&metadata(), &BINARIES).unwrap();
        let notice = third_party_notice(&found);
        assert!(notice.starts_with("# Third-party software\n"));
        assert!(notice.contains(
            "| serde | 1.0.0 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |\n"
        ));
        assert_eq!(notice.lines().filter(|l| l.starts_with("| ")).count(), 4);
    }
}
