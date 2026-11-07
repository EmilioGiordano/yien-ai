//! Parser for `git status --porcelain=v2 --branch -z`, a format git keeps stable
//! for scripts. Its output is untrusted: anything malformed is a [`ParseError`],
//! never a panic.

use std::borrow::Cow;

use crate::ParseError;

/// What HEAD points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// On a branch with at least one commit.
    Branch {
        /// The branch name, such as `main` or `feat/x`.
        name: String,
        /// The full commit id.
        oid: String,
    },
    /// On a branch that has no commit yet (a fresh `git init`).
    Unborn {
        /// The branch name.
        name: String,
    },
    /// Not on a branch.
    Detached {
        /// The full commit id.
        oid: String,
    },
}

impl Head {
    /// The text the UI shows: the branch name, or a short commit id when detached.
    ///
    /// ```
    /// use yien_git::Head;
    /// let head = Head::Detached { oid: "0123456789abcdef".into() };
    /// assert_eq!(head.label(), "0123456");
    /// let head = Head::Unborn { name: "main".into() };
    /// assert_eq!(head.label(), "main");
    /// ```
    pub fn label(&self) -> &str {
        match self {
            Self::Branch { name, .. } | Self::Unborn { name } => name,
            Self::Detached { oid } => short_oid(oid),
        }
    }

    /// The branch name, `None` when detached.
    ///
    /// ```
    /// use yien_git::Head;
    /// assert_eq!(Head::Unborn { name: "main".into() }.branch(), Some("main"));
    /// assert_eq!(Head::Detached { oid: "abc".into() }.branch(), None);
    /// ```
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Branch { name, .. } | Self::Unborn { name } => Some(name),
            Self::Detached { .. } => None,
        }
    }
}

/// The first 7 characters of a commit id, or all of it when shorter.
fn short_oid(oid: &str) -> &str {
    oid.char_indices().nth(7).map_or(oid, |(i, _)| &oid[..i])
}

/// Commits the branch has that its upstream lacks, and the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AheadBehind {
    /// Local commits not in the upstream.
    pub ahead: u32,
    /// Upstream commits not in the local branch.
    pub behind: u32,
}

/// The branch's upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// Its name, such as `origin/main`.
    pub name: String,
    /// `None` when the upstream branch is configured but gone.
    pub ahead_behind: Option<AheadBehind>,
}

/// How a file differs from the last commit, staged and unstaged changes together.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileStatus {
    /// Content or type changed.
    Modified,
    /// New and tracked (staged, or added with intent to add).
    Added,
    /// Removed.
    Deleted,
    /// Moved from another path.
    Renamed {
        /// The path before the rename.
        from: String,
    },
    /// Copied from another path.
    Copied {
        /// The path it was copied from.
        from: String,
    },
    /// Not tracked by git.
    Untracked,
    /// In a merge conflict.
    Conflicted,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Path relative to the repository root, with `/` separators as git prints it.
    pub path: String,
    /// How it changed.
    pub status: FileStatus,
}

/// The parsed output of `git status --porcelain=v2 --branch -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// What HEAD points at.
    pub head: Head,
    /// The upstream, if the branch has one.
    pub upstream: Option<Upstream>,
    /// Changed, untracked and conflicted files, in git's order. Ignored files are left out.
    pub files: Vec<ChangedFile>,
}

impl Status {
    /// Whether anything differs from the last commit, untracked files included.
    ///
    /// ```
    /// let out = b"# branch.oid (initial)\0# branch.head main\0? new.txt\0";
    /// let status = yien_git::parse_status(out)?;
    /// assert!(status.is_dirty());
    /// # Ok::<(), yien_git::ParseError>(())
    /// ```
    pub fn is_dirty(&self) -> bool {
        !self.files.is_empty()
    }
}

/// Parses `git status --porcelain=v2 --branch -z` output.
///
/// Unknown `#` headers are skipped, so a newer git that adds one still parses.
///
/// ```
/// use yien_git::FileStatus;
/// let out = [
///     "# branch.oid 0123456789abcdef0123456789abcdef01234567",
///     "# branch.head main",
///     "# branch.upstream origin/main",
///     "# branch.ab +2 -1",
///     "1 .M N... 100644 100644 100644 aaaa aaaa src/lib.rs",
///     "? notes.txt",
/// ]
/// .join("\0");
/// let status = yien_git::parse_status(out.as_bytes())?;
/// assert_eq!(status.head.label(), "main");
/// let ab = status.upstream.unwrap().ahead_behind.unwrap();
/// assert_eq!((ab.ahead, ab.behind), (2, 1));
/// assert_eq!(status.files[0].status, FileStatus::Modified);
/// assert_eq!(status.files[1].status, FileStatus::Untracked);
/// # Ok::<(), yien_git::ParseError>(())
/// ```
pub fn parse_status(output: &[u8]) -> Result<Status, ParseError> {
    let mut oid: Option<String> = None;
    let mut head_name: Option<String> = None;
    let mut upstream: Option<Upstream> = None;
    let mut files = Vec::new();

    let mut records = output.split(|&b| b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let text = String::from_utf8_lossy(record);
        let (tag, rest) = text.split_once(' ').unwrap_or((&text, ""));
        match tag {
            "#" => {
                let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
                match key {
                    "branch.oid" => oid = Some(value.to_owned()),
                    "branch.head" => head_name = Some(value.to_owned()),
                    "branch.upstream" => {
                        upstream = Some(Upstream {
                            name: value.to_owned(),
                            ahead_behind: upstream.and_then(|u| u.ahead_behind),
                        });
                    }
                    "branch.ab" => {
                        let ab = parse_ahead_behind(value)?;
                        match &mut upstream {
                            Some(u) => u.ahead_behind = Some(ab),
                            None => return Err(bad("# branch.ab", "no branch.upstream before it")),
                        }
                    }
                    _ => {}
                }
            }
            "1" => {
                let fields = exact_fields(rest, 8, "1")?;
                let xy = xy(fields[0], "1")?;
                files.push(ChangedFile {
                    path: fields[7].to_owned(),
                    status: ordinary_status(xy),
                });
            }
            "2" => {
                let fields = exact_fields(rest, 9, "2")?;
                xy(fields[0], "2")?;
                let from = records
                    .next()
                    .map(String::from_utf8_lossy)
                    .ok_or_else(|| bad("2", "missing original path"))?
                    .into_owned();
                let status = match fields[7].as_bytes().first() {
                    Some(b'R') => FileStatus::Renamed { from },
                    Some(b'C') => FileStatus::Copied { from },
                    _ => return Err(bad("2", "score is neither R nor C")),
                };
                files.push(ChangedFile {
                    path: fields[8].to_owned(),
                    status,
                });
            }
            "u" => {
                let fields = exact_fields(rest, 10, "u")?;
                xy(fields[0], "u")?;
                files.push(ChangedFile {
                    path: fields[9].to_owned(),
                    status: FileStatus::Conflicted,
                });
            }
            "?" => files.push(ChangedFile {
                path: non_empty_path(rest, "?")?,
                status: FileStatus::Untracked,
            }),
            "!" => {}
            _ => return Err(bad(tag, "unknown record")),
        }
    }

    let head = match (head_name, oid) {
        (None, _) => return Err(bad("# branch.head", "missing")),
        (Some(_), None) => return Err(bad("# branch.oid", "missing")),
        (Some(name), Some(oid)) if name == "(detached)" => {
            if oid == "(initial)" {
                return Err(bad("# branch.oid", "detached with no commit"));
            }
            Head::Detached { oid }
        }
        (Some(name), Some(oid)) if oid == "(initial)" => Head::Unborn { name },
        (Some(name), Some(oid)) => Head::Branch { name, oid },
    };
    Ok(Status {
        head,
        upstream,
        files,
    })
}

/// Splits `rest` into exactly `n` space-separated fields; the last keeps its spaces.
fn exact_fields<'a>(rest: &'a str, n: usize, kind: &str) -> Result<Vec<&'a str>, ParseError> {
    let fields: Vec<&str> = rest.splitn(n, ' ').collect();
    if fields.len() != n || fields[n - 1].is_empty() {
        return Err(bad(kind, "too few fields"));
    }
    Ok(fields)
}

fn xy(field: &str, kind: &str) -> Result<[u8; 2], ParseError> {
    match field.as_bytes() {
        &[x, y] => Ok([x, y]),
        _ => Err(bad(kind, "XY is not two characters")),
    }
}

fn ordinary_status([x, y]: [u8; 2]) -> FileStatus {
    if x == b'A' || y == b'A' {
        FileStatus::Added
    } else if x == b'D' || y == b'D' {
        FileStatus::Deleted
    } else {
        FileStatus::Modified
    }
}

fn non_empty_path(rest: &str, kind: &str) -> Result<String, ParseError> {
    if rest.is_empty() {
        return Err(bad(kind, "empty path"));
    }
    Ok(rest.to_owned())
}

fn parse_ahead_behind(value: &str) -> Result<AheadBehind, ParseError> {
    let mut parts = value.split(' ');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad("# branch.ab", "expected two counts"));
    };
    let count = |s: &str, sign: char| {
        s.strip_prefix(sign)
            .and_then(|n| n.parse::<u32>().ok())
            .ok_or_else(|| bad("# branch.ab", "bad count"))
    };
    Ok(AheadBehind {
        ahead: count(a, '+')?,
        behind: count(b, '-')?,
    })
}

fn bad(kind: &str, reason: &'static str) -> ParseError {
    const MAX_KIND: usize = 32;
    let kind: Cow<'_, str> = if kind.chars().count() > MAX_KIND {
        Cow::Owned(kind.chars().take(MAX_KIND).collect())
    } else {
        Cow::Borrowed(kind)
    };
    ParseError::BadRecord {
        kind: kind.into_owned(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn parse(text: &str) -> Status {
        parse_status(text.as_bytes()).unwrap()
    }

    #[test]
    fn branch_with_upstream_and_counts() {
        let s = parse(&format!(
            "# branch.oid {OID}\0# branch.head feat/x\0# branch.upstream origin/feat/x\0# branch.ab +3 -12\0"
        ));
        assert_eq!(
            s.head,
            Head::Branch {
                name: "feat/x".into(),
                oid: OID.into()
            }
        );
        assert_eq!(
            s.upstream,
            Some(Upstream {
                name: "origin/feat/x".into(),
                ahead_behind: Some(AheadBehind {
                    ahead: 3,
                    behind: 12
                }),
            })
        );
        assert!(!s.is_dirty());
    }

    #[test]
    fn detached_head_shows_short_oid() {
        let s = parse(&format!("# branch.oid {OID}\0# branch.head (detached)\0"));
        assert_eq!(s.head, Head::Detached { oid: OID.into() });
        assert_eq!(s.head.label(), "0123456");
        assert_eq!(s.head.branch(), None);
        assert_eq!(s.upstream, None);
    }

    #[test]
    fn no_upstream() {
        let s = parse(&format!("# branch.oid {OID}\0# branch.head main\0"));
        assert_eq!(s.upstream, None);
    }

    #[test]
    fn gone_upstream_has_no_counts() {
        let s = parse(&format!(
            "# branch.oid {OID}\0# branch.head main\0# branch.upstream origin/gone\0"
        ));
        assert_eq!(
            s.upstream,
            Some(Upstream {
                name: "origin/gone".into(),
                ahead_behind: None
            })
        );
    }

    #[test]
    fn unborn_branch() {
        let s = parse("# branch.oid (initial)\0# branch.head main\0");
        assert_eq!(
            s.head,
            Head::Unborn {
                name: "main".into()
            }
        );
    }

    #[test]
    fn unknown_headers_are_skipped() {
        let s = parse(&format!(
            "# stash 3\0# branch.oid {OID}\0# branch.future thing\0# branch.head main\0"
        ));
        assert_eq!(s.head.label(), "main");
    }

    #[test]
    fn rf_ctx_04_ordinary_changes() {
        let s = parse(&format!(
            "# branch.oid {OID}\0# branch.head main\0\
             1 .M N... 100644 100644 100644 {OID} {OID} src/lib.rs\0\
             1 M. N... 100644 100644 100644 {OID} {OID} staged.rs\0\
             1 A. N... 000000 100644 100644 {OID} {OID} new.rs\0\
             1 .A N... 000000 000000 100644 {OID} {OID} intent.rs\0\
             1 D. N... 100644 000000 000000 {OID} {OID} gone.rs\0\
             1 .D N... 100644 100644 000000 {OID} {OID} gone2.rs\0\
             1 .T N... 100644 100644 120000 {OID} {OID} link\0"
        ));
        let got: Vec<(&str, FileStatus)> = s
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("src/lib.rs", FileStatus::Modified),
                ("staged.rs", FileStatus::Modified),
                ("new.rs", FileStatus::Added),
                ("intent.rs", FileStatus::Added),
                ("gone.rs", FileStatus::Deleted),
                ("gone2.rs", FileStatus::Deleted),
                ("link", FileStatus::Modified),
            ]
        );
        assert!(s.is_dirty());
    }

    #[test]
    fn rf_ctx_04_renames_and_copies() {
        let s = parse(&format!(
            "# branch.oid {OID}\0# branch.head main\0\
             2 R. N... 100644 100644 100644 {OID} {OID} R100 new name.rs\0old name.rs\0\
             2 C. N... 100644 100644 100644 {OID} {OID} C75 copy.rs\0orig.rs\0\
             ? after.txt\0"
        ));
        assert_eq!(
            s.files,
            vec![
                ChangedFile {
                    path: "new name.rs".into(),
                    status: FileStatus::Renamed {
                        from: "old name.rs".into()
                    }
                },
                ChangedFile {
                    path: "copy.rs".into(),
                    status: FileStatus::Copied {
                        from: "orig.rs".into()
                    }
                },
                ChangedFile {
                    path: "after.txt".into(),
                    status: FileStatus::Untracked
                },
            ]
        );
    }

    #[test]
    fn rf_ctx_04_paths_with_spaces_and_unicode() {
        let s = parse(&format!(
            "# branch.oid {OID}\0# branch.head main\0\
             1 .M N... 100644 100644 100644 {OID} {OID} dir with spaces/año ü 日本.txt\0\
             ?  leading space.txt\0\
             ? emoji 🦀.rs\0"
        ));
        let paths: Vec<&str> = s.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "dir with spaces/año ü 日本.txt",
                " leading space.txt",
                "emoji 🦀.rs"
            ]
        );
    }

    #[test]
    fn rf_ctx_04_paths_with_newlines() {
        let s = parse(&format!(
            "# branch.oid {OID}\0# branch.head main\0\
             1 .M N... 100644 100644 100644 {OID} {OID} two\nlines.rs\0\
             2 R. N... 100644 100644 100644 {OID} {OID} R100 new\nname\0old\nname\0\
             ? un\ntracked\0"
        ));
        assert_eq!(
            s.files,
            vec![
                ChangedFile {
                    path: "two\nlines.rs".into(),
                    status: FileStatus::Modified
                },
                ChangedFile {
                    path: "new\nname".into(),
                    status: FileStatus::Renamed {
                        from: "old\nname".into()
                    }
                },
                ChangedFile {
                    path: "un\ntracked".into(),
                    status: FileStatus::Untracked
                },
            ]
        );
    }

    #[test]
    fn rf_ctx_04_untracked_and_ignored() {
        let s = parse("# branch.oid (initial)\0# branch.head main\0? a.txt\0? dir/\0! target/\0");
        assert_eq!(
            s.files,
            vec![
                ChangedFile {
                    path: "a.txt".into(),
                    status: FileStatus::Untracked
                },
                ChangedFile {
                    path: "dir/".into(),
                    status: FileStatus::Untracked
                },
            ]
        );
    }

    #[test]
    fn rf_ctx_04_conflicts() {
        let s = parse(&format!(
            "# branch.oid {OID}\0# branch.head main\0\
             u UU N... 100644 100644 100644 100644 {OID} {OID} {OID} both edited.rs\0"
        ));
        assert_eq!(
            s.files,
            vec![ChangedFile {
                path: "both edited.rs".into(),
                status: FileStatus::Conflicted
            }]
        );
    }

    #[test]
    fn non_utf8_path_is_replaced_not_rejected() {
        let mut out = format!("# branch.oid {OID}\0# branch.head main\0? bad").into_bytes();
        out.extend_from_slice(&[0xff, 0xfe, b'.', b't', 0]);
        let s = parse_status(&out).unwrap();
        assert!(s.files[0].path.starts_with("bad"));
    }

    #[test]
    fn malformed_input_is_an_error() {
        let head = format!("# branch.oid {OID}\0# branch.head main\0");
        let cases = [
            String::new(),
            format!("# branch.oid {OID}\0"),
            "# branch.head main\0".to_owned(),
            "# branch.oid (initial)\0# branch.head (detached)\0".to_owned(),
            format!("{head}1 .M N... 100644\0"),
            format!("{head}1 .MX N... 100644 100644 100644 {OID} {OID} p\0"),
            format!("{head}2 R. N... 100644 100644 100644 {OID} {OID} R100 new\0"),
            format!("{head}2 R. N... 100644 100644 100644 {OID} {OID} X100 new\0old\0"),
            format!("{head}u UU N... 100644 100644\0"),
            format!("{head}? \0"),
            format!("{head}Z what\0"),
            format!("{head}# branch.ab +1 -2\0"),
            format!("{head}# branch.upstream o/m\0# branch.ab 1 2\0"),
            format!("{head}# branch.upstream o/m\0# branch.ab +1\0"),
            format!("{head}# branch.upstream o/m\0# branch.ab +99999999999 -0\0"),
        ];
        for case in cases {
            assert!(parse_status(case.as_bytes()).is_err(), "{case:?} parsed");
        }
    }

    #[test]
    fn short_oid_handles_short_and_odd_input() {
        assert_eq!(short_oid("abc"), "abc");
        assert_eq!(short_oid(""), "");
        assert_eq!(short_oid("ééééééééé"), "ééééééé");
    }

    fn record() -> impl Strategy<Value = Vec<u8>> {
        let prefix = prop_oneof![
            Just(&b"# branch.oid "[..]),
            Just(&b"# branch.head "[..]),
            Just(&b"# branch.upstream "[..]),
            Just(&b"# branch.ab "[..]),
            Just(&b"1 "[..]),
            Just(&b"2 "[..]),
            Just(&b"u "[..]),
            Just(&b"? "[..]),
            Just(&b"! "[..]),
            Just(&b""[..]),
        ];
        let body = prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..40),
            "[ .MADRCTU+0-9a-z(]{0,60}".prop_map(String::into_bytes),
        ];
        (prefix, body).prop_map(|(p, b)| [p, &b].concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_status(&bytes);
        }

        #[test]
        fn record_shaped_garbage_never_panics(records in proptest::collection::vec(record(), 0..12)) {
            let _ = parse_status(&records.join(&0u8));
        }
    }
}
