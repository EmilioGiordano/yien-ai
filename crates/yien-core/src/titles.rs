//! Who set a pane's terminal title (RF-PAN-12): the program a command runs
//! or the shell at its prompt. Told in the pane actor, where OSC 133 marks
//! and OSC 0/2 titles arrive in byte order, before reports are merged.

use yien_proto::SessionTitle;
use yien_vt::{OscSignal, PromptMark, TitleChange};

/// Who set a title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TitleFrom {
    /// The pane's program: an agent profile's own, or a command the shell
    /// runs (between OSC 133 C and the next D or A).
    Program,
    /// The shell at its prompt, or a command that has ended.
    Shell,
    /// A shell that never sent a command mark (cmd): it cannot be told.
    Unknown,
}

/// The title of one launch and who owns it.
#[derive(Debug)]
pub(crate) struct Titles {
    shell: bool,
    marked: bool,
    running: bool,
    text: Option<SessionTitle>,
    from: TitleFrom,
    /// The owner of the last OSC 0/2 seen, until the terminal applies it:
    /// it holds titles inside a synchronized update (`?2026h`).
    pending: Option<TitleFrom>,
    sent: Option<(Option<SessionTitle>, TitleFrom)>,
}

impl Titles {
    /// For a launch that runs a shell, or an agent profile's own program.
    pub(crate) fn new(shell: bool) -> Self {
        let mut titles = Self {
            shell,
            marked: false,
            running: false,
            text: None,
            from: TitleFrom::Unknown,
            pending: None,
            sent: None,
        };
        titles.from = titles.now();
        titles
    }

    /// Takes one chunk's signals, in byte order, and its title change. The
    /// title and its owner, when either changed since last reported.
    pub(crate) fn absorb(
        &mut self,
        signals: &[OscSignal],
        title: Option<&TitleChange>,
    ) -> Option<(Option<SessionTitle>, TitleFrom)> {
        for signal in signals {
            match signal {
                OscSignal::Prompt(PromptMark::CommandExecuted) => {
                    self.marked = true;
                    self.running = true;
                }
                OscSignal::Prompt(PromptMark::CommandFinished { .. } | PromptMark::PromptStart) => {
                    self.marked = true;
                    if self.running {
                        self.running = false;
                        // The command's title stays on screen but is no
                        // longer anyone's topic.
                        if self.from == TitleFrom::Program {
                            self.from = TitleFrom::Shell;
                        }
                        if self.pending == Some(TitleFrom::Program) {
                            self.pending = Some(TitleFrom::Shell);
                        }
                    }
                }
                OscSignal::Prompt(PromptMark::CommandStart) => self.marked = true,
                OscSignal::Title(_) => self.pending = Some(self.now()),
                _ => {}
            }
        }
        if let Some(change) = title {
            self.text = match change {
                TitleChange::Set(t) => Some(SessionTitle::new(t.as_str())),
                TitleChange::Reset => None,
            };
            // A title popped from the stack has no OSC 0/2 of its own.
            self.from = self.pending.take().unwrap_or_else(|| self.now());
        }
        let now = (self.text.clone(), self.from);
        if self.sent.as_ref() == Some(&now) || (self.sent.is_none() && now.0.is_none()) {
            return None;
        }
        self.sent = Some(now.clone());
        Some(now)
    }

    fn now(&self) -> TitleFrom {
        if !self.shell || self.running {
            TitleFrom::Program
        } else if self.marked {
            TitleFrom::Shell
        } else {
            TitleFrom::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use yien_vt::{OscSignal, PromptMark, TitleChange};

    use super::{TitleFrom, Titles};

    fn title(t: &str) -> (Vec<OscSignal>, Option<TitleChange>) {
        (
            vec![OscSignal::Title(t.to_owned())],
            Some(TitleChange::Set(t.to_owned())),
        )
    }

    fn mark(m: PromptMark) -> OscSignal {
        OscSignal::Prompt(m)
    }

    const C: PromptMark = PromptMark::CommandExecuted;
    const D: PromptMark = PromptMark::CommandFinished { exit_code: Some(0) };

    fn owner(t: &mut Titles, signals: &[OscSignal], change: Option<&TitleChange>) -> TitleFrom {
        t.absorb(signals, change).expect("a change").1
    }

    #[test]
    fn rf_pan_12_an_agent_profile_owns_every_title() {
        let mut t = Titles::new(false);
        let (s, c) = title("✳ Fix login bug");
        assert_eq!(owner(&mut t, &s, c.as_ref()), TitleFrom::Program);
        // The same title again is not news.
        assert_eq!(t.absorb(&s, c.as_ref()), None);
    }

    #[test]
    fn rf_pan_12_a_title_inside_a_command_is_the_program_s_until_it_ends() {
        let mut t = Titles::new(true);
        let (mut s, c) = title("✳ Fix login bug");
        s.insert(0, mark(C));
        assert_eq!(owner(&mut t, &s, c.as_ref()), TitleFrom::Program);
        // The CLI exits: D and A, then a prompt theme's title.
        let (ended, _) = t
            .absorb(&[mark(D), mark(PromptMark::PromptStart)], None)
            .unwrap();
        assert_eq!(ended.unwrap().as_str(), "✳ Fix login bug");
        assert_eq!(t.sent.as_ref().unwrap().1, TitleFrom::Shell);
        let (s, c) = title("pwsh in yien");
        assert_eq!(owner(&mut t, &s, c.as_ref()), TitleFrom::Shell);
    }

    #[test]
    fn rf_pan_12_byte_order_decides_inside_one_chunk() {
        let mut t = Titles::new(true);
        // A title, then the command ends, all in one read.
        let signals = [mark(C), OscSignal::Title("✳ Fix login bug".into()), mark(D)];
        let change = TitleChange::Set("✳ Fix login bug".into());
        assert_eq!(owner(&mut t, &signals, Some(&change)), TitleFrom::Shell);
        // The prompt's title, then the next command starts.
        let signals = [OscSignal::Title("pwsh in yien".into()), mark(C)];
        let change = TitleChange::Set("pwsh in yien".into());
        assert_eq!(owner(&mut t, &signals, Some(&change)), TitleFrom::Shell);
        // The new command's CLI sets its own.
        let (s, c) = title("✳ Review the docs");
        assert_eq!(owner(&mut t, &s, c.as_ref()), TitleFrom::Program);
    }

    #[test]
    fn rf_pan_12_a_shell_without_marks_cannot_tell() {
        let mut t = Titles::new(true);
        let (s, c) = title("✳ Fix login bug");
        assert_eq!(owner(&mut t, &s, c.as_ref()), TitleFrom::Unknown);
    }

    #[test]
    fn rf_pan_12_a_popped_title_belongs_to_whoever_runs_now() {
        let mut t = Titles::new(true);
        let (s, c) = title("pwsh");
        t.absorb(&[mark(PromptMark::PromptStart)], None);
        assert_eq!(owner(&mut t, &s, c.as_ref()), TitleFrom::Shell);
        let popped = TitleChange::Set("✳ Fix login bug".into());
        assert_eq!(owner(&mut t, &[mark(C)], Some(&popped)), TitleFrom::Program);
        assert_eq!(
            t.absorb(&[], Some(&TitleChange::Reset)),
            Some((None, TitleFrom::Program))
        );
    }

    #[test]
    fn rf_pan_12_a_held_title_keeps_its_owner_until_it_is_applied() {
        let mut t = Titles::new(true);
        let (s, c) = title("pwsh in yien");
        t.absorb(&[mark(PromptMark::PromptStart)], None);
        assert_eq!(owner(&mut t, &s, c.as_ref()), TitleFrom::Shell);
        // The OSC 0 arrives but the terminal holds it: the shown title is
        // still the shell's, not re-reported as the command's.
        let held = [mark(C), OscSignal::Title("✳ Fix login bug".into())];
        assert_eq!(t.absorb(&held, None), None);
        let change = TitleChange::Set("✳ Fix login bug".into());
        assert_eq!(owner(&mut t, &[], Some(&change)), TitleFrom::Program);
    }

    /// Feeds `chunks` through the real terminal engine, one read each.
    fn through_vt(t: &mut Titles, chunks: &[&[u8]]) -> Vec<(Option<String>, TitleFrom)> {
        use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};
        let mut engine = AlacrittyEngine::new(VtConfig::default(), 40, 5);
        chunks
            .iter()
            .filter_map(|bytes| {
                let out = engine.feed(bytes);
                t.absorb(&out.signals, out.title.as_ref())
            })
            .map(|(title, from)| (title.map(|s| s.as_str().to_owned()), from))
            .collect()
    }

    #[test]
    fn rf_pan_12_a_title_held_by_a_synchronized_update_across_reads() {
        let mut t = Titles::new(true);
        let fix = Some("\u{2733} Fix login bug".to_owned());
        let sent = through_vt(
            &mut t,
            &[
                b"\x1b]133;A\x07\x1b]0;pwsh\x07",
                "\x1b]133;C\x07\x1b[?2026h\x1b]0;\u{2733} Fix login bug\x07".as_bytes(),
                b"\x1b[?2026l",
            ],
        );
        assert_eq!(
            sent,
            [
                (Some("pwsh".to_owned()), TitleFrom::Shell),
                (fix.clone(), TitleFrom::Program)
            ]
        );
        // The command ends inside the block that holds its last title.
        let sent = through_vt(
            &mut t,
            &[
                "\x1b[?2026h\x1b]0;\u{2733} Fix login bug\x07\x1b]133;D;0\x07".as_bytes(),
                b"\x1b[?2026l",
            ],
        );
        assert_eq!(sent, [(fix, TitleFrom::Shell)]);
    }
}
