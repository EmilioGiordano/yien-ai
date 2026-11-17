//! The startup screens the built-in profiles read (RF-AGE-05, RN-01, RN-02).
//!
//! The Codex screens are the render snapshots in Codex 0.167.2's own
//! sources. The Claude Code screens are laid out from the 2.1.325 binary's
//! strings and components; no live capture of them exists.

use yien_agent::{ScreenRule, ScreenVerdict, Version, builtin_profiles, evaluate_screen};

const CODEX: Version = Version::new(0, 167, 2);
const CLAUDE: Version = Version::new(2, 1, 325);

/// Claude Code 2.1.325's folder trust screen, as the CLI drew it in a
/// ConPTY; the folder row is elided to a neutral path.
const CLAUDE_REAL_TRUST: &str = include_str!("fixtures/startup-screens/claude-2.1.325-trust.txt");

/// The screen rules `id` runs for a version its profile verified.
fn rules(id: &str, version: Version) -> Vec<ScreenRule> {
    let profiles = builtin_profiles().unwrap_or_default();
    let choice = profiles
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.select_rule(Some(version)));
    assert!(choice.is_some_and(|c| c.verified), "{id} {version}");
    choice.map(|c| c.rule.screen.clone()).unwrap_or_default()
}

/// `text` as a pane shows it: under earlier output, over blank rows.
fn verdict(rules: &[ScreenRule], text: &str) -> ScreenVerdict {
    let mut rows = vec!["PS C:\\work\\demo> codex", ""];
    rows.extend(text.lines());
    rows.extend([""; 20]);
    evaluate_screen(rules, &rows)
}

macro_rules! codex_screen {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!(
                "fixtures/startup-screens/codex-0.167.2-",
                $name,
                ".txt"
            )),
        )
    };
}

const CODEX_SCREENS: [(&str, &str); 9] = [
    codex_screen!("trust"),
    codex_screen!("trust-40x13"),
    codex_screen!("trust-subdirectory"),
    codex_screen!("hooks-review"),
    codex_screen!("update"),
    codex_screen!("update-28x12"),
    codex_screen!("sign-in"),
    codex_screen!("sign-in-browser"),
    codex_screen!("api-key"),
];

const CLAUDE_TRUST: &str = "\
╭─ Accessing workspace: ───────────────────────────────────────────────────────╮
│                                                                              │
│ C:\\work\\new-project                                                          │
│                                                                              │
│ Quick safety check: Is this a project you created or one you trust? (Like    │
│ your own code, a well-known open source project, or work from your team).   │
│ If not, take a moment to review what's in this folder first.                 │
│ Claude Code'll be able to read, edit, and execute files here.                │
│                                                                              │
│ Security guide                                                               │
│                                                                              │
│ ❯ No, exit                                                                   │
│   Yes, I trust this folder                                                   │
│                                                                              │
│ Enter to confirm · Esc to cancel                                             │
╰──────────────────────────────────────────────────────────────────────────────╯";

const CLAUDE_TRUST_PLAIN: &str = "\
 Accessing workspace:

 C:\\work\\new-project

 This folder pre-approves 2 tool permissions in .claude/settings.json:
   Bash(npm test), Bash(npm run build)
 These will apply without asking. Only proceed if you trust this configuration.

 ❯ Yes, I trust this folder
   No, continue without these permissions";

const CLAUDE_THEME: &str = "\
 Let's get started.

 Choose the text style that looks best with your terminal
 To change this later, run /theme

 ❯ 1. Auto (match terminal)
   2. Dark mode ✔
   3. Light mode
   4. Dark mode (colorblind-friendly)
   5. Light mode (colorblind-friendly)
   6. Dark mode (ANSI colors only)
   7. Light mode (ANSI colors only)

 ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  1  function greet() {
  2 -  console.log(\"Hello, World!\");
  2 +  console.log(\"Hello, Claude!\");
  3  }
 ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  Syntax highlighting enabled (ctrl+t to disable)";

const CLAUDE_LOGIN: &str = "\
 Claude Code can be used with your Claude subscription or billed based on API
 usage through your Console account.

 Select login method:

 ❯ 1. Claude account with subscription · Individual, Team, or Enterprise
   2. Anthropic Console account · API usage billing
   3. 3rd-party platform";

const CLAUDE_PASTE_CODE: &str = "\
 Opening browser to sign in…

 Paste code here if prompted > ";

const CLAUDE_API_KEY: &str = "\
╭─ Detected a custom API key in your environment ──────────────────────────────╮
│ ANTHROPIC_API_KEY: sk-ant-...demo                                            │
│ Do you want to use this API key?                                             │
│ ❯ Yes                                                                        │
│   No (recommended)                                                           │
╰──────────────────────────────────────────────────────────────────────────────╯";

const CLAUDE_SECURITY_NOTES: &str = "\
 Security notes:

 1. Claude can make mistakes.
    You're responsible for Claude's actions and should always
    review them, especially when running code.

 2. Due to prompt injection risks, only use it with code you trust

 Press Enter to continue…";

const CLAUDE_TERMINAL_SETUP: &str = "\
 Use Claude Code's terminal setup?

 For the optimal coding experience, enable the recommended settings
 for your terminal: Shift+Enter for newlines

 ❯ 1. Yes, use recommended settings
   2. No, maybe later with /terminal-setup";

const CLAUDE_SCREENS: [(&str, &str); 9] = [
    ("trust", CLAUDE_TRUST),
    ("trust, captured 2.1.325", CLAUDE_REAL_TRUST),
    ("trust with grants", CLAUDE_TRUST_PLAIN),
    ("theme", CLAUDE_THEME),
    ("login", CLAUDE_LOGIN),
    ("paste code", CLAUDE_PASTE_CODE),
    ("api key", CLAUDE_API_KEY),
    ("security notes", CLAUDE_SECURITY_NOTES),
    ("terminal setup", CLAUDE_TERMINAL_SETUP),
];

/// Text a shell or an agent's chat can show that names these screens
/// without being one.
const ORDINARY: &[&str] = &[
    "commit 1f2e3d4\n    fix/answer the Trust this folder? prompt\n\n    1. Trust and continue was ignored",
    "Folder access is denied.\nPS C:\\work> ",
    "✨ Update available! 0.167.2 -> 0.168.0\nRun npm install -g @openai/codex to update.\n› ",
    "Hooks need review before the release.\nNext, 1. Review hooks and 2. ship.",
    "> Pick Yes, I trust this folder or No, exit on the dialog.",
    "Docs: Choose the text style that looks best with your terminal, then Select login method: Claude account.",
    "README.md:  Security notes: none. Claude can make mistakes.",
    "PS C:\\work> echo 'Use Claude Code''s terminal setup? later'\nUse Claude Code's terminal setup? later",
    "test result: ok. 86 passed; 0 failed",
    "",
];

#[test]
fn rf_age_05_codex_startup_screens_need_you() {
    let rules = rules("codex", CODEX);
    for (name, screen) in CODEX_SCREENS {
        assert_eq!(verdict(&rules, screen), ScreenVerdict::NeedsYou, "{name}");
    }
}

#[test]
fn rf_age_05_claude_startup_screens_need_you() {
    let rules = rules("claude", CLAUDE);
    for (name, screen) in CLAUDE_SCREENS {
        assert_eq!(verdict(&rules, screen), ScreenVerdict::NeedsYou, "{name}");
    }
}

#[test]
fn rn_02_ordinary_output_is_no_startup_screen() {
    for id in ["claude", "codex"] {
        let version = if id == "claude" { CLAUDE } else { CODEX };
        let rules = rules(id, version);
        for text in ORDINARY {
            assert_eq!(
                verdict(&rules, text),
                ScreenVerdict::NoMatch,
                "{id}: {text}"
            );
        }
    }
}

#[test]
fn rn_02_a_profile_listing_its_own_patterns_is_no_startup_screen() {
    for (id, text, version) in [
        ("claude", include_str!("../profiles/claude.toml"), CLAUDE),
        ("codex", include_str!("../profiles/codex.toml"), CODEX),
    ] {
        let rules = rules(id, version);
        assert_eq!(verdict(&rules, text), ScreenVerdict::NoMatch, "{id}");
    }
}

#[test]
fn rn_02_a_startup_screen_that_scrolled_up_is_no_longer_read() {
    let rules = rules("codex", CODEX);
    let trust = CODEX_SCREENS[0].1;
    let below = "› Write tests for @filename\n\n  ? for shortcuts\n".repeat(20);
    assert_eq!(
        verdict(&rules, &format!("{trust}\n{below}")),
        ScreenVerdict::NoMatch
    );
}

#[test]
fn rn_02_only_the_versions_whose_screens_were_read_get_startup_rules() {
    let profiles = builtin_profiles().unwrap();
    let claude = profiles.iter().find(|p| p.id == "claude").unwrap();
    let old = claude.select_rule(Some(Version::new(2, 0, 50))).rule;
    for (name, screen) in CLAUDE_SCREENS {
        assert_eq!(
            verdict(&old.screen, screen),
            ScreenVerdict::NoMatch,
            "{name}"
        );
    }
    let codex = profiles.iter().find(|p| p.id == "codex").unwrap();
    let catch_all = codex.rules.iter().find(|r| r.versions.is_none()).unwrap();
    assert!(catch_all.screen.is_empty());
    assert!(!catch_all.machine_config().reads_startup_screens());
    let newest = codex.select_rule(Some(CODEX)).rule;
    assert!(newest.machine_config().reads_startup_screens());
}

#[test]
fn rn_02_opencode_and_the_shell_read_no_startup_screens() {
    // OpenCode reports idle over OSC 7501 as it starts, before any dialog;
    // a CLI typed in a shell is read with its own profile's patterns.
    let profiles = builtin_profiles().unwrap();
    for id in ["opencode", "shell"] {
        let profile = profiles.iter().find(|p| p.id == id).unwrap();
        assert!(profile.rules.iter().all(|r| r.screen.is_empty()), "{id}");
        assert!(profile.typed_startup_screens().is_empty(), "{id}");
    }
}

/// The patterns a shell pane reads for `id` typed in it.
fn typed(id: &str) -> Vec<ScreenRule> {
    let profiles = builtin_profiles().unwrap_or_default();
    let profile = profiles.iter().find(|p| p.id == id);
    profile.map_or_else(Vec::new, |p| p.typed_startup_screens().to_vec())
}

#[test]
fn rf_age_05_an_agent_typed_in_a_shell_reads_the_screens_of_its_newest_rule() {
    // The version of a typed CLI is not probed, as in its own pane.
    for (name, screen) in CODEX_SCREENS {
        assert_eq!(
            verdict(&typed("codex"), screen),
            ScreenVerdict::NeedsYou,
            "{name}"
        );
    }
    for (name, screen) in CLAUDE_SCREENS {
        assert_eq!(
            verdict(&typed("claude"), screen),
            ScreenVerdict::NeedsYou,
            "{name}"
        );
    }
}

#[test]
fn rn_02_a_typed_agent_needs_a_rule_that_reports_over_osc_7501() {
    // A shell pane never gets hooks: only OSC 7501 can end the reads.
    let hooks_only = "schema = 1\nid = \"a\"\nname = \"A\"\n[[rules]]\n\
                      sources = [\"hooks\", \"screen\"]\nhooks = \"claude-command-v1\"\n\
                      [[rules.screen]]\nconfirms = \"needs_you\"\nbottom_rows = 3\npattern = \"x\"\n";
    let profile = yien_agent::Profile::from_toml(hooks_only).unwrap();
    assert!(profile.typed_startup_screens().is_empty());
    let reporting = hooks_only.replace(
        "\"hooks\", \"screen\"",
        "\"hooks\", \"osc7501\", \"screen\"",
    );
    let profile = yien_agent::Profile::from_toml(&reporting).unwrap();
    assert_eq!(profile.typed_startup_screens().len(), 1);
}

/// What a shell shows for plain commands, around the agents' names.
const SHELL_OUTPUT: &[&str] = &[
    "PS C:\\work\\demo> git status\nOn branch main\nnothing to commit, working tree clean\nPS C:\\work\\demo> ",
    "PS C:\\work\\demo> codex --version\ncodex-cli 0.167.2\nPS C:\\work\\demo> ",
    "C:\\work\\demo>dir\n Volume in drive C has no label.\n 11/15/2026  10:00 AM    <DIR>          src\n               1 File(s)          1,024 bytes\nC:\\work\\demo>",
    "$ claude --help\nUsage: claude [options] [command] [prompt]\n\nOptions:\n  -p, --print  Print response and exit\n$ ",
    "PS C:\\work\\demo> npm install -g @openai/codex\nadded 1 package in 3s\nPS C:\\work\\demo> ",
];

#[test]
fn rn_02_a_plain_command_s_output_never_matches_a_typed_agent_s_screens() {
    for id in ["claude", "codex"] {
        let rules = typed(id);
        assert!(!rules.is_empty(), "{id}");
        for text in SHELL_OUTPUT.iter().chain(ORDINARY) {
            assert_eq!(
                verdict(&rules, text),
                ScreenVerdict::NoMatch,
                "{id}: {text}"
            );
        }
    }
}
