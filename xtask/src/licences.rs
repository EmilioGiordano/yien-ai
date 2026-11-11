//! `THIRD-PARTY-LICENSES.txt`: the licence and notice texts of every third-party
//! package, taken from the package source, or from a vendored SPDX template when
//! the source carries none.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, bail};

use crate::dist::Package;

const NO_COPYRIGHT_NOTE: &str = "licence text from SPDX template, no copyright line in source";

/// The SPDX templates in `third-party/licenses/`, by id. They carry no copyright line.
const TEMPLATES: [(&str, &str); 3] = [
    ("MIT", include_str!("../../third-party/licenses/MIT.txt")),
    (
        "Apache-2.0",
        include_str!("../../third-party/licenses/Apache-2.0.txt"),
    ),
    ("Zlib", include_str!("../../third-party/licenses/Zlib.txt")),
];

/// Extensions of files that start with a licence word but are not licence text.
const NOT_TEXT: [&str; 8] = ["rs", "sh", "toml", "json", "py", "yml", "yaml", "lock"];

/// One licence or notice text of a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// The file it came from, or the SPDX id of the template.
    pub label: String,
    pub body: String,
    pub from_template: bool,
}

/// Whether a file name in a package folder holds a licence or notice text:
/// `LICENSE*`, `LICENCE*`, `COPYING*`, `NOTICE*`, `UNLICENSE*`, `COPYRIGHT*`, any case.
pub fn is_licence_file_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let starts = [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "NOTICE",
        "UNLICENSE",
        "COPYRIGHT",
    ]
    .iter()
    .any(|p| upper.starts_with(p));
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    starts && !extension.is_some_and(|e| NOT_TEXT.contains(&e.as_str()))
}

/// The licence files among the names in a package folder, plus the manifest's
/// `license-file`, sorted and without repeats.
pub fn licence_file_names(
    folder_names: impl IntoIterator<Item = String>,
    license_file: Option<&str>,
) -> Vec<String> {
    let mut names: BTreeSet<String> = folder_names
        .into_iter()
        .filter(|n| is_licence_file_name(n))
        .collect();
    if let Some(file) = license_file.filter(|f| !f.is_empty()) {
        names.insert(file.to_owned());
    }
    names.into_iter().collect()
}

/// The licence ids in an SPDX expression, in order, without the operators and
/// the exception after `WITH`.
pub fn spdx_ids(expression: &str) -> Vec<String> {
    let spaced = expression.replace(['(', ')', '/'], " ");
    let mut ids = Vec::new();
    let mut after_with = false;
    for token in spaced.split_whitespace() {
        match token {
            "AND" | "OR" => after_with = false,
            "WITH" => after_with = true,
            _ if after_with => after_with = false,
            id if !ids.iter().any(|i| i == id) => ids.push(id.to_owned()),
            _ => {}
        }
    }
    ids
}

fn template(id: &str) -> Option<&'static str> {
    TEMPLATES.iter().find(|(t, _)| *t == id).map(|(_, b)| *b)
}

fn tidy(body: &str) -> String {
    format!("{}\n", body.replace("\r\n", "\n").trim_end())
}

/// The template texts for a licence expression. Alternatives joined by `OR`
/// need one template (all that exist are given); an expression with `AND` needs
/// every id, or nothing is returned.
pub fn template_texts(expression: &str) -> Vec<Text> {
    let ids = spdx_ids(expression);
    let texts: Vec<Text> = ids
        .iter()
        .filter_map(|id| {
            Some(Text {
                label: id.clone(),
                body: tidy(template(id)?),
                from_template: true,
            })
        })
        .collect();
    let needs_all = expression.split_whitespace().any(|t| t == "AND");
    if needs_all && texts.len() != ids.len() {
        return Vec::new();
    }
    texts
}

/// The texts of one package: its own files when it has any with content,
/// otherwise the templates of its declared licence.
pub fn package_texts(package: &Package, files: Vec<(String, String)>) -> Vec<Text> {
    let own: Vec<Text> = files
        .into_iter()
        .filter(|(_, body)| !body.trim().is_empty())
        .map(|(label, body)| Text {
            label,
            body: tidy(&body),
            from_template: false,
        })
        .collect();
    if own.is_empty() {
        template_texts(&package.license)
    } else {
        own
    }
}

/// The packages that ended with no text, as `name version (licence)`.
pub fn without_text(entries: &[(Package, Vec<Text>)]) -> Vec<String> {
    entries
        .iter()
        .filter(|(_, texts)| texts.is_empty())
        .map(|(p, _)| format!("{} {} ({})", p.name, p.version, p.license))
        .collect()
}

/// The text of `THIRD-PARTY-LICENSES.txt`. Packages with the same text share
/// one block that lists them; blocks and lists are sorted.
pub fn render(entries: &[(Package, Vec<Text>)]) -> String {
    let mut groups: Vec<(&str, BTreeSet<String>)> = Vec::new();
    for (package, texts) in entries {
        for text in texts {
            let note = if text.from_template {
                format!("{NO_COPYRIGHT_NOTE}: {}", text.label)
            } else {
                format!("file {}", text.label)
            };
            let line = format!(
                "{} {} ({}) [{note}]",
                package.name, package.version, package.license
            );
            match groups.iter_mut().find(|(body, _)| *body == text.body) {
                Some((_, lines)) => {
                    lines.insert(line);
                }
                None => groups.push((&text.body, BTreeSet::from([line]))),
            }
        }
    }
    groups.sort_by(|a, b| a.1.cmp(&b.1));

    let rule = "=".repeat(78);
    let mut out = String::from(
        "Third-party licences\n\n\
         The YIEN binaries include the packages below. Each block lists the packages it \
         applies to, then the text. THIRD-PARTY.md lists every package with its version and \
         repository. The ConPTY package licence is in LICENSE-MIT-ConPTY.txt.\n",
    );
    for (body, lines) in groups {
        out.push_str(&format!("\n{rule}\n"));
        for line in lines {
            out.push_str(&format!("{line}\n"));
        }
        out.push_str(&format!("{rule}\n\n{body}"));
    }
    out
}

/// The text of `THIRD-PARTY-LICENSES.txt`, or an error naming every package with no text.
pub fn render_checked(entries: &[(Package, Vec<Text>)]) -> anyhow::Result<String> {
    let missing = without_text(entries);
    if !missing.is_empty() {
        bail!(
            "no licence text for {} package(s):\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }
    Ok(render(entries))
}

/// Reads the licence files in a package's folder as `(file name, text)`.
fn read_files(package: &Package) -> anyhow::Result<Vec<(String, String)>> {
    let dir = &package.manifest_dir;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry.with_context(|| format!("listing {}", dir.display()))?;
        if entry.file_type().is_ok_and(|t| t.is_file())
            && let Ok(name) = entry.file_name().into_string()
        {
            names.push(name);
        }
    }
    let mut files = Vec::new();
    for name in licence_file_names(names, package.license_file.as_deref()) {
        let path = dir.join(&name);
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let label = Path::new(&name)
            .file_name()
            .map_or(name.clone(), |n| n.to_string_lossy().into_owned());
        files.push((label, String::from_utf8_lossy(&bytes).into_owned()));
    }
    Ok(files)
}

/// The text of `THIRD-PARTY-LICENSES.txt` for `packages`, read from their
/// source folders; an error names every package that has no text.
pub fn build(packages: &[Package]) -> anyhow::Result<String> {
    let mut entries = Vec::new();
    for package in packages {
        let texts = package_texts(package, read_files(package)?);
        entries.push((package.clone(), texts));
    }
    render_checked(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, license: &str) -> Package {
        Package {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            license: license.to_owned(),
            repository: String::new(),
            manifest_dir: Default::default(),
            license_file: None,
        }
    }

    fn file(label: &str, body: &str) -> (String, String) {
        (label.to_owned(), body.to_owned())
    }

    #[test]
    fn licence_file_names_cover_the_usual_spellings() {
        let names = [
            "Cargo.toml",
            "LICENSE-MIT",
            "license-apache.txt",
            "LICENCE",
            "COPYING.md",
            "NOTICE",
            "license.rs",
            "licenses.toml",
            "README.md",
        ]
        .map(String::from);
        assert_eq!(
            licence_file_names(names, None),
            [
                "COPYING.md",
                "LICENCE",
                "LICENSE-MIT",
                "NOTICE",
                "license-apache.txt"
            ]
        );
    }

    #[test]
    fn licence_file_names_add_the_manifest_licence_file_once() {
        let names = ["LICENSE".to_owned(), "terms.txt".to_owned()];
        assert_eq!(
            licence_file_names(names.clone(), Some("terms.txt")),
            ["LICENSE", "terms.txt"]
        );
        assert_eq!(licence_file_names(names, Some("LICENSE")), ["LICENSE"]);
    }

    #[test]
    fn spdx_ids_drop_operators_and_exceptions() {
        assert_eq!(spdx_ids("MIT OR Apache-2.0"), ["MIT", "Apache-2.0"]);
        assert_eq!(spdx_ids("MIT/Apache-2.0"), ["MIT", "Apache-2.0"]);
        assert_eq!(
            spdx_ids("(MIT OR Apache-2.0) AND Unicode-3.0"),
            ["MIT", "Apache-2.0", "Unicode-3.0"]
        );
        assert_eq!(
            spdx_ids("Apache-2.0 WITH LLVM-exception OR MIT"),
            ["Apache-2.0", "MIT"]
        );
        assert_eq!(spdx_ids("MIT OR MIT"), ["MIT"]);
    }

    #[test]
    fn a_package_with_files_uses_them_not_the_template() {
        let p = package("a", "MIT");
        let texts = package_texts(
            &p,
            vec![file("LICENSE", "Copyright (c) A\r\n\r\nMIT text\r\n\r\n")],
        );
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].body, "Copyright (c) A\n\nMIT text\n");
        assert!(!texts[0].from_template);
    }

    #[test]
    fn a_package_without_files_falls_back_to_the_templates_of_its_licence() {
        let p = package("a", "MIT OR Apache-2.0 OR 0BSD");
        let texts = package_texts(&p, vec![file("LICENSE", "  \n")]);
        let labels: Vec<&str> = texts.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["MIT", "Apache-2.0"]);
        assert!(texts.iter().all(|t| t.from_template));
    }

    #[test]
    fn an_and_expression_needs_every_template() {
        assert_eq!(template_texts("MIT AND Zlib").len(), 2);
        assert!(template_texts("MIT AND Unicode-3.0").is_empty());
    }

    #[test]
    fn a_licence_with_no_template_and_no_file_has_no_text() {
        let p = package("odd", "0BSD");
        assert!(package_texts(&p, Vec::new()).is_empty());
    }

    #[test]
    fn the_build_fails_naming_every_package_without_text() {
        let entries = vec![
            (package("ok", "MIT"), template_texts("MIT")),
            (package("odd", "0BSD"), Vec::new()),
            (package("odder", "BSD-2-Clause"), Vec::new()),
        ];
        let error = render_checked(&entries).unwrap_err().to_string();
        assert!(error.contains("2 package(s)"), "{error}");
        assert!(error.contains("odd 1.0.0 (0BSD)"), "{error}");
        assert!(error.contains("odder 1.0.0 (BSD-2-Clause)"), "{error}");
        assert!(!error.contains("ok 1.0.0"), "{error}");
    }

    #[test]
    fn identical_texts_share_one_block_listing_their_packages() {
        let apache = || template_texts("Apache-2.0");
        let entries = vec![
            (package("b", "Apache-2.0"), apache()),
            (package("a", "Apache-2.0"), apache()),
            (
                package("c", "MIT"),
                package_texts(
                    &package("c", "MIT"),
                    vec![file("LICENSE", "Copyright (c) C\n")],
                ),
            ),
        ];
        let out = render_checked(&entries).unwrap();
        assert_eq!(out.matches("Apache License\n").count(), 1);
        let a = out.find("a 1.0.0 (Apache-2.0)").unwrap();
        let b = out.find("b 1.0.0 (Apache-2.0)").unwrap();
        assert!(a < b);
        assert!(out.contains("c 1.0.0 (MIT) [file LICENSE]"));
        assert!(out.contains("Copyright (c) C\n"));
    }

    #[test]
    fn template_use_is_recorded_as_having_no_copyright_line() {
        let entries = vec![(package("t", "MIT"), template_texts("MIT"))];
        let out = render_checked(&entries).unwrap();
        assert!(out.contains(
            "t 1.0.0 (MIT) [licence text from SPDX template, no copyright line in source: MIT]"
        ));
    }

    #[test]
    fn a_package_with_two_files_appears_under_both_texts() {
        let p = package("two", "MIT OR Apache-2.0");
        let texts = package_texts(
            &p,
            vec![
                file("LICENSE-MIT", "mit body"),
                file("LICENSE-APACHE", "apache body"),
            ],
        );
        let out = render_checked(&[(p, texts)]).unwrap();
        assert!(out.contains("[file LICENSE-MIT]"));
        assert!(out.contains("[file LICENSE-APACHE]"));
        assert!(out.contains("mit body\n"));
        assert!(out.contains("apache body\n"));
    }

    #[test]
    fn the_vendored_templates_have_text_and_no_copyright_line() {
        for (id, body) in TEMPLATES {
            assert!(body.len() > 500, "{id} is too short");
            assert!(
                !body
                    .lines()
                    .any(|l| l.trim_start().starts_with("Copyright (c) 2")),
                "{id} has a copyright line"
            );
        }
    }
}
