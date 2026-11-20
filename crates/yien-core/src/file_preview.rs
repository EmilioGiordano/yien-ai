//! Read-only file loading, binary and size capping, and syntax highlighting for
//! file preview panes (RF-CTX-02, ADR-06).
//!
//! Files are read and highlighted off the UI thread, capped at
//! [`MAX_PREVIEW_BYTES`] (2 MiB) and [`MAX_PREVIEW_LINES`] (20,000 lines), and
//! sliced to the pane's visible line viewport before crossing to the UI.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use synoptic::TokOpt;
use yien_proto::PaneId;
use yien_proto::file_preview::{FileFrame, FileLine, FileNotice, SyntaxKind, SyntaxSpan};

/// Maximum bytes read and highlighted for a previewed file (2 MiB).
pub const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

/// Maximum lines highlighted and retained for a previewed file (20,000 lines).
pub const MAX_PREVIEW_LINES: usize = 20_000;

/// Leading bytes inspected to classify a file as binary (8 KiB).
pub const BINARY_PROBE_BYTES: usize = 8_192;

/// Maximum byte length of a single line passed to the regex syntax highlighter.
/// Longer lines (such as minified bundles) are shown as plain text without
/// running regexes on them.
pub const MAX_HIGHLIGHT_LINE_BYTES: usize = 4_096;

/// Default visible row window before the UI reports a file pane's viewport.
pub const DEFAULT_VIEWPORT_ROWS: usize = 80;

/// Waiting visible frames for file preview panes, one slot per pane ("last one
/// wins", ADR-06).
#[derive(Debug, Default)]
pub(crate) struct FileFrames(Mutex<HashMap<PaneId, FileFrame>>);

impl FileFrames {
    fn slots(&self) -> MutexGuard<'_, HashMap<PaneId, FileFrame>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stores `frame` as `pane`'s latest visible frame. Returns `true` when the
    /// slot was empty and the UI is owed a [`yien_proto::CoreMsg::FileFrameReady`].
    pub fn put(&self, pane: PaneId, frame: FileFrame) -> bool {
        self.slots().insert(pane, frame).is_none()
    }

    /// Takes `pane`'s waiting visible frame; the next update is announced again.
    pub fn take(&self, pane: PaneId) -> Option<FileFrame> {
        self.slots().remove(&pane)
    }

    /// Drops any waiting frame for a closed pane.
    pub fn forget(&self, pane: PaneId) {
        self.slots().remove(&pane);
    }
}

/// A loaded and syntax-highlighted document held by the core so viewport
/// scrolls slice visible rows without re-reading disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDocument {
    /// Project-relative path of the file.
    pub path: String,
    /// Detected language label (`"Rust"`, `"TOML"`, `"Markdown"`), if any.
    pub language: Option<String>,
    /// All highlighted lines up to [`MAX_PREVIEW_LINES`].
    pub lines: Vec<FileLine>,
    /// Banner or replacement notice (truncated, binary, or error), if any.
    pub notice: Option<FileNotice>,
}

impl LoadedDocument {
    /// Slices only the visible rows `[top_line .. top_line + rows]` for the UI
    /// (ADR-06). A `top_line` past the last full page moves back to it, so the
    /// pane never shows a mostly empty page at the end.
    ///
    /// ```
    /// use yien_core::file_preview::load_bytes;
    ///
    /// let doc = load_bytes("src/lib.rs", b"fn a() {}\nfn b() {}\nfn c() {}\n", 30, false);
    /// let frame = doc.frame(1, 1);
    /// assert_eq!(frame.total_lines, 3);
    /// assert_eq!(frame.top_line, 1);
    /// assert_eq!(frame.lines.len(), 1);
    /// assert_eq!(frame.lines[0].number, 2);
    /// assert_eq!(doc.frame(9, 2).top_line, 1);
    /// ```
    pub fn frame(&self, top_line: usize, rows: usize) -> FileFrame {
        let rows = rows.max(1);
        let total_lines = self.lines.len();
        if total_lines == 0 {
            return FileFrame {
                path: self.path.clone(),
                language: self.language.clone(),
                total_lines: 0,
                top_line: 0,
                lines: Vec::new(),
                notice: self.notice.clone(),
            };
        }
        let top_line = top_line.min(total_lines.saturating_sub(rows));
        let end = top_line.saturating_add(rows).min(total_lines);
        FileFrame {
            path: self.path.clone(),
            language: self.language.clone(),
            total_lines,
            top_line,
            lines: self.lines[top_line..end].to_vec(),
            notice: self.notice.clone(),
        }
    }
}

/// Reads and highlights `rel_path` under `root`, capping at [`MAX_PREVIEW_BYTES`]
/// and [`MAX_PREVIEW_LINES`]. Never panics on missing files, directories,
/// binary content, or invalid UTF-8.
pub fn load_file(root: &Path, rel_path: &str) -> LoadedDocument {
    let full = match resolve_safe_path(root, rel_path) {
        Ok(path) => path,
        Err(message) => return error_document(rel_path, message),
    };
    let file = match File::open(&full) {
        Ok(file) => file,
        Err(err) => {
            return error_document(rel_path, format!("Could not read {rel_path}: {err}"));
        }
    };
    let metadata = match file.metadata() {
        Ok(meta) => meta,
        Err(err) => {
            return error_document(rel_path, format!("Could not inspect {rel_path}: {err}"));
        }
    };
    if metadata.is_dir() {
        return error_document(rel_path, format!("{rel_path} is a folder."));
    }
    let total_bytes = metadata.len();
    let cap = (MAX_PREVIEW_BYTES as u64).saturating_add(1);
    let mut buf = Vec::with_capacity((total_bytes.min(cap)) as usize);
    if let Err(err) = file.take(cap).read_to_end(&mut buf) {
        return error_document(rel_path, format!("Could not read {rel_path}: {err}"));
    }
    let truncated_bytes = buf.len() > MAX_PREVIEW_BYTES || total_bytes > MAX_PREVIEW_BYTES as u64;
    if buf.len() > MAX_PREVIEW_BYTES {
        buf.truncate(MAX_PREVIEW_BYTES);
    }
    let actual_total = total_bytes.max(buf.len() as u64);
    load_bytes(rel_path, &buf, actual_total, truncated_bytes)
}

fn error_document(rel_path: &str, message: String) -> LoadedDocument {
    LoadedDocument {
        path: rel_path.to_owned(),
        language: None,
        lines: Vec::new(),
        notice: Some(FileNotice::Error(message)),
    }
}

/// Resolves `rel_path` inside `root`, refusing empty paths, root/prefix
/// components, or parent-directory (`..`) traversal.
fn resolve_safe_path(root: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let trimmed = rel_path.trim();
    if trimmed.is_empty() {
        return Err("No file path was given.".to_owned());
    }
    let rel = Path::new(trimmed);
    let mut clean = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("Only files inside the project folder can be previewed.".to_owned());
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("No file path was given.".to_owned());
    }
    Ok(root.join(clean))
}

/// Classifies, caps, and syntax-highlights `bytes` for `rel_path`.
///
/// Pure and deterministic: requires no disk access and never panics on
/// arbitrary bytes or invalid UTF-8.
///
/// ```
/// use yien_core::file_preview::load_bytes;
/// use yien_proto::file_preview::{FileNotice, SyntaxKind};
///
/// let doc = load_bytes("main.rs", b"pub fn main() {\n    let x = 42;\n}\n", 34, false);
/// assert_eq!(doc.language.as_deref(), Some("Rust"));
/// assert_eq!(doc.lines.len(), 3);
/// assert!(doc.lines[0].spans.iter().any(|s| s.kind == SyntaxKind::Keyword));
///
/// let bin = load_bytes("app.exe", b"MZ\x90\x00\x03\x00\x00\x00", 8, false);
/// assert!(matches!(bin.notice, Some(FileNotice::Binary { bytes: 8 })));
/// assert!(bin.lines.is_empty());
/// ```
pub fn load_bytes(
    rel_path: &str,
    bytes: &[u8],
    total_bytes: u64,
    mut truncated_bytes: bool,
) -> LoadedDocument {
    let bytes = if bytes.len() > MAX_PREVIEW_BYTES {
        truncated_bytes = true;
        &bytes[..MAX_PREVIEW_BYTES]
    } else {
        bytes
    };
    let total_bytes = total_bytes.max(bytes.len() as u64);

    if is_binary(bytes) {
        return LoadedDocument {
            path: rel_path.to_owned(),
            language: None,
            lines: Vec::new(),
            notice: Some(FileNotice::Binary { bytes: total_bytes }),
        };
    }

    let usable = if truncated_bytes {
        trim_incomplete_utf8_tail(bytes)
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(usable);
    let (raw_lines, bytes_in_lines, capped_by_lines) = collect_capped_lines(&text);

    let notice = if truncated_bytes || capped_by_lines {
        let bytes_shown = if capped_by_lines {
            bytes_in_lines.min(usable.len())
        } else {
            usable.len()
        };
        Some(FileNotice::Truncated {
            bytes_shown,
            total_bytes,
            lines_shown: raw_lines.len(),
            capped_by_lines,
        })
    } else {
        None
    };

    let (language, ext_key) = detect_language(rel_path);
    let lines = highlight_lines(&raw_lines, ext_key);

    LoadedDocument {
        path: rel_path.to_owned(),
        language,
        lines,
        notice,
    }
}

/// Checks the first [`BINARY_PROBE_BYTES`] of `bytes` for NUL bytes or a high
/// share (> 30%) of non-whitespace control bytes.
pub fn is_binary(bytes: &[u8]) -> bool {
    let probe = &bytes[..bytes.len().min(BINARY_PROBE_BYTES)];
    if probe.is_empty() {
        return false;
    }
    let mut control = 0usize;
    for &b in probe {
        if b == 0 {
            return true;
        }
        if b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c | 0x1b) {
            control += 1;
        }
    }
    control * 10 > probe.len() * 3
}

/// Drops up to 3 trailing bytes when a byte cap cut a multi-byte UTF-8
/// codepoint in half at the end of the slice.
fn trim_incomplete_utf8_tail(bytes: &[u8]) -> &[u8] {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes,
        Err(err) => {
            if err.error_len().is_none() && bytes.len().saturating_sub(err.valid_up_to()) <= 3 {
                &bytes[..err.valid_up_to()]
            } else {
                bytes
            }
        }
    }
}

/// Splits `text` into at most [`MAX_PREVIEW_LINES`] lines, expanding tabs to
/// 4 spaces so column widths and token indices stay aligned.
fn collect_capped_lines(text: &str) -> (Vec<String>, usize, bool) {
    if text.is_empty() {
        return (Vec::new(), 0, false);
    }
    let mut lines = Vec::new();
    let mut bytes_shown = 0usize;
    let mut capped_by_lines = false;
    let mut iter = text.split('\n');
    for raw in iter.by_ref() {
        if lines.len() >= MAX_PREVIEW_LINES {
            // An empty trailing segment after a final '\n' is not an extra line.
            if !raw.is_empty() || iter.next().is_some() {
                capped_by_lines = true;
            }
            break;
        }
        bytes_shown += raw.len() + 1;
        let clean = raw.strip_suffix('\r').unwrap_or(raw);
        let expanded = if clean.contains('\t') {
            clean.replace('\t', "    ")
        } else {
            clean.to_owned()
        };
        lines.push(expanded);
    }
    // If the file ended with a trailing newline and was not line-capped,
    // `split('\n')` produces one empty element after the last '\n'; drop it.
    if !capped_by_lines && text.ends_with('\n') && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    (lines, bytes_shown.min(text.len()), capped_by_lines)
}

/// Returns `(display_language, synoptic_extension_key)` for `rel_path`.
fn detect_language(rel_path: &str) -> (Option<String>, Option<&'static str>) {
    let file_name = Path::new(rel_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(rel_path);
    let lower_name = file_name.to_ascii_lowercase();
    match lower_name.as_str() {
        "cargo.lock" => return (Some("TOML".into()), Some("toml")),
        "dockerfile" => return (Some("Shell".into()), Some("sh")),
        "makefile" | "gnumakefile" => return (Some("Shell".into()), Some("sh")),
        ".gitignore" | ".gitattributes" | ".yien-local" | ".yien-local.example" => {
            return (Some("Config".into()), Some("sh"));
        }
        _ => {}
    }
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => (Some("Rust".into()), Some("rs")),
        "toml" | "lock" => (Some("TOML".into()), Some("toml")),
        "json" | "jsonl" => (Some("JSON".into()), Some("json")),
        "md" | "markdown" => (Some("Markdown".into()), Some("md")),
        "yaml" | "yml" => (Some("YAML".into()), Some("yaml")),
        "html" | "htm" | "xhtml" => (Some("HTML".into()), Some("html")),
        "css" => (Some("CSS".into()), Some("css")),
        "js" | "jsx" | "mjs" | "cjs" => (Some("JavaScript".into()), Some("js")),
        "ts" | "tsx" | "mts" | "cts" => (Some("TypeScript".into()), Some("ts")),
        "py" | "pyw" => (Some("Python".into()), Some("py")),
        "sh" | "bash" | "zsh" | "fish" => (Some("Shell".into()), Some("sh")),
        "ps1" | "psm1" | "psd1" => (Some("PowerShell".into()), Some("sh")),
        "go" => (Some("Go".into()), Some("go")),
        "c" | "h" => (Some("C".into()), Some("c")),
        "cpp" | "hpp" | "cc" | "cxx" | "c++" => (Some("C++".into()), Some("cpp")),
        "cs" | "csproj" => (Some("C#".into()), Some("cs")),
        "java" => (Some("Java".into()), Some("java")),
        "kt" => (Some("Kotlin".into()), Some("kt")),
        "swift" => (Some("Swift".into()), Some("swift")),
        "rb" => (Some("Ruby".into()), Some("rb")),
        "lua" => (Some("Lua".into()), Some("lua")),
        "sql" => (Some("SQL".into()), Some("sql")),
        "xml" | "svg" => (Some("XML".into()), Some("xml")),
        "diff" | "patch" => (Some("Diff".into()), Some("diff")),
        "csv" => (Some("CSV".into()), Some("csv")),
        "nu" => (Some("Nushell".into()), Some("nu")),
        "tex" => (Some("TeX".into()), Some("tex")),
        "asm" | "s" => (Some("Assembly".into()), Some("asm")),
        _ => (None, None),
    }
}

fn highlight_lines(raw_lines: &[String], ext_key: Option<&'static str>) -> Vec<FileLine> {
    if raw_lines.is_empty() {
        return Vec::new();
    }
    let Some(ext) = ext_key else {
        return plain_lines(raw_lines);
    };
    let Some(mut highlighter) = synoptic::from_extension(ext, 4) else {
        return plain_lines(raw_lines);
    };

    // Guard regex evaluation against huge single-line minified files.
    let safe_lines: Vec<String> = raw_lines
        .iter()
        .map(|line| {
            if line.len() <= MAX_HIGHLIGHT_LINE_BYTES {
                line.clone()
            } else {
                String::new()
            }
        })
        .collect();
    highlighter.run(&safe_lines);

    raw_lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            if line.is_empty() || line.len() > MAX_HIGHLIGHT_LINE_BYTES {
                FileLine {
                    number: idx + 1,
                    text: line.clone(),
                    spans: Vec::new(),
                }
            } else {
                let (text, spans) = tokens_to_line(highlighter.line(idx, line));
                FileLine {
                    number: idx + 1,
                    text,
                    spans,
                }
            }
        })
        .collect()
}

fn plain_lines(raw_lines: &[String]) -> Vec<FileLine> {
    raw_lines
        .iter()
        .enumerate()
        .map(|(idx, line)| FileLine {
            number: idx + 1,
            text: line.clone(),
            spans: Vec::new(),
        })
        .collect()
}

fn tokens_to_line(tokens: Vec<TokOpt>) -> (String, Vec<SyntaxSpan>) {
    let mut line_text = String::new();
    let mut spans: Vec<SyntaxSpan> = Vec::new();
    for token in tokens {
        let (tok_text, kind) = match token {
            TokOpt::Some(tok_text, name) => {
                let kind = classify_token(&name, &tok_text);
                (tok_text, kind)
            }
            TokOpt::None(tok_text) => (tok_text, SyntaxKind::Plain),
        };
        if tok_text.is_empty() {
            continue;
        }
        let start = line_text.len();
        line_text.push_str(&tok_text);
        let end = line_text.len();
        if kind == SyntaxKind::Plain {
            continue;
        }
        if let Some(last) = spans.last_mut()
            && last.kind == kind
            && last.end == start
        {
            last.end = end;
        } else {
            spans.push(SyntaxSpan { start, end, kind });
        }
    }
    (line_text, spans)
}

fn classify_token(name: &str, text: &str) -> SyntaxKind {
    match name {
        "comment" | "quote" | "deletion" => SyntaxKind::Comment,
        "string" | "character" | "block" | "insertion" => SyntaxKind::String,
        "digit" | "boolean" | "math" => SyntaxKind::Number,
        "function" | "heading" | "table" => SyntaxKind::Function,
        "struct" | "namespace" | "type" | "class" | "tag" | "key" => SyntaxKind::Type,
        "macro" | "attribute" | "link" | "image" => SyntaxKind::Macro,
        "keyword" | "reference" | "bold" | "italic" | "list" | "linebreak" => {
            if is_type_keyword(text.trim()) {
                SyntaxKind::Type
            } else {
                SyntaxKind::Keyword
            }
        }
        _ => SyntaxKind::Plain,
    }
}

fn is_type_keyword(word: &str) -> bool {
    if word.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return true;
    }
    matches!(
        word,
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
            | "bool"
            | "str"
            | "char"
            | "int"
            | "float"
            | "double"
            | "void"
            | "number"
            | "string"
            | "boolean"
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn span_has(line: &FileLine, kind: SyntaxKind, needle: &str) -> bool {
        line.spans
            .iter()
            .any(|s| s.kind == kind && line.text[s.start..s.end].contains(needle))
    }

    #[test]
    fn rf_ctx_02_rust_source_highlights_keywords_types_functions_strings_and_comments() {
        let src = b"// Header comment\n#[derive(Debug)]\npub fn greet(name: &str) -> usize {\n    let msg = \"hello\";\n    println!(\"{msg}\");\n    42\n}\n";
        let doc = load_bytes("src/lib.rs", src, src.len() as u64, false);
        assert_eq!(doc.language.as_deref(), Some("Rust"));
        assert_eq!(doc.notice, None);
        assert_eq!(doc.lines.len(), 7);
        assert!(span_has(&doc.lines[0], SyntaxKind::Comment, "// Header"));
        assert!(span_has(&doc.lines[1], SyntaxKind::Macro, "#[derive"));
        assert!(span_has(&doc.lines[2], SyntaxKind::Keyword, "pub"));
        assert!(span_has(&doc.lines[2], SyntaxKind::Function, "greet"));
        assert!(span_has(&doc.lines[2], SyntaxKind::Type, "usize"));
        assert!(span_has(&doc.lines[3], SyntaxKind::String, "hello"));
        assert!(span_has(&doc.lines[5], SyntaxKind::Number, "42"));
    }

    #[test]
    fn rf_ctx_02_binary_files_show_a_binary_notice_and_no_garbage_lines() {
        let bin = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let doc = load_bytes("target/app", bin, 16_384, false);
        assert_eq!(doc.notice, Some(FileNotice::Binary { bytes: 16_384 }));
        assert!(doc.lines.is_empty());
        let frame = doc.frame(0, 40);
        assert_eq!(frame.notice, Some(FileNotice::Binary { bytes: 16_384 }));
        assert!(frame.lines.is_empty());
    }

    #[test]
    fn rf_ctx_02_large_files_are_capped_by_lines_and_bytes_with_a_clear_notice() {
        let many_lines: String = (0..MAX_PREVIEW_LINES + 500)
            .map(|i| format!("let line_{i} = {i};\n"))
            .collect();
        let doc = load_bytes(
            "src/big.rs",
            many_lines.as_bytes(),
            many_lines.len() as u64,
            false,
        );
        assert_eq!(doc.lines.len(), MAX_PREVIEW_LINES);
        assert!(matches!(
            doc.notice,
            Some(FileNotice::Truncated {
                lines_shown: MAX_PREVIEW_LINES,
                capped_by_lines: true,
                ..
            })
        ));

        let byte_capped = load_bytes("src/huge.txt", b"hello\nworld\n", 5 * 1024 * 1024, true);
        assert_eq!(
            byte_capped.notice,
            Some(FileNotice::Truncated {
                bytes_shown: 12,
                total_bytes: 5 * 1024 * 1024,
                lines_shown: 2,
                capped_by_lines: false,
            })
        );
    }

    #[test]
    fn rf_ctx_02_invalid_utf8_without_nul_never_panics() {
        let invalid = b"valid line\n\xff\xfeinvalid utf8 sequence\nthird line\n";
        let doc = load_bytes("notes.txt", invalid, invalid.len() as u64, false);
        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.lines[0].text, "valid line");
        assert!(doc.lines[1].text.contains("invalid utf8 sequence"));
        assert_eq!(doc.lines[2].text, "third line");
    }

    #[test]
    fn rf_ctx_02_path_traversal_outside_project_root_is_refused() {
        let root = Path::new("C:/Code/yien");
        for bad in ["../secret.txt", "src/../../outside.rs", "/etc/passwd", ""] {
            let doc = load_file(root, bad);
            assert!(
                matches!(doc.notice, Some(FileNotice::Error(_))),
                "expected error notice for {bad:?}, got {:?}",
                doc.notice
            );
        }
    }

    #[test]
    fn rf_ctx_02_a_frame_past_the_end_shows_the_last_full_page() {
        let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let doc = load_bytes("notes.txt", text.as_bytes(), text.len() as u64, false);
        let frame = doc.frame(99, 30);
        assert_eq!(frame.top_line, 70);
        assert_eq!(frame.lines.len(), 30);
        assert_eq!(frame.lines[29].number, 100);
        let short = doc.frame(5, 500);
        assert_eq!((short.top_line, short.lines.len()), (0, 100));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn rf_ctx_02_arbitrary_bytes_and_extensions_never_panic(
            ext in prop::sample::select(vec!["rs", "toml", "json", "md", "ts", "py", "sh", "txt", ""]),
            bytes in prop::collection::vec(any::<u8>(), 0..4096),
            truncated in any::<bool>(),
            top in 0usize..100,
            rows in 1usize..80,
        ) {
            let path = if ext.is_empty() {
                "Makefile".to_owned()
            } else {
                format!("src/sample.{ext}")
            };
            let doc = load_bytes(&path, &bytes, bytes.len() as u64, truncated);
            for line in &doc.lines {
                for span in &line.spans {
                    prop_assert!(span.start <= span.end);
                    prop_assert!(span.end <= line.text.len());
                    prop_assert!(line.text.is_char_boundary(span.start));
                    prop_assert!(line.text.is_char_boundary(span.end));
                }
            }
            let frame = doc.frame(top, rows);
            prop_assert!(frame.lines.len() <= rows);
        }
    }
}
