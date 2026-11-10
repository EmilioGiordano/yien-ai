//! Builds a per-user Windows installer from the existing portable distribution.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::dist;
use anyhow::{Context, bail};

pub fn output_name(version: &str) -> String {
    format!("yien-{version}-x86_64-setup.exe")
}

pub fn sha256_sidecar(exe_name: &str, hash: &str) -> String {
    format!("{hash}  {exe_name}\n")
}

fn find_iscc() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ISCC") {
        return Some(path.into());
    }
    let exe = if cfg!(windows) { "ISCC.exe" } else { "ISCC" };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|p| p.join(exe))
        .find(|p| p.is_file())
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|p| p.join("Programs/Inno Setup 6/ISCC.exe"))
                .filter(|p| p.is_file())
        })
        .or_else(|| {
            let p = PathBuf::from(r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe");
            p.is_file().then_some(p)
        })
}

fn quote_ispp(path: &Path) -> String {
    path.to_string_lossy().replace('"', "''")
}

pub fn run(root: &Path) -> anyhow::Result<ExitCode> {
    if !cfg!(windows) {
        bail!("installer builds the Windows installer and runs only on Windows");
    }
    let iscc =
        find_iscc().context("Inno Setup compiler not found; install Inno Setup 6 or set ISCC")?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let metadata = Command::new(cargo)
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            dist::TARGET_TRIPLE,
        ])
        .output()
        .context("starting cargo metadata")?;
    if !metadata.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata.stderr).trim()
        );
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&metadata.stdout).context("parsing cargo metadata")?;
    let target = dist::target_directory(&metadata)?;
    let version = dist::workspace_version(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
    let package = target.join("dist").join(dist::package_name(&version));
    if !package.join("yien.exe").is_file() {
        bail!("portable package missing; run cargo xtask dist first");
    }
    let out = target.join("dist");
    let license = package.join("INSTALLER-LICENSE.txt");
    if !license.exists() {
        std::fs::write(
            &license,
            "YIEN's project licence has not been decided yet.\n",
        )?;
    }
    let setup = out.join(output_name(&version));
    let script = root.join("installer/yien.iss");
    let status = Command::new(iscc)
        .current_dir(root)
        .arg(format!("/DAppVersion={version}"))
        .arg(format!("/DSourceDir={}", quote_ispp(&package)))
        .arg(format!("/DOutputDir={}", quote_ispp(&out)))
        .arg(format!(
            "/DOutputBase={}",
            setup.file_stem().unwrap_or_default().to_string_lossy()
        ))
        .arg(&script)
        .status()
        .context("starting Inno Setup compiler")?;
    if !status.success() {
        bail!("Inno Setup compiler failed ({status})");
    }
    let digest = sha256_file(&setup)?;
    std::fs::write(
        setup.with_extension("exe.sha256"),
        sha256_sidecar(
            &setup.file_name().unwrap_or_default().to_string_lossy(),
            &digest,
        ),
    )
    .context("writing installer SHA-256")?;
    println!("{}", setup.display());
    Ok(ExitCode::SUCCESS)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("certutil")
        .args(["-hashfile"])
        .arg(path)
        .arg("SHA256")
        .output()
        .context("running certutil to calculate SHA-256")?;
    if !output.status.success() {
        bail!("certutil failed to calculate SHA-256");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find(|line| line.trim().len() == 64 && line.trim().bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|line| line.trim().to_ascii_lowercase())
        .context("certutil returned no SHA-256 digest")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rnf_21_setup_filename_uses_version() {
        assert_eq!(output_name("0.1.2"), "yien-0.1.2-x86_64-setup.exe");
    }
    #[test]
    fn rf_cfg_07_checksum_is_sha256sum_compatible() {
        assert_eq!(sha256_sidecar("setup.exe", "abc"), "abc  setup.exe\n");
    }
}
