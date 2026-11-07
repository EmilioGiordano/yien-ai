//! `osc_scan` yields the same signals however the PTY reader cuts the stream.

use proptest::prelude::*;
use yien_vt::osc_scan::{OscScanner, OscSignal};

fn scan_whole(bytes: &[u8]) -> Vec<OscSignal> {
    OscScanner::new().scan(bytes)
}

fn scan_chunks(chunks: &[&[u8]]) -> Vec<OscSignal> {
    let mut scanner = OscScanner::new();
    let mut out = Vec::new();
    for chunk in chunks {
        scanner.scan_into(chunk, &mut out);
    }
    out
}

/// Payload text without control characters, with non-ASCII so UTF-8
/// continuation bytes (some equal to C1 controls) land in the payload.
fn payload() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ;:/._%\u{e9}\u{4e2d}\u{2733}\u{1f600}-]{0,24}"
}

fn terminator() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("\x07"), Just("\x1b\\")]
}

/// A sequence the scanner must recognise.
fn known_sequence() -> impl Strategy<Value = Vec<u8>> {
    let body = prop_oneof![
        payload().prop_map(|p| format!("0;{p}")),
        payload().prop_map(|p| format!("2;{p}")),
        payload().prop_map(|p| format!("7;file://host/{p}x")),
        "[a-zA-Z][a-zA-Z0-9 ]{0,20}".prop_map(|p| format!("9;{p}")),
        (payload(), payload()).prop_map(|(t, b)| format!("777;notify;{t};{b}")),
        Just("133;A".to_owned()),
        Just("133;B".to_owned()),
        Just("133;C".to_owned()),
        (any::<i32>()).prop_map(|code| format!("133;D;{code}")),
    ];
    (body, terminator()).prop_map(|(body, end)| format!("\x1b]{body}{end}").into_bytes())
}

/// Anything at all: known sequences, unknown OSC, other escapes, raw bytes.
fn any_piece() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => known_sequence(),
        1 => payload().prop_map(|p| format!("\x1b]1337;{p}\x07").into_bytes()),
        1 => payload().prop_map(|p| format!("\x1b[1;31m{p}\x1b[0m").into_bytes()),
        2 => payload().prop_map(String::into_bytes),
        2 => proptest::collection::vec(any::<u8>(), 0..12),
    ]
}

fn stream(piece: impl Strategy<Value = Vec<u8>>) -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(piece, 0..8).prop_map(|pieces| pieces.concat())
}

proptest! {
    #[test]
    fn osc_scan_split_at_every_byte_matches_unsplit(bytes in stream(any_piece())) {
        let whole = scan_whole(&bytes);
        for at in 0..=bytes.len() {
            let (head, tail) = bytes.split_at(at);
            prop_assert_eq!(scan_chunks(&[head, tail]), whole.clone(), "split at {}", at);
        }
    }

    #[test]
    fn osc_scan_byte_by_byte_matches_unsplit(bytes in stream(any_piece())) {
        let chunks: Vec<&[u8]> = bytes.chunks(1).collect();
        prop_assert_eq!(scan_chunks(&chunks), scan_whole(&bytes));
    }

    #[test]
    fn osc_scan_random_splits_match_unsplit(
        bytes in stream(any_piece()),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..6),
    ) {
        let mut points: Vec<usize> = cuts.iter().map(|c| c.index(bytes.len() + 1)).collect();
        points.sort_unstable();
        let mut chunks = Vec::new();
        let mut from = 0;
        for to in points {
            chunks.push(&bytes[from..to]);
            from = to;
        }
        chunks.push(&bytes[from..]);
        prop_assert_eq!(scan_chunks(&chunks), scan_whole(&bytes));
    }

    #[test]
    fn osc_scan_finds_every_known_sequence(
        pieces in proptest::collection::vec(known_sequence(), 0..8),
        filler in "[a-z \u{e9}]{0,6}",
    ) {
        let joined = pieces.join(filler.as_bytes());
        prop_assert_eq!(scan_whole(&joined).len(), pieces.len());
    }
}
