//! What a pane runs: a profile resolved to a program, its arguments and the
//! agent detection rule (RF-AGE-01, RF-PAN-18), and the placeholders filled
//! in for each launch.

use yien_agent::{CLAUDE_SESSION_MARKERS, MachineConfig, Profile, ScreenRule, Source};
use yien_platform::hook_pipe::{HOOK_PIPE_ENV, HOOK_TOKEN_ENV};
use yien_proto::{PaneId, ProfileId};
use yien_pty::SpawnSpec;

use crate::shell::ShellCommand;
use crate::{CoreConfig, CoreError};

pub(crate) use yien_proto::SHELL_PROFILE;

/// The placeholder for a session id YIEN assigns before launch.
const NEW_UUID: &str = "{new_uuid}";

/// A profile resolved for launching. Arguments and env values are still
/// templates; [`LaunchPlan::spec`] fills them for one launch.
#[derive(Debug, Clone)]
pub(crate) struct LaunchPlan {
    /// The profile's id (`claude`, `shell`), as the saved session records it.
    pub profile: String,
    pub program: String,
    pub args: Vec<String>,
    /// The arguments that resume `{session_id}` instead of `args`; empty
    /// when the profile cannot resume (RN-05).
    pub resume_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub machine: MachineConfig,
    pub screen: Vec<ScreenRule>,
    pub profile_name: String,
    pub default_name: String,
    /// The rule reports through hooks: each launch gets a hook token and the
    /// settings file, or has no structured channel.
    pub hooks: bool,
}

/// The values one launch fills in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fill<'a> {
    pub pane: PaneId,
    /// `{new_uuid}`: the session id assigned to this launch.
    pub session_id: Option<&'a str>,
    /// `{session_id}`: the session this launch resumes. When set, the plan's
    /// resume arguments replace its launch arguments.
    pub resume: Option<&'a str>,
    /// The hook values, when the hook pipe runs and a token was issued.
    pub hooks: Option<HookFill<'a>>,
}

/// `{hooks_settings_file}`, `{hook_pipe}` and `{hook_token}`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HookFill<'a> {
    pub settings_file: &'a str,
    pub pipe: &'a str,
    pub token: &'a str,
}

impl LaunchPlan {
    /// Whether the profile can resume a session by its exact id.
    pub fn can_resume(&self) -> bool {
        !self.resume_args.is_empty()
    }

    /// Whether a launch needs a session id assigned in advance.
    pub fn preassigns_session(&self) -> bool {
        self.args.iter().any(|a| a.contains(NEW_UUID))
            || self.env.iter().any(|(_, v)| v.contains(NEW_UUID))
    }

    /// The spawn spec for one launch.
    ///
    /// An argument whose placeholder has no value is left out, with the flag
    /// before it: without hooks Claude starts with `--session-id` but no
    /// `--settings`. The hook variables are always set, empty when this pane
    /// has no token, so a value inherited from the environment that started
    /// YIEN can never light another pane red. Claude Code's session markers
    /// are never inherited, in any pane.
    pub fn spec(&self, fill: &Fill<'_>, cwd: &str, cols: u16, rows: u16) -> SpawnSpec {
        let mut args: Vec<String> = Vec::new();
        let mut last_was_flag = false;
        let templates = match fill.resume {
            Some(_) if self.can_resume() => &self.resume_args,
            _ => &self.args,
        };
        for template in templates {
            match fill_template(template, fill) {
                Some(arg) => {
                    last_was_flag = arg.starts_with('-') && arg == *template;
                    args.push(arg);
                }
                None => {
                    if last_was_flag {
                        args.pop();
                    }
                    last_was_flag = false;
                }
            }
        }
        let mut spec = SpawnSpec::new(&self.program)
            .args(args)
            .size(cols.max(1), rows.max(1));
        if !cwd.is_empty() {
            spec = spec.cwd(cwd);
        }
        for marker in CLAUDE_SESSION_MARKERS {
            spec = spec.env_remove(*marker);
        }
        let mut hook_vars = [(HOOK_TOKEN_ENV, false), (HOOK_PIPE_ENV, false)];
        for (key, value) in &self.env {
            let hook_var = hook_vars.iter_mut().find(|(k, _)| k == key);
            match (fill_template(value, fill), hook_var) {
                (Some(value), Some((_, set))) => {
                    *set = true;
                    spec = spec.env(key, value);
                }
                (Some(value), None) => spec = spec.env(key, value),
                (None, _) => {}
            }
        }
        for (key, set) in hook_vars {
            if !set {
                spec = spec.env(key, "");
            }
        }
        spec
    }
}

/// `template` with its placeholders filled, or `None` when one of them has
/// no value in this launch.
fn fill_template(template: &str, fill: &Fill<'_>) -> Option<String> {
    let pane = fill.pane.get().to_string();
    let hooks = fill.hooks;
    let values: [(&str, Option<&str>); 6] = [
        ("{pane_id}", Some(&pane)),
        (NEW_UUID, fill.session_id),
        ("{session_id}", fill.resume),
        ("{hooks_settings_file}", hooks.map(|h| h.settings_file)),
        ("{hook_pipe}", hooks.map(|h| h.pipe)),
        ("{hook_token}", hooks.map(|h| h.token)),
    ];
    let mut out = template.to_owned();
    for (placeholder, value) in values {
        if out.contains(placeholder) {
            out = out.replace(placeholder, value?);
        }
    }
    Some(out)
}

/// A profile program as a pane name: its file name, without the extension
/// only when it is one Windows runs a program by (`claude.cmd` is
/// `claude`, `python3.12` stays as it is).
fn program_name(command: &str) -> String {
    let file = command
        .rsplit(['/', '\\'])
        .next()
        .filter(|f| !f.is_empty())
        .unwrap_or(command);
    match file.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && ["exe", "cmd", "bat", "com", "ps1"]
                    .contains(&ext.to_ascii_lowercase().as_str()) =>
        {
            stem.to_owned()
        }
        _ => file.to_owned(),
    }
}

/// A random (version 4) UUID, or `None` when the OS random generator fails.
pub(crate) fn new_uuid() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(format_uuid(bytes))
}

fn format_uuid(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// [`resolve`] against the core's profiles, refusing an id whose user file
/// was skipped rather than starting anything else in its place.
pub(crate) fn resolve_in(
    profile: Option<&ProfileId>,
    config: &CoreConfig,
) -> Result<LaunchPlan, CoreError> {
    if let Some(id) = profile
        && let Some(file) = config.broken_profiles.get(id.as_str())
        && !config.profiles.iter().any(|p| p.id == id.as_str())
    {
        return Err(CoreError::BrokenProfile {
            id: id.as_str().to_owned(),
            file: file.clone(),
        });
    }
    resolve(profile, &config.profiles, &config.shell)
}

/// Resolves `profile` (the shell when `None`) against the known profiles.
///
/// An agent profile runs its detected command with its launch arguments and
/// variables. The CLI version is not probed, so the newest rule is used,
/// unverified.
pub(crate) fn resolve(
    profile: Option<&ProfileId>,
    profiles: &[Profile],
    shell: &ShellCommand,
) -> Result<LaunchPlan, CoreError> {
    let id = profile.map_or(SHELL_PROFILE, ProfileId::as_str);
    let found = profiles.iter().find(|p| p.id == id);
    let Some(found) = found else {
        // Reached when the user's `shell.toml` is broken: the marks come from
        // YIEN's own launch arguments, and OSC 133 can never turn a pane red.
        if id == SHELL_PROFILE {
            return Ok(LaunchPlan {
                profile: SHELL_PROFILE.to_owned(),
                program: shell.program.clone(),
                args: shell.args.clone(),
                resume_args: Vec::new(),
                env: shell.env.clone(),
                machine: MachineConfig::new(&[Source::Osc133], None),
                screen: Vec::new(),
                profile_name: "Shell".to_owned(),
                default_name: shell.display_name(),
                hooks: false,
            });
        }
        return Err(CoreError::UnknownProfile(id.to_owned()));
    };

    let rule = found.select_rule(None).rule;
    let profile_env = found.launch.env.iter().map(|(k, v)| (k.clone(), v.clone()));
    let env = match &found.detect {
        Some(_) => profile_env.collect(),
        None => shell.env.iter().cloned().chain(profile_env).collect(),
    };
    let (program, args, default_name) = match &found.detect {
        Some(detect) => (
            detect.command.clone(),
            found.launch.args.clone(),
            program_name(&detect.command),
        ),
        None => (
            shell.program.clone(),
            shell.args.clone(),
            shell.display_name(),
        ),
    };
    Ok(LaunchPlan {
        profile: found.id.clone(),
        program,
        args,
        resume_args: found.launch.resume.clone(),
        env,
        machine: rule.machine_config(),
        screen: rule.screen.clone(),
        profile_name: found.name.clone(),
        default_name,
        hooks: rule.hooks.is_some() && rule.sources.contains(&Source::Hooks),
    })
}

#[cfg(test)]
mod tests {
    use yien_agent::builtin_profiles;

    use super::*;

    fn shell() -> ShellCommand {
        ShellCommand::new("pwsh.exe")
    }

    fn claude() -> LaunchPlan {
        resolve(
            Some(&ProfileId::new("claude")),
            &builtin_profiles().unwrap(),
            &shell(),
        )
        .unwrap()
    }

    fn env<'a>(spec: &'a SpawnSpec, key: &str) -> Option<&'a str> {
        spec.env
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.to_str())
    }

    fn args(spec: &SpawnSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    const HOOKS: HookFill<'static> = HookFill {
        settings_file: "D:/data/hooks/claude-hooks-1.json",
        pipe: r"\\.\pipe\yien-hook-1-0123456789abcdef",
        token: "feedface",
    };

    #[test]
    fn rf_pan_18_no_profile_runs_the_shell_with_the_shell_rule() {
        let plan = resolve(None, &builtin_profiles().unwrap(), &shell()).unwrap();
        assert_eq!(plan.program, "pwsh.exe");
        assert_eq!(plan.default_name, "pwsh");
        assert_eq!(plan.profile_name, "Shell");
        assert!(plan.machine.declares(Source::Osc133));
        assert!(!plan.hooks);
        assert!(!plan.preassigns_session());
    }

    #[test]
    fn rf_age_04_a_profile_with_a_full_path_is_named_by_its_file() {
        let profile = Profile::from_toml(
            "schema = 1\nid = \"mine\"\nname = \"Mine\"\n\
             detect = { command = 'D:/tools/bin/my-tool.cmd' }\n[[rules]]\nsources = []\n",
        )
        .unwrap();
        let plan = resolve(Some(&ProfileId::new("mine")), &[profile], &shell()).unwrap();
        assert_eq!(plan.program, "D:/tools/bin/my-tool.cmd");
        assert_eq!(plan.default_name, "my-tool");
        assert_eq!(claude().default_name, "claude");
        for (command, name) in [
            ("python3.12", "python3.12"),
            (r"C:\Tools\Agent.EXE", "Agent"),
            ("/usr/bin/run.sh", "run.sh"),
            ("tool.ps1", "tool"),
            (".cmd", ".cmd"),
        ] {
            assert_eq!(program_name(command), name, "{command}");
        }
    }

    #[test]
    fn rf_age_01_claude_gets_its_session_id_settings_and_hook_env() {
        let plan = claude();
        assert_eq!(plan.program, "claude");
        assert_eq!(plan.profile_name, "Claude Code");
        assert!(plan.hooks && plan.preassigns_session());
        let fill = Fill {
            pane: PaneId::new(42),
            session_id: Some("3f2a0c1e-0000-4000-8000-000000000000"),
            resume: None,
            hooks: Some(HOOKS),
        };
        let spec = plan.spec(&fill, "C:/Code", 100, 30);
        assert_eq!(
            args(&spec),
            [
                "--session-id",
                "3f2a0c1e-0000-4000-8000-000000000000",
                "--settings",
                "D:/data/hooks/claude-hooks-1.json",
            ]
        );
        assert_eq!(env(&spec, "YIEN_PANE"), Some("42"));
        assert_eq!(env(&spec, HOOK_TOKEN_ENV), Some("feedface"));
        assert_eq!(env(&spec, HOOK_PIPE_ENV), Some(HOOKS.pipe));
    }

    #[test]
    fn rn_02_claude_without_hooks_drops_the_settings_and_blanks_the_hook_env() {
        let fill = Fill {
            pane: PaneId::new(7),
            session_id: Some("s-1"),
            resume: None,
            hooks: None,
        };
        let spec = claude().spec(&fill, "C:/Code", 80, 24);
        assert_eq!(args(&spec), ["--session-id", "s-1"]);
        assert_eq!(env(&spec, HOOK_TOKEN_ENV), Some(""));
        assert_eq!(env(&spec, HOOK_PIPE_ENV), Some(""));
        for arg in args(&spec) {
            assert!(!arg.contains('{'), "{arg}");
        }
    }

    #[test]
    fn a_launch_without_a_session_id_drops_the_flag_too() {
        let fill = Fill {
            pane: PaneId::new(7),
            session_id: None,
            resume: None,
            hooks: Some(HOOKS),
        };
        let spec = claude().spec(&fill, "", 80, 24);
        assert_eq!(args(&spec), ["--settings", HOOKS.settings_file]);
    }

    #[test]
    fn rn_01_a_shell_pane_never_inherits_a_hook_token() {
        let plan = resolve(None, &[], &shell()).unwrap();
        let fill = Fill {
            pane: PaneId::new(1),
            session_id: None,
            resume: None,
            hooks: Some(HOOKS),
        };
        let spec = plan.spec(&fill, "C:/Code", 80, 24);
        assert_eq!(env(&spec, HOOK_TOKEN_ENV), Some(""));
        assert_eq!(env(&spec, HOOK_PIPE_ENV), Some(""));
    }

    #[test]
    fn no_pane_inherits_the_markers_of_a_claude_session() {
        let profiles = builtin_profiles().unwrap();
        let fill = Fill {
            pane: PaneId::new(1),
            session_id: Some("s-1"),
            resume: None,
            hooks: Some(HOOKS),
        };
        for id in [None, Some("claude"), Some("codex")] {
            let plan = resolve(id.map(ProfileId::new).as_ref(), &profiles, &shell()).unwrap();
            let spec = plan.spec(&fill, "", 80, 24);
            for marker in [
                "CLAUDECODE",
                "CLAUDE_CODE_ENTRYPOINT",
                "CLAUDE_CODE_SESSION_ID",
            ] {
                assert!(
                    spec.env_remove.iter().any(|k| k == marker),
                    "{id:?} {marker}"
                );
            }
            for kept in ["ANTHROPIC_API_KEY", "CLAUDE_CONFIG_DIR", "YIEN_HOOK_TOKEN"] {
                assert!(!spec.env_remove.iter().any(|k| k == kept), "{id:?} {kept}");
            }
        }
    }

    #[test]
    fn rf_age_03_agent_clis_run_on_the_users_own_account() {
        let profiles = builtin_profiles().unwrap();
        let fill = Fill {
            pane: PaneId::new(1),
            session_id: Some("s-1"),
            resume: None,
            hooks: Some(HOOKS),
        };
        for id in ["claude", "codex", "opencode"] {
            let plan = resolve(Some(&ProfileId::new(id)), &profiles, &shell()).unwrap();
            let spec = plan.spec(&fill, "C:/Code", 80, 24);
            // The environment is the user's, minus Claude's session markers:
            // YIEN adds only its own variables and no credentials of its own.
            for (key, _) in &spec.env {
                let key = key.to_string_lossy();
                assert!(key.starts_with("YIEN_"), "{id} sets {key}");
            }
            for removed in &spec.env_remove {
                let removed = removed.to_string_lossy();
                assert!(
                    CLAUDE_SESSION_MARKERS.contains(&removed.as_ref()),
                    "{id} drops {removed}"
                );
            }
            for arg in args(&spec) {
                let arg = arg.to_ascii_lowercase();
                for word in ["key", "token", "login", "auth", "password"] {
                    assert!(!arg.contains(word), "{id} passes {arg}");
                }
            }
        }
    }

    #[test]
    fn rf_ses_03_a_resume_launch_passes_the_exact_session_and_the_hooks() {
        let plan = claude();
        assert!(plan.can_resume());
        let fill = Fill {
            pane: PaneId::new(3),
            session_id: Some("unused"),
            resume: Some("4f1c-9a"),
            hooks: Some(HOOKS),
        };
        let spec = plan.spec(&fill, "C:/Code", 80, 24);
        assert_eq!(
            args(&spec),
            ["--resume", "4f1c-9a", "--settings", HOOKS.settings_file]
        );
        assert_eq!(env(&spec, HOOK_TOKEN_ENV), Some("feedface"));
        let fill = Fill {
            resume: None,
            ..fill
        };
        assert_eq!(args(&plan.spec(&fill, "", 80, 24))[0], "--session-id");
    }

    #[test]
    fn rn_05_a_profile_without_resume_arguments_launches_fresh() {
        let plan = resolve(None, &builtin_profiles().unwrap(), &shell()).unwrap();
        assert!(!plan.can_resume());
        assert_eq!(plan.profile, SHELL_PROFILE);
        let fill = Fill {
            pane: PaneId::new(3),
            session_id: None,
            resume: Some("4f1c"),
            hooks: None,
        };
        assert_eq!(
            plan.spec(&fill, "", 80, 24).args,
            plan.spec(
                &Fill {
                    resume: None,
                    ..fill
                },
                "",
                80,
                24
            )
            .args
        );
    }

    #[test]
    fn rf_age_01_codex_and_opencode_launch_without_hooks() {
        let profiles = builtin_profiles().unwrap();
        for id in ["codex", "opencode"] {
            let plan = resolve(Some(&ProfileId::new(id)), &profiles, &shell()).unwrap();
            assert_eq!(plan.program, id);
            assert!(!plan.hooks && !plan.preassigns_session());
        }
    }

    #[test]
    fn uuids_are_version_4_and_random() {
        let a = new_uuid().unwrap();
        let b = new_uuid().unwrap();
        assert_ne!(a, b);
        assert_eq!(
            format_uuid([0xff; 16]),
            "ffffffff-ffff-4fff-bfff-ffffffffffff"
        );
        assert_eq!(format_uuid([0; 16]), "00000000-0000-4000-8000-000000000000");
        let parts: Vec<usize> = a.split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12]);
        assert_eq!(&a[14..15], "4");
    }

    #[test]
    fn unknown_profile_is_an_error() {
        let err = resolve(
            Some(&ProfileId::new("nope")),
            &builtin_profiles().unwrap(),
            &shell(),
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::UnknownProfile(id) if id == "nope"));
    }

    #[test]
    fn rf_age_05_the_shell_reads_command_marks_even_without_its_profile() {
        let plan = resolve(None, &[], &shell()).unwrap();
        assert_eq!(plan.program, "pwsh.exe");
        assert_eq!(plan.profile, SHELL_PROFILE);
        assert!(plan.machine.declares(Source::Osc133));
        let builtin = resolve(None, &builtin_profiles().unwrap(), &shell()).unwrap();
        assert_eq!(plan.machine, builtin.machine);
        assert!(!plan.hooks);
    }

    #[test]
    fn rf_age_05_a_powershell_pane_starts_with_the_marks_and_reads_them() {
        let pwsh = ShellCommand::new("pwsh.exe").reporting_cwd(None);
        for profiles in [Vec::new(), builtin_profiles().unwrap()] {
            let plan = resolve(None, &profiles, &pwsh).unwrap();
            assert!(plan.machine.declares(Source::Osc133));
            let fill = Fill {
                pane: PaneId::new(1),
                session_id: None,
                resume: None,
                hooks: None,
            };
            let spec = plan.spec(&fill, "C:/Code", 80, 24);
            let script = args(&spec).pop().unwrap();
            assert!(script.contains("']133;C'"), "{script}");
            assert!(script.contains("']133;D;'"), "{script}");
        }
    }

    #[test]
    fn rf_ses_01_the_shell_variables_reach_its_panes() {
        let mut cmd = ShellCommand::new("cmd.exe");
        cmd.env = vec![("PROMPT".into(), "$E]9;9;\"$P\"$E\\$P$G".into())];
        let fill = Fill {
            pane: PaneId::new(1),
            session_id: None,
            resume: None,
            hooks: None,
        };
        for profiles in [Vec::new(), builtin_profiles().unwrap()] {
            let plan = resolve(None, &profiles, &cmd).unwrap();
            let spec = plan.spec(&fill, "C:/Code", 80, 24);
            assert_eq!(env(&spec, "PROMPT"), Some("$E]9;9;\"$P\"$E\\$P$G"));
        }
        let claude = resolve(
            Some(&ProfileId::new("claude")),
            &builtin_profiles().unwrap(),
            &cmd,
        )
        .unwrap();
        assert!(env(&claude.spec(&fill, "C:/Code", 80, 24), "PROMPT").is_none());
    }

    #[test]
    fn spec_sets_size_and_directory() {
        let plan = resolve(None, &[], &shell()).unwrap();
        let fill = Fill {
            pane: PaneId::new(1),
            session_id: None,
            resume: None,
            hooks: None,
        };
        let spec = plan.spec(&fill, "C:/Code", 0, 30);
        assert_eq!((spec.cols, spec.rows), (1, 30));
        assert_eq!(spec.cwd.as_deref(), Some(std::path::Path::new("C:/Code")));
    }
}
