//! The context bar's Files view (RF-CTX-01, RF-CTX-03): the tree of the
//! selected tab's folder with its filter, and opening files in the editor.
//! What the tree shows is decided in `crate::file_tree`.

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tokio::sync::oneshot;
use yien_proto::TabId;
use yien_proto::files::{EntryKind, SearchEnd, SearchResult};

use super::Shell;
use crate::actions::*;
use crate::assets;
use crate::file_tree::{Activation, FileTree, Row, SearchRequest};
use crate::files::{Files, FilesHost};
use crate::region::{Modality, Region};
use crate::sidebars::{self, Bar};

/// How long typing must pause before the filter searches.
const SEARCH_PAUSE: Duration = Duration::from_millis(100);

/// The height of every tree row, so the list can draw only what shows.
const ROW_HEIGHT: f32 = 24.0;
/// The indent of one folder level, as the prototype's.
const INDENT: f32 = 14.0;

/// Which view the context bar shows under the branch head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextView {
    Changes,
    Files,
}

/// The Files view's state across tabs.
pub(super) struct FilesPanel {
    host: Box<dyn FilesHost>,
    seed: Vec<String>,
    /// Each tab's tree, kept while the tab is open.
    trees: HashMap<TabId, FileTree>,
    /// The tab and folder the view last showed.
    shown: Option<(TabId, String)>,
    input: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _input_events: Subscription,
}

impl FilesPanel {
    pub(super) fn new(files: Files, window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find file by name"));
        let input_events = cx.subscribe_in(&input, window, Shell::on_files_input);
        Self {
            host: files.host,
            seed: files.seed,
            trees: HashMap::new(),
            shown: None,
            input,
            scroll: UniformListScrollHandle::new(),
            _input_events: input_events,
        }
    }

    /// Whether the window starts on the Files view (a seeded window check).
    pub(super) fn seeded(&self) -> bool {
        !self.seed.is_empty()
    }
}

impl Shell {
    /// The tree of the selected tab, while the Files view shows.
    fn shown_tree(&mut self) -> Option<(TabId, &mut FileTree)> {
        let (tab, _) = self.files.shown.clone()?;
        self.files.trees.get_mut(&tab).map(|t| (tab, t))
    }

    /// Makes the Files view follow the selected tab: a new tab or folder
    /// gets a new tree, a tab shown again lists its folders again, and
    /// closed tabs' trees go.
    pub(super) fn sync_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = &self.workspace;
        self.files
            .trees
            .retain(|tab, _| workspace.tab(*tab).is_some());
        let visible = self.bar_shown(Bar::Context) && self.context_view == ContextView::Files;
        let selected = self.workspace.selected_tab().map(|t| (t.id, t.dir.clone()));
        let Some((tab, dir)) = selected.filter(|_| visible) else {
            self.files.shown = None;
            return;
        };
        if self.files.shown.as_ref() == Some(&(tab, dir.clone())) {
            return;
        }
        self.files.shown = Some((tab, dir.clone()));
        let mut resumed = None;
        let requests = match self.files.trees.get_mut(&tab) {
            Some(tree) if tree.root() == dir => {
                resumed = tree.resume_search();
                tree.refresh()
            }
            _ => {
                let (mut tree, root) = FileTree::new(&dir);
                let mut requests = vec![root];
                for folder in &self.files.seed {
                    requests.extend(tree.reveal(folder, true));
                }
                self.files.trees.insert(tab, tree);
                requests
            }
        };
        let query = self
            .files
            .trees
            .get(&tab)
            .map(|t| t.query().to_owned())
            .unwrap_or_default();
        self.files
            .input
            .update(cx, |input, cx| input.set_value(query, window, cx));
        self.list_folders(tab, requests, cx);
        if let Some(request) = resumed {
            self.start_search(tab, request, cx);
        }
    }

    /// Lists the selected tab's folders again after its git facts changed.
    pub(super) fn files_git_changed(&mut self, tabs: &[TabId], cx: &mut Context<Self>) {
        let Some((tab, tree)) = self.shown_tree() else {
            return;
        };
        if tabs.contains(&tab) {
            let requests = tree.refresh();
            self.list_folders(tab, requests, cx);
        }
    }

    fn list_folders(&mut self, tab: TabId, dirs: Vec<String>, cx: &mut Context<Self>) {
        let Some(root) = self.files.trees.get(&tab).map(|t| t.root().to_owned()) else {
            return;
        };
        for dir in dirs {
            let (done, answer) = oneshot::channel();
            self.files.host.list(
                PathBuf::from(&root),
                dir.clone(),
                Box::new(move |listing| {
                    let _ = done.send(listing);
                }),
            );
            let root = root.clone();
            cx.spawn(async move |this, cx| {
                // A host that dropped the callback still ends the wait.
                let listing = answer
                    .await
                    .unwrap_or_else(|_| Err("the listing did not answer".to_owned()));
                let _ = this.update(cx, |shell, cx| {
                    if let Some(tree) = shell.files.trees.get_mut(&tab)
                        && tree.root() == root
                    {
                        let again = tree.listed(&dir, listing);
                        shell.list_folders(tab, again, cx);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn start_search(&mut self, tab: TabId, request: SearchRequest, cx: &mut Context<Self>) {
        let Some(root) = self.files.trees.get(&tab).map(|t| t.root().to_owned()) else {
            return;
        };
        let (done, answer) = oneshot::channel();
        self.files.host.search(
            PathBuf::from(&root),
            request.query,
            Box::new(move |result| {
                let _ = done.send(result);
            }),
        );
        cx.spawn(async move |this, cx| {
            let result = answer.await.unwrap_or(SearchResult {
                hits: Vec::new(),
                end: SearchEnd::Unavailable,
            });
            let _ = this.update(cx, |shell, cx| {
                if let Some(tree) = shell.files.trees.get_mut(&tab)
                    && tree.root() == root
                {
                    tree.searched(request.id, result);
                    shell.files.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn on_files_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let query = input.read(cx).value();
        self.set_files_query(&query, cx);
    }

    /// Takes the filter's text: a new query searches once typing pauses,
    /// and clearing it stops the search that runs.
    fn set_files_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some((tab, tree)) = self.shown_tree() else {
            return;
        };
        let was_searching = tree.searching();
        match tree.set_query(query) {
            Some(request) => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SEARCH_PAUSE).await;
                    let _ = this.update(cx, |shell, cx| {
                        let current = shell
                            .files
                            .trees
                            .get(&tab)
                            .is_some_and(|t| t.awaits(request.id));
                        if current {
                            shell.start_search(tab, request, cx);
                        }
                    });
                })
                .detach();
            }
            None if was_searching && !tree.searching() => self.files.host.cancel_search(),
            None => {}
        }
        cx.notify();
    }

    /// After an activation: a folder match ends the search, so the filter
    /// is emptied to match.
    fn sync_filter_text(
        &mut self,
        was_filtering: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filtering = self.shown_tree().is_some_and(|(_, t)| t.filtering());
        if was_filtering && !filtering {
            self.files.host.cancel_search();
            self.files
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Opens a file in the configured editor, or the OS default app
    /// (RF-CTX-03); a failure goes to the status bar.
    pub(super) fn open_file(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        let editor = self.settings.config().editor.clone();
        let (done, outcome) = oneshot::channel();
        self.files.host.open(
            file,
            editor,
            Box::new(move |error| {
                let _ = done.send(error);
            }),
        );
        cx.spawn(async move |this, cx| {
            if let Ok(Some(error)) = outcome.await {
                let _ = this.update(cx, |shell, cx| {
                    shell.fail(error, cx);
                });
            }
        })
        .detach();
    }

    fn act(&mut self, tab: TabId, activation: Activation, cx: &mut Context<Self>) {
        match activation {
            Activation::Open(rel) => {
                let file = self.files.trees.get(&tab).and_then(|t| t.path_of(&rel));
                if let Some(file) = file {
                    self.open_file(file, cx);
                }
            }
            Activation::List(dirs) => self.list_folders(tab, dirs, cx),
            Activation::Nothing => {}
        }
        self.scroll_to_cursor();
        cx.notify();
    }

    fn scroll_to_cursor(&mut self) {
        let Some((_, tree)) = self.shown_tree() else {
            return;
        };
        if let Some(at) = tree.cursor_index(&tree.rows()) {
            self.files
                .scroll
                .scroll_to_item(at, ScrollStrategy::Nearest);
        }
    }

    /// Shows the Files view and gives it the keyboard, in its filter.
    pub(super) fn show_files(
        &mut self,
        _: &ShowFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modality = Modality::Keyboard;
        self.open_files_view(window, cx);
    }

    fn open_files_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.change_side_bars(|b| sidebars::show(b, Bar::Context), window, cx);
        self.context_view = ContextView::Files;
        self.sync_files(window, cx);
        self.focus_files(window, cx);
        cx.notify();
    }

    /// Gives the keyboard to the Files view's filter.
    pub(super) fn focus_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.files.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.keyboard_in_bar = true;
    }

    pub(super) fn show_changes(
        &mut self,
        _: &ShowChanges,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modality = Modality::Keyboard;
        self.change_side_bars(|b| sidebars::show(b, Bar::Context), window, cx);
        self.context_view = ContextView::Changes;
        self.sync_files(window, cx);
        window.focus(&self.context_focus, cx);
        self.keyboard_in_bar = true;
        cx.notify();
    }

    pub(super) fn refresh_files(
        &mut self,
        _: &RefreshFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_files(window, cx);
        if let Some((tab, tree)) = self.shown_tree() {
            let requests = tree.refresh();
            self.list_folders(tab, requests, cx);
        }
    }

    fn with_tree(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut FileTree) -> Activation) {
        if let Some((tab, tree)) = self.shown_tree() {
            let activation = f(tree);
            self.act(tab, activation, cx);
        }
    }

    pub(super) fn file_tree_up(&mut self, _: &FileTreeUp, _: &mut Window, cx: &mut Context<Self>) {
        self.modality = Modality::Keyboard;
        self.with_tree(cx, |tree| {
            tree.move_cursor(-1);
            Activation::Nothing
        });
    }

    pub(super) fn file_tree_down(
        &mut self,
        _: &FileTreeDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modality = Modality::Keyboard;
        self.with_tree(cx, |tree| {
            tree.move_cursor(1);
            Activation::Nothing
        });
    }

    /// Left and right move the filter's caret while it holds text.
    fn filter_has_text(&mut self) -> bool {
        self.shown_tree().is_none_or(|(_, tree)| tree.filtering())
    }

    pub(super) fn file_tree_collapse(
        &mut self,
        _: &FileTreeCollapse,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modality = Modality::Keyboard;
        if self.filter_has_text() {
            cx.propagate();
            return;
        }
        self.with_tree(cx, |tree| {
            tree.left();
            Activation::Nothing
        });
    }

    pub(super) fn file_tree_expand(
        &mut self,
        _: &FileTreeExpand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modality = Modality::Keyboard;
        if self.filter_has_text() {
            cx.propagate();
            return;
        }
        self.with_tree(cx, |tree| Activation::List(tree.right()));
    }

    pub(super) fn file_tree_open(
        &mut self,
        _: &FileTreeOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modality = Modality::Keyboard;
        let was_filtering = self.shown_tree().is_some_and(|(_, t)| t.filtering());
        self.with_tree(cx, FileTree::activate);
        self.sync_filter_text(was_filtering, window, cx);
    }

    /// Escape clears the filter, then leaves the context bar.
    pub(super) fn file_tree_escape(
        &mut self,
        _: &FileTreeEscape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shown_tree().is_some_and(|(_, t)| t.filtering()) {
            self.files
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.set_files_query("", cx);
        } else {
            self.give_back_keyboard(window, cx);
        }
    }

    fn click_file_row(
        &mut self,
        path: String,
        double: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_files(window, cx);
        let was_filtering = self.shown_tree().is_some_and(|(_, t)| t.filtering());
        if let Some((tab, tree)) = self.shown_tree() {
            let activation = tree.click(&path, double);
            self.act(tab, activation, cx);
        }
        self.sync_filter_text(was_filtering, window, cx);
    }

    /// The tab strip under the branch head: Changes, with its count, and
    /// Files, as the prototype's.
    pub(super) fn render_context_tabs(
        &self,
        changed: Option<u32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let mono = self.fonts.mono.clone();
        let tab = |id: &'static str, label: &'static str, view: ContextView| {
            let selected = self.context_view == view;
            h_flex()
                .id(id)
                .role(Role::Tab)
                .aria_label(label)
                .aria_selected(selected)
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(6.0))
                .border_b_2()
                .border_color(if selected {
                    t.text
                } else {
                    transparent_black().into()
                })
                .text_size(t.text(13.0))
                .text_color(if selected { t.text } else { t.muted })
                .cursor_pointer()
                .child(label)
        };
        h_flex()
            .id("context-tabs")
            .role(Role::TabList)
            .aria_label("Context views")
            .flex_none()
            .gap(px(2.0))
            .px(px(8.0))
            .pt(px(6.0))
            .border_b_1()
            .border_color(t.line)
            .child(
                tab("context-tab-changes", "Changes", ContextView::Changes)
                    .when_some(changed, |tab, n| {
                        tab.child(
                            div()
                                .text_size(t.text(11.0))
                                .text_color(t.faint)
                                .when_some(mono.clone(), |d, family| d.font_family(family))
                                .child(n.to_string()),
                        )
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.context_view = ContextView::Changes;
                        this.sync_files(window, cx);
                        cx.notify();
                    })),
            )
            .child(
                tab("context-tab-files", "Files", ContextView::Files)
                    .on_click(cx.listener(|this, _, window, cx| this.open_files_view(window, cx))),
            )
            .child(div().flex_1())
            .child(self.render_review_button(cx))
    }

    /// Widens the context bar to review the changes, or back (RF-NAV-10),
    /// as the prototype's "Review mode" button.
    fn render_review_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let review = self.side_bars.review;
        div()
            .id("review-mode")
            .role(Role::Button)
            .aria_label(format!(
                "{} ({})",
                super::bars::review_label(review),
                REVIEW_KEYS
            ))
            .aria_toggled(super::toggled(review))
            .flex_none()
            .mb(px(4.0))
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(if review { t.ai } else { t.line })
            .text_size(t.text(12.0))
            .text_color(if review { t.text } else { t.muted })
            .cursor_pointer()
            .hover(|s| s.bg(t.hover))
            .child(super::bars::review_label(review))
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_review_mode(&ToggleReviewMode, window, cx);
            }))
    }

    /// The Files view: the filter, the tree or the matches, and a note.
    pub(super) fn render_files(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let tree = self
            .files
            .shown
            .as_ref()
            .and_then(|(tab, _)| self.files.trees.get(tab));
        let rows = tree.map_or(0, |tree| tree.rows().len());
        let note = tree.and_then(|tree| {
            if tree.filtering() {
                tree.search_note()
            } else if tree.root_is_empty() {
                Some("This folder is empty.".to_owned())
            } else {
                None
            }
        });
        v_flex()
            .id("files")
            .key_context(FILE_TREE_CONTEXT)
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .px(px(12.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(t.line)
                    .text_size(t.text(13.0))
                    .child(Input::new(&self.files.input).aria_label("Find file by name")),
            )
            .child(
                v_flex()
                    .id("file-tree-box")
                    .role(Role::Tree)
                    .aria_label("Files")
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list(
                            "file-tree",
                            rows,
                            cx.processor(|this, range: Range<usize>, window, cx| {
                                this.render_file_rows(range, window, cx)
                            }),
                        )
                        .track_scroll(&self.files.scroll)
                        .flex_1()
                        .min_h_0()
                        .px(px(6.0))
                        .py(px(4.0)),
                    ),
            )
            .when_some(note, |view, note| {
                view.child(
                    div()
                        .flex_none()
                        .px(px(12.0))
                        .py(px(8.0))
                        .text_size(t.text(12.5))
                        .text_color(t.muted)
                        .child(note),
                )
            })
    }

    fn render_file_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some((_, tree)) = self.shown_tree() else {
            return Vec::new();
        };
        let rows = tree.rows();
        let cursor = tree.cursor_index(&rows);
        let t = self.theme;
        let mono = self.fonts.mono.clone();
        let ringed = self.ringed(Region::ContextBar);
        let hover = t.hover;
        rows.into_iter()
            .enumerate()
            .skip(range.start)
            .take(range.len())
            .map(|(at, row)| {
                let base = h_flex()
                    .id(("file-row", at))
                    .h(px(ROW_HEIGHT))
                    .items_center()
                    .gap(px(6.0))
                    .pr(px(6.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(transparent_black())
                    .text_size(t.text(12.5))
                    .aria_selected(cursor == Some(at))
                    .when(cursor == Some(at), |row| {
                        row.bg(t.hover).when(ringed, |row| row.border_color(t.ai))
                    });
                match row {
                    Row::Entry {
                        path,
                        name,
                        kind,
                        depth,
                        expanded,
                    } => {
                        let label = entry_label(&name, kind, Some(expanded));
                        base.role(Role::TreeItem)
                            .aria_label(label)
                            .when(kind.is_dir(), |row| row.aria_expanded(expanded))
                            .pl(px(8.0 + depth as f32 * INDENT))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .child(chevron(kind, expanded, &t))
                            .child(icon(kind, &t))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(name),
                            )
                            .when(matches!(kind, EntryKind::Link { .. }), |row| {
                                row.child(div().flex_none().text_color(t.faint).child("link"))
                            })
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.click_file_row(
                                    path.clone(),
                                    event.click_count() >= 2,
                                    window,
                                    cx,
                                );
                            }))
                            .into_any_element()
                    }
                    Row::Hit { path, kind } => {
                        let (folder, name) = match path.rsplit_once('/') {
                            Some((folder, name)) => (format!("{folder}/"), name.to_owned()),
                            None => (String::new(), path.clone()),
                        };
                        base.role(Role::TreeItem)
                            .aria_label(entry_label(&path, kind, None))
                            .pl(px(8.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .when_some(mono.clone(), |d, family| d.font_family(family))
                            .child(icon(kind, &t))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .child(
                                        div()
                                            .flex_shrink_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_color(t.muted)
                                            .child(folder),
                                    )
                                    .child(div().flex_none().child(name)),
                            )
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.click_file_row(
                                    path.clone(),
                                    event.click_count() >= 2,
                                    window,
                                    cx,
                                );
                            }))
                            .into_any_element()
                    }
                    Row::Loading { depth } => note_row(base, depth, "Loading…", t.faint),
                    Row::More { depth } => note_row(base, depth, "… more", t.faint),
                    Row::Failed { depth, reason } => {
                        note_row(base, depth, &format!("Cannot list: {reason}"), t.muted)
                    }
                }
            })
            .collect()
    }
}

/// What a screen reader says for an entry.
fn entry_label(name: &str, kind: EntryKind, expanded: Option<bool>) -> String {
    let what = match kind {
        EntryKind::Dir => "Folder",
        EntryKind::File => "File",
        EntryKind::Link { dir: true } => "Link to a folder",
        EntryKind::Link { dir: false } => "Link",
    };
    match expanded {
        Some(true) if kind.is_dir() => format!("{what} {name}, expanded"),
        Some(false) if kind.is_dir() => format!("{what} {name}, collapsed"),
        _ => format!("{what} {name}"),
    }
}

fn note_row(row: Stateful<Div>, depth: usize, text: &str, color: Rgba) -> AnyElement {
    row.pl(px(8.0 + depth as f32 * INDENT + 30.0))
        .text_color(color)
        .child(text.to_owned())
        .into_any_element()
}

fn chevron(kind: EntryKind, expanded: bool, t: &crate::theme::Theme) -> AnyElement {
    if !kind.is_dir() {
        return div().flex_none().w(px(11.0)).into_any_element();
    }
    svg()
        .path(assets::CHEVRON)
        .size(px(11.0))
        .flex_none()
        .text_color(t.faint)
        .when(!expanded, |c| {
            c.with_transformation(Transformation::rotate(radians(-FRAC_PI_2)))
        })
        .into_any_element()
}

fn icon(kind: EntryKind, t: &crate::theme::Theme) -> AnyElement {
    if !kind.is_dir() {
        return div().flex_none().w(px(13.0)).into_any_element();
    }
    svg()
        .path(assets::FOLDER)
        .size(px(13.0))
        .flex_none()
        .text_color(t.muted)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::entry_label;
    use yien_proto::files::EntryKind;

    #[test]
    fn rf_ctx_01_rows_have_names_that_say_what_they_are() {
        assert_eq!(
            entry_label("src", EntryKind::Dir, Some(false)),
            "Folder src, collapsed"
        );
        assert_eq!(
            entry_label("src", EntryKind::Dir, Some(true)),
            "Folder src, expanded"
        );
        assert_eq!(
            entry_label("main.rs", EntryKind::File, Some(false)),
            "File main.rs"
        );
        assert_eq!(
            entry_label("a/b", EntryKind::Link { dir: false }, None),
            "Link a/b"
        );
    }
}
