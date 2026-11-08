//! The user's own profiles (RF-AGE-04, 04 §4.7): the files of the profiles
//! folder, already read, turned into profiles and the problems that kept
//! some of them out. Reading the folder is the platform layer's job.

use std::collections::BTreeMap;

use crate::profile::{Profile, ProfileError};

/// The folder under YIEN's config dir that holds user profiles.
pub const PROFILES_DIR: &str = "profiles";

/// The most profile files read from the folder; later ones by name are
/// skipped with a problem.
pub const MAX_USER_PROFILES: usize = 64;

/// What kind of problem kept a profile file out. Logs carry the kind, not
/// the message, which can quote the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    /// The file could not be read, is not UTF-8 or is too large.
    Unreadable,
    /// The TOML does not parse or a value has the wrong type.
    Syntax,
    /// A value parses but is not acceptable, or the schema is unknown.
    Invalid,
    /// An earlier file by name already defines the same id.
    DuplicateId,
    /// More files than [`MAX_USER_PROFILES`], or a folder YIEN refuses.
    Skipped,
}

impl ProblemKind {
    /// A short name for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            ProblemKind::Unreadable => "unreadable",
            ProblemKind::Syntax => "syntax",
            ProblemKind::Invalid => "invalid",
            ProblemKind::DuplicateId => "duplicate id",
            ProblemKind::Skipped => "skipped",
        }
    }
}

/// Why a profile file was left out, and where in it.
///
/// ```
/// let p = yien_agent::ProfileProblem {
///     file: "bad.toml".into(),
///     place: Some((3, 7)),
///     kind: yien_agent::ProblemKind::Syntax,
///     message: "expected a value".into(),
/// };
/// assert_eq!(p.to_string(), "profiles/bad.toml, line 3, column 7: expected a value");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProblem {
    /// The file's name in the profiles folder; the folder itself when the
    /// problem is not about one file.
    pub file: String,
    /// 1-based line and column, when the problem points into the file.
    pub place: Option<(usize, usize)>,
    /// What kind of problem it is.
    pub kind: ProblemKind,
    /// What is wrong, in a sentence without the place.
    pub message: String,
}

impl std::fmt::Display for ProfileProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let file = if self.file.is_empty() {
            PROFILES_DIR.to_owned()
        } else {
            format!("{PROFILES_DIR}/{}", self.file)
        };
        match self.place {
            Some((line, column)) => {
                write!(f, "{file}, line {line}, column {column}: {}", self.message)
            }
            None => write!(f, "{file}: {}", self.message),
        }
    }
}

/// The profiles that loaded and the problems with the files that did not.
#[derive(Debug, Clone, Default)]
pub struct UserProfiles {
    /// Valid profiles in file name order, one per id.
    pub profiles: Vec<Profile>,
    /// Problems in file name order.
    pub problems: Vec<ProfileProblem>,
    /// Ids a skipped file was meant to define, by its name or its `id`,
    /// with that file's name. A built-in profile with such an id must not
    /// run in its place: the user meant to replace it.
    pub broken: BTreeMap<String, String>,
}

impl UserProfiles {
    /// Records that `file` was skipped, with the id it meant to define if
    /// its text tells, and the id its name gives.
    pub fn skipped(&mut self, file: &str, id: Option<String>) {
        for id in id.into_iter().chain(stem_id(file)) {
            self.broken.entry(id).or_insert_with(|| file.to_owned());
        }
    }

    /// Drops broken ids that a valid file defines after all, and the shell,
    /// which stays the fallback every pane needs.
    pub fn settle(&mut self) {
        let defined: Vec<String> = self.profiles.iter().map(|p| p.id.clone()).collect();
        self.broken
            .retain(|id, _| id != "shell" && !defined.contains(id));
    }
}

/// The profile id a file name gives: its name without `.toml`, when that is
/// a valid id.
fn stem_id(file: &str) -> Option<String> {
    let lower = file.to_ascii_lowercase();
    let stem = lower.strip_suffix(".toml")?;
    let ok = !stem.is_empty()
        && stem.len() <= 64
        && stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    ok.then(|| stem.to_owned())
}

/// The `id` string of a profile text that did not validate, if it parses
/// that far.
fn raw_id(text: &str) -> Option<String> {
    let root = toml::de::DeTable::parse(text).ok()?;
    let (_, value) = root.get_ref().iter().find(|(k, _)| k.get_ref() == "id")?;
    match value.get_ref() {
        toml::de::DeValue::String(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Parses each `(file name, text)` with the built-in schema and checks.
/// Files go in name order; an invalid file is skipped and the rest still
/// load; when two files define one id the first by name wins. Merge the
/// result over the built-in profiles with [`crate::resolve_profiles`],
/// after dropping the built-in ids in [`UserProfiles::broken`].
///
/// ```
/// use yien_agent::{builtin_profiles, load_user_profiles, resolve_profiles};
///
/// let mine = "schema = 1\nid = \"aider\"\nname = \"Aider\"\n[[rules]]\nsources = []\n";
/// let loaded = load_user_profiles(vec![
///     ("aider.toml".into(), mine.into()),
///     ("bad.toml".into(), "schema = = 1".into()),
/// ]);
/// assert_eq!(loaded.problems.len(), 1);
/// let all = resolve_profiles(builtin_profiles().unwrap(), loaded.profiles);
/// assert_eq!(all.last().unwrap().id, "aider");
/// ```
pub fn load_user_profiles(mut files: Vec<(String, String)>) -> UserProfiles {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut loaded = UserProfiles::default();
    let mut owners: Vec<(String, String)> = Vec::new();
    for (file, text) in files {
        match Profile::from_toml(&text) {
            Ok(profile) => {
                if let Some((_, first)) = owners.iter().find(|(id, _)| *id == profile.id) {
                    let place = ProfileError::Invalid {
                        field: "id".into(),
                        reason: String::new(),
                    }
                    .place(&text);
                    loaded.problems.push(ProfileProblem {
                        message: format!(
                            "the id \"{}\" is already defined by {PROFILES_DIR}/{first}; \
                             this file is skipped",
                            profile.id
                        ),
                        file,
                        place,
                        kind: ProblemKind::DuplicateId,
                    });
                    continue;
                }
                owners.push((profile.id.clone(), file));
                loaded.profiles.push(profile);
            }
            Err(error) => {
                let kind = match error {
                    ProfileError::TooLarge { .. } => ProblemKind::Unreadable,
                    ProfileError::Syntax { .. } => ProblemKind::Syntax,
                    ProfileError::UnsupportedSchema(_) | ProfileError::Invalid { .. } => {
                        ProblemKind::Invalid
                    }
                };
                let message = match &error {
                    ProfileError::Syntax { message, .. } => message.clone(),
                    other => other.to_string(),
                };
                loaded.skipped(&file, raw_id(&text));
                loaded.problems.push(ProfileProblem {
                    place: error.place(&text),
                    file,
                    kind,
                    message: format!("{message}; this profile is skipped"),
                });
            }
        }
    }
    loaded.settle();
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentKind, MAX_PROFILE_BYTES, builtin_profiles, resolve_profiles};

    fn profile(id: &str, name: &str) -> String {
        format!("schema = 1\nid = \"{id}\"\nname = \"{name}\"\n[[rules]]\nsources = []\n")
    }

    fn file(name: &str, text: impl Into<String>) -> (String, String) {
        (name.to_owned(), text.into())
    }

    #[test]
    fn rf_age_04_a_user_file_overrides_a_builtin_and_another_adds_a_profile() {
        let loaded = load_user_profiles(vec![
            file("mytool.toml", profile("mytool", "My tool")),
            file("claude.toml", profile("claude", "Fake Claude")),
        ]);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let all = resolve_profiles(builtin_profiles().unwrap(), loaded.profiles);
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["claude", "codex", "opencode", "shell", "mytool"]);
        assert_eq!(all[0].name, "Fake Claude");
        assert_eq!(all[4].kind, AgentKind::Custom);
    }

    #[test]
    fn rf_age_04_an_invalid_file_is_skipped_with_its_place_and_the_rest_load() {
        let loaded = load_user_profiles(vec![
            file("a.toml", profile("a", "A")),
            file("bad.toml", "schema = 1\nid = \"b\"\nname = = \"B\"\n"),
            file("c.toml", profile("c", "C")),
        ]);
        let ids: Vec<&str> = loaded.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
        let [problem] = loaded.problems.as_slice() else {
            panic!("{:?}", loaded.problems);
        };
        assert_eq!(problem.file, "bad.toml");
        assert_eq!(problem.kind, ProblemKind::Syntax);
        assert_eq!(problem.place, Some((3, 8)));
        assert!(
            problem
                .to_string()
                .starts_with("profiles/bad.toml, line 3, column 8: "),
            "{problem}"
        );
        assert!(problem.message.ends_with("this profile is skipped"));
    }

    #[test]
    fn rf_age_04_a_bad_value_names_the_line_and_column_of_the_value() {
        let text = "schema = 1\nid = \"x\"\nname = \"X\"\n\n[[rules]]\nsources = [\"screen\"]\n\n\
                    [[rules.screen]]\nconfirms = \"needs_you\"\nbottom_rows = 3\npattern = '(open'\n";
        let loaded = load_user_profiles(vec![file("x.toml", text)]);
        let problem = &loaded.problems[0];
        assert_eq!(problem.kind, ProblemKind::Invalid);
        assert_eq!(problem.place, Some((11, 11)));
        assert!(problem.message.contains("rules[0].screen[0].pattern"));

        let schema = "schema = 2\nid = \"x\"\nname = \"X\"\n[[rules]]\nsources = []\n";
        let loaded = load_user_profiles(vec![file("s.toml", schema)]);
        assert_eq!(loaded.problems[0].place, Some((1, 10)));

        // A missing key points at the table that should hold it.
        let hooks = "schema = 1\nid = \"x\"\nname = \"X\"\n[[rules]]\nsources = [\"hooks\"]\n";
        let loaded = load_user_profiles(vec![file("h.toml", hooks)]);
        assert_eq!(loaded.problems[0].place.map(|p| p.0), Some(4));

        // No rules at all: nothing in the file to point at.
        let none = "schema = 1\nid = \"x\"\nname = \"X\"\n";
        let loaded = load_user_profiles(vec![file("n.toml", none)]);
        assert_eq!(loaded.problems[0].place, None);
        assert_eq!(
            loaded.problems[0].to_string(),
            "profiles/n.toml: rules: at least one [[rules]] entry is required; \
             this profile is skipped"
        );
    }

    #[test]
    fn rf_age_04_a_broken_override_names_the_ids_it_meant_to_replace() {
        let loaded = load_user_profiles(vec![
            file(
                "claude.toml",
                "schema = 1\nid = \"claude\"\nname = = \"X\"\n",
            ),
            file("other.toml", "schema = 1\nid = \"codex\"\nname = \"X\"\n"),
            file("shell.toml", "schema = = 1\n"),
            file("Bad Name.toml", "schema = = 1\n"),
            file("mine.toml", profile("mine", "Mine")),
            file("mine-broken.toml", "schema = 1\nid = \"mine\"\n"),
        ]);
        let broken: Vec<(&str, &str)> = loaded
            .broken
            .iter()
            .map(|(id, f)| (id.as_str(), f.as_str()))
            .collect();
        // claude by its name (the id did not parse); codex by its id and
        // "other" by its name; never the shell; never an id a valid file
        // defines; never a name that is no id.
        assert_eq!(
            broken,
            [
                ("claude", "claude.toml"),
                ("codex", "other.toml"),
                ("mine-broken", "mine-broken.toml"),
                ("other", "other.toml"),
            ]
        );
    }

    #[test]
    fn rf_age_04_two_files_with_one_id_keep_the_first_by_name() {
        let loaded = load_user_profiles(vec![
            file("b-second.toml", profile("mine", "Second")),
            file("a-first.toml", profile("mine", "First")),
        ]);
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].name, "First");
        let problem = &loaded.problems[0];
        assert_eq!(problem.kind, ProblemKind::DuplicateId);
        assert_eq!(problem.file, "b-second.toml");
        assert_eq!(problem.place, Some((2, 6)));
        assert!(
            problem.message.contains("profiles/a-first.toml"),
            "{problem}"
        );
    }

    #[test]
    fn rf_age_04_an_oversized_file_is_skipped_without_a_place() {
        let big = format!(
            "{}# {}",
            profile("big", "Big"),
            "x".repeat(MAX_PROFILE_BYTES)
        );
        let loaded = load_user_profiles(vec![file("big.toml", big)]);
        assert!(loaded.profiles.is_empty());
        assert_eq!(loaded.problems[0].kind, ProblemKind::Unreadable);
        assert_eq!(loaded.problems[0].place, None);
    }

    #[test]
    fn rf_age_04_a_user_profile_cannot_ask_for_an_unknown_hook_template() {
        // Hook settings come only from YIEN's templates, written to its own
        // data dir (RNF-14); a profile cannot name another file to write.
        let text = "schema = 1\nid = \"x\"\nname = \"X\"\n[[rules]]\nsources = [\"hooks\"]\n\
                    hooks = \"write-my-file\"\n";
        let loaded = load_user_profiles(vec![file("x.toml", text)]);
        assert_eq!(loaded.problems[0].kind, ProblemKind::Syntax);
        assert_eq!(loaded.problems[0].place.map(|p| p.0), Some(6));
    }
}
