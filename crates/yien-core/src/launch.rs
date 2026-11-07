//! What a pane runs: a profile resolved to a program, its arguments and the
//! agent detection rule (RF-AGE-01, RF-PAN-18).

use yien_agent::{MachineConfig, Profile, ScreenRule};
use yien_proto::{PaneId, ProfileId};
use yien_pty::SpawnSpec;

use crate::CoreError;
use crate::shell::ShellCommand;

/// The id of the built-in shell profile.
const SHELL_PROFILE: &str = "shell";

/// Placeholders only the hooks pipe and the session layer can fill. Until
/// they exist, nothing carrying one reaches a child.
const UNRESOLVED: &[&str] = &[
    "{new_uuid}",
    "{session_id}",
    "{hooks_settings_file}",
    "{hook_token}",
];

/// A profile resolved for launching.
#[derive(Debug, Clone)]
pub(crate) struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub machine: MachineConfig,
    pub screen: Vec<ScreenRule>,
    pub profile_name: String,
    pub default_name: String,
}

impl LaunchPlan {
    /// The spawn spec for `pane`, with `{pane_id}` filled in.
    pub fn spec(&self, pane: PaneId, cwd: &str, cols: u16, rows: u16) -> SpawnSpec {
        let fill = |s: &str| s.replace("{pane_id}", &pane.get().to_string());
        let mut spec = SpawnSpec::new(&self.program)
            .args(self.args.iter().map(|a| fill(a)))
            .size(cols.max(1), rows.max(1));
        if !cwd.is_empty() {
            spec = spec.cwd(cwd);
        }
        for (key, value) in &self.env {
            spec = spec.env(key, fill(value));
        }
        spec
    }
}

fn unresolved(text: &str) -> bool {
    UNRESOLVED.iter().any(|p| text.contains(p))
}

/// Resolves `profile` (the shell when `None`) against the known profiles.
///
/// An agent profile runs its detected command. Its launch arguments are kept
/// only when none needs a placeholder this core cannot fill yet (session id,
/// hook settings, hook token); otherwise it starts with no arguments. The
/// CLI version is not probed, so the newest rule is used, unverified.
pub(crate) fn resolve(
    profile: Option<&ProfileId>,
    profiles: &[Profile],
    shell: &ShellCommand,
) -> Result<LaunchPlan, CoreError> {
    let id = profile.map_or(SHELL_PROFILE, ProfileId::as_str);
    let found = profiles.iter().find(|p| p.id == id);
    let Some(found) = found else {
        if id == SHELL_PROFILE {
            return Ok(LaunchPlan {
                program: shell.program.clone(),
                args: shell.args.clone(),
                env: Vec::new(),
                machine: MachineConfig::unprofiled(),
                screen: Vec::new(),
                profile_name: "Shell".to_owned(),
                default_name: shell.display_name(),
            });
        }
        return Err(CoreError::UnknownProfile(id.to_owned()));
    };

    let rule = found.select_rule(None).rule;
    let env = found
        .launch
        .env
        .iter()
        .filter(|(_, v)| !unresolved(v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let (program, args, default_name) = match &found.detect {
        Some(detect) => {
            let args = if found.launch.args.iter().any(|a| unresolved(a)) {
                Vec::new()
            } else {
                found.launch.args.clone()
            };
            (detect.command.clone(), args, detect.command.clone())
        }
        None => (
            shell.program.clone(),
            shell.args.clone(),
            shell.display_name(),
        ),
    };
    Ok(LaunchPlan {
        program,
        args,
        env,
        machine: rule.machine_config(),
        screen: rule.screen.clone(),
        profile_name: found.name.clone(),
        default_name,
    })
}

#[cfg(test)]
mod tests {
    use yien_agent::{Source, builtin_profiles};

    use super::*;

    fn shell() -> ShellCommand {
        ShellCommand::new("pwsh.exe")
    }

    #[test]
    fn rf_pan_18_no_profile_runs_the_shell_with_the_shell_rule() {
        let plan = resolve(None, &builtin_profiles().unwrap(), &shell()).unwrap();
        assert_eq!(plan.program, "pwsh.exe");
        assert_eq!(plan.default_name, "pwsh");
        assert_eq!(plan.profile_name, "Shell");
        assert!(plan.machine.declares(Source::Osc133));
    }

    #[test]
    fn rf_pan_18_agent_profile_never_passes_unfilled_placeholders() {
        let plan = resolve(
            Some(&ProfileId::new("claude")),
            &builtin_profiles().unwrap(),
            &shell(),
        )
        .unwrap();
        assert_eq!(plan.program, "claude");
        assert_eq!(plan.profile_name, "Claude Code");
        assert!(plan.machine.declares(Source::Hooks));
        let spec = plan.spec(PaneId::new(42), "C:/Code", 100, 30);
        for arg in &spec.args {
            assert!(!arg.to_string_lossy().contains('{'), "{arg:?}");
        }
        for (key, value) in &spec.env {
            assert!(!value.to_string_lossy().contains('{'), "{key:?}");
        }
        assert!(spec.env.iter().any(|(k, v)| k == "YIEN_PANE" && v == "42"));
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
    fn shell_works_without_any_profiles() {
        let plan = resolve(None, &[], &shell()).unwrap();
        assert_eq!(plan.program, "pwsh.exe");
        assert!(!plan.machine.declares(Source::Osc133));
    }

    #[test]
    fn spec_sets_size_and_directory() {
        let plan = resolve(None, &[], &shell()).unwrap();
        let spec = plan.spec(PaneId::new(1), "C:/Code", 0, 30);
        assert_eq!((spec.cols, spec.rows), (1, 30));
        assert_eq!(spec.cwd.as_deref(), Some(std::path::Path::new("C:/Code")));
    }
}
