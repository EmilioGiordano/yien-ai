//! A screen dump (RF-SES-10) of any screen is a dump a fresh engine takes
//! back whole: it passes the check, answers nothing, and dumps the same again.

use proptest::prelude::*;
use yien_vt::{AlacrittyEngine, DumpLimits, VtConfig, VtEngine, is_screen_dump};

/// Bytes biased toward what programs draw with: text, colours, attributes,
/// wide and combining characters, moves and erases.
fn drawing() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        2 => proptest::collection::vec(any::<u8>(), 0..16),
        4 => "[a-z \u{4e2d}\u{1f600}]{0,24}".prop_map(String::into_bytes),
        2 => Just(b"\r\n".to_vec()),
        1 => Just("e\u{301}".as_bytes().to_vec()),
        2 => (0u8..110).prop_map(|n| format!("\x1b[{n}m").into_bytes()),
        1 => (0u16..256).prop_map(|n| format!("\x1b[38;5;{n};48;5;{}m", 255 - n).into_bytes()),
        1 => any::<(u8, u8, u8)>().prop_map(|(r, g, b)| format!("\x1b[38;2;{r};{g};{b}m").into_bytes()),
        1 => (0u8..6).prop_map(|n| format!("\x1b[4:{n};58;5;{n}m").into_bytes()),
        1 => (0u16..40, 0u16..90).prop_map(|(r, c)| format!("\x1b[{r};{c}H").into_bytes()),
        1 => (0u8..3).prop_map(|n| format!("\x1b[{n}K").into_bytes()),
        1 => (0u8..3).prop_map(|n| format!("\x1b[{n}J").into_bytes()),
    ];
    proptest::collection::vec(piece, 1..40).prop_map(|p| p.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn rf_ses_10_a_dump_restores_into_the_same_dump(
        cols in 2u16..90,
        rows in 1u16..40,
        bytes in drawing(),
    ) {
        let config = VtConfig { scrollback_lines: 200, ..VtConfig::default() };
        let mut old = AlacrittyEngine::new(config.clone(), cols, rows);
        old.feed(&bytes);
        let dump = old.screen_dump(DumpLimits::default());
        prop_assert!(is_screen_dump(&dump));
        let mut new = AlacrittyEngine::new(config, cols, rows);
        let out = new.feed(&dump);
        prop_assert!(out.replies.is_empty() && out.title.is_none() && out.signals.is_empty());
        prop_assert_eq!(new.screen_dump(DumpLimits::default()), dump);
    }
}
