//! The agent profiles panes start with: the built-in ones with the user's
//! files from `<config dir>/profiles` over them (RF-AGE-04, 04 §4.7). Read
//! once at start; a change applies at the next start.

use std::collections::BTreeMap;

use yien_agent::{Profile, UserProfiles, builtin_profiles};

/// What the core and the window get from the profile files.
pub(crate) struct Loaded {
    /// The profiles panes can start with.
    pub(crate) profiles: Vec<Profile>,
    /// Ids whose file was skipped, with the file; they never fall back to a
    /// built-in profile.
    pub(crate) broken: BTreeMap<String, String>,
    /// One line per skipped file, for the status bar.
    pub(crate) notices: Vec<String>,
}

/// Reads the user's files. Without a data dir only the built-in profiles
/// load.
pub(crate) fn load() -> Loaded {
    let user = match yien_platform::paths::data_dir() {
        Ok(dir) => yien_platform::user_profiles::load(&dir),
        Err(error) => {
            tracing::warn!("no data dir, user profiles are off: {error}");
            UserProfiles::default()
        }
    };
    merge(builtin_profiles(), user)
}

fn merge(builtin: Result<Vec<Profile>, yien_agent::ProfileError>, user: UserProfiles) -> Loaded {
    // The message can quote the file, so the log names only its place.
    for problem in &user.problems {
        let (line, column) = problem.place.unwrap_or((0, 0));
        tracing::warn!(
            file = ?problem.file,
            line,
            column,
            kind = problem.kind.as_str(),
            "a profile file was skipped"
        );
    }
    if !user.problems.is_empty() {
        tracing::warn!(problems = user.problems.len(), "profile files were skipped");
    }
    if !user.broken.is_empty() {
        let ids: Vec<&String> = user.broken.keys().collect();
        tracing::warn!(?ids, "profiles not loaded because their file was skipped");
    }
    let builtin = builtin.unwrap_or_else(|error| {
        tracing::error!("the built-in profiles are invalid: {error}");
        Vec::new()
    });
    let ids: Vec<&str> = user.profiles.iter().map(|p| p.id.as_str()).collect();
    if !ids.is_empty() {
        tracing::info!(?ids, "user profiles loaded");
    }
    let notices = user.problems.iter().map(ToString::to_string).collect();
    let (profiles, broken) = user.over(builtin);
    Loaded {
        profiles,
        broken,
        notices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf_age_04_user_profiles_go_over_the_builtin_ones() {
        let user = yien_agent::load_user_profiles(vec![
            (
                "claude.toml".into(),
                "schema = 1\nid = \"claude\"\nname = \"Fake\"\n[[rules]]\nsources = []\n".into(),
            ),
            ("bad.toml".into(), "schema = = 1\n".into()),
        ]);
        let loaded = merge(builtin_profiles(), user);
        assert_eq!(loaded.profiles.len(), 4);
        assert_eq!(loaded.profiles[0].name, "Fake");
        assert_eq!(loaded.notices.len(), 1);
        assert!(loaded.notices[0].starts_with("profiles/bad.toml, line 1, column "));
        assert_eq!(loaded.broken["bad"], "profiles/bad.toml");
    }

    #[test]
    fn rf_age_04_a_broken_override_drops_the_builtin_profile() {
        let user =
            yien_agent::load_user_profiles(vec![("codex.toml".into(), "schema = = 1\n".into())]);
        let loaded = merge(builtin_profiles(), user);
        assert!(loaded.profiles.iter().all(|p| p.id != "codex"));
        assert_eq!(loaded.broken["codex"], "profiles/codex.toml");
    }
}
