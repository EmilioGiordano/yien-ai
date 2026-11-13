//! The working tab rows' spinner: one timer for the whole window, running
//! only while a drawn tab row works and the window shows.

use gpui_kit::*;

use super::Shell;
use crate::assets::{self, SPINNER_FRAME};
use crate::sidebars::Bar;

/// The frame every working row shows, and whether a timer turns it.
#[derive(Debug, Default)]
pub(super) struct Spinner {
    frame: usize,
    ticking: bool,
}

impl Spinner {
    pub(super) fn frame(&self) -> usize {
        self.frame
    }

    /// Whether a timer must start now: it should turn and none does.
    fn start(&mut self, turns: bool) -> bool {
        let start = turns && !self.ticking;
        self.ticking |= start;
        start
    }

    /// One tick of the timer; whether it goes on. A stopped spinner keeps
    /// its frame, so reduced motion shows a still glyph.
    fn tick(&mut self, turns: bool) -> bool {
        if turns {
            self.frame = assets::next_frame(self.frame);
        } else {
            self.ticking = false;
        }
        turns
    }
}

/// Whether the spinner turns: a working row is drawn, the tab bar and the
/// window show, and the system does not ask for reduced motion.
fn turns(working_row: bool, bar_shown: bool, window_visible: bool, reduce_motion: bool) -> bool {
    working_row && bar_shown && window_visible && !reduce_motion
}

impl Shell {
    fn spinner_turns(&self, window: &Window, cx: &App) -> bool {
        turns(
            self.workspace.any_drawn_tab_working(),
            self.bar_shown(Bar::Tab),
            window.is_visible(),
            cx.reduce_motion(),
        )
    }

    /// Starts the spinner's timer when a working row needs it; called after
    /// anything that may make one appear.
    pub(super) fn keep_spinning(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let turns = self.spinner_turns(window, cx);
        if !self.spinner.start(turns) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(SPINNER_FRAME).await;
                let going = this.update_in(cx, |shell, window, cx| {
                    let turns = shell.spinner_turns(window, cx);
                    let going = shell.spinner.tick(turns);
                    if going {
                        cx.notify();
                    }
                    going
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{Spinner, turns};
    use crate::assets::SPINNER;

    #[test]
    fn rf_atn_02_the_spinner_turns_only_for_a_shown_working_row() {
        assert!(turns(true, true, true, false));
        assert!(!turns(false, true, true, false), "nothing works");
        assert!(!turns(true, false, true, false), "the tab bar is hidden");
        assert!(!turns(true, true, false, false), "the window is hidden");
        assert!(!turns(true, true, true, true), "reduced motion");
    }

    #[test]
    fn rf_atn_02_one_timer_runs_while_something_works_and_stops_after() {
        let mut spinner = Spinner::default();
        assert!(!spinner.start(false), "no timer while nothing works");
        assert!(spinner.start(true), "the first working row starts it");
        assert!(!spinner.start(true), "a second row shares it");
        for want in 1..=SPINNER.len() {
            assert!(spinner.tick(true));
            assert_eq!(spinner.frame(), want % SPINNER.len());
        }
        assert!(spinner.tick(true));
        assert!(!spinner.tick(false), "the work ended: the timer stops");
        assert_eq!(spinner.frame(), 1, "a stopped spinner keeps its frame");
        assert!(spinner.start(true), "new work starts a new timer");
        assert!(!spinner.start(true));
    }
}
