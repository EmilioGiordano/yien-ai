//! Agent profiles in TOML (ADR-11, 04 §4.7): how to launch an agent, how to
//! read its version, and which detection sources each version range uses.

use std::collections::BTreeMap;
use std::time::Duration;

use regex::{Regex, RegexBuilder};
use serde::Deserialize;

use crate::hooks::HookTemplate;
use crate::machine::MachineConfig;
use crate::signal::{ScreenVerdict, Source};
use crate::version::{Version, VersionReq};

/// The profile schema this YIEN reads (`schema = 1`).
pub const PROFILE_SCHEMA: u32 = 1;

/// The largest profile file accepted.
pub const MAX_PROFILE_BYTES: usize = 64 * 1024;

const MAX_REGEX_BYTES: usize = 1 << 20;
const MAX_SCREEN_ROWS: u16 = 200;
const PLACEHOLDERS: &[&str] = &[
    "new_uuid",
    "session_id",
    "hooks_settings_file",
    "pane_id",
    "hook_token",
    "hook_pipe",
];

const BUILTIN_SOURCES: &[(&str, &str)] = &[
    ("claude.toml", include_str!("../profiles/claude.toml")),
    ("codex.toml", include_str!("../profiles/codex.toml")),
    ("opencode.toml", include_str!("../profiles/opencode.toml")),
    ("shell.toml", include_str!("../profiles/shell.toml")),
];

/// Why a profile was rejected. The previous valid profile stays in use.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The file exceeds [`MAX_PROFILE_BYTES`].
    #[error("profile is {len} bytes; the limit is {max}")]
    TooLarge {
        /// File size.
        len: usize,
        /// The limit.
        max: usize,
    },
    /// The TOML does not parse, or a value has the wrong type.
    #[error("line {line}: {message}")]
    Syntax {
        /// 1-based line of the error.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// `schema` is not [`PROFILE_SCHEMA`].
    #[error("profile schema {0} is not supported; this YIEN reads schema 1")]
    UnsupportedSchema(u32),
    /// A value parses but is not acceptable.
    #[error("{field}: {reason}")]
    Invalid {
        /// Path of the offending key, such as `rules[0].screen[1].pattern`.
        field: String,
        /// What is wrong.
        reason: String,
    },
}

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> ProfileError {
    ProfileError::Invalid {
        field: field.into(),
        reason: reason.into(),
    }
}

/// Which agent a profile drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code (`id = "claude"`).
    Claude,
    /// Codex (`id = "codex"`).
    Codex,
    /// OpenCode (`id = "opencode"`).
    OpenCode,
    /// A plain shell (`id = "shell"`).
    Shell,
    /// A user-defined profile (RF-AGE-04).
    Custom,
}

impl AgentKind {
    fn from_id(id: &str) -> Self {
        match id {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            "opencode" => AgentKind::OpenCode,
            "shell" => AgentKind::Shell,
            _ => AgentKind::Custom,
        }
    }
}

/// How the agent's session id is learned (04 §4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionIdMode {
    /// YIEN assigns it before launch (`{new_uuid}`).
    Preassigned,
    /// The agent reports it through a hook.
    FromHook,
    /// Read from the agent's session list.
    FromList,
}

/// Where an agent keeps its past sessions, and so which reader lists them
/// (RF-SES-07, 04 §4.6). The files are the agent's own and only read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionReader {
    /// Claude Code: `~/.claude/projects/<encoded folder>/<id>.jsonl`.
    ClaudeProjects,
    /// Codex: `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-*.jsonl`. Not read
    /// yet; it lists nothing.
    CodexSessions,
}

/// What a screen rule confirms. Only "needs you" exists: a screen rule never
/// lights the red mark alone, it confirms a weaker signal (ADR-08).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirms {
    /// The agent waits for the user.
    NeedsYou,
}

/// How to find the agent's executable and version.
#[derive(Debug, Clone)]
pub struct Detect {
    /// The executable, looked up on `PATH` by the caller.
    pub command: String,
    /// Arguments that make it print its version.
    pub version_args: Vec<String>,
    version_regex: Regex,
}

impl Detect {
    /// Extracts the version from the output of `command version_args`, using
    /// the profile's `version_regex` (first capture group, or the whole match).
    ///
    /// ```
    /// let profiles = yien_agent::builtin_profiles().unwrap();
    /// let claude = profiles.iter().find(|p| p.id == "claude").unwrap();
    /// let detect = claude.detect.as_ref().unwrap();
    /// assert_eq!(
    ///     detect.parse_version("2.1.300 (Claude Code)"),
    ///     Some(yien_agent::Version::new(2, 1, 300))
    /// );
    /// ```
    pub fn parse_version(&self, output: &str) -> Option<Version> {
        let caps = self.version_regex.captures(output)?;
        let text = caps.get(1).or_else(|| caps.get(0))?.as_str();
        text.parse().ok()
    }
}

/// How to launch and resume the agent. Arguments may contain the
/// placeholders `{new_uuid}`, `{session_id}`, `{hooks_settings_file}`,
/// `{pane_id}`, `{hook_token}` and `{hook_pipe}`, filled by the caller.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    /// Arguments for a new session.
    pub args: Vec<String>,
    /// Arguments to resume `{session_id}` (RN-05).
    pub resume: Vec<String>,
    /// How the session id is learned; `None` for agents without sessions.
    pub session_id: Option<SessionIdMode>,
    /// Which reader lists the agent's past sessions; `None` lists none.
    pub sessions: Option<SessionReader>,
    /// Extra environment variables.
    pub env: BTreeMap<String, String>,
}

/// A screen rule, evaluated on the bottom rows of the visible screen.
#[derive(Debug, Clone)]
pub struct ScreenRule {
    /// What a match confirms.
    pub confirms: Confirms,
    /// How many rows from the bottom the pattern sees.
    pub bottom_rows: u16,
    pattern: Regex,
}

impl ScreenRule {
    /// Whether the pattern matches the last `bottom_rows` of `rows` (top to
    /// bottom), joined with newlines.
    pub fn matches(&self, rows: &[&str]) -> bool {
        let start = rows.len().saturating_sub(usize::from(self.bottom_rows));
        self.pattern.is_match(&rows[start..].join("\n"))
    }
}

/// Runs screen rules for a [`crate::Effect::CheckScreen`] and returns the
/// answer to feed back as [`crate::Signal::Screen`].
///
/// ```
/// use yien_agent::{ScreenVerdict, builtin_profiles, evaluate_screen};
///
/// let profiles = builtin_profiles().unwrap();
/// let claude = profiles.iter().find(|p| p.id == "claude").unwrap();
/// let rules = &claude.rules[0].screen;
/// assert_eq!(evaluate_screen(rules, &["", "  Do you want to proceed?"]), ScreenVerdict::NeedsYou);
/// assert_eq!(evaluate_screen(rules, &["$ ls"]), ScreenVerdict::NoMatch);
/// ```
pub fn evaluate_screen(rules: &[ScreenRule], rows: &[&str]) -> ScreenVerdict {
    if rules
        .iter()
        .any(|r| r.confirms == Confirms::NeedsYou && r.matches(rows))
    {
        ScreenVerdict::NeedsYou
    } else {
        ScreenVerdict::NoMatch
    }
}

/// The detection setup for one range of CLI versions.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Versions verified with this rule; `None` is a catch-all that is never
    /// counted as verified (RF-AGE-11).
    pub versions: Option<VersionReq>,
    /// Detection sources, in the profile's order.
    pub sources: Vec<Source>,
    /// The hook template, when `sources` includes `hooks`.
    pub hooks: Option<HookTemplate>,
    /// Screen rules, when `sources` includes `screen`.
    pub screen: Vec<ScreenRule>,
    /// Re-check a silent "working" pane with the screen rules after this long.
    pub watchdog: Option<Duration>,
}

impl Rule {
    /// The state machine configuration for this rule.
    pub fn machine_config(&self) -> MachineConfig {
        MachineConfig::new(&self.sources, self.watchdog)
    }
}

/// The rule chosen for a detected version.
#[derive(Debug, Clone, Copy)]
pub struct RuleChoice<'a> {
    /// The rule to run.
    pub rule: &'a Rule,
    /// `false` when no rule's range covers the version: the caller warns once
    /// per version (RF-AGE-11).
    pub verified: bool,
}

/// An agent profile (RF-AGE-01, RF-AGE-04).
///
/// ```
/// let text = r#"
/// schema = 1
/// id = "aider"
/// name = "Aider"
/// detect = { command = "aider", version_args = ["--version"] }
///
/// [[rules]]
/// versions = ">=0.80"
/// sources = ["bel"]
/// "#;
/// let profile = yien_agent::Profile::from_toml(text).unwrap();
/// assert_eq!(profile.kind, yien_agent::AgentKind::Custom);
/// ```
#[derive(Debug, Clone)]
pub struct Profile {
    /// Stable identifier; a user profile with a built-in id replaces it.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Which agent this is.
    pub kind: AgentKind,
    /// How to find the executable and its version; `None` for the shell,
    /// whose command is the user's default shell.
    pub detect: Option<Detect>,
    /// How to launch and resume.
    pub launch: Launch,
    /// Detection rules by version range; never empty.
    pub rules: Vec<Rule>,
}

#[derive(Deserialize)]
struct RawProfile {
    schema: u32,
    id: String,
    name: String,
    #[serde(default)]
    detect: Option<RawDetect>,
    #[serde(default)]
    launch: RawLaunch,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawDetect {
    command: String,
    #[serde(default)]
    version_args: Vec<String>,
    #[serde(default)]
    version_regex: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawLaunch {
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    resume: Vec<String>,
    #[serde(default)]
    session_id: Option<SessionIdMode>,
    #[serde(default)]
    sessions: Option<SessionReader>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RawRule {
    #[serde(default)]
    versions: Option<String>,
    sources: Vec<Source>,
    #[serde(default)]
    hooks: Option<HookTemplate>,
    #[serde(default)]
    screen: Vec<RawScreen>,
    #[serde(default)]
    watchdog_secs: Option<u64>,
}

#[derive(Deserialize)]
struct RawScreen {
    confirms: Confirms,
    bottom_rows: u16,
    pattern: String,
}

const DEFAULT_VERSION_REGEX: &str = r"(\d+\.\d+\.\d+)";

fn build_regex(field: &str, pattern: &str) -> Result<Regex, ProfileError> {
    RegexBuilder::new(pattern)
        .size_limit(MAX_REGEX_BYTES)
        .dfa_size_limit(MAX_REGEX_BYTES)
        .build()
        .map_err(|e| invalid(field, format!("invalid regular expression: {e}")))
}

fn check_placeholders(field: &str, value: &str) -> Result<(), ProfileError> {
    let mut rest = value;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(invalid(field, "unclosed '{'"));
        };
        let name = &after[..close];
        if !PLACEHOLDERS.contains(&name) {
            return Err(invalid(
                field,
                format!(
                    "unknown placeholder {{{name}}}; known: {}",
                    PLACEHOLDERS.join(", ")
                ),
            ));
        }
        rest = &after[close + 1..];
    }
    Ok(())
}

impl Profile {
    /// Parses and validates a profile. Never panics on any input.
    pub fn from_toml(text: &str) -> Result<Self, ProfileError> {
        if text.len() > MAX_PROFILE_BYTES {
            return Err(ProfileError::TooLarge {
                len: text.len(),
                max: MAX_PROFILE_BYTES,
            });
        }
        let raw: RawProfile = toml::from_str(text).map_err(|e| {
            let offset = e.span().map_or(0, |s| s.start).min(text.len());
            let line = text
                .get(..offset)
                .map_or(1, |prefix| prefix.matches('\n').count() + 1);
            ProfileError::Syntax {
                line,
                message: e.message().trim().to_owned(),
            }
        })?;
        Self::validate(raw)
    }

    fn validate(raw: RawProfile) -> Result<Self, ProfileError> {
        if raw.schema != PROFILE_SCHEMA {
            return Err(ProfileError::UnsupportedSchema(raw.schema));
        }
        let id_ok = !raw.id.is_empty()
            && raw.id.len() <= 64
            && raw
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !id_ok {
            return Err(invalid("id", "use 1 to 64 of a-z, 0-9, '-' and '_'"));
        }
        if raw.name.trim().is_empty() {
            return Err(invalid("name", "must not be empty"));
        }

        let detect = match raw.detect {
            None => None,
            Some(d) => {
                if d.command.trim().is_empty() {
                    return Err(invalid("detect.command", "must not be empty"));
                }
                let pattern = d.version_regex.as_deref().unwrap_or(DEFAULT_VERSION_REGEX);
                Some(Detect {
                    command: d.command,
                    version_args: d.version_args,
                    version_regex: build_regex("detect.version_regex", pattern)?,
                })
            }
        };

        for (i, arg) in raw.launch.args.iter().enumerate() {
            check_placeholders(&format!("launch.args[{i}]"), arg)?;
        }
        for (i, arg) in raw.launch.resume.iter().enumerate() {
            check_placeholders(&format!("launch.resume[{i}]"), arg)?;
        }
        for (key, value) in &raw.launch.env {
            check_placeholders(&format!("launch.env.{key}"), value)?;
        }
        if !raw.launch.resume.is_empty()
            && !raw.launch.resume.iter().any(|a| a.contains("{session_id}"))
        {
            return Err(invalid(
                "launch.resume",
                "must pass {session_id}; resuming never means \"the last session\" (RN-05)",
            ));
        }

        if raw.rules.is_empty() {
            return Err(invalid("rules", "at least one [[rules]] entry is required"));
        }
        let mut rules = Vec::with_capacity(raw.rules.len());
        for (i, r) in raw.rules.into_iter().enumerate() {
            rules.push(Self::validate_rule(i, r)?);
        }

        Ok(Profile {
            kind: AgentKind::from_id(&raw.id),
            id: raw.id,
            name: raw.name,
            detect,
            launch: Launch {
                args: raw.launch.args,
                resume: raw.launch.resume,
                session_id: raw.launch.session_id,
                sessions: raw.launch.sessions,
                env: raw.launch.env,
            },
            rules,
        })
    }

    fn validate_rule(i: usize, r: RawRule) -> Result<Rule, ProfileError> {
        let at = |key: &str| format!("rules[{i}].{key}");
        let versions = match r.versions {
            None => None,
            Some(v) => Some(
                v.parse::<VersionReq>()
                    .map_err(|e| invalid(at("versions"), e.to_string()))?,
            ),
        };
        let declares = |s: Source| r.sources.contains(&s);
        match (declares(Source::Hooks), r.hooks) {
            (true, None) => {
                return Err(invalid(at("hooks"), "required when sources has \"hooks\""));
            }
            (false, Some(_)) => {
                return Err(invalid(at("hooks"), "set, but sources lacks \"hooks\""));
            }
            _ => {}
        }
        if !r.screen.is_empty() && !declares(Source::Screen) {
            return Err(invalid(at("screen"), "set, but sources lacks \"screen\""));
        }
        let mut screen = Vec::with_capacity(r.screen.len());
        for (j, s) in r.screen.into_iter().enumerate() {
            let field = format!("rules[{i}].screen[{j}]");
            if s.bottom_rows == 0 || s.bottom_rows > MAX_SCREEN_ROWS {
                return Err(invalid(
                    format!("{field}.bottom_rows"),
                    format!("must be 1 to {MAX_SCREEN_ROWS}"),
                ));
            }
            screen.push(ScreenRule {
                confirms: s.confirms,
                bottom_rows: s.bottom_rows,
                pattern: build_regex(&format!("{field}.pattern"), &s.pattern)?,
            });
        }
        let watchdog = match r.watchdog_secs {
            Some(0) => return Err(invalid(at("watchdog_secs"), "must be at least 1")),
            Some(secs) => Some(Duration::from_secs(secs)),
            None => None,
        };
        Ok(Rule {
            versions,
            sources: r.sources,
            hooks: r.hooks,
            screen,
            watchdog,
        })
    }

    /// Picks the rule for a detected version (04 §4.7, RF-AGE-11).
    ///
    /// The first rule whose range covers the version is verified. Otherwise,
    /// or when the version is unknown, the newest rule (highest lower bound)
    /// is used, unverified.
    ///
    /// ```
    /// use yien_agent::{Version, builtin_profiles};
    ///
    /// let profiles = builtin_profiles().unwrap();
    /// let claude = profiles.iter().find(|p| p.id == "claude").unwrap();
    /// assert!(claude.select_rule(Some(Version::new(2, 1, 300))).verified);
    /// assert!(!claude.select_rule(Some(Version::new(4, 0, 0))).verified);
    /// ```
    pub fn select_rule(&self, version: Option<Version>) -> RuleChoice<'_> {
        if let Some(v) = version
            && let Some(rule) = self
                .rules
                .iter()
                .find(|r| r.versions.as_ref().is_some_and(|req| req.matches(v)))
        {
            return RuleChoice {
                rule,
                verified: true,
            };
        }
        let mut newest = &self.rules[0];
        for rule in &self.rules[1..] {
            if lower_bound(rule) > lower_bound(newest) {
                newest = rule;
            }
        }
        RuleChoice {
            rule: newest,
            verified: false,
        }
    }
}

fn lower_bound(rule: &Rule) -> Option<Version> {
    rule.versions.as_ref().and_then(VersionReq::lower_bound)
}

/// The profiles shipped with YIEN: Claude Code, Codex, OpenCode and a plain
/// shell (RF-AGE-01).
///
/// ```
/// let ids: Vec<String> = yien_agent::builtin_profiles()
///     .unwrap()
///     .into_iter()
///     .map(|p| p.id)
///     .collect();
/// assert_eq!(ids, ["claude", "codex", "opencode", "shell"]);
/// ```
pub fn builtin_profiles() -> Result<Vec<Profile>, ProfileError> {
    BUILTIN_SOURCES
        .iter()
        .map(|(file, text)| Profile::from_toml(text).map_err(|e| invalid(*file, e.to_string())))
        .collect()
}

/// Merges user profiles over the built-in ones: a user profile with a
/// built-in id replaces it in place, others are appended (04 §4.7).
///
/// ```
/// use yien_agent::{builtin_profiles, resolve_profiles, Profile};
///
/// let mine = Profile::from_toml(
///     "schema = 1\nid = \"shell\"\nname = \"My shell\"\n[[rules]]\nsources = []\n",
/// )
/// .unwrap();
/// let all = resolve_profiles(builtin_profiles().unwrap(), vec![mine]);
/// assert_eq!(all.len(), 4);
/// assert_eq!(all[3].name, "My shell");
/// ```
pub fn resolve_profiles(builtin: Vec<Profile>, user: Vec<Profile>) -> Vec<Profile> {
    let mut all = builtin;
    for profile in user {
        match all.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => all.push(profile),
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HOOK_TOKEN_ENV;

    const MINIMAL: &str = "schema = 1\nid = \"x\"\nname = \"X\"\n[[rules]]\nsources = []\n";

    fn err(text: &str) -> ProfileError {
        Profile::from_toml(text).unwrap_err()
    }

    #[test]
    fn rf_ses_07_a_profile_declares_the_reader_of_its_past_sessions() {
        let profiles = builtin_profiles().unwrap();
        let reader = |id: &str| {
            profiles
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.launch.sessions)
        };
        assert_eq!(reader("claude"), Some(SessionReader::ClaudeProjects));
        assert_eq!(reader("codex"), Some(SessionReader::CodexSessions));
        assert_eq!(reader("shell"), None);
        assert_eq!(Profile::from_toml(MINIMAL).unwrap().launch.sessions, None);
        let unknown = format!("{MINIMAL}[launch]\nsessions = \"aider-history\"\n");
        assert!(matches!(err(&unknown), ProfileError::Syntax { .. }));
    }

    #[test]
    fn rf_age_01_builtin_profiles_cover_the_three_agents_and_a_shell() {
        let profiles = builtin_profiles().unwrap();
        let kinds: Vec<AgentKind> = profiles.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            [
                AgentKind::Claude,
                AgentKind::Codex,
                AgentKind::OpenCode,
                AgentKind::Shell
            ]
        );
        let claude = &profiles[0];
        assert!(claude.rules.iter().all(|r| r.hooks.is_some()));
        assert_eq!(claude.launch.session_id, Some(SessionIdMode::Preassigned));
        assert!(profiles[3].detect.is_none());
    }

    #[test]
    fn rf_age_01_claude_launch_passes_the_hook_token_by_environment() {
        let profiles = builtin_profiles().unwrap();
        let claude = &profiles[0];
        assert_eq!(
            claude.launch.env.get(HOOK_TOKEN_ENV).map(String::as_str),
            Some("{hook_token}")
        );
        assert_eq!(
            claude.launch.env.get("YIEN_HOOK_PIPE").map(String::as_str),
            Some("{hook_pipe}")
        );
        assert!(!claude.launch.args.iter().any(|a| a.contains("hook_")));
    }

    #[test]
    fn rnf_14_claude_hooks_come_from_a_per_pane_settings_file() {
        let profiles = builtin_profiles().unwrap();
        let claude = &profiles[0];
        for args in [&claude.launch.args, &claude.launch.resume] {
            let at = args.iter().position(|a| a == "--settings").unwrap();
            assert_eq!(args[at + 1], "{hooks_settings_file}");
        }
        let json = crate::hooks::render_claude_settings("D:/yien/yien-hook.exe").unwrap();
        assert!(!json.contains(".claude"));
    }

    #[test]
    fn rn_02_codex_and_opencode_never_declare_a_red_capable_source_alone() {
        // Without hooks or OSC 7501 their machines never reach "working", so
        // they never mark red; S7 and S8 decide richer sources.
        let profiles = builtin_profiles().unwrap();
        for p in &profiles[1..3] {
            for rule in &p.rules {
                assert!(!rule.sources.contains(&Source::Hooks), "{}", p.id);
                assert!(!rule.sources.contains(&Source::Osc7501), "{}", p.id);
            }
        }
    }

    #[test]
    fn rf_age_04_user_profile_parses() {
        let text = r#"
schema = 1
id = "my-agent"
name = "Mine"
detect = { command = "mine", version_args = ["-V"], version_regex = 'v(\d+\.\d+)' }

[launch]
args = ["--pane", "{pane_id}"]
resume = ["--resume", "{session_id}"]
session_id = "from_list"
env = { MINE = "1" }

[[rules]]
versions = ">=1.2"
sources = ["osc7501", "screen"]
watchdog_secs = 30

[[rules.screen]]
confirms = "needs_you"
bottom_rows = 5
pattern = '\[y/N\]'
"#;
        let p = Profile::from_toml(text).unwrap();
        assert_eq!(p.kind, AgentKind::Custom);
        let detect = p.detect.as_ref().unwrap();
        assert_eq!(
            detect.parse_version("mine v1.4"),
            Some(Version::new(1, 4, 0))
        );
        assert_eq!(detect.parse_version("no version"), None);
        let rule = &p.rules[0];
        assert_eq!(rule.watchdog, Some(Duration::from_secs(30)));
        assert!(rule.screen[0].matches(&["a", "b", "Continue? [y/N]"]));
        assert!(rule.machine_config().declares(Source::Screen));
    }

    #[test]
    fn rf_age_04_user_profile_replaces_builtin_with_same_id() {
        let mine = Profile::from_toml(&MINIMAL.replace("\"x\"", "\"claude\"")).unwrap();
        let extra = Profile::from_toml(MINIMAL).unwrap();
        let all = resolve_profiles(builtin_profiles().unwrap(), vec![mine, extra]);
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].id, "claude");
        assert_eq!(all[0].name, "X");
        assert_eq!(all[4].id, "x");
    }

    #[test]
    fn rf_age_11_matching_range_is_verified() {
        let profiles = builtin_profiles().unwrap();
        let claude = &profiles[0];
        let choice = claude.select_rule(Some(Version::new(2, 0, 5)));
        assert!(choice.verified);
        assert!(!choice.rule.sources.contains(&Source::Osc7501));
    }

    #[test]
    fn rf_age_11_unverified_version_uses_newest_rule_and_warns() {
        let text = r#"
schema = 1
id = "a"
name = "A"
[[rules]]
versions = ">=1, <2"
sources = ["bel"]
[[rules]]
versions = ">=2, <3"
sources = ["osc9"]
[[rules]]
versions = "<1"
sources = []
"#;
        let p = Profile::from_toml(text).unwrap();
        for version in [Some(Version::new(9, 0, 0)), None] {
            let choice = p.select_rule(version);
            assert!(!choice.verified);
            assert_eq!(choice.rule.sources, [Source::Osc9]);
        }
    }

    #[test]
    fn rf_age_11_catch_all_rule_is_never_verified() {
        let profiles = builtin_profiles().unwrap();
        let codex = &profiles[1];
        assert!(!codex.select_rule(Some(Version::new(0, 50, 0))).verified);
    }

    #[test]
    fn invalid_toml_reports_the_line() {
        let text = "schema = 1\nid = \"x\"\nname = = \"X\"\n";
        match err(text) {
            ProfileError::Syntax { line, .. } => assert_eq!(line, 3),
            other => panic!("unexpected {other:?}"),
        }
        let text = "schema = 1\nid = \"x\"\nname = \"X\"\n[[rules]]\nsources = [\"smoke\"]\n";
        match err(text) {
            ProfileError::Syntax { line, .. } => assert_eq!(line, 5),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn invalid_values_are_errors() {
        let cases = [
            (MINIMAL.replace("schema = 1", "schema = 2"), "schema"),
            (MINIMAL.replace("\"x\"", "\"Bad Id\""), "id"),
            (MINIMAL.replace("\"X\"", "\"  \""), "name"),
            (MINIMAL.replace("[[rules]]\nsources = []\n", ""), "rules"),
            (
                MINIMAL.replace("sources = []", "sources = [\"hooks\"]"),
                "rules[0].hooks",
            ),
            (
                MINIMAL.replace(
                    "sources = []",
                    "sources = []\nhooks = \"claude-command-v1\"",
                ),
                "rules[0].hooks",
            ),
            (
                MINIMAL.replace("sources = []", "sources = []\nversions = \"~1\""),
                "rules[0].versions",
            ),
            (
                MINIMAL.replace("sources = []", "sources = []\nwatchdog_secs = 0"),
                "rules[0].watchdog_secs",
            ),
            (
                MINIMAL.replace("[[rules]]", "[launch]\nargs = [\"{nope}\"]\n[[rules]]"),
                "launch.args[0]",
            ),
            (
                MINIMAL.replace(
                    "[[rules]]",
                    "[launch]\nresume = [\"--continue\"]\n[[rules]]",
                ),
                "launch.resume",
            ),
            (
                MINIMAL.replace("[[rules]]", "detect = { command = \"\" }\n[[rules]]"),
                "detect.command",
            ),
            (
                format!(
                    "{MINIMAL}[[rules.screen]]\nconfirms = \"needs_you\"\nbottom_rows = 3\npattern = \"x\"\n"
                ),
                "rules[0].screen",
            ),
        ];
        for (text, field) in cases {
            let e = err(&text);
            let ok = match &e {
                ProfileError::Invalid { field: f, .. } => f == field,
                ProfileError::UnsupportedSchema(_) => field == "schema",
                _ => false,
            };
            assert!(ok, "{field}: got {e:?}");
        }
    }

    #[test]
    fn bad_screen_rules_are_errors() {
        let base = MINIMAL.replace("sources = []", "sources = [\"screen\"]");
        for (rows, pattern) in [
            ("0", "x"),
            ("201", "x"),
            ("3", "(unclosed"),
            ("3", "a{99999}{99999}"),
        ] {
            let text = format!(
                "{base}[[rules.screen]]\nconfirms = \"needs_you\"\nbottom_rows = {rows}\npattern = '{pattern}'\n"
            );
            assert!(
                matches!(err(&text), ProfileError::Invalid { .. }),
                "{rows} {pattern}"
            );
        }
    }

    #[test]
    fn oversized_profiles_are_refused() {
        let text = format!("{MINIMAL}# {}", "x".repeat(MAX_PROFILE_BYTES));
        assert!(matches!(err(&text), ProfileError::TooLarge { .. }));
    }

    #[test]
    fn arbitrary_text_never_panics() {
        let alphabet = "schema=1id\"name[[rules]]sources{}'\n .,:#";
        let chars: Vec<char> = alphabet.chars().collect();
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        for len in 0..400 {
            let text: String = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    chars[(seed % chars.len() as u64) as usize]
                })
                .collect();
            let _ = Profile::from_toml(&text);
        }
    }
}
