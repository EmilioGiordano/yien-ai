//! `AlacrittyEngine` behaviour through the public `VtEngine` API.

use yien_vt::{
    AlacrittyEngine, CellFlags, CursorShape, DEFAULT_SCROLLBACK, Frame, GridPoint, LinkKind,
    MouseEncoding, MouseTracking, OscSignal, Palette, PromptMark, Rgb, SelectionKind, Side, Size,
    TitleChange, ViewportPoint, VtConfig, VtEngine,
};

fn engine(cols: u16, rows: u16) -> AlacrittyEngine {
    AlacrittyEngine::new(VtConfig::default(), cols, rows)
}

fn numbered_lines(engine: &mut AlacrittyEngine, count: usize) {
    for i in 1..=count {
        engine.feed(format!("line {i}\r\n").as_bytes());
    }
}

fn vp(row: u16, col: u16) -> ViewportPoint {
    ViewportPoint::new(row, col)
}

/// The frame as text with the cursor drawn as `█`, for golden comparisons.
fn render(frame: &Frame) -> String {
    let mut rows = Vec::new();
    for (r, line) in frame.lines.iter().enumerate() {
        let mut text = String::new();
        for (c, cell) in line.iter().enumerate() {
            if cell.flags.contains(CellFlags::SPACER) {
                continue;
            }
            let at_cursor = frame.cursor.is_some_and(|cur| {
                usize::from(cur.point.row) == r && usize::from(cur.point.col) == c
            });
            if at_cursor {
                text.push('█');
            } else {
                text.push(cell.ch);
                text.extend(&cell.zerowidth);
            }
        }
        rows.push(format!("|{}|", text.trim_end()));
    }
    rows.join("\n")
}

#[test]
fn engine_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<AlacrittyEngine>();
}

#[test]
fn rf_age_02_dsr_gets_cursor_position_reply() {
    let mut e = engine(20, 5);
    assert_eq!(e.feed(b"\x1b[6n").replies, b"\x1b[1;1R");
    assert_eq!(e.feed(b"\x1b[3;5H\x1b[6n").replies, b"\x1b[3;5R");
}

#[test]
fn rf_age_02_replies_keep_request_order() {
    let mut e = engine(20, 5);
    let out = e.feed(b"\x1b]11;?\x07\x1b[c");
    assert_eq!(out.replies, b"\x1b]11;rgb:0d0d/0c0c/0b0b\x07\x1b[?6c");
}

#[test]
fn rf_age_02_bytes_reach_the_screen_unaltered() {
    let mut e = engine(20, 5);
    e.feed(b"hello\r\nworld \x1b[1mbold\x1b[0m");
    let frame = e.frame();
    assert_eq!(frame.text(), "hello\nworld bold");
    assert!(frame.lines[1][6].flags.contains(CellFlags::BOLD));
    assert!(!frame.lines[1][0].flags.contains(CellFlags::BOLD));
}

#[test]
fn rf_age_02_sequences_and_utf8_cut_between_feeds() {
    let mut e = engine(20, 5);
    let text = "\x1b[3;5H\u{e1}\u{4e2d}".as_bytes();
    for byte in text {
        e.feed(std::slice::from_ref(byte));
    }
    assert_eq!(e.frame().line_text(2), "    \u{e1}\u{4e2d}");
}

#[test]
fn rf_pan_06_alt_screen_restores_the_shell_screen() {
    let mut e = engine(20, 4);
    e.feed(b"shell prompt");
    e.feed(b"\x1b[?1049h\x1b[Hfull screen app");
    assert!(e.modes().alt_screen);
    assert_eq!(e.frame().line_text(0), "full screen app");
    e.feed(b"\x1b[?1049l");
    assert!(!e.modes().alt_screen);
    assert_eq!(e.frame().line_text(0), "shell prompt");
}

#[test]
fn rf_pan_06_resize_reflow_keeps_a_long_line() {
    let mut e = engine(20, 5);
    let long: String = ('a'..='z').chain('A'..='D').collect();
    e.feed(long.as_bytes());
    assert_eq!(e.frame().line_text(0), &long[..20]);
    assert_eq!(e.frame().line_text(1), &long[20..]);

    e.resize(40, 5);
    assert_eq!(e.size(), Size { cols: 40, rows: 5 });
    assert_eq!(e.frame().line_text(0), long);

    // Shrinking may push the first rows into the history, so read from the top.
    e.resize(10, 5);
    e.scroll(i32::MAX);
    let expected = format!("{}\n{}\n{}", &long[..10], &long[10..20], &long[20..]);
    assert!(e.frame().text().starts_with(&expected));
}

#[test]
fn rf_pan_06_resize_below_minimum_is_clamped() {
    let mut e = engine(20, 5);
    e.resize(0, 0);
    assert_eq!(e.size(), Size { cols: 2, rows: 1 });
    e.feed(b"abc\r\n\x1b[6n");
    let frame = e.frame();
    assert_eq!(frame.lines.len(), 1);
    assert_eq!(frame.lines[0].len(), 2);
}

#[test]
fn rf_pan_07_default_scrollback_is_ten_thousand() {
    assert_eq!(VtConfig::default().scrollback_lines, 10_000);
    assert_eq!(DEFAULT_SCROLLBACK, 10_000);
}

#[test]
fn rf_pan_07_scrollback_is_capped_at_the_configured_limit() {
    let config = VtConfig {
        scrollback_lines: 10,
        ..VtConfig::default()
    };
    let mut e = AlacrittyEngine::new(config, 20, 3);
    numbered_lines(&mut e, 50);
    assert_eq!(e.frame().history_len, 10);
    e.scroll(1_000);
    let frame = e.frame();
    assert_eq!(frame.display_offset, 10);
    assert_eq!(frame.line_text(0), "line 39");
}

#[test]
fn rf_pan_07_scroll_moves_the_viewport_and_display_offset() {
    let mut e = engine(20, 3);
    numbered_lines(&mut e, 10);
    let live = e.frame();
    assert_eq!(live.display_offset, 0);
    assert_eq!(live.line_text(0), "line 9");
    assert!(live.cursor.is_some());

    e.scroll(3);
    let back = e.frame();
    assert_eq!(back.display_offset, 3);
    assert_eq!(back.line_text(0), "line 6");
    assert_eq!(back.cursor, None, "the cursor row is out of view");

    e.scroll(i32::MAX);
    assert_eq!(e.frame().display_offset, e.frame().history_len);
    e.scroll(i32::MIN);
    assert_eq!(e.frame().display_offset, 0);

    e.scroll(2);
    e.scroll_to_bottom();
    assert_eq!(e.frame(), live);
}

#[test]
fn rf_pan_08_search_finds_text_in_scrollback_and_screen() {
    let mut e = engine(20, 3);
    numbered_lines(&mut e, 30);
    let matches = e.search("line 2").unwrap();
    // line 2 and line 20 to line 29.
    assert_eq!(matches.len(), 11);
    assert!(matches.windows(2).all(|w| w[0].start < w[1].start));
    let first = matches[0];
    assert!(first.start.line < 0, "line 2 is in the scrollback");
    assert_eq!(first.start.col, 0);
    assert_eq!(first.end, GridPoint::new(first.start.line, 5));
}

#[test]
fn rf_pan_08_search_is_literal_and_smart_case() {
    let mut e = engine(30, 3);
    e.feed(b"Alpha a.b axb\r\nalpha");
    assert_eq!(e.search("a.b").unwrap().len(), 1);
    assert_eq!(e.search("alpha").unwrap().len(), 2);
    assert_eq!(e.search("Alpha").unwrap().len(), 1);
    assert!(e.search("").unwrap().is_empty());
    assert!(e.search("missing").unwrap().is_empty());
    assert_eq!(e.search("(").unwrap().len(), 0);
}

#[test]
fn rf_pan_08_scroll_to_brings_a_match_into_view() {
    let mut e = engine(20, 3);
    numbered_lines(&mut e, 30);
    let target = e.search("line 4").unwrap()[0];
    e.scroll_to(target.start);
    let frame = e.frame();
    let at = target
        .start
        .to_viewport(frame.display_offset, frame.size.rows);
    let at = at.expect("the match is in view");
    assert_eq!(frame.line_text(usize::from(at.row)), "line 4");
}

#[test]
fn rf_pan_09_selection_yields_text_across_lines() {
    let mut e = engine(20, 5);
    e.feed(b"word stuff\r\nnext");
    e.start_selection(vp(0, 5), Side::Left, SelectionKind::Simple);
    e.update_selection(vp(1, 3), Side::Right);
    assert_eq!(e.selection_text().as_deref(), Some("stuff\nnext"));

    let frame = e.frame();
    assert!(!frame.lines[0][4].flags.contains(CellFlags::SELECTED));
    assert!(frame.lines[0][5].flags.contains(CellFlags::SELECTED));
    assert!(frame.lines[1][3].flags.contains(CellFlags::SELECTED));
    assert!(!frame.lines[1][4].flags.contains(CellFlags::SELECTED));

    e.clear_selection();
    assert_eq!(e.selection_text(), None);
    assert!(!e.frame().lines[0][5].flags.contains(CellFlags::SELECTED));
}

#[test]
fn rf_pan_09_selection_follows_the_scrollback() {
    let mut e = engine(20, 3);
    numbered_lines(&mut e, 10);
    e.scroll(3);
    e.start_selection(vp(0, 0), Side::Left, SelectionKind::Simple);
    e.update_selection(vp(0, 5), Side::Right);
    assert_eq!(e.selection_text().as_deref(), Some("line 6"));
}

#[test]
fn rf_pan_09_semantic_and_line_selection() {
    let mut e = engine(30, 3);
    e.feed(b"one two three");
    e.start_selection(vp(0, 5), Side::Left, SelectionKind::Semantic);
    assert_eq!(e.selection_text().as_deref(), Some("two"));
    e.start_selection(vp(0, 5), Side::Left, SelectionKind::Lines);
    assert_eq!(e.selection_text().as_deref(), Some("one two three\n"));
}

#[test]
fn rf_pan_09_out_of_range_points_are_clamped() {
    let mut e = engine(10, 3);
    e.feed(b"abc");
    e.start_selection(vp(500, 500), Side::Left, SelectionKind::Simple);
    e.update_selection(vp(0, 0), Side::Left);
    assert!(e.selection_text().is_some());
    assert_eq!(e.link_at(vp(u16::MAX, u16::MAX)), None);
    e.scroll_to(GridPoint::new(i32::MIN, u16::MAX));
}

#[test]
fn rf_pan_09_block_selection_from_the_right_edge_does_not_panic() {
    let mut e = engine(2, 5);
    e.feed(b"ab\r\ncd");
    e.start_selection(vp(4, 1), Side::Left, SelectionKind::Block);
    e.update_selection(vp(0, 1), Side::Right);
    assert_eq!(e.selection_text(), None);
}

#[test]
fn rf_pan_09_block_selection_yields_a_rectangle() {
    let mut e = engine(10, 3);
    e.feed(b"abcdef\r\nghijkl\r\nmnopqr");
    e.start_selection(vp(0, 1), Side::Left, SelectionKind::Block);
    e.update_selection(vp(2, 3), Side::Right);
    assert_eq!(e.selection_text().as_deref(), Some("bcd\nhij\nnop"));
}

#[test]
fn rf_pan_09_bracketed_paste_mode_is_reported() {
    let mut e = engine(10, 3);
    assert!(!e.modes().bracketed_paste);
    e.feed(b"\x1b[?2004h");
    assert!(e.modes().bracketed_paste);
}

#[test]
fn rf_pan_10_wide_chars_own_two_cells() {
    let mut e = engine(10, 3);
    e.feed("\u{4e2d}\u{6587}ab".as_bytes());
    let frame = e.frame();
    assert_eq!(frame.line_text(0), "\u{4e2d}\u{6587}ab");
    let row = &frame.lines[0];
    assert!(row[0].flags.contains(CellFlags::WIDE));
    assert!(row[1].flags.contains(CellFlags::SPACER));
    assert!(row[2].flags.contains(CellFlags::WIDE));
    assert_eq!(row[4].ch, 'a');
    assert!(frame.lines.iter().all(|line| line.len() == 10));
}

#[test]
fn rf_pan_10_wide_char_at_the_edge_wraps_with_a_spacer() {
    let mut e = engine(5, 3);
    e.feed("abcd\u{d55c}".as_bytes());
    let frame = e.frame();
    assert!(frame.lines[0][4].flags.contains(CellFlags::SPACER));
    assert_eq!(frame.line_text(0), "abcd");
    assert_eq!(frame.line_text(1), "\u{d55c}");
}

#[test]
fn rf_pan_10_combining_marks_and_emoji_survive() {
    let mut e = engine(10, 3);
    e.feed("e\u{301} \u{1f600} \u{2764}\u{fe0f}".as_bytes());
    let text = e.frame().line_text(0);
    assert_eq!(text, "e\u{301} \u{1f600} \u{2764}\u{fe0f}");
}

#[test]
fn rf_pan_11_url_is_detected_without_trailing_punctuation() {
    let mut e = engine(60, 3);
    e.feed(b"see https://example.com/a_(b). done");
    let link = e.link_at(vp(0, 10)).expect("a link");
    assert_eq!(link.kind, LinkKind::Url);
    assert_eq!(link.target, "https://example.com/a_(b)");
    assert_eq!(link.start, GridPoint::new(0, 4));
    assert_eq!(link.end, GridPoint::new(0, 28));
    assert_eq!(e.link_at(vp(0, 29)), None);
    assert_eq!(e.link_at(vp(0, 1)), None);
}

#[test]
fn rf_pan_11_wrapped_url_is_found_from_either_row() {
    let mut e = engine(20, 4);
    let url = "https://example.com/long/path/x";
    e.feed(url.as_bytes());
    assert_eq!(e.link_at(vp(0, 2)).map(|l| l.target).as_deref(), Some(url));
    assert_eq!(e.link_at(vp(1, 2)).map(|l| l.target).as_deref(), Some(url));
}

#[test]
fn rf_pan_11_windows_path_is_detected() {
    let mut e = engine(60, 3);
    e.feed(b"error at C:\\src\\main.rs:12:5");
    let link = e.link_at(vp(0, 12)).expect("a path");
    assert_eq!(link.kind, LinkKind::Path);
    assert_eq!(link.target, "C:\\src\\main.rs");
}

#[test]
fn rf_pan_11_osc8_hyperlink_wins() {
    let mut e = engine(40, 3);
    e.feed(b"\x1b]8;;https://x.example\x1b\\link\x1b]8;;\x1b\\ rest");
    let link = e.link_at(vp(0, 2)).expect("a hyperlink");
    assert_eq!(link.kind, LinkKind::Hyperlink);
    assert_eq!(link.target, "https://x.example");
    assert_eq!(link.start, GridPoint::new(0, 0));
    assert_eq!(link.end, GridPoint::new(0, 3));
    assert_eq!(e.link_at(vp(0, 6)), None);
}

#[test]
fn rf_pan_14_truecolor_and_indexed_colors_are_resolved() {
    let palette = Palette::default();
    let mut e = engine(10, 3);
    e.feed(b"\x1b[38;2;1;2;3mA\x1b[0m\x1b[48;5;196mB\x1b[0m\x1b[31mC\x1b[7mD");
    let row = &e.frame().lines[0];
    assert_eq!(row[0].fg, Rgb::new(1, 2, 3));
    assert_eq!(row[0].bg, palette.background);
    assert_eq!(row[1].bg, Rgb::new(255, 0, 0));
    assert_eq!(row[2].fg, palette.ansi[1]);
    assert_eq!(row[3].fg, palette.background, "inverse swaps");
    assert_eq!(row[3].bg, palette.ansi[1]);
}

#[test]
fn rf_pan_14_program_color_overrides_win_over_the_palette() {
    let mut e = engine(10, 3);
    e.feed(b"\x1b]4;1;rgb:10/20/30\x07\x1b[31mX");
    assert_eq!(e.frame().lines[0][0].fg, Rgb::new(0x10, 0x20, 0x30));
}

#[test]
fn rf_pan_14_faint_and_hidden_are_resolved() {
    let palette = Palette::default();
    let mut e = engine(10, 3);
    e.feed(b"\x1b[2mA\x1b[0m\x1b[8mB");
    let row = &e.frame().lines[0];
    assert_eq!(row[0].fg, palette.foreground.dim());
    assert_eq!(row[1].fg, row[1].bg);
    assert!(row[1].flags.contains(CellFlags::HIDDEN));
}

#[test]
fn rf_pan_14_mouse_modes_are_reported() {
    let mut e = engine(10, 3);
    assert_eq!(e.modes().mouse, MouseTracking::Off);
    e.feed(b"\x1b[?1000h");
    assert_eq!(e.modes().mouse, MouseTracking::Click);
    e.feed(b"\x1b[?1002h\x1b[?1006h");
    let modes = e.modes();
    assert_eq!(modes.mouse, MouseTracking::Drag);
    assert_eq!(modes.mouse_encoding, MouseEncoding::Sgr);
    e.feed(b"\x1b[?1003h\x1b[?1h\x1b[?1004h");
    let modes = e.modes();
    assert_eq!(modes.mouse, MouseTracking::Motion);
    assert!(modes.app_cursor);
    assert!(modes.focus_reporting);
}

#[test]
fn feed_reports_signals_title_and_bell() {
    let mut e = engine(20, 3);
    let out = e.feed(b"\x1b]0;agent\x07\x1b]133;D;2\x07\x07");
    assert_eq!(out.title, Some(TitleChange::Set("agent".into())));
    assert!(out.bell);
    assert_eq!(
        out.signals,
        vec![
            OscSignal::Title("agent".into()),
            OscSignal::Prompt(PromptMark::CommandFinished { exit_code: Some(2) }),
        ]
    );
    assert_eq!(e.feed(b"plain").title, None);
}

#[test]
fn feed_reports_clipboard_writes_only() {
    let mut e = engine(20, 3);
    let out = e.feed(b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(out.clipboard.as_deref(), Some("hello"));
    let read = e.feed(b"\x1b]52;c;?\x07");
    assert!(read.replies.is_empty(), "clipboard reads stay disabled");
}

#[test]
fn synchronized_update_shows_when_closed() {
    let mut e = engine(20, 3);
    e.feed(b"\x1b[?2026hheld");
    assert!(e.sync_pending());
    assert_eq!(e.frame().line_text(0), "");
    e.feed(b"\x1b[?2026l");
    assert!(!e.sync_pending());
    assert_eq!(e.frame().line_text(0), "held");
}

#[test]
fn rf_age_02_unclosed_synchronized_update_is_flushed_with_its_replies() {
    let mut e = engine(20, 3);
    let out = e.feed(b"\x1b[?2026hlate\x1b[6n");
    assert!(e.sync_pending());
    assert!(out.replies.is_empty());
    let flushed = e.flush_sync();
    assert!(!e.sync_pending());
    assert_eq!(flushed.replies, b"\x1b[1;5R");
    assert_eq!(e.frame().line_text(0), "late");
    assert_eq!(e.flush_sync(), Default::default());
}

#[test]
fn cursor_shape_and_visibility_follow_the_program() {
    let mut e = engine(10, 3);
    let cursor = e.frame().cursor.expect("visible");
    assert_eq!(cursor.shape, CursorShape::Block);
    e.feed(b"ab\x1b[6 q");
    let cursor = e.frame().cursor.expect("visible");
    assert_eq!(cursor.shape, CursorShape::Beam);
    assert_eq!(cursor.point, vp(0, 2));
    e.feed(b"\x1b[?25l");
    assert_eq!(e.frame().cursor, None);
}

#[test]
fn frame_matches_golden_after_a_small_replay() {
    let mut e = engine(12, 4);
    e.feed(b"$ ls\r\n\x1b[34msrc\x1b[0m  \xe4\xb8\xad.txt\r\n$ ");
    let expected = "\
|$ ls|
|src  \u{4e2d}.txt|
|$ \u{2588}|
||";
    assert_eq!(render(&e.frame()), expected);
}
