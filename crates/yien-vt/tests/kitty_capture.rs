//! OpenCode 2.3.0's real start-up bytes, replayed through the engine, and
//! the bytes YIEN then encodes for the keys its turn control depends on
//! (RF-AGE-02: the official CLI receives its input verbatim; RF-NAV-07:
//! YIEN hands every unassigned combination to the pane).
//!
//! The fixture is a verbatim copy of the capture on branch test/cli-captures
//! (`crates/yien-core/tests/fixtures/osc7501/opencode-2.3.0-startup.bin`);
//! it is not in `main` yet. Byte-identical on both sides.

use yien_input::{Key, KeyModes, KeyQuirks, Mods, encode_with};
use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};

const OPENCODE_STARTUP: &[u8] = include_bytes!("fixtures/opencode-2.3.0-startup.bin");

fn startup_engine() -> AlacrittyEngine {
    let mut engine = AlacrittyEngine::new(VtConfig::default(), 120, 40);
    engine.feed(OPENCODE_STARTUP);
    engine
}

/// The modes the pane hands the encoder, exactly as the grid builds them.
fn pane_key_modes(engine: &AlacrittyEngine) -> KeyModes {
    let modes = engine.modes();
    KeyModes {
        app_cursor: modes.app_cursor,
        win32_input: modes.win32_input,
        kitty_flags: modes.kitty_keyboard,
        quirks: KeyQuirks::default(),
    }
}

#[test]
fn rf_age_02_the_opencode_start_up_bytes_are_answered_and_tracked_per_screen() {
    let mut engine = AlacrittyEngine::new(VtConfig::default(), 120, 40);
    let out = engine.feed(OPENCODE_STARTUP);
    // The program queried `CSI ? u` before pushing any flags: the answer
    // describes the main screen at that moment, which knew none.
    assert!(
        out.replies.windows(5).any(|w| w == b"\x1b[?0u".as_slice()),
        "{:?}",
        String::from_utf8_lossy(&out.replies)
    );
    // It then entered the alternate screen and pushed flags 5
    // (disambiguate | report alternate keys) there; the main screen keeps 0.
    assert!(engine.modes().alt_screen);
    assert_eq!(engine.modes().kitty_keyboard, 5);
    assert!(engine.modes().win32_input);
}

#[test]
fn rf_age_02_the_opencode_start_up_bytes_give_the_pane_kitty_disambiguate() {
    let engine = startup_engine();
    let key_modes = pane_key_modes(&engine);
    // Esc and Ctrl+C are the keys this build reacts to mid-turn; both must
    // be CSI u, not the legacy bytes it ignores.
    assert_eq!(
        encode_with(&Key::Escape, Mods::NONE, key_modes).as_deref(),
        Some(b"\x1b[27u".as_slice())
    );
    assert_eq!(
        encode_with(&Key::Char('c'), Mods::CTRL, key_modes).as_deref(),
        Some(b"\x1b[99;5u".as_slice())
    );
}

#[test]
fn rf_age_02_the_opencode_start_up_bytes_keep_legacy_for_text_and_exceptions() {
    let engine = startup_engine();
    let key_modes = pane_key_modes(&engine);
    // Enter, Tab and Backspace keep their legacy bytes even under
    // disambiguation: the shell must stay reachable.
    assert_eq!(
        encode_with(&Key::Enter, Mods::NONE, key_modes).as_deref(),
        Some(b"\r".as_slice())
    );
    assert_eq!(
        encode_with(&Key::Tab, Mods::NONE, key_modes).as_deref(),
        Some(b"\t".as_slice())
    );
    assert_eq!(
        encode_with(&Key::Backspace, Mods::NONE, key_modes).as_deref(),
        Some(vec![0x7f].as_slice())
    );
    // Shift+Enter is CSI 13;2u, as the bundled OpenConsole passes it.
    assert_eq!(
        encode_with(&Key::Enter, Mods::SHIFT, key_modes).as_deref(),
        Some(b"\x1b[13;2u".as_slice())
    );
    // A plain letter is text, unchanged.
    assert_eq!(
        encode_with(&Key::Char('a'), Mods::NONE, key_modes).as_deref(),
        Some(b"a".as_slice())
    );
    // Arrows use the kitty-valid `CSI 1; modifier [A]` form with modifiers.
    assert_eq!(
        encode_with(&Key::Up, Mods::NONE, key_modes).as_deref(),
        Some(if key_modes.app_cursor {
            b"\x1bOA".as_slice()
        } else {
            b"\x1b[A".as_slice()
        })
    );
    assert_eq!(
        encode_with(&Key::Right, Mods::CTRL, key_modes).as_deref(),
        Some(b"\x1b[1;5C".as_slice())
    );
}
