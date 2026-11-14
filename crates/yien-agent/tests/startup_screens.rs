//! The startup screens the built-in profiles read (RF-AGE-05, RN-01, RN-02).
//!
//! The Codex screens are the render snapshots in Codex 0.167.2's own
//! sources. The Claude Code screens are laid out from the 2.1.325 binary's
//! strings and components; no live capture of them exists.

use yien_agent::{ScreenRule, ScreenVerdict, Version, builtin_profiles, evaluate_screen};

const CODEX: Version = Version::new(0, 167, 2);
const CLAUDE: Version = Version::new(2, 1, 325);

fn rules(id: &str, version: Version) -> Vec<ScreenRule> {
    let profiles = builtin_profiles().unwrap();
    let profile = profiles.iter().find(|p| p.id == id).unwrap();
    let choice = profile.select_rule(Some(version));
    assert!(choice.verified, "{id} {version}");
    choice.rule.screen.clone()
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

const CLAUDE_SCREENS: [(&str, &str); 8] = [
    ("trust", CLAUDE_TRUST),
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
    // a typed CLI in a shell is left to its own branch.
    let profiles = builtin_profiles().unwrap();
    for id in ["opencode", "shell"] {
        let profile = profiles.iter().find(|p| p.id == id).unwrap();
        assert!(profile.rules.iter().all(|r| r.screen.is_empty()), "{id}");
    }
}
