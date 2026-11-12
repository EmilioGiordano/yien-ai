//! The parser's handler: alacritty's `Term`, except that erasing cells does
//! not end the selection on them.
//!
//! alacritty ends the selection in two places. An erase of a line or of the
//! screen ends it when it touches a selected line, which is every redraw of
//! a TUI. A scroll ends it when its text leaves the screen, the scrolling
//! region or the scrollback, and so do the switch to the other screen and a
//! reset. Only the handler sees which one happened, so the erases are
//! intercepted here and everything else is alacritty's own.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::vte::ansi::cursor_icon::CursorIcon;
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, PrivateMode, Rgb,
    ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
};

pub(crate) struct KeepSelection<'a, L: EventListener>(pub(crate) &'a mut Term<L>);

macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {
        $(
            #[inline]
            fn $name(&mut self $(, $arg: $ty)*) {
                Handler::$name(&mut *self.0 $(, $arg)*);
            }
        )*
    };
}

// A handler method left to its default does nothing, so a method alacritty
// adds later would be dropped without a word; this lint names it instead.
#[deny(clippy::missing_trait_methods)]
impl<L: EventListener> Handler for KeepSelection<'_, L> {
    #[inline]
    fn clear_line(&mut self, mode: LineClearMode) {
        let held = self.0.selection.take();
        Handler::clear_line(&mut *self.0, mode);
        self.0.selection = held;
    }

    /// Erasing the scrollback is left to alacritty: a selection in it has no
    /// cells left.
    #[inline]
    fn clear_screen(&mut self, mode: ClearMode) {
        if matches!(mode, ClearMode::Saved) {
            Handler::clear_screen(&mut *self.0, mode);
            return;
        }
        let held = self.0.selection.take();
        Handler::clear_screen(&mut *self.0, mode);
        self.0.selection = held;
    }

    forward! {
        set_title(title: Option<String>);
        set_cursor_style(style: Option<CursorStyle>);
        set_cursor_shape(shape: CursorShape);
        input(c: char);
        goto(line: i32, col: usize);
        goto_line(line: i32);
        goto_col(col: usize);
        insert_blank(count: usize);
        move_up(rows: usize);
        move_down(rows: usize);
        identify_terminal(intermediate: Option<char>);
        device_status(arg: usize);
        move_forward(cols: usize);
        move_backward(cols: usize);
        move_down_and_cr(rows: usize);
        move_up_and_cr(rows: usize);
        put_tab(count: u16);
        backspace();
        carriage_return();
        linefeed();
        bell();
        substitute();
        newline();
        set_horizontal_tabstop();
        scroll_up(rows: usize);
        scroll_down(rows: usize);
        insert_blank_lines(rows: usize);
        delete_lines(rows: usize);
        erase_chars(count: usize);
        delete_chars(count: usize);
        move_backward_tabs(count: u16);
        move_forward_tabs(count: u16);
        save_cursor_position();
        restore_cursor_position();
        clear_tabs(mode: TabulationClearMode);
        set_tabs(interval: u16);
        reset_state();
        reverse_index();
        terminal_attribute(attr: Attr);
        set_mode(mode: Mode);
        unset_mode(mode: Mode);
        report_mode(mode: Mode);
        set_private_mode(mode: PrivateMode);
        unset_private_mode(mode: PrivateMode);
        report_private_mode(mode: PrivateMode);
        set_scrolling_region(top: usize, bottom: Option<usize>);
        set_keypad_application_mode();
        unset_keypad_application_mode();
        set_active_charset(index: CharsetIndex);
        configure_charset(index: CharsetIndex, charset: StandardCharset);
        set_color(index: usize, color: Rgb);
        dynamic_color_sequence(prefix: String, index: usize, terminator: &str);
        reset_color(index: usize);
        clipboard_store(clipboard: u8, base64: &[u8]);
        clipboard_load(clipboard: u8, terminator: &str);
        decaln();
        push_title();
        pop_title();
        text_area_size_pixels();
        text_area_size_chars();
        set_hyperlink(hyperlink: Option<Hyperlink>);
        set_mouse_cursor_icon(icon: CursorIcon);
        report_keyboard_mode();
        push_keyboard_mode(mode: KeyboardModes);
        pop_keyboard_modes(to_pop: u16);
        set_keyboard_mode(mode: KeyboardModes, behavior: KeyboardModesApplyBehavior);
        set_modify_other_keys(mode: ModifyOtherKeys);
        report_modify_other_keys();
        set_scp(char_path: ScpCharPath, update_mode: ScpUpdateMode);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, Sender, channel};

    use alacritty_terminal::event::{Event, WindowSize};
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::color::COUNT;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;

    #[derive(Clone)]
    struct Listener(Sender<Event>);

    impl EventListener for Listener {
        fn send_event(&self, event: Event) {
            let _ = self.0.send(event);
        }
    }

    const COLS: usize = 20;
    const ROWS: usize = 6;

    struct Side {
        term: Term<Listener>,
        parser: Processor<StdSyncHandler>,
        events: Receiver<Event>,
    }

    impl Side {
        fn new() -> Self {
            let (sender, events) = channel();
            let config = Config {
                scrolling_history: 40,
                ..Config::default()
            };
            Self {
                term: Term::new(config, &TermSize::new(COLS, ROWS), Listener(sender)),
                parser: Processor::new(),
                events,
            }
        }

        fn feed(&mut self, wrapped: bool, bytes: &[u8]) {
            if wrapped {
                self.parser
                    .advance(&mut KeepSelection(&mut self.term), bytes);
            } else {
                self.parser.advance(&mut self.term, bytes);
            }
        }

        fn end_sync(&mut self, wrapped: bool) {
            if wrapped {
                self.parser.stop_sync(&mut KeepSelection(&mut self.term));
            } else {
                self.parser.stop_sync(&mut self.term);
            }
        }

        /// Everything a program or the window can see of the terminal.
        fn state(&self) -> Vec<String> {
            let term = &self.term;
            let grid = term.grid();
            let mut out = vec![
                format!("mode {:?}", term.mode()),
                format!("cursor {:?}", grid.cursor.point),
                format!("style {:?}", term.cursor_style()),
                format!("history {}", grid.history_size()),
                format!("selection {:?}", term.selection),
            ];
            for index in 0..COUNT {
                if let Some(color) = term.colors()[index] {
                    out.push(format!("color {index} {color:?}"));
                }
            }
            let top = -i32::try_from(grid.history_size()).unwrap();
            for line in top..i32::try_from(grid.screen_lines()).unwrap() {
                for col in 0..grid.columns() {
                    let cell = &grid[Point::new(Line(line), Column(col))];
                    // Not the cell's own `Debug`: a hyperlink without an
                    // id gets one from a counter the two terminals share.
                    out.push(format!(
                        "{line},{col} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
                        cell.c,
                        cell.fg,
                        cell.bg,
                        cell.flags,
                        cell.zerowidth(),
                        cell.underline_color(),
                        cell.hyperlink().map(|link| link.uri().to_owned()),
                    ));
                }
            }
            while let Ok(event) = self.events.try_recv() {
                out.push(match event {
                    Event::ColorRequest(index, format) => {
                        format!("color request {index} {}", format(Rgb { r: 1, g: 2, b: 3 }))
                    }
                    Event::TextAreaSizeRequest(format) => format(WindowSize {
                        num_lines: 6,
                        num_cols: 20,
                        cell_width: 8,
                        cell_height: 16,
                    }),
                    Event::ClipboardLoad(_, format) => format!("load {}", format("clip")),
                    other => format!("{other:?}"),
                });
            }
            out
        }
    }

    /// Rows, a scrolling region and a cursor inside it, so that a sequence
    /// that moves, scrolls or erases shows.
    const BEFORE: &[u8] = b"\x1b]2;title\x07row one\r\nrow two\ttab\r\nrow three\r\nrow four\r\n\
        row five\r\nrow six\x1b[2;5r\x1b[3;4H";

    /// Queries and text that bring out state the grid does not show: the
    /// keyboard modes, modes by report, colours, the title stack, tab stops,
    /// charsets, the saved cursor, the scrolling region.
    const AFTER: &[u8] = b"\x1b[6n\x1b[?u\x1b[?25$p\x1b[?7$p\x1b[?6$p\x1b[?1$p\x1b[?2004$p\
        \x1b[?2026$p\x1b[4$p\x1b[20$p\x1b]4;1;?\x07\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07\
        \x1b[23t\x1b[18t\x1b[14t\x1b[>4m\x1b[?4m\tx\ty\r\nab\x1b[3Gc\nzz\x1b8Q\x0eq\x0fq\
        \r\n\r\n\r\n\r\n\r\nend";

    fn sequences() -> Vec<Vec<u8>> {
        let mut all: Vec<Vec<u8>> = Vec::new();
        for byte in 0u8..0x20 {
            all.push(vec![byte]);
        }
        for end in 0x30u8..0x7f {
            all.push(vec![0x1b, end]);
            for middle in b" #%()*+-./" {
                all.push(vec![0x1b, *middle, end]);
            }
        }
        let arguments: [&[u8]; 14] = [
            b"", b"0", b"1", b"2", b"3", b"4", b"6", b"12", b"2;4", b"1;2;3", b"25", b"1049",
            b"2026", b"38;5;1",
        ];
        for prefix in [&b""[..], b"?", b">", b"<", b"="] {
            for argument in arguments {
                for middle in [&b""[..], b" ", b"$", b"!", b"\"", b"'", b"#"] {
                    for end in 0x40u8..0x7f {
                        let mut sequence = b"\x1b[".to_vec();
                        sequence.extend_from_slice(prefix);
                        sequence.extend_from_slice(argument);
                        sequence.extend_from_slice(middle);
                        sequence.push(end);
                        all.push(sequence);
                    }
                }
            }
        }
        for number in [
            0u16, 1, 2, 4, 8, 10, 11, 12, 22, 50, 52, 104, 110, 111, 112, 133,
        ] {
            for text in [
                &b""[..],
                b";?",
                b";text",
                b";1;#102030",
                b";#102030",
                b";c;aGVsbG8=",
                b";c;?",
                b";;https://x.example",
                b";1",
                b";pointer",
                b";CursorShape=1",
            ] {
                for end in [&b"\x07"[..], b"\x1b\\"] {
                    let mut sequence = format!("\x1b]{number}").into_bytes();
                    sequence.extend_from_slice(text);
                    sequence.extend_from_slice(end);
                    all.push(sequence);
                }
            }
        }
        all.push(b"\x1bP1$r\x1b\\".to_vec());
        all.push(
            "wide \u{4e2d}\u{6587} e\u{301} \u{1f600}"
                .as_bytes()
                .to_vec(),
        );
        all.push(b"a line longer than the width of this small terminal".to_vec());
        all
    }

    #[test]
    fn every_sequence_does_to_the_wrapped_terminal_what_it_does_to_alacritty() {
        for sequence in sequences() {
            let mut plain = Side::new();
            let mut wrapped = Side::new();
            for (side, is_wrapped) in [(&mut plain, false), (&mut wrapped, true)] {
                side.feed(is_wrapped, BEFORE);
                side.feed(is_wrapped, &sequence);
                side.feed(is_wrapped, AFTER);
                side.end_sync(is_wrapped);
            }
            assert_eq!(
                plain.state(),
                wrapped.state(),
                "{}",
                sequence.escape_ascii()
            );
        }
    }

    #[test]
    fn the_sequences_reach_the_handlers_they_are_meant_to_check() {
        let mut side = Side::new();
        side.feed(true, BEFORE);
        let before = side.state();
        let changed = sequences()
            .into_iter()
            .filter(|sequence| {
                let mut side = Side::new();
                side.feed(true, BEFORE);
                side.feed(true, sequence);
                side.end_sync(true);
                side.state() != before
            })
            .count();
        assert!(changed > 300, "only {changed} sequences changed anything");
    }
}
