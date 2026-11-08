//! The user's profile files on disk (RF-AGE-04, 04 §4.7): every `*.toml`
//! in the `profiles` folder of YIEN's config dir, read at start with a
//! bound on the file count and each file's size. The folder is never
//! created or written, and never followed outside the config dir.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use yien_agent::{
    MAX_PROFILE_BYTES, MAX_USER_PROFILES, PROFILES_DIR, ProblemKind, ProfileProblem, UserProfiles,
    load_user_profiles,
};

/// The most folder entries looked at, profile or not.
const MAX_ENTRIES: usize = 1024;

/// Reads and parses the profile files under `config_dir`. A missing folder
/// is no profiles and no problem; anything refused is a problem, and the
/// other files still load.
///
/// ```no_run
/// let dir = yien_platform::paths::data_dir()?;
/// let user = yien_platform::user_profiles::load(&dir);
/// for problem in &user.problems {
///     eprintln!("{problem}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn load(config_dir: &Path) -> UserProfiles {
    let mut problems = Vec::new();
    let files = read_files(config_dir, MAX_USER_PROFILES, &mut problems);
    let mut user = load_user_profiles(files);
    problems.append(&mut user.problems);
    user.problems = problems;
    user
}

fn folder_problem(message: impl Into<String>) -> ProfileProblem {
    ProfileProblem {
        file: String::new(),
        place: None,
        kind: ProblemKind::Skipped,
        message: message.into(),
    }
}

fn file_problem(file: &str, kind: ProblemKind, message: impl Into<String>) -> ProfileProblem {
    ProfileProblem {
        file: file.to_owned(),
        place: None,
        kind,
        message: message.into(),
    }
}

/// The `(name, text)` of each readable profile file, in name order, at most
/// `max_files` of them.
fn read_files(
    config_dir: &Path,
    max_files: usize,
    problems: &mut Vec<ProfileProblem>,
) -> Vec<(String, String)> {
    let folder = config_dir.join(PROFILES_DIR);
    match fs::symlink_metadata(&folder) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            problems.push(folder_problem(format!("the folder could not be read: {e}")));
            return Vec::new();
        }
        Ok(_) => {}
    }
    let inside = match (fs::canonicalize(config_dir), fs::canonicalize(&folder)) {
        (Ok(root), Ok(folder)) if folder.starts_with(&root) && folder.is_dir() => folder,
        (Ok(_), Ok(_)) => {
            problems.push(folder_problem(
                "is not a folder inside YIEN's config folder; no profile was read from it",
            ));
            return Vec::new();
        }
        (Err(e), _) | (_, Err(e)) => {
            problems.push(folder_problem(format!("the folder could not be read: {e}")));
            return Vec::new();
        }
    };
    let entries = match fs::read_dir(&inside) {
        Ok(entries) => entries,
        Err(e) => {
            problems.push(folder_problem(format!("the folder could not be read: {e}")));
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for (seen, entry) in entries.enumerate() {
        if seen == MAX_ENTRIES {
            problems.push(folder_problem(format!(
                "holds more than {MAX_ENTRIES} entries; only the first {MAX_ENTRIES} were looked at"
            )));
            break;
        }
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_profile_name(&name) {
            continue;
        }
        // `file_type` does not follow links: a link is never a profile.
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            problems.push(file_problem(
                &name,
                ProblemKind::Skipped,
                "is not a plain file; it is skipped",
            ));
            continue;
        }
        names.push(name);
    }
    names.sort();
    if names.len() > max_files {
        let skipped = names.split_off(max_files);
        problems.push(folder_problem(format!(
            "holds {} profile files; only the first {max_files} by name are read, \
             {} and later are skipped",
            max_files + skipped.len(),
            skipped[0]
        )));
    }
    let mut files = Vec::with_capacity(names.len());
    for name in names {
        match read_one(&inside, &name) {
            Ok(text) => files.push((name, text)),
            Err(why) => problems.push(file_problem(&name, ProblemKind::Unreadable, why)),
        }
    }
    files
}

/// `*.toml`, ignoring case, and not a bare `.toml`.
fn is_profile_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.len() > ".toml".len() && lower.ends_with(".toml")
}

/// The file's text, read no further than [`MAX_PROFILE_BYTES`] and only
/// when it still resolves to a file of the folder itself.
fn read_one(folder: &Path, name: &str) -> Result<String, String> {
    let path = folder.join(name);
    let real = fs::canonicalize(&path).map_err(|e| format!("could not be read: {e}"))?;
    if real.parent() != Some(folder) {
        return Err("leads outside the profiles folder; it is skipped".into());
    }
    let file = fs::File::open(&real).map_err(|e| format!("could not be read: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("could not be read: {e}"))?;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(format!(
            "is larger than {} KiB; it is skipped",
            MAX_PROFILE_BYTES / 1024
        ));
    }
    String::from_utf8(bytes).map_err(|_| "is not UTF-8; it is skipped".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("yien-user-profiles-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn profile(id: &str) -> String {
        format!("schema = 1\nid = \"{id}\"\nname = \"{id}\"\n[[rules]]\nsources = []\n")
    }

    fn ids(user: &UserProfiles) -> Vec<&str> {
        user.profiles.iter().map(|p| p.id.as_str()).collect()
    }

    #[test]
    fn rf_age_04_a_missing_folder_is_no_profiles_and_is_never_created() {
        let dir = scratch("missing");
        let user = load(&dir);
        assert!(user.profiles.is_empty());
        assert!(user.problems.is_empty());
        assert!(!dir.join(PROFILES_DIR).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_age_04_profiles_load_from_the_folder_and_bad_files_are_named() {
        let dir = scratch("load");
        let folder = dir.join(PROFILES_DIR);
        fs::create_dir_all(folder.join("nested.toml")).unwrap();
        fs::write(folder.join("claude.toml"), profile("claude")).unwrap();
        fs::write(folder.join("MyTool.TOML"), profile("mytool")).unwrap();
        fs::write(folder.join("bad.toml"), "schema = 1\nid = = 2\n").unwrap();
        fs::write(folder.join("notes.txt"), "not a profile").unwrap();
        fs::write(folder.join("latin1.toml"), b"name = \"\xe9\"\n").unwrap();
        fs::write(folder.join("huge.toml"), vec![b'#'; MAX_PROFILE_BYTES + 1]).unwrap();
        let user = load(&dir);
        assert_eq!(ids(&user), ["mytool", "claude"], "name order");
        let named: Vec<(&str, Option<(usize, usize)>)> = user
            .problems
            .iter()
            .map(|p| (p.file.as_str(), p.place))
            .collect();
        assert!(named.contains(&("bad.toml", Some((2, 6)))), "{named:?}");
        assert!(named.contains(&("huge.toml", None)), "{named:?}");
        assert!(named.contains(&("latin1.toml", None)), "{named:?}");
        assert!(
            named.contains(&("nested.toml", None)),
            "a folder: {named:?}"
        );
        assert_eq!(user.problems.len(), 4, "{named:?}");
        assert!(!named.iter().any(|(f, _)| *f == "notes.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_age_04_only_the_first_files_by_name_are_read() {
        let dir = scratch("count");
        let folder = dir.join(PROFILES_DIR);
        fs::create_dir_all(&folder).unwrap();
        for i in 0..5 {
            fs::write(folder.join(format!("p{i}.toml")), profile(&format!("p{i}"))).unwrap();
        }
        let mut problems = Vec::new();
        let files = read_files(&dir, 3, &mut problems);
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["p0.toml", "p1.toml", "p2.toml"]);
        let [problem] = problems.as_slice() else {
            panic!("{problems:?}")
        };
        assert_eq!(problem.kind, ProblemKind::Skipped);
        assert!(problem.message.contains("p3.toml"), "{problem}");
        assert!(
            problem.to_string().starts_with("profiles: holds 5"),
            "{problem}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rf_age_04_a_file_named_profiles_is_refused() {
        let dir = scratch("file");
        fs::write(dir.join(PROFILES_DIR), profile("x")).unwrap();
        let user = load(&dir);
        assert!(user.profiles.is_empty());
        assert_eq!(user.problems.len(), 1);
        assert_eq!(user.problems[0].file, "");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Unix only: creating a symbolic link on Windows needs a privilege.
    #[cfg(unix)]
    #[test]
    fn rf_age_04_links_out_of_the_config_dir_are_never_followed() {
        use std::os::unix::fs::symlink;

        let outside = scratch("outside");
        fs::write(outside.join("evil.toml"), profile("evil")).unwrap();

        let dir = scratch("linked-folder");
        symlink(&outside, dir.join(PROFILES_DIR)).unwrap();
        let user = load(&dir);
        assert!(user.profiles.is_empty());
        assert_eq!(user.problems.len(), 1, "{:?}", user.problems);

        let dir2 = scratch("linked-file");
        let folder = dir2.join(PROFILES_DIR);
        fs::create_dir_all(&folder).unwrap();
        symlink(outside.join("evil.toml"), folder.join("evil.toml")).unwrap();
        fs::write(folder.join("good.toml"), profile("good")).unwrap();
        let user = load(&dir2);
        assert_eq!(ids(&user), ["good"]);
        assert_eq!(user.problems[0].file, "evil.toml");

        for d in [outside, dir, dir2] {
            let _ = fs::remove_dir_all(&d);
        }
    }
}
