//! The Files view's context menu on screen (RF-CTX-01), built on the
//! window's popup menu like the tab menu: a right click on a tree row, or
//! Shift+F10 and the menu key on the row under the cursor, offers the
//! actions of [`crate::file_menu`]. The name field of New File, New Folder
//! and Rename opens in the tree; what touches the disk goes to the core.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;
use yien_proto::file_ops::{FileAction, check_name, rel_name};
use yien_proto::files::EntryKind;
use yien_proto::{Command, TabGit, TabId};

use super::menu::{EDGE_MARGIN, Menu, MenuEntry, MenuEvent, MenuItem, MenuOutcome, OpenMenu};
use super::pane_menu::menu_may_open;
use super::{Asked, Shell};
use crate::actions::*;
use crate::catalog::shortcut;
use crate::file_menu::{
    FileClip, FileEffect, FileMenuAction, FileMenuEntry, FileTarget, MenuFacts, file_effect,
    file_menu, palette_actions,
};
use crate::file_tree::NameEdit;
use crate::palette::{Entry, Target};

/// The key the blank part of the tree records its place under, for a menu
/// on the folder itself opened by keyboard.
const BLANK: &str = "";

/// The menu while open, the name field, the file clipboard and where the
/// rows were painted.
#[derive(Default)]
pub(super) struct FileMenuState {
    open: Option<OpenFileMenu>,
    field: Option<NameField>,
    clip: Option<FileClip>,
    rows: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
}

struct OpenFileMenu {
    tab: TabId,
    target: FileTarget,
    menu: OpenMenu<FileMenuAction>,
}

/// The name being typed for a new entry or a rename.
struct NameField {
    tab: TabId,
    input: Entity<InputState>,
    _input_events: Subscription,
}

impl FileMenuState {
    pub(super) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub(super) fn is_editing(&self) -> bool {
        self.field.is_some()
    }

    /// Drops the menu without moving the keyboard, for a menu opening now.
    pub(super) fn forget(&mut self) {
        self.open = None;
    }
}

/// What the question before Delete Permanently says.
pub(super) fn delete_detail(folder: bool) -> &'static str {
    if folder {
        "The folder and everything in it are deleted for good. They do not go to the \
         Recycle Bin or the Trash, and this cannot be undone."
    } else {
        "The file is deleted for good. It does not go to the Recycle Bin or the Trash, and \
         this cannot be undone."
    }
}

/// The part of a new name the field selects first: the name without its
/// extension for a file, all of it for a folder, as in VS Code.
fn rename_selection(name: &str, is_dir: bool) -> Range<usize> {
    match name.rfind('.') {
        Some(dot) if !is_dir && dot > 0 => 0..dot,
        _ => 0..name.len(),
    }
}

/// What the field's text does once confirmed: the action to send, nothing
/// (empty, or the same name), or why it cannot be a name.
fn named_action(edit: &NameEdit, typed: &str) -> Result<Option<FileAction>, String> {
    if typed.trim().is_empty() {
        return Ok(None);
    }
    match edit {
        NameEdit::Rename { path } if rel_name(path) == typed => return Ok(None),
        _ => {}
    }
    check_name(typed, cfg!(windows))?;
    let name = typed.to_owned();
    Ok(Some(match edit {
        NameEdit::New { dir, folder: false } => FileAction::NewFile {
            dir: dir.clone(),
            name,
        },
        NameEdit::New { dir, folder: true } => FileAction::NewFolder {
            dir: dir.clone(),
            name,
        },
        NameEdit::Rename { path } => FileAction::Rename {
            path: path.clone(),
            name,
        },
    }))
}

/// The keyboard command that does the same as an item, if any.
fn item_keys_action(action: FileMenuAction) -> Option<Box<dyn Action>> {
    Some(match action {
        FileMenuAction::Preview => Box::new(FileTreeOpen),
        FileMenuAction::Edit => Box::new(FileTreeOpenInEditor),
        FileMenuAction::Rename => Box::new(FileTreeRename),
        FileMenuAction::Trash => Box::new(FileTreeTrash),
        FileMenuAction::Delete => Box::new(FileTreeDelete),
        _ => return None,
    })
}

impl Shell {
    /// Whether the selected tab's folder is in a git repository.
    fn files_git(&self, tab: TabId) -> bool {
        matches!(self.workspace.git_of(tab), Some(TabGit::Repository(_)))
    }

    /// The tab the Files view shows, its folder, and what the menu knows.
    fn files_menu_facts(&self) -> Option<(TabId, String, MenuFacts)> {
        let (tab, root) = self.files_shown()?;
        let facts = MenuFacts::new(self.files_git(tab), self.file_menu.clip.as_ref(), &root);
        Some((tab, root, facts))
    }

    /// The entry under the Files view's cursor, or the folder itself.
    fn cursor_target(&self) -> Option<FileTarget> {
        let (_, tree) = self.shown_tree_ref()?;
        let Some(path) = tree.cursor().map(str::to_owned) else {
            return Some(FileTarget::root());
        };
        let kind = tree.kind_of(&path)?;
        Some(FileTarget { path, kind })
    }

    /// Forgets where rows were painted: the rows painted next record
    /// themselves again, and rows gone from the tree stay gone.
    pub(super) fn forget_file_rows(&self) {
        self.file_menu.rows.borrow_mut().clear();
    }

    /// Lets a right click on a tree row open its menu, and records where
    /// the row is for the menu opened by keyboard.
    pub(super) fn file_menu_row(
        &self,
        row: Stateful<Div>,
        target: FileTarget,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let rows = Rc::clone(&self.file_menu.rows);
        let key = target.path.clone();
        row.relative()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // Or the tree's blank part would open the folder's menu too.
                    cx.stop_propagation();
                    window.prevent_default();
                    this.open_file_menu(target.clone(), event.position, window, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        rows.borrow_mut().insert(key.clone(), bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }

    /// Lets a right click on the tree's blank part open the menu of the
    /// tab's folder itself.
    pub(super) fn file_menu_blank(
        &self,
        tree: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let rows = Rc::clone(&self.file_menu.rows);
        tree.on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                window.prevent_default();
                this.open_file_menu(FileTarget::root(), event.position, window, cx);
            }),
        )
        .child(
            canvas(
                move |bounds, _, _| {
                    rows.borrow_mut().insert(BLANK.to_owned(), bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }

    /// Opens the menu of `target` with its top left at `at`, moving the
    /// keyboard to it; the cursor goes to the entry. Closing gives the
    /// keyboard back to the Files view.
    pub(super) fn open_file_menu(
        &mut self,
        target: FileTarget,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !menu_may_open(
            self.confirm.is_some(),
            self.worktree_close.is_some(),
            self.worktree_prompt.is_some(),
        ) {
            return;
        }
        self.end_name_field(true, cx);
        let Some((tab, _, facts)) = self.files_menu_facts() else {
            return;
        };
        // Dropped first, so their blur cannot close the menu opened now.
        self.file_menu.open = None;
        self.tab_menu.forget();
        self.new_tab_menu.forget();
        self.menu = None;
        self.split_menu = None;
        self.close_tray(window, cx);
        self.close_palette(window, cx);
        self.finish_rename(true, window, cx);
        if !target.path.is_empty()
            && let Some((_, tree)) = self.shown_tree()
        {
            tree.set_cursor(&target.path);
        }
        let entries = {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            file_menu(&target, facts)
                .into_iter()
                .map(|entry| match entry {
                    FileMenuEntry::Separator => MenuEntry::Separator,
                    FileMenuEntry::Item { action, enabled } => MenuEntry::Item(MenuItem {
                        label: action.label().to_owned(),
                        keys: item_keys_action(action).and_then(|a| shortcut(&keymap, a.as_ref())),
                        enabled,
                        action,
                    }),
                })
                .collect()
        };
        let name = if target.path.is_empty() {
            "the folder".to_owned()
        } else {
            rel_name(&target.path).to_owned()
        };
        let menu = OpenMenu::open(
            Menu::new(entries),
            format!("Actions for {name}"),
            at,
            window,
            cx,
            |this: &mut Self, cx| {
                this.file_menu.open = None;
                cx.notify();
            },
        );
        self.file_menu.open = Some(OpenFileMenu { tab, target, menu });
        cx.notify();
    }

    /// Shift+F10 or the menu key in the Files view: the menu of the entry
    /// under the cursor, under its row; of the folder itself with no cursor.
    pub(super) fn file_tree_menu(
        &mut self,
        _: &FileTreeMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_menu.is_editing() {
            cx.propagate();
            return;
        }
        let Some(target) = self.cursor_target() else {
            return;
        };
        let rows = self.file_menu.rows.borrow();
        let at = match rows.get(&target.path) {
            Some(row) if !target.path.is_empty() => {
                point(row.left() + px(24.0), row.bottom() + px(2.0))
            }
            _ => rows.get(BLANK).map_or_else(
                || point(px(EDGE_MARGIN), px(EDGE_MARGIN)),
                |tree| point(tree.left() + px(EDGE_MARGIN), tree.top() + px(EDGE_MARGIN)),
            ),
        };
        drop(rows);
        self.open_file_menu(target, at, window, cx);
    }

    fn close_file_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_menu.open.take().is_some() {
            self.files_take_keyboard(window, cx);
        }
    }

    /// The keyboard goes back to the Files view while it shows, else to the
    /// pane.
    pub(super) fn files_take_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.files_shown().is_some() {
            self.focus_files(window, cx);
            cx.notify();
        } else {
            self.give_back_keyboard(window, cx);
        }
    }

    /// A menu of a tab that closed, or of a folder the view no longer
    /// shows, goes; so does a name field.
    pub(super) fn drop_stale_file_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.files_shown().map(|(tab, _)| tab);
        if self
            .file_menu
            .open
            .as_ref()
            .is_some_and(|m| Some(m.tab) != shown)
        {
            self.close_file_menu(window, cx);
        }
        if self
            .file_menu
            .field
            .as_ref()
            .is_some_and(|f| Some(f.tab) != shown)
        {
            self.end_name_field(false, cx);
        }
    }

    /// A key or click in the open menu. Closing runs first, so the keyboard
    /// is back in the Files view when an action runs.
    fn on_file_menu_event(
        &mut self,
        event: MenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.file_menu.open.as_mut() else {
            return;
        };
        let (tab, target) = (open.tab, open.target.clone());
        match open.menu.model_mut().respond(event) {
            MenuOutcome::Stay => cx.notify(),
            MenuOutcome::Close => self.close_file_menu(window, cx),
            MenuOutcome::Run(action) => {
                self.close_file_menu(window, cx);
                self.run_file_action(tab, &target, action, window, cx);
            }
        }
    }

    pub(super) fn render_file_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.file_menu.open.as_ref()?;
        Some(open.menu.render(&self.theme, cx, Self::on_file_menu_event))
    }

    /// Runs `action` on `target` of the tab's folder, as the Files view
    /// shows it now.
    fn run_file_action(
        &mut self,
        tab: TabId,
        target: &FileTarget,
        action: FileMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((shown, root, facts)) = self.files_menu_facts() else {
            return;
        };
        if shown != tab {
            return;
        }
        let Some(effect) = file_effect(action, target, facts, &root, self.file_menu.clip.as_ref())
        else {
            return;
        };
        match effect {
            FileEffect::Preview(path) => self.command(Command::OpenFile { tab, path }, cx),
            FileEffect::Edit(path) => {
                let editor = self.settings.config().editor.clone();
                self.open_tree_file(&path, editor, cx);
            }
            FileEffect::OpenDefault(path) => self.open_tree_file(&path, None, cx),
            FileEffect::Terminal(path) => self.command(Command::OpenTerminal { tab, path }, cx),
            FileEffect::Run(action) => {
                if matches!(action, FileAction::Paste { cut: true, .. }) {
                    // A cut entry moves once; its old place is gone after.
                    self.file_menu.clip = None;
                }
                self.command(Command::FileAction { tab, action }, cx);
            }
            FileEffect::New { dir, folder } => {
                self.start_name_field(tab, NameEdit::New { dir, folder }, window, cx);
            }
            FileEffect::Rename(path) => {
                self.start_name_field(tab, NameEdit::Rename { path }, window, cx);
            }
            FileEffect::AskDelete(path) => {
                let folder = target.kind.is_dir();
                self.ask(Asked::DeleteFile { tab, path, folder }, window, cx);
            }
            FileEffect::Clip(clip) => {
                let verb = if clip.cut { "Cut" } else { "Copied" };
                let name = rel_name(&clip.path).to_owned();
                self.file_menu.clip = Some(clip);
                self.say(format!("{verb} {name}; Paste puts it in a folder."), cx);
            }
            FileEffect::CopyText(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.say("Copied the path.".to_owned(), cx);
            }
            FileEffect::CollapseAll => {
                if let Some((_, tree)) = self.shown_tree() {
                    tree.collapse_all();
                }
                self.scroll_to_cursor();
                cx.notify();
            }
        }
    }

    /// Runs `action` on the entry under the Files view's cursor: the
    /// palette's file commands (RF-NAV-02).
    pub(super) fn run_file_action_on_cursor(
        &mut self,
        action: FileMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.files_shown().map(|(tab, _)| tab);
        match (tab, self.cursor_target()) {
            (Some(tab), Some(target)) => self.run_file_action(tab, &target, action, window, cx),
            _ => self.say("Show the Files view to act on its files.".to_owned(), cx),
        }
    }

    /// The palette rows for what the Files view's cursor entry offers now.
    pub(super) fn file_palette_entries(
        &self,
        keys: &dyn Fn(&dyn Action) -> Option<String>,
    ) -> Vec<Entry> {
        let Some((_, _, facts)) = self.files_menu_facts() else {
            return Vec::new();
        };
        let Some(target) = self.cursor_target() else {
            return Vec::new();
        };
        palette_actions(&target, facts)
            .into_iter()
            .map(|action| Entry {
                text: action.title().to_owned(),
                dim: "files:".len(),
                hint: item_keys_action(action)
                    .and_then(|a| keys(a.as_ref()))
                    .unwrap_or_default(),
                state: None,
                target: Target::File(action),
                depth: 0,
            })
            .collect()
    }

    /// F2 in the Files view: renames the entry under the cursor.
    pub(super) fn file_tree_rename(
        &mut self,
        _: &FileTreeRename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_key(FileMenuAction::Rename, false, window, cx);
    }

    /// Delete in the Files view: moves the entry under the cursor to the
    /// trash. While the filter holds text, the key edits it instead.
    pub(super) fn file_tree_trash(
        &mut self,
        _: &FileTreeTrash,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_key(FileMenuAction::Trash, true, window, cx);
    }

    /// Shift+Delete in the Files view: asks, then deletes the entry under
    /// the cursor for good.
    pub(super) fn file_tree_delete(
        &mut self,
        _: &FileTreeDelete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_key(FileMenuAction::Delete, true, window, cx);
    }

    /// A key that runs `action` on the cursor's entry. The name field keeps
    /// its keys, and so does the filter while it holds text when
    /// `filter_keeps` (Delete edits text there).
    fn file_key(
        &mut self,
        action: FileMenuAction,
        filter_keeps: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filtering = self.shown_tree().is_some_and(|(_, t)| t.filtering());
        if self.file_menu.is_editing() || (filter_keeps && filtering) {
            cx.propagate();
            return;
        }
        let Some(tab) = self.files_shown().map(|(tab, _)| tab) else {
            return;
        };
        let Some(target) = self.cursor_target().filter(|t| !t.path.is_empty()) else {
            return;
        };
        self.run_file_action(tab, &target, action, window, cx);
    }

    /// Opens the name field in the tree for `edit`, holding the entry's name
    /// for a rename, and gives it the keyboard.
    fn start_name_field(
        &mut self,
        tab: TabId,
        edit: NameEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_name_field(false, window, cx);
        let was_filtering = self.shown_tree().is_some_and(|(_, t)| t.filtering());
        let (initial, selection) = match &edit {
            NameEdit::Rename { path } => {
                let name = rel_name(path).to_owned();
                let is_dir = self
                    .shown_tree()
                    .and_then(|(_, t)| t.kind_of(path))
                    .is_some_and(EntryKind::is_dir);
                let selection = rename_selection(&name, is_dir);
                (name, selection)
            }
            NameEdit::New { .. } => (String::new(), 0..0),
        };
        let Some((_, tree)) = self.shown_tree() else {
            return;
        };
        let requests = tree.start_edit(edit);
        self.list_folders(tab, requests, cx);
        self.sync_filter_text(was_filtering, window, cx);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        input.update(cx, |state, cx| state.set_selected_range(selection, cx));
        let input_events = cx.subscribe_in(&input, window, Self::on_name_field_input);
        window.focus(&input.focus_handle(cx), cx);
        self.file_menu.field = Some(NameField {
            tab,
            input,
            _input_events: input_events,
        });
        self.scroll_to_cursor();
        cx.notify();
    }

    /// Clicking away keeps the name typed, as Enter does, when it is one.
    fn on_name_field_input(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The keyboard stays where it went.
        if matches!(event, InputEvent::Blur) {
            self.end_name_field(true, cx);
        }
    }

    /// Ends the name field and gives the keyboard back to the Files view;
    /// `keep` sends the name typed.
    fn finish_name_field(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.end_name_field(keep, cx) {
            self.files_take_keyboard(window, cx);
        }
    }

    /// Ends the name field, if open; `keep` sends the name typed. A name
    /// that cannot be one is said in the status bar and nothing is sent.
    fn end_name_field(&mut self, keep: bool, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.file_menu.field.take() else {
            return false;
        };
        let typed = field.input.read(cx).value().to_string();
        let edit = self.files_tree(field.tab).and_then(|t| t.end_edit());
        if keep && let Some(edit) = edit {
            match named_action(&edit, &typed) {
                Ok(Some(action)) => self.command(
                    Command::FileAction {
                        tab: field.tab,
                        action,
                    },
                    cx,
                ),
                Ok(None) => {}
                Err(reason) => self.fail(reason, cx),
            }
        }
        cx.notify();
        true
    }

    /// Enter in the name field: sends the name, or keeps the field open
    /// and says why the name cannot be one.
    pub(super) fn file_name_confirm(
        &mut self,
        _: &FileNameConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(reason) = self.check_name_field(cx) {
            self.fail(reason, cx);
            return;
        }
        self.finish_name_field(true, window, cx);
    }

    /// Escape in the name field: nothing is made or renamed.
    pub(super) fn file_name_cancel(
        &mut self,
        _: &FileNameCancel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_name_field(false, window, cx);
    }

    fn check_name_field(&self, cx: &App) -> Result<(), String> {
        let Some(field) = &self.file_menu.field else {
            return Ok(());
        };
        let typed = field.input.read(cx).value().to_string();
        let Some(edit) = self
            .shown_tree_ref()
            .filter(|(tab, _)| *tab == field.tab)
            .and_then(|(_, t)| t.editing().cloned())
        else {
            return Ok(());
        };
        named_action(&edit, &typed).map(|_| ())
    }

    /// The name field, for the row that holds it.
    pub(super) fn name_field_input(&self) -> Option<&Entity<InputState>> {
        self.file_menu.field.as_ref().map(|f| &f.input)
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the gpui glob there would shadow `#[test]`.
    use super::{named_action, rename_selection};
    use crate::file_tree::NameEdit;
    use yien_proto::file_ops::FileAction;

    #[test]
    fn rf_ctx_01_a_rename_selects_the_name_without_its_extension() {
        assert_eq!(rename_selection("main.rs", false), 0..4);
        assert_eq!(rename_selection("archive.tar.gz", false), 0..11);
        assert_eq!(rename_selection(".gitignore", false), 0..10);
        assert_eq!(rename_selection("Makefile", false), 0..8);
        assert_eq!(rename_selection("v1.2", true), 0..4, "a folder: all of it");
    }

    #[test]
    fn rf_ctx_01_the_name_field_sends_a_valid_new_name_only() {
        let new = NameEdit::New {
            dir: "src".into(),
            folder: false,
        };
        assert_eq!(
            named_action(&new, "lib.rs"),
            Ok(Some(FileAction::NewFile {
                dir: "src".into(),
                name: "lib.rs".into()
            }))
        );
        let folder = NameEdit::New {
            dir: String::new(),
            folder: true,
        };
        assert_eq!(
            named_action(&folder, "docs"),
            Ok(Some(FileAction::NewFolder {
                dir: String::new(),
                name: "docs".into()
            }))
        );
        assert_eq!(named_action(&new, "  "), Ok(None), "empty: nothing");
        assert!(named_action(&new, "a/b").is_err());
        assert!(named_action(&new, ".git").is_err());
        let rename = NameEdit::Rename {
            path: "src/a.rs".into(),
        };
        assert_eq!(named_action(&rename, "a.rs"), Ok(None), "unchanged");
        assert_eq!(
            named_action(&rename, "b.rs"),
            Ok(Some(FileAction::Rename {
                path: "src/a.rs".into(),
                name: "b.rs".into()
            }))
        );
    }
}
