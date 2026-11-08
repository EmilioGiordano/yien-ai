//! The agent profiles panes start with: the built-in ones with the user's
//! files from `<config dir>/profiles` over them (RF-AGE-04, 04 §4.7). Read
//! once at start; a change applies at the next start.

use yien_agent::{Profile, UserProfiles, builtin_profiles, resolve_profiles};

/// The profiles for the core and one line per skipped file for the status
/// bar. Without a data dir only the built-in profiles load.
pub(crate) fn load() -> (Vec<Profile>, Vec<String>) {
    let user = match yien_platform::paths::data_dir() {
        Ok(dir) => yien_platform::user_profiles::load(&dir),
        Err(error) => {
            tracing::warn!("no data dir, user profiles are off: {error}");
            UserProfiles::default()
        }
    };
    merge(builtin_profiles(), user)
}

fn merge(
    builtin: Result<Vec<Profile>, yien_agent::ProfileError>,
    user: UserProfiles,
) -> (Vec<Profile>, Vec<String>) {
    // The message can quote the file, so the log names only its place.
    for problem in &user.problems {
        let (line, column) = problem.place.unwrap_or((0, 0));
        tracing::warn!(
            file = %problem.file,
            line,
            column,
            kind = problem.kind.as_str(),
            "a profile file was skipped"
        );
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
    (resolve_profiles(builtin, user.profiles), notices)
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
        let (profiles, notices) = merge(builtin_profiles(), user);
        assert_eq!(profiles.len(), 4);
        assert_eq!(profiles[0].name, "Fake");
        assert_eq!(notices.len(), 1);
        assert!(notices[0].starts_with("profiles/bad.toml, line 1, column "));
    }
}
