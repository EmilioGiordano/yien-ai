//! A newer YIEN in the window: the notice, the release notes and the
//! install the user asks for. What each step means is decided in
//! `crate::update`; the notes' blocks come from `crate::release_notes`.

use std::ops::Range;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tokio::sync::{mpsc, oneshot};

use super::status::Notice;
use super::{CONFIRM_WIDTH, PALETTE_TOP, Shell};
use crate::actions::*;
use crate::catalog::{CATALOG, Scope};
use crate::palette::{Entry, Target};
use crate::release_notes::{self, Block, Kind, Span, openable};
use crate::theme::Theme;
use crate::update::{Downloaded, Install, UpdateHost, UpdateModel, UpdateOffer, Updates};

const NOTES_WIDTH: f32 = 640.0;
const NOTES_HEIGHT: f32 = 460.0;
/// How far one arrow key scrolls the notes.
const LINE_STEP: f32 = 40.0;
/// The catalog commands that only mean something while an update is offered.
const UPDATE_COMMANDS: [&str; 2] = ["update.whats_new", "update.install"];

/// The update state the window keeps.
pub(super) struct UpdatePanel {
    model: UpdateModel,
    host: Box<dyn UpdateHost>,
    /// The notes, while shown.
    notes: Option<Notes>,
    /// The text of the last notice the update put up.
    shown: Option<String>,
}

/// The release notes, while shown.
struct Notes {
    version: String,
    blocks: Vec<Block>,
    focus: FocusHandle,
    close: FocusHandle,
    install: FocusHandle,
    scroll: ScrollHandle,
}

impl UpdatePanel {
    pub(super) fn new(host: Box<dyn UpdateHost>) -> Self {
        Self {
            model: UpdateModel::default(),
            host,
            notes: None,
            shown: None,
        }
    }

    pub(super) fn notes_open(&self) -> bool {
        self.notes.is_some()
    }
}

/// Whether the palette lists `id` now: the update commands only while an
/// update is offered.
fn listed(id: &str, offered: bool) -> bool {
    offered || !UPDATE_COMMANDS.contains(&id)
}

/// The scroll offset after moving by `by` pixels (down is positive), kept
/// between the top and the bottom.
fn scrolled(offset: f32, max: f32, by: f32) -> f32 {
    (offset - by).clamp(-max.max(0.0), 0.0)
}

impl Shell {
    /// Takes every offer, or withdrawal, the binary's check sends.
    fn listen_to_updates(
        mut offers: mpsc::UnboundedReceiver<Option<UpdateOffer>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(offer) = offers.recv().await {
                let taken = this.update_in(cx, |shell, window, cx| match offer {
                    Some(offer) => shell.update_offered(offer, cx),
                    None => shell.update_withdrawn(window, cx),
                });
                if taken.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    pub(super) fn update_panel(
        updates: Updates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> UpdatePanel {
        Self::listen_to_updates(updates.offers, window, cx);
        UpdatePanel::new(updates.host)
    }

    fn update_offered(&mut self, offer: UpdateOffer, cx: &mut Context<Self>) {
        tracing::info!(version = %offer.version, "a newer YIEN is available");
        let Some(text) = self.updates.model.offered(offer) else {
            return;
        };
        let notice = Notice::lasting_info(text);
        match &self.notice {
            None => self.show_update_notice(notice, cx),
            // A passing notice goes first; the update one stands after it.
            Some(current) if current.lifetime().is_some() && self.behind.is_none() => {
                self.updates.shown = Some(notice.text.clone());
                self.behind = Some(notice);
            }
            // Another standing notice stays; the update one comes back when
            // it goes (`clear_notice`).
            Some(_) => {}
        }
    }

    fn show_update_notice(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.updates.shown = Some(notice.text.clone());
        self.show(notice, cx);
    }

    /// `update.check` was turned off: the offer, its notice and notes go.
    fn update_withdrawn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.updates.model.withdraw() {
            return;
        }
        tracing::info!("the update offer was withdrawn");
        self.close_whats_new(window, cx);
        let ours = self.updates.shown.take();
        if self.behind.as_ref().map(|n| &n.text) == ours.as_ref() {
            self.behind = None;
        }
        if self.notice.as_ref().map(|n| &n.text) == ours.as_ref() {
            self.clear_notice(cx);
        }
    }

    /// A notice went down; when it was the "available" one, the user
    /// dismissed it.
    pub(super) fn update_notice_cleared(&mut self, notice: &Notice) {
        if self.updates.model.is_available_notice(&notice.text) {
            self.updates.model.dismiss();
        }
    }

    /// The update notice to stand when nothing else shows.
    pub(super) fn update_standing_notice(&mut self) -> Option<Notice> {
        let text = self.updates.model.standing_notice()?;
        self.updates.shown = Some(text.clone());
        Some(Notice::lasting_info(text))
    }

    /// Whether the palette lists `entry` now.
    pub(super) fn update_lists(&self, entry: &Entry) -> bool {
        match entry.target {
            Target::Command(at) => CATALOG
                .get(at)
                .is_none_or(|spec| listed(spec.id, self.updates.model.offer().is_some())),
            _ => true,
        }
    }

    /// `update: show what's new`.
    pub(super) fn show_whats_new(
        &mut self,
        _: &ShowWhatsNew,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(offer) = self.updates.model.offer() else {
            self.say("No YIEN update has been found.", cx);
            return;
        };
        let notes = Notes {
            version: offer.version.clone(),
            blocks: release_notes::parse(&offer.notes),
            focus: cx.focus_handle(),
            close: cx.focus_handle().tab_stop(true).tab_index(1),
            install: cx.focus_handle().tab_stop(true).tab_index(2),
            scroll: ScrollHandle::new(),
        };
        self.split_menu = None;
        self.close_palette(window, cx);
        self.close_tray(window, cx);
        self.finish_rename(true, window, cx);
        window.focus(&notes.focus, cx);
        self.updates.notes = Some(notes);
        cx.notify();
    }

    fn close_whats_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.updates.notes.take().is_some() {
            self.give_back_keyboard(window, cx);
        }
    }

    pub(super) fn whats_new_close(
        &mut self,
        _: &WhatsNewClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_whats_new(window, cx);
    }

    fn scroll_notes(&mut self, by: f32, cx: &mut Context<Self>) {
        if let Some(notes) = &self.updates.notes {
            let offset = notes.scroll.offset();
            let max = notes.scroll.max_offset();
            notes.scroll.set_offset(point(
                offset.x,
                px(scrolled(offset.y.as_f32(), max.y.as_f32(), by)),
            ));
            cx.notify();
        }
    }

    fn whats_new_up(&mut self, _: &WhatsNewUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_notes(-LINE_STEP, cx);
    }

    fn whats_new_down(&mut self, _: &WhatsNewDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_notes(LINE_STEP, cx);
    }

    fn whats_new_page_up(&mut self, _: &WhatsNewPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_notes(-NOTES_HEIGHT * 0.9, cx);
    }

    fn whats_new_page_down(
        &mut self,
        _: &WhatsNewPageDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_notes(NOTES_HEIGHT * 0.9, cx);
    }

    /// `update: install`: downloads and checks the installer, then quits so
    /// it can run; the session is saved on the way out (RNF-21).
    pub(super) fn install_update(
        &mut self,
        _: &InstallUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_whats_new(window, cx);
        match self.updates.model.install() {
            Install::Say(text) => self.say(text, cx),
            Install::Quit => self.quit_to_install(window, cx),
            Install::Download(version) => {
                self.show_update_notice(
                    Notice::lasting_info(format!("Downloading YIEN {version}…")),
                    cx,
                );
                let (done, outcome) = oneshot::channel();
                self.updates.host.download(
                    &version,
                    Box::new(move |result| {
                        let _ = done.send(result);
                    }),
                );
                cx.spawn_in(window, async move |this, cx| {
                    let result = outcome
                        .await
                        .unwrap_or_else(|_| Err("the download stopped".into()));
                    let _ = this.update_in(cx, |shell, window, cx| {
                        shell.update_downloaded(&version, result, window, cx);
                    });
                })
                .detach();
            }
        }
    }

    fn update_downloaded(
        &mut self,
        version: &str,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.updates.model.downloaded(version, result) {
            Downloaded::Ready => self.quit_to_install(window, cx),
            Downloaded::Failed(text) => {
                tracing::warn!("{text}");
                self.fail(text, cx);
            }
            Downloaded::Stale => {}
        }
    }

    /// The installer starts once YIEN has quit; while agents work the quit
    /// asks first, and a cancelled quit leaves it ready for another try.
    fn quit_to_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.updates.model.arm();
        self.updates.host.install_on_quit(true);
        self.say("Installing the update: YIEN closes and opens again.", cx);
        self.quit(&Quit, window, cx);
    }

    /// The quit question was cancelled: the installer must not run at the
    /// next ordinary quit.
    pub(super) fn quit_cancelled(&mut self, cx: &mut Context<Self>) {
        if self.updates.model.disarm() {
            self.updates.host.install_on_quit(false);
            self.say("The update waits; run \"update: install\" again.", cx);
        }
    }

    /// While the notes are open, a window command closes them and goes no
    /// further, so the palette or a prompt never opens underneath.
    fn close_notes_on_window_commands(
        &self,
        dialog: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        CATALOG
            .iter()
            .filter(|spec| spec.scope == Scope::Window && spec.id != "update.install")
            .fold(dialog, |dialog, spec| {
                dialog.on_boxed_action(
                    &*(spec.action)(),
                    cx.listener(|this, _: &dyn Action, window, cx| {
                        this.close_whats_new(window, cx);
                    }),
                )
            })
    }

    /// The release notes over the window, read-only. Links open in the
    /// browser only when clicked; Tab reaches the buttons, the arrows and
    /// Page Up/Down scroll.
    pub(super) fn render_whats_new(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notes = self.updates.notes.as_ref()?;
        let t = self.theme;
        let hover = t.hover;
        let ring = t.focus_ring();
        let title = format!("What's new in YIEN {}", notes.version);
        let body: Vec<AnyElement> = if notes.blocks.is_empty() {
            vec![
                div()
                    .text_color(t.muted)
                    .child("This release has no notes.")
                    .into_any_element(),
            ]
        } else {
            notes
                .blocks
                .iter()
                .enumerate()
                .map(|(n, block)| self.render_block(block, n, &t))
                .collect()
        };
        let button = |id: &'static str, label: &'static str, focus: &FocusHandle| {
            h_flex()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .track_focus(focus)
                .items_center()
                .px(px(12.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(t.line)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .focus(move |s| s.border_color(ring))
                .child(label)
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .justify_center()
                .items_start()
                .bg(rgba(0x0000_0059))
                .child(
                    v_flex()
                        .id("whats-new")
                        .role(Role::Dialog)
                        .aria_label(title.clone())
                        .track_focus(&notes.focus)
                        .key_context(WHATS_NEW_CONTEXT)
                        .mt(px(PALETTE_TOP))
                        .w(px(NOTES_WIDTH.max(CONFIRM_WIDTH)))
                        .max_w(relative(0.9))
                        .gap(px(10.0))
                        .p(px(16.0))
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(t.line)
                        .bg(t.raised)
                        .shadow_lg()
                        .on_action(cx.listener(Self::whats_new_close))
                        .on_action(cx.listener(Self::whats_new_up))
                        .on_action(cx.listener(Self::whats_new_down))
                        .on_action(cx.listener(Self::whats_new_page_up))
                        .on_action(cx.listener(Self::whats_new_page_down))
                        .map(|dialog| self.close_notes_on_window_commands(dialog, cx))
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.close_whats_new(window, cx);
                        }))
                        .child(
                            div()
                                .text_size(t.text(15.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(
                            v_flex()
                                .id("whats-new-notes")
                                .role(Role::Document)
                                .aria_label("Release notes")
                                .track_scroll(&notes.scroll)
                                .overflow_y_scroll()
                                .max_h(px(NOTES_HEIGHT))
                                .gap(px(8.0))
                                .pr(px(6.0))
                                .text_size(t.text(13.0))
                                .children(body),
                        )
                        .child(
                            h_flex()
                                .justify_end()
                                .gap(px(8.0))
                                .pt(px(4.0))
                                .text_size(t.text(13.0))
                                .child(button("whats-new-close", "Close", &notes.close).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_whats_new(window, cx)
                                    }),
                                ))
                                .child(
                                    button(
                                        "whats-new-install",
                                        "Install and restart",
                                        &notes.install,
                                    )
                                    .font_weight(FontWeight::MEDIUM)
                                    .border_color(t.muted)
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.install_update(&InstallUpdate, window, cx);
                                        },
                                    )),
                                ),
                        ),
                ),
        )
    }

    /// One block; blocks are flat, so this never recurses.
    fn render_block(&self, block: &Block, n: usize, t: &Theme) -> AnyElement {
        let id = format!("notes-{n}");
        let inner = match &block.kind {
            Kind::Heading(level) => div()
                .pt(px(if *level <= 2 { 6.0 } else { 2.0 }))
                .text_size(t.text(match level {
                    1 => 18.0,
                    2 => 16.0,
                    3 => 14.5,
                    _ => 13.5,
                }))
                .font_weight(FontWeight::SEMIBOLD)
                .child(rich_text(&id, &block.spans, t))
                .into_any_element(),
            Kind::Paragraph => div()
                .child(rich_text(&id, &block.spans, t))
                .into_any_element(),
            Kind::Item { depth, marker } => h_flex()
                .items_start()
                .gap(px(6.0))
                .pl(px(f32::from(*depth) * 18.0))
                .child(
                    div()
                        .flex_none()
                        .min_w(px(16.0))
                        .text_color(t.muted)
                        .child(marker.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(rich_text(&id, &block.spans, t)),
                )
                .into_any_element(),
            Kind::Code => div()
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(t.panel)
                .border_1()
                .border_color(t.line)
                .text_size(t.text(12.5))
                .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                .children(block.spans.first().map(|s| s.text.clone()))
                .into_any_element(),
            Kind::Rule => div().h(px(1.0)).bg(t.line).into_any_element(),
        };
        if block.quote {
            div()
                .pl(px(10.0))
                .border_l_2()
                .border_color(t.line)
                .text_color(t.muted)
                .child(inner)
                .into_any_element()
        } else {
            inner
        }
    }
}

/// Spans as one text with their styles; the link ranges open in the browser
/// when clicked, and only when they are `http` or `https`.
fn rich_text(id: &str, spans: &[Span], t: &Theme) -> AnyElement {
    let (text, styles, links) = layout_spans(spans);
    let highlights: Vec<(Range<usize>, HighlightStyle)> = styles
        .into_iter()
        .map(|(range, span)| (range, highlight(&span, t)))
        .collect();
    let styled = StyledText::new(text).with_highlights(highlights);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let (ranges, urls): (Vec<_>, Vec<_>) = links.into_iter().unzip();
    InteractiveText::new(SharedString::from(id.to_owned()), styled)
        .on_click(ranges, move |at, _, cx| {
            if let Some(url) = urls.get(at).filter(|url| openable(url)) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

/// Byte ranges of a text, each with what it carries.
type Ranges<T> = Vec<(Range<usize>, T)>;

/// The text of `spans`, each span's byte range, and the link ranges with
/// their targets.
fn layout_spans(spans: &[Span]) -> (String, Ranges<Span>, Ranges<String>) {
    let mut text = String::new();
    let mut styles = Vec::new();
    let mut links = Vec::new();
    for span in spans {
        let range = text.len()..text.len() + span.text.len();
        text.push_str(&span.text);
        if let Some(url) = &span.link {
            links.push((range.clone(), url.clone()));
        }
        styles.push((range, span.clone()));
    }
    (text, styles, links)
}

fn highlight(span: &Span, t: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: Some(if span.link.is_some() { t.ai } else { t.text }.into()),
        font_weight: span.bold.then_some(FontWeight::BOLD),
        font_style: span.italic.then_some(FontStyle::Italic),
        background_color: span.code.then(|| t.panel.into()),
        underline: span.link.is_some().then(|| UnderlineStyle {
            thickness: px(1.0),
            color: Some(t.ai.into()),
            wavy: false,
        }),
        strikethrough: span.strike.then(|| StrikethroughStyle {
            thickness: px(1.0),
            color: Some(t.muted.into()),
        }),
        ..HighlightStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{layout_spans, listed, release_notes, scrolled};

    #[test]
    fn link_ranges_cover_exactly_their_text() {
        let blocks = release_notes::parse(
            "Read **the** [notes](https://example.com/n) — é [x](file:///c:/x)",
        );
        let (text, styles, links) = layout_spans(&blocks[0].spans);
        assert_eq!(links.len(), 1, "the file link is plain text");
        let (range, url) = &links[0];
        assert_eq!(&text[range.clone()], "notes");
        assert_eq!(url, "https://example.com/n");
        let covered: usize = styles.iter().map(|(r, _)| r.len()).sum();
        assert_eq!(covered, text.len());
        for (range, _) in styles {
            assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
        }
    }

    #[test]
    fn the_update_commands_are_listed_only_while_an_update_is_offered() {
        assert!(!listed("update.install", false));
        assert!(!listed("update.whats_new", false));
        assert!(listed("update.install", true));
        assert!(listed("palette.open", false));
    }

    #[test]
    fn keyboard_scrolling_stays_inside_the_notes() {
        assert_eq!(scrolled(0.0, 500.0, 40.0), -40.0);
        assert_eq!(scrolled(-480.0, 500.0, 40.0), -500.0, "stops at the bottom");
        assert_eq!(scrolled(-10.0, 500.0, -40.0), 0.0, "stops at the top");
        assert_eq!(scrolled(0.0, 0.0, 40.0), 0.0, "nothing to scroll");
    }
}
