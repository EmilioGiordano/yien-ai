//! The layer rule (AGENTS.md "Workspace" and "The layer rule", plan §3.2),
//! checked on each crate's direct dependencies.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// Layers 0 to 3, lower is purer.
    Level(u8),
    /// Test support: only ever a dev-dependency.
    Dev,
}

const LAYERS: &[(&str, Layer)] = &[
    ("yien-proto", Layer::Level(0)),
    ("yien-layout", Layer::Level(0)),
    ("yien-input", Layer::Level(0)),
    ("yien-vt", Layer::Level(1)),
    ("yien-agent", Layer::Level(1)),
    ("yien-session", Layer::Level(1)),
    ("yien-pty", Layer::Level(2)),
    ("yien-git", Layer::Level(2)),
    ("yien-platform", Layer::Level(2)),
    ("yien-core", Layer::Level(3)),
    ("yien-ui", Layer::Level(3)),
    ("yien-testkit", Layer::Dev),
];

/// The only crates allowed to contain `unsafe`.
const UNSAFE_ALLOWED: &[&str] = &["yien-pty", "yien-platform"];

/// External dependencies a crate may have at all, for crates held to a list.
const EXTERNAL_ALLOWLIST: &[(&str, &[&str])] = &[("yien-proto", &["serde"])];

/// OS, window and I/O crates that layers 0 and 1 must never pull in.
const OS_CRATES_EXACT: &[&str] = &[
    "winapi",
    "portable-pty",
    "winit",
    "libc",
    "nix",
    "tokio",
    "mio",
    "raw-window-handle",
];
const OS_CRATES_PREFIX: &[&str] = &[
    "windows",
    "gpui",
    "objc",
    "cocoa",
    "core-foundation",
    "x11",
    "wayland",
];

/// What the checker needs from one `crates/*` member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    /// Real package names of `[dependencies]` and `[target.*.dependencies]`,
    /// renames resolved.
    pub deps: Vec<String>,
    /// Whether the crate root declares `#![forbid(unsafe_code)]`.
    pub forbids_unsafe: bool,
}

impl Manifest {
    /// Parses a `Cargo.toml`; `root_source` is the text of `src/lib.rs` or
    /// `src/main.rs`.
    pub fn parse(cargo_toml: &str, root_source: &str) -> anyhow::Result<Self> {
        let doc: toml::Table = cargo_toml.parse().context("invalid TOML")?;
        let Some(name) = doc
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        else {
            bail!("no [package] name");
        };

        let mut deps = Vec::new();
        if let Some(table) = doc.get("dependencies").and_then(|d| d.as_table()) {
            collect_deps(table, &mut deps);
        }
        if let Some(targets) = doc.get("target").and_then(|t| t.as_table()) {
            for target in targets.values() {
                if let Some(table) = target.get("dependencies").and_then(|d| d.as_table()) {
                    collect_deps(table, &mut deps);
                }
            }
        }
        deps.sort();
        deps.dedup();

        Ok(Self {
            name: name.to_owned(),
            deps,
            forbids_unsafe: root_source
                .lines()
                .any(|l| l.trim() == "#![forbid(unsafe_code)]"),
        })
    }
}

fn collect_deps(table: &toml::Table, out: &mut Vec<String>) {
    for (key, value) in table {
        let package = value.get("package").and_then(|p| p.as_str()).unwrap_or(key);
        out.push(package.to_owned());
    }
}

fn layer_of(name: &str) -> Option<Layer> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

fn is_os_crate(name: &str) -> bool {
    OS_CRATES_EXACT.contains(&name) || OS_CRATES_PREFIX.iter().any(|p| name.starts_with(p))
}

/// Every rule broken by the given crates, as one message each.
pub fn check(manifests: &[Manifest]) -> Vec<String> {
    let mut violations = Vec::new();
    for m in manifests {
        let Some(layer) = layer_of(&m.name) else {
            violations.push(format!(
                "{}: not in the layer table; add it to xtask/src/check_deps.rs and AGENTS.md",
                m.name
            ));
            continue;
        };

        if !m.forbids_unsafe && !UNSAFE_ALLOWED.contains(&m.name.as_str()) {
            violations.push(format!(
                "{}: crate root lacks #![forbid(unsafe_code)]",
                m.name
            ));
        }

        let allowlist = EXTERNAL_ALLOWLIST
            .iter()
            .find(|(n, _)| *n == m.name)
            .map(|(_, list)| *list);

        for dep in &m.deps {
            if let Some(dep_layer) = layer_of(dep) {
                if let Err(why) = may_depend(&m.name, layer, dep, dep_layer) {
                    violations.push(format!("{} -> {dep}: {why}", m.name));
                }
                continue;
            }
            if dep.starts_with("yien-") {
                violations.push(format!(
                    "{} -> {dep}: {dep} is not in the layer table",
                    m.name
                ));
                continue;
            }
            if let Some(list) = allowlist
                && !list.contains(&dep.as_str())
            {
                violations.push(format!(
                    "{} -> {dep}: {} may only depend on {}",
                    m.name,
                    m.name,
                    list.join(", ")
                ));
                continue;
            }
            if matches!(layer, Layer::Level(0 | 1)) && is_os_crate(dep) {
                violations.push(format!(
                    "{} -> {dep}: layers 0 and 1 build with no window and no OS APIs",
                    m.name
                ));
            }
        }
    }
    violations
}

fn may_depend(from: &str, from_layer: Layer, to: &str, to_layer: Layer) -> Result<(), String> {
    match (from_layer, to_layer) {
        (_, Layer::Dev) => Err(format!("{to} is dev-only; use [dev-dependencies]")),
        (Layer::Dev, _) if to == "yien-ui" => Err("test support never depends on the UI".into()),
        (Layer::Dev, _) => Ok(()),
        (Layer::Level(_), _) if from == "yien-ui" && to == "yien-core" => {
            Err("yien-ui never depends on yien-core; it talks through yien-proto".into())
        }
        (Layer::Level(_), Layer::Level(t)) if from == "yien-ui" && t > 1 => {
            Err("yien-ui may depend only on layers 0 and 1".into())
        }
        (Layer::Level(f), Layer::Level(t)) if t < f => Ok(()),
        (Layer::Level(f), Layer::Level(t)) => Err(format!(
            "layer {f} may depend only on lower layers, and {to} is layer {t}"
        )),
    }
}

fn load(dir: &Path) -> anyhow::Result<Manifest> {
    let cargo_toml = std::fs::read_to_string(dir.join("Cargo.toml"))
        .with_context(|| format!("reading {}", dir.join("Cargo.toml").display()))?;
    let lib = dir.join("src").join("lib.rs");
    let main = dir.join("src").join("main.rs");
    let root = if lib.is_file() { lib } else { main };
    let source = std::fs::read_to_string(&root).unwrap_or_default();
    Manifest::parse(&cargo_toml, &source).with_context(|| format!("parsing {}", dir.display()))
}

pub fn run(root: &Path) -> anyhow::Result<ExitCode> {
    let manifests = crate::crate_dirs(root)?
        .iter()
        .map(|d| load(d))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let violations = check(&manifests);
    if violations.is_empty() {
        println!(
            "check-deps: {} crates follow the layer rule",
            manifests.len()
        );
        return Ok(ExitCode::SUCCESS);
    }
    for v in &violations {
        eprintln!("check-deps: {v}");
    }
    eprintln!("check-deps: {} violation(s)", violations.len());
    Ok(ExitCode::FAILURE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORBID: &str = "#![forbid(unsafe_code)]\n";

    fn manifest(name: &str, deps: &str) -> Manifest {
        let toml = format!("[package]\nname = \"{name}\"\n\n{deps}");
        Manifest::parse(&toml, FORBID).unwrap()
    }

    fn assert_violation(manifests: &[Manifest], needle: &str) {
        let v = check(manifests);
        assert!(
            v.iter().any(|m| m.contains(needle)),
            "expected a violation mentioning {needle:?}, got {v:?}"
        );
    }

    #[test]
    fn a_workspace_that_follows_the_rule_passes() {
        let set = [
            manifest("yien-proto", "[dependencies]\nserde = \"1\""),
            manifest("yien-layout", ""),
            manifest(
                "yien-vt",
                "[dependencies]\nyien-proto = { path = \"../yien-proto\" }\nregex = \"1\"",
            ),
            manifest(
                "yien-pty",
                "[target.'cfg(windows)'.dependencies]\nwindows-sys = \"0.59\"\n\
                 [dependencies]\nyien-vt = { workspace = true }",
            ),
            manifest(
                "yien-core",
                "[dependencies]\nyien-pty = { workspace = true }\ntokio = \"1\"",
            ),
            manifest(
                "yien-ui",
                "[dependencies]\nyien-proto = { workspace = true }\nyien-vt = \"0\"\ngpui = \"0\"\n\
                 [dev-dependencies]\nyien-testkit = { workspace = true }",
            ),
            manifest(
                "yien-testkit",
                "[dependencies]\nyien-core = { workspace = true }",
            ),
        ];
        assert_eq!(check(&set), Vec::<String>::new());
    }

    #[test]
    fn layer_zero_may_not_depend_on_layer_one() {
        let set = [manifest("yien-layout", "[dependencies]\nyien-vt = \"0\"")];
        assert_violation(&set, "yien-layout -> yien-vt");
    }

    #[test]
    fn a_layer_may_not_depend_on_its_own_level() {
        let set = [manifest(
            "yien-layout",
            "[dependencies]\nyien-input = \"0\"",
        )];
        assert_violation(&set, "yien-layout -> yien-input");
    }

    #[test]
    fn ui_never_depends_on_core() {
        let set = [manifest("yien-ui", "[dependencies]\nyien-core = \"0\"")];
        assert_violation(&set, "yien-ui never depends on yien-core");
    }

    #[test]
    fn ui_may_not_reach_os_adapters() {
        let set = [manifest("yien-ui", "[dependencies]\nyien-pty = \"0\"")];
        assert_violation(&set, "yien-ui -> yien-pty");
    }

    #[test]
    fn os_crates_are_caught_under_target_tables() {
        let set = [manifest(
            "yien-session",
            "[target.'cfg(windows)'.dependencies]\nwindows-sys = \"0.59\"",
        )];
        assert_violation(&set, "yien-session -> windows-sys");
    }

    #[test]
    fn window_crates_are_caught_in_layer_zero() {
        for dep in ["gpui", "gpui-component", "winit", "portable-pty", "windows"] {
            let set = [manifest(
                "yien-input",
                &format!("[dependencies]\n{dep} = \"0\""),
            )];
            assert_violation(&set, &format!("yien-input -> {dep}"));
        }
    }

    #[test]
    fn renamed_dependencies_are_resolved() {
        let set = [manifest(
            "yien-agent",
            "[dependencies]\nwin = { package = \"windows-sys\", version = \"0.59\" }",
        )];
        assert_violation(&set, "yien-agent -> windows-sys");
    }

    #[test]
    fn testkit_is_rejected_as_a_normal_dependency() {
        let set = [manifest(
            "yien-core",
            "[dependencies]\nyien-testkit = { workspace = true }",
        )];
        assert_violation(&set, "yien-testkit is dev-only");
    }

    #[test]
    fn proto_is_held_to_serde() {
        let set = [manifest(
            "yien-proto",
            "[dependencies]\nserde = \"1\"\nregex = \"1\"",
        )];
        assert_violation(&set, "yien-proto -> regex");
    }

    #[test]
    fn unknown_crates_must_be_added_to_the_table() {
        let set = [manifest("yien-mystery", "")];
        assert_violation(&set, "yien-mystery: not in the layer table");
        let set = [manifest(
            "yien-layout",
            "[dependencies]\nyien-mystery = \"0\"",
        )];
        assert_violation(&set, "yien-mystery is not in the layer table");
    }

    #[test]
    fn a_crate_without_forbid_unsafe_fails_unless_allowed() {
        let toml = "[package]\nname = \"yien-vt\"\n";
        let m = Manifest::parse(toml, "//! no attribute\n").unwrap();
        assert_violation(&[m], "lacks #![forbid(unsafe_code)]");

        let toml = "[package]\nname = \"yien-pty\"\n";
        let m = Manifest::parse(toml, "").unwrap();
        assert_eq!(check(&[m]), Vec::<String>::new());
    }

    #[test]
    fn dev_and_build_dependencies_are_not_checked() {
        let set = [manifest(
            "yien-layout",
            "[dev-dependencies]\nyien-testkit = \"0\"\ntokio = \"1\"\n\
             [build-dependencies]\nwindows = \"0\"",
        )];
        assert_eq!(check(&set), Vec::<String>::new());
    }

    #[test]
    fn a_manifest_without_a_package_name_is_an_error() {
        assert!(Manifest::parse("[dependencies]\n", FORBID).is_err());
        assert!(Manifest::parse("not toml = = =", FORBID).is_err());
    }

    #[test]
    fn the_real_workspace_follows_the_rule() {
        let root = crate::workspace_root();
        let manifests: Vec<_> = crate::crate_dirs(&root)
            .unwrap()
            .iter()
            .map(|d| load(d).unwrap())
            .collect();
        assert!(manifests.iter().any(|m| m.name == "yien-layout"));
        assert_eq!(check(&manifests), Vec::<String>::new());
    }
}
