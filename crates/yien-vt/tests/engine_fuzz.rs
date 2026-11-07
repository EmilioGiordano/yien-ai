//! `AlacrittyEngine` never panics on arbitrary PTY bytes mixed with any
//! sequence of caller operations.

use proptest::prelude::*;
use yien_vt::{AlacrittyEngine, SelectionKind, Side, ViewportPoint, VtConfig, VtEngine};

#[derive(Debug, Clone)]
enum Op {
    Feed(Vec<u8>),
    Resize(u16, u16),
    Scroll(i32),
    ScrollToBottom,
    Select(u16, u16, u16, u16, u8),
    LinkAt(u16, u16),
    Search(String),
    FlushSync,
    Frame,
}

/// Bytes biased toward escape sequences, so the parser's odd states get hit.
fn pty_bytes() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        3 => proptest::collection::vec(any::<u8>(), 0..32),
        1 => Just(b"\x1b[?2026h".to_vec()),
        1 => Just(b"\x1b[?2026l".to_vec()),
        1 => Just(b"\x1b[?1049h".to_vec()),
        1 => Just(b"\x1b[?1049l".to_vec()),
        1 => Just(b"\x1b[6n\x1b[c\x1b]11;?\x07".to_vec()),
        1 => Just("\u{4e2d}\u{1f600}e\u{301}\r\n".as_bytes().to_vec()),
        1 => Just(b"\x1b]8;;https://x.example\x1b\\a\x1b]8;;\x1b\\ https://y.example C:\\a\\b".to_vec()),
        1 => (0u16..400, 0u16..400).prop_map(|(r, c)| format!("\x1b[{r};{c}H").into_bytes()),
    ];
    proptest::collection::vec(piece, 1..6).prop_map(|p| p.concat())
}

fn op() -> impl Strategy<Value = Op> {
    let coord = || 0u16..400;
    prop_oneof![
        6 => pty_bytes().prop_map(Op::Feed),
        2 => (2u16..300, 1u16..200).prop_map(|(c, r)| Op::Resize(c, r)),
        2 => any::<i32>().prop_map(Op::Scroll),
        1 => Just(Op::ScrollToBottom),
        2 => (coord(), coord(), coord(), coord(), 0u8..4)
            .prop_map(|(a, b, c, d, k)| Op::Select(a, b, c, d, k)),
        2 => (coord(), coord()).prop_map(|(r, c)| Op::LinkAt(r, c)),
        1 => "[a-z.(\\[*\u{4e2d}]{0,6}".prop_map(Op::Search),
        1 => Just(Op::FlushSync),
        2 => Just(Op::Frame),
    ]
}

fn apply(engine: &mut AlacrittyEngine, op: Op) {
    match op {
        Op::Feed(bytes) => {
            engine.feed(&bytes);
        }
        Op::Resize(cols, rows) => engine.resize(cols, rows),
        Op::Scroll(delta) => engine.scroll(delta),
        Op::ScrollToBottom => engine.scroll_to_bottom(),
        Op::Select(r0, c0, r1, c1, kind) => {
            let kind = match kind {
                0 => SelectionKind::Simple,
                1 => SelectionKind::Block,
                2 => SelectionKind::Semantic,
                _ => SelectionKind::Lines,
            };
            engine.start_selection(ViewportPoint::new(r0, c0), Side::Left, kind);
            engine.update_selection(ViewportPoint::new(r1, c1), Side::Right);
            let _ = engine.selection_text();
        }
        Op::LinkAt(row, col) => {
            let _ = engine.link_at(ViewportPoint::new(row, col));
        }
        Op::Search(query) => {
            if let Ok(matches) = engine.search(&query)
                && let Some(first) = matches.first()
            {
                engine.scroll_to(first.start);
            }
        }
        Op::FlushSync => {
            engine.flush_sync();
        }
        Op::Frame => {
            let frame = engine.frame();
            assert_eq!(frame.lines.len(), usize::from(frame.size.rows));
            assert!(
                frame
                    .lines
                    .iter()
                    .all(|line| line.len() == usize::from(frame.size.cols))
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn engine_survives_random_bytes_and_operations(
        cols in 2u16..300,
        rows in 1u16..200,
        ops in proptest::collection::vec(op(), 1..24),
    ) {
        let config = VtConfig { scrollback_lines: 200, ..VtConfig::default() };
        let mut engine = AlacrittyEngine::new(config, cols, rows);
        for op in ops {
            apply(&mut engine, op);
        }
        let _ = engine.frame();
    }
}
