//! The shells a pane can run on Windows, and the profile ids that start one
//! of them for a single pane (RF-PAN-18, RF-ESP-03).

use serde::{Deserialize, Serialize};

use crate::{ProfileId, SHELL_PROFILE};

/// What every profile id that starts a given shell begins with.
const KIND_PREFIX: &str = "shell:";

/// A shell YIEN knows how to find and set up.
///
/// ```
/// use yien_proto::ShellKind;
///
/// assert_eq!(ShellKind::GitBash.id(), "git-bash");
/// assert_eq!(ShellKind::from_id("pwsh"), Some(ShellKind::Pwsh));
/// assert_eq!(ShellKind::Cmd.profile().as_str(), "shell:cmd");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ShellKind {
    /// `cmd.exe`.
    #[serde(rename = "cmd")]
    Cmd,
    /// Windows PowerShell 5.1, `powershell.exe`.
    #[serde(rename = "powershell")]
    PowerShell,
    /// PowerShell 7, `pwsh.exe`.
    #[serde(rename = "pwsh")]
    Pwsh,
    /// Git for Windows' bash.
    #[serde(rename = "git-bash")]
    GitBash,
}

impl ShellKind {
    /// Every kind, in the order menus list them.
    pub const ALL: [ShellKind; 4] = [Self::Cmd, Self::PowerShell, Self::Pwsh, Self::GitBash];

    /// The id the settings file and profile ids use.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::PowerShell => "powershell",
            Self::Pwsh => "pwsh",
            Self::GitBash => "git-bash",
        }
    }

    /// The name menus show.
    ///
    /// ```
    /// assert_eq!(yien_proto::ShellKind::Pwsh.name(), "PowerShell 7");
    /// ```
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cmd => "Command Prompt",
            Self::PowerShell => "Windows PowerShell",
            Self::Pwsh => "PowerShell 7",
            Self::GitBash => "Git Bash",
        }
    }

    /// The kind with this [`ShellKind::id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// The profile that starts this shell in one pane, whatever the default
    /// shell is.
    pub fn profile(self) -> ProfileId {
        ProfileId::new(format!("{KIND_PREFIX}{}", self.id()))
    }

    /// The kind a [`ShellKind::profile`] id starts; `None` for any other id,
    /// the default shell's included.
    ///
    /// ```
    /// use yien_proto::ShellKind;
    ///
    /// assert_eq!(ShellKind::from_profile("shell:git-bash"), Some(ShellKind::GitBash));
    /// assert_eq!(ShellKind::from_profile("shell"), None);
    /// ```
    pub fn from_profile(id: &str) -> Option<Self> {
        id.strip_prefix(KIND_PREFIX).and_then(Self::from_id)
    }
}

/// Whether a pane started with profile `id` runs a shell rather than an
/// agent: the default shell or one of the [`ShellKind`]s.
///
/// ```
/// assert!(yien_proto::is_shell_profile("shell"));
/// assert!(yien_proto::is_shell_profile("shell:cmd"));
/// assert!(!yien_proto::is_shell_profile("claude"));
/// ```
pub fn is_shell_profile(id: &str) -> bool {
    id == SHELL_PROFILE || ShellKind::from_profile(id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_pan_18_every_kind_has_its_own_profile_id() {
        for kind in ShellKind::ALL {
            let profile = kind.profile();
            assert_eq!(ShellKind::from_profile(profile.as_str()), Some(kind));
            assert!(is_shell_profile(profile.as_str()));
            assert_eq!(ShellKind::from_id(kind.id()), Some(kind));
        }
        for other in ["", "claude", "shell:", "shell:bash", "cmd", "Shell:cmd"] {
            assert_eq!(ShellKind::from_profile(other), None, "{other}");
            assert!(!is_shell_profile(other), "{other}");
        }
    }

    #[test]
    fn kinds_serialize_as_their_ids() {
        for kind in ShellKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.id()));
            assert_eq!(serde_json::from_str::<ShellKind>(&json).unwrap(), kind);
        }
    }
}
