//! `THIRD-PARTY-LICENSES.txt`: the licence and notice texts of every third-party
//! package, taken from the package source, or from a vendored SPDX template when
//! the source carries none.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, bail};

use crate::dist::Package;

const COPYRIGHT_FROM_SOURCE_NOTE: &str =
    "licence text from SPDX template, copyright lines copied from the source";
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
    /// For a template: the copyright lines copied above it from the package source.
    pub has_copyright: bool,
}

/// Extra licence files inside a package, by package name (any version), that
/// cover code compiled into the binaries. A listed file that is missing fails the build.
const EXTRA_FILES: [(&str, &[&str]); 1] =
    [("regex-syntax", &["src/unicode_tables/LICENSE-UNICODE"])];

/// The extra licence files listed for a package name.
pub fn extra_files(name: &str) -> &'static [&'static str] {
    EXTRA_FILES
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(&[], |(_, files)| files)
}

/// Packages whose source refers to a licence text the crate does not ship; the
/// text is vendored in `third-party/licenses/`. It never counts as the package's own licence.
const VENDORED_EXTRAS: [(&str, &str, &str); 3] = [
    ("accesskit", ACCESSKIT_CHROMIUM_LABEL, ACCESSKIT_CHROMIUM),
    (
        "accesskit_consumer",
        ACCESSKIT_CHROMIUM_LABEL,
        ACCESSKIT_CHROMIUM,
    ),
    (
        "accesskit_windows",
        ACCESSKIT_CHROMIUM_LABEL,
        ACCESSKIT_CHROMIUM,
    ),
];
const ACCESSKIT_CHROMIUM_LABEL: &str =
    "upstream LICENSE.chromium, not shipped in the crate; referenced by its source";
const ACCESSKIT_CHROMIUM: &str =
    include_str!("../../third-party/licenses/accesskit-LICENSE.chromium.txt");

/// The vendored extra texts for a package name (any version).
pub fn vendored_extras(name: &str) -> Vec<Text> {
    VENDORED_EXTRAS
        .iter()
        .filter(|(n, _, _)| *n == name)
        .map(|(_, label, body)| Text {
            label: (*label).to_owned(),
            body: tidy(body),
            from_template: false,
            has_copyright: false,
        })
        .collect()
}

/// Whether a file is a licence proper: `LICENSE*`, `LICENCE*`, `COPYING*`,
/// `UNLICENSE*`, or the manifest's `license-file`. `NOTICE` and `COPYRIGHT` are not.
pub fn covers_licence(label: &str, license_file: Option<&str>) -> bool {
    let name = label.rsplit(['/', '\\']).next().unwrap_or(label);
    let upper = name.to_ascii_uppercase();
    let manifest = license_file
        .and_then(|f| f.rsplit(['/', '\\']).next())
        .is_some_and(|f| f == name);
    manifest
        || ["LICENSE", "LICENCE", "COPYING", "UNLICENSE"]
            .iter()
            .any(|p| upper.starts_with(p))
}

/// The lines of source text that state a copyright, with comment markers
/// stripped, in order and without repeats. `(c)` counts only with a digit.
pub fn copyright_lines(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for raw in text.lines() {
        let lower = raw.to_ascii_lowercase();
        let marked = lower.contains("copyright")
            || raw.contains('\u{a9}')
            || (lower.contains("(c)") && raw.bytes().any(|b| b.is_ascii_digit()));
        if !marked {
            continue;
        }
        let line = raw
            .trim_start_matches(|c: char| {
                matches!(c, '/' | '*' | '!' | '#' | '>') || c.is_whitespace()
            })
            .trim_end_matches(|c: char| c == '/' || c == '*' || c.is_whitespace());
        if !line.is_empty() && !found.iter().any(|l| l == line) {
            found.push(line.to_owned());
        }
    }
    found
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
                has_copyright: false,
            })
        })
        .collect();
    let needs_all = expression.split_whitespace().any(|t| t == "AND");
    if needs_all && texts.len() != ids.len() {
        return Vec::new();
    }
    texts
}

/// Whether one of a package's own files is its licence, so no template is needed.
pub fn covered_by_own_files(package: &Package, files: &[(String, String)]) -> bool {
    files.iter().any(|(label, body)| {
        !body.trim().is_empty() && covers_licence(label, package.license_file.as_deref())
    })
}

/// The texts of one package: its own files when one of them is a licence;
/// otherwise the templates of its declared licence, each headed by the
/// `copyright` lines found in the source, followed by any notice files.
pub fn package_texts(
    package: &Package,
    files: Vec<(String, String)>,
    copyright: &[String],
) -> Vec<Text> {
    let covered = covered_by_own_files(package, &files);
    let own = files
        .into_iter()
        .filter(|(_, body)| !body.trim().is_empty())
        .map(|(label, body)| Text {
            label,
            body: tidy(&body),
            from_template: false,
            has_copyright: false,
        });
    if covered {
        return own.collect();
    }
    let mut texts = template_texts(&package.license);
    if texts.is_empty() {
        return texts;
    }
    if !copyright.is_empty() {
        for text in &mut texts {
            text.body = format!("{}\n\n{}", copyright.join("\n"), text.body);
            text.has_copyright = true;
        }
    }
    texts.extend(own);
    texts
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
            let note = if text.from_template && text.has_copyright {
                format!("{COPYRIGHT_FROM_SOURCE_NOTE}: {}", text.label)
            } else if text.from_template {
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

/// Reads the files in `extra` (relative to `dir`) as `(relative path, text)`;
/// a missing one is an error.
fn read_extra(dir: &Path, extra: &[&str]) -> anyhow::Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    for rel in extra {
        let path = dir.join(rel);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("the listed licence file {} is missing", path.display()))?;
        files.push((
            (*rel).to_owned(),
            String::from_utf8_lossy(&bytes).into_owned(),
        ));
    }
    Ok(files)
}

/// The copyright lines in a package's `src/**/*.rs` and root `README*`.
fn read_copyright(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => walk(&path, out),
                Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "rs") => {
                    out.push(path)
                }
                _ => {}
            }
        }
    }
    let mut paths = Vec::new();
    walk(&dir.join("src"), &mut paths);
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with("readme") && entry.file_type().is_ok_and(|t| t.is_file()) {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    let mut found: Vec<String> = Vec::new();
    for path in paths {
        if let Ok(bytes) = std::fs::read(&path) {
            for line in copyright_lines(&String::from_utf8_lossy(&bytes)) {
                if !found.contains(&line) {
                    found.push(line);
                }
            }
        }
    }
    found
}

/// Reads the licence files in a package's folder as `(file name, text)`,
/// then the extra files listed for it.
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
    files.extend(read_extra(dir, extra_files(&package.name))?);
    Ok(files)
}

/// The text of `THIRD-PARTY-LICENSES.txt` for `packages`, read from their
/// source folders; an error names every package that has no text.
pub fn build(packages: &[Package]) -> anyhow::Result<String> {
    let mut entries = Vec::new();
    for package in packages {
        let files = read_files(package)?;
        let copyright = if covered_by_own_files(package, &files) {
            Vec::new()
        } else {
            read_copyright(&package.manifest_dir)
        };
        let mut texts = package_texts(package, files, &copyright);
        texts.extend(vendored_extras(&package.name));
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
            &[],
        );
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].body, "Copyright (c) A\n\nMIT text\n");
        assert!(!texts[0].from_template);
    }

    #[test]
    fn a_package_without_files_falls_back_to_the_templates_of_its_licence() {
        let p = package("a", "MIT OR Apache-2.0 OR 0BSD");
        let texts = package_texts(&p, vec![file("LICENSE", "  \n")], &[]);
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
        assert!(package_texts(&p, Vec::new(), &[]).is_empty());
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
                    &[],
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
            &[],
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

    #[test]
    fn copyright_lines_are_copied_verbatim_without_comment_markers() {
        let src = "//! Copyright 2021 The AccessKit Authors. All rights reserved.\n\
                   // Copyright (c) 2023.\n\
                   /* copyright 2020 X */\n\
                   // Copyright 2021 The AccessKit Authors. All rights reserved.\n\
                   let c = (c) + d;\n\
                   fn main() {}\n";
        assert_eq!(
            copyright_lines(src),
            [
                "Copyright 2021 The AccessKit Authors. All rights reserved.",
                "Copyright (c) 2023.",
                "copyright 2020 X"
            ]
        );
        assert!(copyright_lines("fn main() {}\n").is_empty());
    }

    #[test]
    fn a_template_gets_the_copyright_lines_found_in_the_source() {
        let p = package("a", "MIT");
        let lines = ["Copyright 2021 The A Authors.".to_owned()];
        let texts = package_texts(&p, Vec::new(), &lines);
        assert!(
            texts[0]
                .body
                .starts_with("Copyright 2021 The A Authors.\n\nMIT License")
        );
        assert!(texts[0].has_copyright);
        let out = render_checked(&[(p, texts)]).unwrap();
        assert!(out.contains("copyright lines copied from the source: MIT"));
        assert!(!out.contains("no copyright line in source"));
    }

    #[test]
    fn a_template_without_copyright_lines_keeps_the_no_copyright_note() {
        let p = package("a", "MIT");
        let texts = package_texts(&p, Vec::new(), &[]);
        assert!(!texts[0].has_copyright);
        let out = render_checked(&[(p, texts)]).unwrap();
        assert!(out.contains("no copyright line in source: MIT"));
    }

    #[test]
    fn a_notice_file_alone_does_not_cover_the_licence() {
        let p = package("a", "MIT");
        let files = vec![
            file("NOTICE", "Notice text"),
            file("COPYRIGHT", "Copyright 2020 B"),
        ];
        assert!(!covered_by_own_files(&p, &files));
        let texts = package_texts(&p, files, &[]);
        let labels: Vec<&str> = texts.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["MIT", "NOTICE", "COPYRIGHT"]);
        assert!(texts[0].from_template && !texts[1].from_template);
    }

    #[test]
    fn a_notice_file_with_no_known_template_still_fails() {
        let p = package("a", "0BSD");
        assert!(package_texts(&p, vec![file("NOTICE", "n")], &[]).is_empty());
    }

    #[test]
    fn the_manifest_licence_file_covers_the_licence() {
        let mut p = package("a", "MIT");
        p.license_file = Some("docs/terms.txt".to_owned());
        assert!(covered_by_own_files(&p, &[file("terms.txt", "terms")]));
        assert!(covers_licence("LICENSE-MIT", None));
        assert!(covers_licence("src/unicode_tables/LICENSE-UNICODE", None));
        assert!(!covers_licence("NOTICE", None));
    }

    #[test]
    fn regex_syntax_lists_its_nested_unicode_licence_for_any_version() {
        assert_eq!(
            extra_files("regex-syntax"),
            ["src/unicode_tables/LICENSE-UNICODE"]
        );
        assert!(extra_files("regex").is_empty());
    }

    #[test]
    fn accesskit_packages_get_the_vendored_chromium_licence_beside_the_template() {
        for name in ["accesskit", "accesskit_consumer", "accesskit_windows"] {
            let extras = vendored_extras(name);
            assert_eq!(extras.len(), 1, "{name}");
            assert!(extras[0].body.contains("The Chromium Authors"));
            assert!(extras[0].body.contains("Redistribution and use"));
        }
        assert!(vendored_extras("accesskit_other").is_empty());

        let p = package("accesskit", "MIT OR Apache-2.0");
        let mut texts = package_texts(&p, Vec::new(), &[]);
        texts.extend(vendored_extras(&p.name));
        let labels: Vec<&str> = texts.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels[..2], ["MIT", "Apache-2.0"]);
        let out = render_checked(&[(p, texts)]).unwrap();
        assert!(out.contains(
            "[file upstream LICENSE.chromium, not shipped in the crate; referenced by its source]"
        ));
    }

    #[test]
    fn a_listed_extra_file_that_is_missing_fails_loudly() {
        let dir = std::env::temp_dir().join(format!("yien-xtask-extra-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/LICENSE-X"), "text").unwrap();
        let found = read_extra(&dir, &["src/LICENSE-X"]).unwrap();
        assert_eq!(found, [file("src/LICENSE-X", "text")]);
        let error = read_extra(&dir, &["src/NOPE"]).unwrap_err();
        assert!(format!("{error:#}").contains("NOPE"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
