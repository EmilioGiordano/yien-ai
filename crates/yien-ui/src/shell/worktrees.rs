//! The window's worktree overlays (RF-ESP-05, RF-ESP-07): the prompt for a
//! new worktree tab's branch and the question asked before closing the last
//! tab on a worktree. What they say and do is decided in `crate::worktree`.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use yien_proto::{Command, PaneId, ProjectId, TabId, WorktreeCheck};

use super::{CONFIRM_WIDTH, PALETTE_TOP, Shell};
use crate::actions::*;
use crate::catalog::{CATALOG, Scope};
use crate::theme::Theme;
use crate::worktree::{Answer, Question, Step, parse_prompt, prompt_hint};

/// The prompt for a new worktree tab, while open.
pub(super) struct Prompt {
    project: ProjectId,
    project_name: String,
    hint: String,
    error: Option<String>,
    input: Entity<InputState>,
    _input_events: Subscription,
}

/// The question before closing a worktree tab, while open.
pub(super) struct Closing {
    question: Question,
    focus: FocusHandle,
}

impl Shell {
    /// Opens the prompt for a new worktree tab in the selected tab's project.
    pub(super) fn new_worktree_tab(
        &mut self,
        _: &NewWorktreeTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.workspace.project_for_new_tab() else {
            self.notice = Some("There is no project to open a worktree tab in.".to_owned());
            cx.notify();
            return;
        };
        let project_name = self
            .workspace
            .project(project)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let hint = prompt_hint(self.workspace.main_branch_of(project));
        self.split_menu = None;
        self.confirm = None;
        self.worktree_close = None;
        self.finish_rename(true, window, cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("feat/new-branch [base]"));
        let input_events = cx.subscribe_in(&input, window, Self::on_worktree_prompt_input);
        window.focus(&input.focus_handle(cx), cx);
        self.worktree_prompt = Some(Prompt {
            project,
            project_name,
            hint,
            error: None,
            input,
            _input_events: input_events,
        });
        cx.notify();
    }

    fn on_worktree_prompt_input(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let (InputEvent::Change, Some(prompt)) = (event, self.worktree_prompt.as_mut())
            && prompt.error.take().is_some()
        {
            cx.notify();
        }
    }

    pub(super) fn worktree_prompt_confirm(
        &mut self,
        _: &WorktreePromptConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.worktree_prompt.as_mut() else {
            return;
        };
        let typed = prompt.input.read(cx).value();
        match parse_prompt(&typed) {
            Err(error) => {
                prompt.error = Some(error.to_owned());
                cx.notify();
            }
            Ok((branch, base)) => {
                let project = prompt.project;
                self.worktree_prompt = None;
                self.notice = Some(format!("Creating a worktree for {branch}…"));
                self.command(
                    Command::NewWorktreeTab {
                        project,
                        branch,
                        base,
                    },
                    cx,
                );
                self.give_back_keyboard(window, cx);
            }
        }
    }

    pub(super) fn worktree_prompt_cancel(
        &mut self,
        _: &WorktreePromptCancel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.worktree_prompt.take().is_some() {
            self.give_back_keyboard(window, cx);
        }
    }

    /// Asks before closing `pane` when that closes the last tab on a
    /// worktree, and has the core check the worktree meanwhile. `false` when
    /// there is nothing to ask.
    pub(super) fn ask_before_closing(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(question) = self.workspace.worktree_question(pane) else {
            return false;
        };
        let tab = question.tab;
        self.split_menu = None;
        self.finish_rename(true, window, cx);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.worktree_close = Some(Closing { question, focus });
        self.command(Command::CheckWorktree { tab }, cx);
        cx.notify();
        true
    }

    /// The core's answer to the check, for the open question. An answer
    /// that differs from what a second step showed takes the question back
    /// to its first step.
    pub(super) fn worktree_checked(&mut self, tab: TabId, check: WorktreeCheck) {
        if let Some(closing) = self
            .worktree_close
            .as_mut()
            .filter(|c| c.question.tab == tab)
        {
            closing.question.set_check(check);
        }
    }

    /// A question about a tab that is gone no longer applies.
    pub(super) fn drop_stale_worktree_question(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gone = self
            .worktree_close
            .as_ref()
            .is_some_and(|c| self.workspace.tab(c.question.tab).is_none());
        let project_gone = self
            .worktree_prompt
            .as_ref()
            .is_some_and(|p| self.workspace.project(p.project).is_none());
        if project_gone {
            self.worktree_prompt = None;
        }
        if gone {
            self.worktree_close = None;
        }
        if gone || project_gone {
            self.give_back_keyboard(window, cx);
        }
    }

    pub(super) fn worktree_keep(
        &mut self,
        _: &WorktreeKeep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(closing) = self.worktree_close.take() {
            self.command(closing.question.keep(), cx);
            self.give_back_keyboard(window, cx);
        }
    }

    pub(super) fn worktree_remove(
        &mut self,
        _: &WorktreeRemove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(closing) = self.worktree_close.as_mut() else {
            return;
        };
        if closing.question.step == Step::ConfirmRemove {
            return;
        }
        match closing.question.remove() {
            Answer::Wait => {}
            Answer::ConfirmFirst => cx.notify(),
            Answer::Run(command) => {
                self.worktree_close = None;
                self.command(command, cx);
                self.give_back_keyboard(window, cx);
            }
        }
    }

    pub(super) fn worktree_remove_anyway(
        &mut self,
        _: &WorktreeRemoveAnyway,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let confirming = self
            .worktree_close
            .as_ref()
            .is_some_and(|c| c.question.step == Step::ConfirmRemove);
        if !confirming {
            return;
        }
        let Some(closing) = self.worktree_close.as_mut() else {
            return;
        };
        if let Answer::Run(command) = closing.question.remove() {
            self.worktree_close = None;
            self.command(command, cx);
            self.give_back_keyboard(window, cx);
        }
    }

    pub(super) fn worktree_cancel(
        &mut self,
        _: &WorktreeCancel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.worktree_close.take().is_some() {
            self.give_back_keyboard(window, cx);
        }
    }

    /// Any window command closes an open worktree overlay and goes no
    /// further, so a key cannot act on the workspace hidden behind it.
    fn dismiss_on_window_commands(
        &self,
        dialog: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        CATALOG
            .iter()
            .filter(|spec| spec.scope == Scope::Window)
            .fold(dialog, |dialog, spec| {
                dialog.on_boxed_action(
                    &*(spec.action)(),
                    cx.listener(|this, _: &dyn Action, window, cx| {
                        this.worktree_prompt = None;
                        this.worktree_close = None;
                        this.give_back_keyboard(window, cx);
                    }),
                )
            })
    }

    /// The prompt over the window, like the palette: a branch, then
    /// optionally a base. Enter creates the tab, Escape cancels.
    pub(super) fn render_worktree_prompt(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let prompt = self.worktree_prompt.as_ref()?;
        let t = self.theme;
        let title = format!("New worktree tab in {}", prompt.project_name);
        let dialog = dialog_frame("worktree-prompt", &title, &t)
            .key_context(WORKTREE_PROMPT_CONTEXT)
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.worktree_prompt_cancel(&WorktreePromptCancel, window, cx);
            }))
            .map(|dialog| self.dismiss_on_window_commands(dialog, cx))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(t.ai)
                    .bg(t.bg)
                    .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                    .child(
                        Input::new(&prompt.input)
                            .appearance(false)
                            .aria_label("Branch name, then optionally a base"),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(t.muted)
                    .child(prompt.hint.clone()),
            )
            .when_some(prompt.error.clone(), |d, error| {
                d.child(div().text_size(px(12.5)).text_color(t.ochre).child(error))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.0))
                    .pt(px(4.0))
                    .text_size(px(13.0))
                    .child(
                        self.dialog_button("worktree-prompt-cancel", "Cancel", "Esc")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.worktree_prompt_cancel(&WorktreePromptCancel, window, cx);
                            })),
                    )
                    .child(
                        self.dialog_button("worktree-prompt-create", "Create", "Enter")
                            .font_weight(FontWeight::MEDIUM)
                            .border_color(t.muted)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.worktree_prompt_confirm(&WorktreePromptConfirm, window, cx);
                            })),
                    ),
            );
        Some(scrim(dialog))
    }

    /// The question before closing the last tab on a worktree. It is not
    /// red: nobody is waiting (RN-01).
    pub(super) fn render_worktree_close(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let closing = self.worktree_close.as_ref()?;
        let q = &closing.question;
        let t = self.theme;
        let title = q.title();
        let buttons = match q.step {
            Step::Choose => h_flex()
                .child(
                    self.dialog_button("worktree-cancel", "Cancel", "Esc")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.worktree_cancel(&WorktreeCancel, window, cx);
                        })),
                )
                .child(
                    self.dialog_button("worktree-remove", "Remove worktree", "Alt+R")
                        .aria_label(q.remove_name())
                        .when(!q.checked(), |b| b.opacity(0.5).cursor_default())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.worktree_remove(&WorktreeRemove, window, cx);
                        })),
                )
                .child(
                    self.dialog_button("worktree-keep", "Keep worktree", "Enter")
                        .font_weight(FontWeight::MEDIUM)
                        .border_color(t.muted)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.worktree_keep(&WorktreeKeep, window, cx);
                        })),
                ),
            Step::ConfirmRemove => h_flex()
                .child(
                    self.dialog_button("worktree-cancel", "Cancel", "Esc")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.worktree_cancel(&WorktreeCancel, window, cx);
                        })),
                )
                .child(
                    self.dialog_button("worktree-keep", "Keep worktree", "Enter")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.worktree_keep(&WorktreeKeep, window, cx);
                        })),
                )
                .child(
                    self.dialog_button("worktree-remove-anyway", "Remove anyway", "Ctrl+Enter")
                        .font_weight(FontWeight::MEDIUM)
                        .border_color(t.ochre)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.worktree_remove_anyway(&WorktreeRemoveAnyway, window, cx);
                        })),
                ),
        };
        let dialog = dialog_frame("worktree-close", &title, &t)
            .key_context(WORKTREE_CLOSE_CONTEXT)
            .track_focus(&closing.focus)
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.worktree_cancel(&WorktreeCancel, window, cx);
            }))
            .map(|dialog| self.dismiss_on_window_commands(dialog, cx))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(t.muted)
                    .child(q.detail()),
            )
            .when(q.step == Step::Choose, |d| {
                d.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(t.faint)
                        .child(q.work_text()),
                )
            })
            .child(
                buttons
                    .justify_end()
                    .gap(px(8.0))
                    .pt(px(4.0))
                    .text_size(px(13.0)),
            );
        Some(scrim(dialog))
    }

    fn dialog_button(
        &self,
        id: &'static str,
        label: &'static str,
        keys: &'static str,
    ) -> Stateful<Div> {
        let t = self.theme;
        let hover = t.hover;
        h_flex()
            .id(id)
            .role(Role::Button)
            .aria_label(format!("{label} ({keys})"))
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(t.line)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(label)
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(t.faint)
                    .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                    .child(keys),
            )
    }
}

/// The dialog box, placed and drawn like the close-project question.
fn dialog_frame(id: &'static str, label: &str, t: &Theme) -> Stateful<Div> {
    v_flex()
        .id(id)
        .role(Role::Dialog)
        .aria_label(label.to_owned())
        .mt(px(PALETTE_TOP))
        .w(px(CONFIRM_WIDTH))
        .max_w(relative(0.8))
        .gap(px(10.0))
        .p(px(16.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(t.line)
        .bg(t.raised)
        .shadow_lg()
}

/// The dimmed layer over the window that holds a dialog.
fn scrim(dialog: Stateful<Div>) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .justify_center()
        .items_start()
        .bg(rgba(0x0000_0059))
        .child(dialog)
}
