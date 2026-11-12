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
use crate::release_notes::{self, Block, Span, openable};
use crate::theme::Theme;
use crate::update::{Downloaded, Install, UpdateHost, UpdateModel, UpdateOffer, Updates};

const NOTES_WIDTH: f32 = 640.0;
const NOTES_HEIGHT: f32 = 460.0;

/// The update state the window keeps.
pub(super) struct UpdatePanel {
    model: UpdateModel,
    host: Box<dyn UpdateHost>,
    /// The notes, while shown.
    notes: Option<Notes>,
}

/// The release notes, while shown.
struct Notes {
    version: String,
    blocks: Vec<Block>,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

impl UpdatePanel {
    pub(super) fn new(host: Box<dyn UpdateHost>) -> Self {
        Self {
            model: UpdateModel::default(),
            host,
            notes: None,
        }
    }

    pub(super) fn notes_open(&self) -> bool {
        self.notes.is_some()
    }
}

impl Shell {
    /// Takes every offer the binary's check sends, as it arrives.
    pub(super) fn listen_to_updates(
        mut offers: mpsc::UnboundedReceiver<UpdateOffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(offer) = offers.recv().await {
                let taken = this.update(cx, |shell, cx| shell.update_offered(offer, cx));
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
        if let Some(text) = self.updates.model.offered(offer) {
            self.show(Notice::lasting_info(text), cx);
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
                self.show(
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
        self.updates.host.install_on_quit(true);
        self.say("Installing the update: YIEN closes and opens again.", cx);
        self.quit(&Quit, window, cx);
    }

    /// The quit question was cancelled: the installer must not run at the
    /// next ordinary quit.
    pub(super) fn quit_cancelled(&mut self, cx: &mut Context<Self>) {
        self.updates.host.install_on_quit(false);
        if self.updates.model.offer().is_some() {
            self.say("The update waits; run \"update: install\" again.", cx);
        }
    }

    /// The release notes over the window, read-only. Links open in the
    /// browser only when clicked.
    pub(super) fn render_whats_new(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notes = self.updates.notes.as_ref()?;
        let t = self.theme;
        let hover = t.hover;
        let title = format!("What's new in YIEN {}", notes.version);
        let body = if notes.blocks.is_empty() {
            vec![
                div()
                    .text_color(t.muted)
                    .child("This release has no notes.")
                    .into_any_element(),
            ]
        } else {
            self.render_blocks(&notes.blocks, "notes", &t)
        };
        let button = |id: &'static str, label: &'static str| {
            h_flex()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .items_center()
                .px(px(12.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(t.line)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
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
                                .child(button("whats-new-close", "Close").on_click(cx.listener(
                                    |this, _, window, cx| this.close_whats_new(window, cx),
                                )))
                                .child(
                                    button("whats-new-install", "Install and restart")
                                        .font_weight(FontWeight::MEDIUM)
                                        .border_color(t.muted)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.install_update(&InstallUpdate, window, cx);
                                        })),
                                ),
                        ),
                ),
        )
    }

    fn render_blocks(&self, blocks: &[Block], id: &str, t: &Theme) -> Vec<AnyElement> {
        blocks
            .iter()
            .enumerate()
            .map(|(n, block)| self.render_block(block, &format!("{id}-{n}"), t))
            .collect()
    }

    fn render_block(&self, block: &Block, id: &str, t: &Theme) -> AnyElement {
        match block {
            Block::Heading { level, spans } => div()
                .pt(px(if *level <= 2 { 6.0 } else { 2.0 }))
                .text_size(t.text(match level {
                    1 => 18.0,
                    2 => 16.0,
                    3 => 14.5,
                    _ => 13.5,
                }))
                .font_weight(FontWeight::SEMIBOLD)
                .child(rich_text(id, spans, t))
                .into_any_element(),
            Block::Paragraph(spans) => div().child(rich_text(id, spans, t)).into_any_element(),
            Block::List { start, items } => v_flex()
                .gap(px(4.0))
                .children(items.iter().enumerate().map(|(n, item)| {
                    let marker = match start {
                        Some(first) => format!("{}.", u64::from(*first) + n as u64),
                        None => "•".to_owned(),
                    };
                    h_flex()
                        .items_start()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(16.0))
                                .text_color(t.muted)
                                .child(marker),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(4.0))
                                .children(self.render_blocks(item, &format!("{id}-{n}"), t)),
                        )
                }))
                .into_any_element(),
            Block::Code(code) => div()
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(t.panel)
                .border_1()
                .border_color(t.line)
                .text_size(t.text(12.5))
                .when_some(self.fonts.mono.clone(), |d, family| d.font_family(family))
                .child(code.clone())
                .into_any_element(),
            Block::Quote(blocks) => v_flex()
                .gap(px(6.0))
                .pl(px(10.0))
                .border_l_2()
                .border_color(t.line)
                .text_color(t.muted)
                .children(self.render_blocks(blocks, id, t))
                .into_any_element(),
            Block::Rule => div().h(px(1.0)).bg(t.line).into_any_element(),
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
    use super::{Block, layout_spans, release_notes};

    #[test]
    fn link_ranges_cover_exactly_their_text() {
        let spans = release_notes::parse(
            "Read **the** [notes](https://example.com/n) — é [x](file:///c:/x)",
        );
        let Block::Paragraph(spans) = &spans[0] else {
            panic!("{spans:?}");
        };
        let (text, styles, links) = layout_spans(spans);
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
}
