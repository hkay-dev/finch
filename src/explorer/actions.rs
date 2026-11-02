//! The explorer's file actions: new file, new folder, rename in the row, duplicate, move to
//! Trash, and Cmd-Z and Shift-Cmd-Z to take the last of them back and do it again.
//!
//! The disk work is `file_ops`; this file is the tree's side. Every action changes the tree in
//! place (the parent folder is re-read and its rows merged by path, so the watcher's batch that
//! follows finds nothing new) and tells the owner what happened to open tabs through
//! `ExplorerEvent::Renamed` and `ExplorerEvent::Gone`. A new item is made on disk at once under a
//! free name and its name field opens in the row, as Finder does; Escape keeps that name.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use super::{Explorer, ExplorerEvent, PLACEHOLDER_EMPTY, find, read_children, row};
use crate::{file_ops, motion, tokens::{COPY_QUIET, DROP_OPEN_DELAY, UNDO_STEPS}, workspace::UNSAVED_NAMES_SHOWN};

/// The name field open in a row.
pub(super) struct Editing {
    /// The row's path: the item being renamed, or the new item.
    pub path: PathBuf,
    /// Made a moment ago and named now; once named it opens (a file) in a tab.
    new: bool,
    pub input: Entity<InputState>,
    /// What is wrong with the typed name, shown under the field.
    pub error: Option<SharedString>,
    /// The field has had the keyboard, so losing it means the user left.
    focused: bool,
    /// Text dropped to make this file: written when the name settles, and the file is taken away again on Escape.
    text: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// What Cmd-Z in the explorer takes back, most recent last.
pub(super) enum Made {
    Created(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    /// Dropped into another folder.
    Moved { from: PathBuf, to: PathBuf },
    Duplicated(PathBuf),
    Trashed { original: PathBuf, trashed: PathBuf },
}

/// What Shift-Cmd-Z in the explorer does again: a `Made` step that Cmd-Z took back.
pub(super) enum Undone {
    Renamed { from: PathBuf, to: PathBuf },
    Moved { from: PathBuf, to: PathBuf },
    /// Put back from the Trash: goes to the Trash again.
    Trashed(PathBuf),
    /// A new item (or a copy, `copy`) taken away: made again empty, or brought back from the
    /// Trash (`kept`) when it held something and `file_ops::discard` put it there.
    Removed { path: PathBuf, copy: bool, is_dir: bool, kept: Option<PathBuf> },
}

/// The undo list (`Made` steps) and the redo list (`Undone` steps), most recent last. A new action
/// empties the redo list, as in the editor; together they hold at most `UNDO_STEPS`, the oldest let
/// go of first. A new root empties both (`Explorer::set_root`), since their paths belong to the
/// old tree.
#[derive(Default)]
pub(super) struct Steps {
    done: Vec<Made>,
    undone: Vec<Undone>,
}

impl Steps {
    /// A new action.
    pub fn push(&mut self, step: Made) {
        self.undone.clear();
        self.redone(step);
    }

    /// A step done again by a redo: the redo list stays.
    pub fn redone(&mut self, step: Made) {
        if self.done.len() >= UNDO_STEPS {
            self.done.remove(0);
        }
        self.done.push(step);
    }

    /// A step taken back by an undo, for a redo to do again.
    pub fn undid(&mut self, step: Undone) {
        self.undone.push(step);
    }

    pub fn retain(&mut self, keep: impl FnMut(&Made) -> bool) {
        self.done.retain(keep);
    }

    pub fn last(&self) -> Option<&Made> {
        self.done.last()
    }

    pub fn pop(&mut self) -> Option<Made> {
        self.done.pop()
    }

    pub fn last_undone(&self) -> Option<&Undone> {
        self.undone.last()
    }

    pub fn pop_undone(&mut self) -> Option<Undone> {
        self.undone.pop()
    }

    /// Takes the most recent redo step that `which` picks out of the redo list.
    pub fn take_undone(&mut self, which: impl Fn(&Undone) -> bool) -> Option<Undone> {
        let at = self.undone.iter().rposition(which)?;
        Some(self.undone.remove(at))
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}

/// The files on the pasteboard (a Finder copy, or Cmd-C in the tree), if any.
pub(super) fn clipboard_files(cx: &App) -> Option<Vec<PathBuf>> {
    files_in(&cx.read_from_clipboard()?)
}

/// The files a pasteboard item holds.
fn files_in(item: &ClipboardItem) -> Option<Vec<PathBuf>> {
    item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
        _ => None,
    })
}

/// The folder a paste goes into: `at` when it is a folder, the folder of a file, else the root.
fn target_folder(at: Option<PathBuf>, root: &Path) -> PathBuf {
    match at {
        Some(path) if path.is_dir() => path,
        Some(path) => path.parent().map_or_else(|| root.to_path_buf(), Path::to_path_buf),
        None => root.to_path_buf(),
    }
}

/// Where `copy_in_background` puts each copy.
enum CopyTo {
    /// Next to the original, as `name copy` (Duplicate).
    Beside,
    /// Into a folder it was dropped on, as `name 2` on a clash.
    Dropped(PathBuf),
    /// Into a folder it was pasted in, as `name copy` on a clash.
    Pasted(PathBuf),
}

impl Explorer {
    fn alert(&self, title: &str, detail: &str, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window.prompt(PromptLevel::Critical, title, Some(detail), &["OK"], cx);
    }

    /// Re-reads `dir` if the tree has it loaded, merging the rows by path, and syncs the tree.
    fn refresh(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if self.merge_dir(dir, cx) {
            self.sync(cx);
        }
    }

    /// `refresh` without the sync, for callers that merge several folders and sync once (`select`
    /// does). Says whether `dir` was loaded.
    fn merge_dir(&mut self, dir: &Path, cx: &mut Context<Self>) -> bool {
        let loaded = dir == self.root || self.loaded.contains(dir);
        if loaded {
            let kids = read_children(dir, self.listing(cx));
            self.merge_children(dir, kids);
        }
        loaded
    }

    /// `refresh` after the row of `path` was removed from `dir`: when the tree's selection went with
    /// it (the row itself, or something under it), the selection moves to the row now at its index,
    /// else the last one, and glides there. Nothing opens or previews: `set_selected_index` emits
    /// no event.
    fn refresh_removed(&mut self, dir: &Path, path: &Path, cx: &mut Context<Self>) {
        let id = SharedString::from(path.to_string_lossy().into_owned());
        let place = {
            let tree = self.tree.read(cx);
            tree.selected_index().and_then(|_| tree.index_of(&id))
        };
        self.refresh(dir, cx);
        let Some(place) = place.filter(|_| !self.filter.is_active()) else { return };
        self.tree.update(cx, |tree, cx| {
            if tree.selected_index().is_some() {
                return;
            }
            let last = (0..).take_while(|ix| tree.entry(*ix).is_some()).count().saturating_sub(1);
            let near = (0..=place.min(last)).rev().find(|ix| tree.entry(*ix).is_some_and(|entry| !entry.is_disabled()));
            tree.set_selected_index(near, cx);
        });
    }

    /// Makes the row of `path` appear (its folders read and opened, its folder re-read), selected.
    fn show(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path == self.root || !path.starts_with(&self.root) {
            return;
        }
        let dirs = self.load_to(path, cx);
        if let Some(dir) = path.parent() {
            self.merge_dir(dir, cx);
        }
        self.select_settling(&dirs, path, cx);
    }

    /// `show` for a row that is new (a new file or folder, a copy, a put-back): it opens out of a
    /// gap, the rows below sliding down, and washes in as an arrival does when `wash` is set. It is
    /// selected when `select` is set (a batch selects only its first row).
    fn show_arrived(&mut self, path: &Path, wash: bool, select: bool, cx: &mut Context<Self>) {
        if path == self.root || !path.starts_with(&self.root) {
            return;
        }
        let (mut found, mut grown) = (Vec::new(), Vec::new());
        if let Some(dir) = path.parent().filter(|dir| *dir == self.root || self.loaded.contains(*dir)) {
            let swaps = self.children_mut(dir).is_some_and(|slot| matches!(slot.as_slice(), [only] if only.id.ends_with(PLACEHOLDER_EMPTY)));
            found = self.merge_children(dir, read_children(dir, self.listing(cx)));
            if !swaps {
                grown = found.clone();
            }
        }
        if wash {
            row::arrived(self.window_id, found);
        }
        let dirs = self.load_to(path, cx);
        if select {
            self.select_settling(&dirs, path, cx);
        } else {
            self.expand(&dirs, cx);
        }
        self.open_gaps(grown, cx);
    }

    /// `from` is now at `to`: the tree follows (an open folder stays open), and the owner moves
    /// its tabs. The old row closes its gap as a trashed one does, a shut folder it went into folds
    /// open, and the new row opens out of a gap and washes in; a rename that leaves the row in its
    /// place does none of that, and says so (true) so an undo or redo can flash it. The new row is
    /// selected when `select` is set (a batch selects only its first row).
    fn moved(&mut self, from: &Path, to: &Path, select: bool, cx: &mut Context<Self>) -> bool {
        let was_open = find(&self.items, from).is_some_and(|item| item.is_expanded());
        let old_ix = self.tree.read(cx).index_of(&SharedString::from(from.to_string_lossy().into_owned()));
        let same_dir = from.parent() == to.parent();
        let swaps = to.parent().is_some_and(|dir| self.children_mut(dir).is_some_and(|slot| matches!(slot.as_slice(), [only] if only.id.ends_with(PLACEHOLDER_EMPTY))));
        let mut found = Vec::new();
        for dir in [from.parent(), to.parent().filter(|_| !same_dir)].into_iter().flatten() {
            if dir == self.root || self.loaded.contains(dir) {
                let kids = read_children(dir, self.listing(cx));
                found.extend(self.merge_children(dir, kids));
            }
        }
        let dirs = self.load_to(to, cx);
        // A folder that was open stays open as it moves, its rows in with it.
        let folder_opens = was_open && to.is_dir();
        if folder_opens {
            let kids = read_children(to, self.listing(cx));
            self.merge_children(to, kids);
            if let Some(item) = find(&self.items, to) {
                item.clone().expanded(true);
            }
        }
        let shut = dirs.iter().filter_map(|dir| find(&self.items, dir)).any(|item| !self.is_open(&item.id, item.is_expanded()));
        let ghosts = self.folds.borrow().ghosts.len();
        // The merges queued the old row as gone: this sync turns it into a closing ghost.
        self.expand_folding(&dirs, cx);
        let to_id = SharedString::from(to.to_string_lossy().into_owned());
        let in_place = same_dir && !shut && old_ix.is_some() && old_ix == self.tree.read(cx).index_of(&to_id);
        if in_place {
            self.folds.borrow_mut().ghosts.truncate(ghosts);
        } else {
            found.push(to.to_path_buf());
            row::arrived(self.window_id, found.clone());
        }
        if select {
            self.select_settling(&[], to, cx);
        }
        if !in_place && !shut && !swaps && !folder_opens {
            self.open_gaps(found, cx);
        }
        cx.emit(ExplorerEvent::Renamed { from: from.to_path_buf(), to: to.to_path_buf() });
        in_place
    }

    /// An undo or redo of a rename that stays in its row: the row flashes once, the way a copy of
    /// the selected row does.
    fn flash_in_place(&mut self, in_place: bool, cx: &mut Context<Self>) {
        if in_place {
            row::flash(self.window_id, motion::drawn_now(self.window_id));
            self.shapes.update(cx, |_, cx| cx.notify());
        }
    }

    /// Makes a new file or folder in `at` (or in the folder of `at`, when that is a file) and
    /// opens its name field. With `name`, it is named at once, as typing it and pressing Return.
    pub fn new_item(&mut self, at: &Path, is_dir: bool, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let dir = if at.is_dir() { at.to_path_buf() } else { at.parent().map_or_else(|| self.root.clone(), Path::to_path_buf) };
        if dir != self.root && !dir.starts_with(&self.root) {
            return Err(format!("not below the explorer root {}", self.root.display()));
        }
        self.end_edit(window, cx);
        if self.filter.is_active() {
            self.escape_filter(window, cx);
        }
        let default = if is_dir { file_ops::NEW_FOLDER_NAME } else { file_ops::NEW_FILE_NAME };
        let path = file_ops::create(&dir, default, is_dir).inspect_err(|err| self.alert("Couldn’t create it", err, window, cx))?;
        self.show_arrived(&path, false, true, cx);
        self.made.push(Made::Created(path.clone()));
        self.begin_edit(path, true, window, cx);
        match name {
            Some(name) => self.commit_named(name, window, cx),
            None => Ok(()),
        }
    }

    /// Text dropped on a folder (`dir`, or the root): makes an empty file there named from the text's
    /// first line and opens its name field on it. Return writes the text and opens the file in a
    /// tab (`end_edit`); Escape takes the file away again (`cancel_edit`).
    pub(super) fn drop_text(&mut self, dir: PathBuf, dropped: crate::text_drag::DraggedText, window: &mut Window, cx: &mut Context<Self>) {
        self.set_drop_target(None, cx);
        if dropped.text.trim().is_empty() || (dir != self.root && !dir.starts_with(&self.root)) {
            return;
        }
        self.end_edit(window, cx);
        if self.filter.is_active() {
            self.escape_filter(window, cx);
        }
        let name = crate::text_drag::file_name(&dropped.text, dropped.ext.as_deref());
        let Ok(path) = file_ops::create(&dir, &name, false).inspect_err(|err| self.alert("Couldn’t create it", err, window, cx)) else { return };
        self.show_arrived(&path, false, true, cx);
        self.made.push(Made::Created(path.clone()));
        self.begin_edit(path, true, window, cx);
        if let Some(edit) = self.editing.as_mut() {
            edit.text = Some(dropped.text);
        }
    }

    /// Escape in a name field. A file made from dropped text is taken away again, unwritten; any
    /// other field keeps the name as it is on disk.
    pub(super) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = self.editing.as_mut().filter(|edit| edit.text.is_some()) {
            edit.text = None;
            let path = edit.path.clone();
            self.made.retain(|step| !matches!(step, Made::Created(made) if *made == path));
            match file_ops::discard(&path) {
                Ok(_) => {
                    if let Some(dir) = path.parent() {
                        self.refresh_removed(dir, &path, cx);
                    }
                    cx.emit(ExplorerEvent::Gone(path));
                }
                Err(err) => self.alert("Couldn’t remove it", &err, window, cx),
            }
        }
        self.end_edit(window, cx);
    }

    /// Opens the name field in the row of `path`. With `name`, renames at once.
    pub fn rename_path(&mut self, path: &Path, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        self.below_root(path)?;
        if !path.exists() {
            return Err(format!("no such file: {}", path.display()));
        }
        self.end_edit(window, cx);
        self.show(path, cx);
        self.begin_edit(path.to_path_buf(), false, window, cx);
        match name {
            Some(name) => self.commit_named(name, window, cx),
            None => Ok(()),
        }
    }

    /// Types `name` in the open field and presses Return; the error under the field if it refuses.
    fn commit_named(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if let Some(edit) = &self.editing {
            edit.input.update(cx, |input, cx| input.set_value(name.to_string(), window, cx));
        }
        self.commit_edit(window, cx);
        match self.editing.as_ref().and_then(|edit| edit.error.clone()) {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    }

    fn begin_edit(&mut self, path: PathBuf, new: bool, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        // The name without its extension is selected, so typing replaces just that.
        let stem = if path.is_dir() { name.len() } else { name.rfind('.').filter(|&dot| dot > 0).unwrap_or(name.len()) };
        let opened_with = name.clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(edit) = this.editing.as_mut().filter(|edit| edit.error.is_some()) {
                        edit.error = None;
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => this.commit_edit(window, cx),
                InputEvent::Focus => {
                    if let Some(edit) = this.editing.as_mut() {
                        edit.focused = true;
                    }
                }
                InputEvent::Blur => {
                    if this.editing.as_ref().is_some_and(|edit| edit.focused) {
                        // Leaving the field keeps a good name; a refused one is dropped.
                        this.commit_edit(window, cx);
                        this.end_edit(window, cx);
                    }
                }
            }),
            // The field draws inside this view, which is cached; its caret blinks in the explorer's caret layer.
            cx.observe(&input, |_, _, cx| cx.notify()),
        ];
        self.carets.update(cx, |carets, cx| carets.set_name_field(Some(&input), cx));
        self.editing = Some(Editing { path, new, input: input.clone(), error: None, focused: false, text: None, _subscriptions: subscriptions });
        // The field is drawn first, then it takes the keyboard. Two frames on, as `show_menu` waits.
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                input.update(cx, |input, cx| {
                    input.focus(window, cx);
                    // A name typed or filled in before these two frames passed stays as it is, not
                    // under the old stem's selection.
                    if input.value().to_string() == opened_with {
                        input.set_selected_range(0..stem, cx);
                    }
                });
            });
        });
        cx.notify();
    }

    /// Return in the field: renames to what is typed. A name that is taken, empty or invalid
    /// shows why under the field and keeps it open.
    pub(super) fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.as_ref() else { return };
        let name = edit.input.read(cx).value().to_string();
        let (from, new, holds_text) = (edit.path.clone(), edit.new, edit.text.is_some());
        if holds_text && name.ends_with('/') {
            if let Some(edit) = self.editing.as_mut() {
                edit.error = Some("That file has text in it, so it can’t become a folder.".into());
            }
            cx.notify();
            return;
        }
        if from.file_name().is_some_and(|current| *current == *name) {
            self.end_edit(window, cx);
            return;
        }
        let busy = if new { Ok(()) } else { file_ops::refuse_busy(window, std::slice::from_ref(&from), false, cx) };
        let named = match busy {
            Err(why) => Err(why),
            Ok(()) if new => file_ops::name_new(&from, &name),
            Ok(()) => file_ops::rename(&from, &name),
        };
        match named {
            Ok(to) if to == from => self.end_edit(window, cx),
            Ok(to) => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.path = to.clone();
                }
                self.moved(&from, &to, true, cx);
                self.made.push(Made::Renamed { from, to });
                self.end_edit(window, cx);
            }
            Err(error) => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.error = Some(error.into());
                }
                cx.notify();
            }
        }
    }

    /// Closes the name field with the name as it stands on disk, and gives the keyboard back to
    /// the tree. A new file opens in a tab once its name is settled.
    pub(super) fn end_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.take() else { return };
        self.carets.update(cx, |carets, cx| carets.set_name_field(None, cx));
        self.tree.update(cx, |tree, cx| tree.focus(window, cx));
        if let Some(text) = edit.text.as_ref().filter(|_| edit.path.is_file()) {
            if let Err(err) = std::fs::write(&edit.path, text) {
                self.alert(&file_ops::failed_title("save", &[edit.path.clone()]), &file_ops::explain("save", &edit.path, &err), window, cx);
            }
        }
        if edit.new && edit.path.is_file() {
            cx.emit(ExplorerEvent::Open(edit.path));
        }
        cx.notify();
    }

    /// Whether a name field is open.
    pub(super) fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// A name or the filter is being typed, so a window key leaves the files alone.
    pub fn is_typing(&self) -> bool {
        self.filter.focused() || self.is_editing()
    }

    /// Makes `name copy.ext` next to each of `paths` and selects the first copy.
    pub fn duplicate_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        for path in &paths {
            self.below_root(path)?;
        }
        self.copy_in_background(paths, CopyTo::Beside, "duplicate", window, cx);
        Ok(())
    }

    /// Cmd-C with the tree focused: the picked rows' files (else the selected row's) go on the
    /// pasteboard as a Finder copy does, so Finder pastes them and a text field gets their paths.
    /// False when no row is selected.
    pub fn copy_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let paths = self.picked_paths(cx);
        if paths.is_empty() {
            return false;
        }
        self.copy_files(paths, cx);
        true
    }

    /// Puts the files and folders at `paths` on the pasteboard. When the selected row is one of
    /// them, the selection flashes once, the way a jump to a row in view does.
    pub fn copy_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let selected = self.selected_path(cx).is_some_and(|selected| paths.contains(&selected));
        cx.write_to_clipboard(ClipboardItem { entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(paths.into()))] });
        // Edit > Move item here turns on at once.
        cx.defer(crate::menu::refresh);
        if selected {
            row::flash(self.window_id, motion::drawn_now(self.window_id));
            self.shapes.update(cx, |_, cx| cx.notify());
        }
    }

    /// Cmd-V with the tree focused: the files on the pasteboard are copied into the folder at `at`
    /// (the folder of a file; `None` is the selected row, else the root), each under its own name or
    /// as `name copy`, and the rows that appear wash in. False when the pasteboard holds no files.
    pub fn paste_files(&mut self, at: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((paths, dir)) = self.clipboard_target(at, window, cx) else { return false };
        self.copy_in_background(paths, CopyTo::Pasted(dir), "copy", window, cx);
        true
    }

    /// Option-Cmd-V with the tree focused, Finder's Move item here: the files on the pasteboard are
    /// moved into the folder at `at` (as `paste_files` picks it) the way a drag moves them, so a name
    /// taken there is refused with an alert, open tabs follow, and Cmd-Z puts each file back. False
    /// when the pasteboard holds no files or the folder lies outside the tree.
    pub fn move_files_here(&mut self, at: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((paths, dir)) = self.clipboard_target(at, window, cx) else { return false };
        self.drop_onto(paths, dir, false, window, cx);
        true
    }

    /// The pasteboard's files and the folder they go to, with any name field or filter closed. `None`
    /// when the pasteboard holds no files or the folder lies outside the tree.
    fn clipboard_target(&mut self, at: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Option<(Vec<PathBuf>, PathBuf)> {
        let paths = clipboard_files(cx)?;
        let dir = target_folder(at.or_else(|| self.selected_path(cx)), &self.root);
        if dir != self.root && !dir.starts_with(&self.root) {
            return None;
        }
        self.end_edit(window, cx);
        if self.filter.is_active() {
            self.escape_filter(window, cx);
        }
        Some((paths, dir))
    }

    /// Copies `paths` on the background executor, so a big folder never stalls the window: each
    /// one next to itself, or into a folder (`to`). Copies run one at a time, so two never race
    /// for one free name. As it lands the tree shows it and Cmd-Z can take it back; the reasons
    /// for any that failed come back joined in one alert. A copy still running after `COPY_QUIET`
    /// makes its source rows' names breathe until it lands.
    fn copy_in_background(&mut self, paths: Vec<PathBuf>, to: CopyTo, doing: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        static COPY_LANE: Mutex<()> = Mutex::new(());
        let sources = paths.clone();
        row::copy_begins(self.window_id, &sources);
        let (window_id, quiet) = (self.window_id, sources.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPY_QUIET).await;
            this.update(cx, |this, cx| {
                if row::copy_runs_long(window_id, &quiet) {
                    this.tree.update(cx, |_, cx| cx.notify());
                }
            })
            .ok();
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move {
                    let _one_at_a_time = COPY_LANE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    paths
                        .into_iter()
                        .map(|from| {
                            let result = match &to {
                                CopyTo::Beside => file_ops::duplicate(&from),
                                CopyTo::Dropped(dir) => file_ops::copy_into(&from, dir),
                                CopyTo::Pasted(dir) => file_ops::paste_into(&from, dir),
                            };
                            (from, result)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let (mut failed, mut errors, mut first) = (Vec::new(), Vec::new(), true);
                for (from, result) in done {
                    match result {
                        Ok(copy) => {
                            this.show_arrived(&copy, true, std::mem::take(&mut first), cx);
                            this.made.push(Made::Duplicated(copy));
                        }
                        Err(error) => {
                            failed.push(from);
                            errors.push(error);
                        }
                    }
                }
                if !errors.is_empty() {
                    this.alert(&file_ops::failed_title(doing, &failed), &errors.join("\n"), window, cx);
                }
                // The sources' rows settle to rest; the tree redraws once to start it.
                row::copy_lands(this.window_id, &sources);
                this.tree.update(cx, |_, cx| cx.notify());
            })
            .ok();
        })
        .detach();
    }

    /// Moves `paths` to the Trash (an item inside another one of them goes with it). Asks once
    /// first, only when an open tab of them, or under them, has unsaved edits.
    pub fn trash_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        for path in &paths {
            self.below_root(path)?;
        }
        let paths = outermost(paths);
        let title = match paths.as_slice() {
            [one] => {
                let name = one.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                format!("Move \u{201C}{name}\u{201D} to the Trash?")
            }
            many => format!("Move {} items to the Trash?", many.len()),
        };
        let targets = paths.clone();
        self.confirm_unsaved(&targets, title, "Move to Trash", window, cx, move |this, window, cx| {
            let (mut failed, mut errors) = (Vec::new(), Vec::new());
            for path in paths {
                match file_ops::trash(&path) {
                    Ok(trashed) => {
                        if let Some(dir) = path.parent() {
                            this.refresh_removed(dir, &path, cx);
                        }
                        this.made.push(Made::Trashed { original: path.clone(), trashed });
                        cx.emit(ExplorerEvent::Gone(path));
                    }
                    Err(err) => {
                        failed.push(path);
                        errors.push(err);
                    }
                }
            }
            if !errors.is_empty() {
                this.alert(&format!("{} to the Trash", file_ops::failed_title("move", &failed)), &errors.join("\n"), window, cx);
            }
        })
    }

    /// Runs `then` now, or after the user agrees when an open tab of `paths`, or under them, has
    /// unsaved edits that `then` would lose. A path with a save still writing, or the draft file
    /// of an open Untitled page, is refused instead (told in an alert, and returned), both before
    /// asking and again after the answer, since either can come about while the question is up.
    fn confirm_unsaved(
        &mut self,
        paths: &[PathBuf],
        title: String,
        action: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Result<(), String> {
        let refused = if action == "Remove" { "Can’t remove it" } else { "Can’t move it to the Trash" };
        if let Err(why) = file_ops::refuse_busy(window, paths, true, cx) {
            self.alert(refused, &why, window, cx);
            return Err(why);
        }
        let mut unsaved = Vec::new();
        for path in paths {
            unsaved.extend(file_ops::under(&self.marked, path));
            unsaved.extend(file_ops::unsaved_elsewhere(window.window_handle(), path, cx));
        }
        if unsaved.is_empty() {
            then(self, window, cx);
            return Ok(());
        }
        let names: Vec<String> = unsaved.iter().filter_map(|path| path.file_name()).map(|name| name.to_string_lossy().into_owned()).collect();
        let detail = unsaved_detail(&names, action);
        // Cancel is first, so it keeps Return; nothing answers Escape (gpui-editor#s3np). The
        // action loses the unsaved edits, so it is the red button.
        let buttons = [PromptButton::cancel("Cancel"), PromptButton::destructive(action)];
        let answer = window.prompt(PromptLevel::Warning, &title, Some(&detail), &buttons, cx);
        let paths = paths.to_vec();
        cx.spawn_in(window, async move |this, cx| {
            if matches!(answer.await, Ok(1)) {
                this.update_in(cx, |this, window, cx| match file_ops::refuse_busy(window, &paths, true, cx) {
                    Ok(()) => then(this, window, cx),
                    Err(why) => this.alert(refused, &why, window, cx),
                })
                .ok();
            }
        })
        .detach();
        Ok(())
    }

    /// Whether `dir` is a folder of the tree that is shut.
    fn is_shut_folder(&self, dir: &Path) -> bool {
        let state = find(&self.items, dir).filter(|item| item.is_folder()).map(|item| (item.id.clone(), item.is_expanded()));
        state.is_some_and(|(id, expanded)| !self.is_open(&id, expanded))
    }

    /// Files are being dragged over `target` (a folder, the root, or nothing). A shut folder opens
    /// after the pointer has rested on it for `DROP_OPEN_DELAY`.
    pub(super) fn set_drop_target(&mut self, target: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.drop_target == target {
            return;
        }
        self.drop_target = target.clone();
        match target {
            None => self.drop_timer = None,
            // The root's claim comes every frame behind a row's; it leaves the timer be.
            Some(dir) if dir == self.root => {}
            Some(dir) => {
                if self.drop_timer.as_ref().is_some_and(|(timed, _)| *timed == dir) || !self.is_shut_folder(&dir) {
                    cx.notify();
                    return;
                }
                let target = dir.clone();
                self.drop_timer = Some((
                    dir,
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(DROP_OPEN_DELAY).await;
                        this.update(cx, |this, cx| {
                            if this.drop_target.as_ref() == Some(&target) && cx.has_active_drag() && this.is_shut_folder(&target) {
                                this.toggle_path(&target, cx).ok();
                            }
                        })
                        .ok();
                    }),
                ));
            }
        }
        cx.notify();
    }

    /// Files are being dragged at `position` over the tree whose bounds are `bounds`: near its top
    /// or bottom edge the tree scrolls (`edge_scroll`), faster the nearer the edge.
    pub(super) fn set_edge_pull(&mut self, position: Point<Pixels>, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let speed = if bounds.contains(&position) { edge_speed(f32::from(position.y - bounds.origin.y), f32::from(bounds.size.height)) } else { 0. };
        let before = self.edge_pull.map_or(0., |(_, speed)| speed);
        self.edge_pull = (speed != 0.).then_some((position, speed));
        if speed == 0. {
            self.edge_at = None;
        }
        if speed != before {
            cx.notify();
        }
    }

    /// One display frame of the edge scroll, called from `render`: moves the list's offset by the
    /// band's speed over the time since the last frame and asks for the next frame, until the drag
    /// ends, the pointer leaves the band or the list reaches its end. The pointer is replayed once
    /// the frame is drawn, so the row now under it claims the drop.
    pub(super) fn edge_scroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pointer, speed)) = self.edge_pull else { return };
        if !cx.has_active_drag() {
            self.edge_pull = None;
            self.edge_at = None;
            return;
        }
        let now = crate::motion::drawn_now(self.window_id);
        let seconds = self.edge_at.map_or(0., |at| now.saturating_duration_since(at).as_secs_f32().min(0.05));
        self.edge_at = Some(now);
        let scroll = self.tree.read(cx).scroll_handle().clone();
        let edge = crate::scrollbar::Edge::of(&scroll, 0.);
        let to = (edge.scrolled + speed * seconds).clamp(0., edge.max);
        let at_end = if speed < 0. { to <= 0. } else { to >= edge.max };
        if to != edge.scrolled {
            self.glide.borrow_mut().cancel();
            let base = scroll.0.borrow().base_handle.clone();
            base.set_offset(point(base.offset().x, px(-to)));
            let modifiers = window.modifiers();
            window.on_next_frame(move |window, cx| {
                let moved = MouseMoveEvent { position: pointer, pressed_button: Some(MouseButton::Left), modifiers };
                window.dispatch_event(PlatformInput::MouseMove(moved), cx);
            });
        }
        if !at_end || seconds == 0. {
            window.request_animation_frame();
        } else {
            self.edge_at = None;
        }
    }

    /// Files dropped on the tree: moved into `dir`, or copied there with `copy` (Option held; files
    /// from outside are always copies). What can't go is refused with the reason, the rest goes.
    pub(super) fn drop_onto(&mut self, paths: Vec<PathBuf>, dir: PathBuf, copy: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Dropping a row on the folder it is in, or a folder on itself, moves nothing; an item
        // inside another dragged folder goes with that folder.
        let paths = if copy { paths } else { outermost(paths).into_iter().filter(|path| path.parent() != Some(dir.as_path()) && *path != dir).collect() };
        if paths.is_empty() {
            self.set_drop_target(None, cx);
            return;
        }
        if let Err((failed, why)) = self.drop_files(paths, dir, copy, window, cx) {
            self.alert(&file_ops::failed_title(if copy { "copy" } else { "move" }, &failed), &why, window, cx);
        }
    }

    /// Move to…: asks for a folder, then moves `paths` into it; what can't go is refused with
    /// the reason.
    pub(super) fn move_to_picked_folder(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Move".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(dirs))) = picked.await else { return };
            let Some(dir) = dirs.into_iter().next() else { return };
            this.update_in(cx, |this, window, cx| {
                if let Err((failed, why)) = this.move_paths(paths, dir, window, cx) {
                    this.alert(&file_ops::failed_title("move", &failed), &why, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Moves `paths` into the folder `dir` (an item inside another of them goes with it). Inside
    /// the tree it is a drop on that folder; outside it, the rows leave and open tabs follow.
    /// Each move joins the Cmd-Z list; the ones that can't go come back with the reasons joined.
    pub fn move_paths(&mut self, paths: Vec<PathBuf>, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Result<(), (Vec<PathBuf>, String)> {
        // The tree's root is canonical (`/private/tmp`, not `/tmp`).
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let paths: Vec<PathBuf> = outermost(paths).into_iter().filter(|path| path.parent() != Some(dir.as_path()) && *path != dir).collect();
        if paths.is_empty() {
            return Ok(());
        }
        if dir == self.root || dir.starts_with(&self.root) {
            return self.drop_files(paths, dir, false, window, cx);
        }
        if !dir.is_dir() {
            return Err((paths, format!("not a folder: {}", dir.display())));
        }
        self.end_edit(window, cx);
        if self.filter.is_active() {
            self.escape_filter(window, cx);
        }
        let (mut failed, mut reasons) = (Vec::new(), Vec::new());
        for from in paths {
            if let Err(reason) = file_ops::refuse_busy(window, std::slice::from_ref(&from), false, cx) {
                failed.push(from);
                reasons.push(reason);
                continue;
            }
            match file_ops::move_into(&from, &dir) {
                Ok(to) => {
                    if let Some(parent) = from.parent() {
                        self.refresh_removed(parent, &from, cx);
                    }
                    self.made.push(Made::Moved { from: from.clone(), to: to.clone() });
                    cx.emit(ExplorerEvent::Renamed { from, to });
                }
                Err(reason) => {
                    failed.push(from);
                    reasons.push(reason);
                }
            }
        }
        if failed.is_empty() { Ok(()) } else { Err((failed, reasons.join("\n"))) }
    }

    /// Moves `paths` into the folder `dir`, or copies them there with `copy`. Every item that can
    /// go goes (the tree updates in place, open tabs follow a move, each step joins the Cmd-Z
    /// list); the ones that can't come back with the reasons joined.
    pub fn drop_files(&mut self, paths: Vec<PathBuf>, dir: PathBuf, copy: bool, window: &mut Window, cx: &mut Context<Self>) -> Result<(), (Vec<PathBuf>, String)> {
        self.set_drop_target(None, cx);
        if dir != self.root && !dir.starts_with(&self.root) {
            return Err((paths, format!("not below the explorer root {}", self.root.display())));
        }
        if !dir.is_dir() {
            return Err((paths, format!("not a folder: {}", dir.display())));
        }
        self.end_edit(window, cx);
        if self.filter.is_active() {
            self.escape_filter(window, cx);
        }
        if copy {
            // Refusals come back now; only what goes ahead is copied in the background.
            let (mut allowed, mut failed, mut reasons) = (Vec::new(), Vec::new(), Vec::new());
            for from in paths {
                match file_ops::check_copy_into(&from, &dir) {
                    Ok(()) => allowed.push(from),
                    Err(reason) => {
                        failed.push(from);
                        reasons.push(reason);
                    }
                }
            }
            if !allowed.is_empty() {
                self.copy_in_background(allowed, CopyTo::Dropped(dir), "copy", window, cx);
            }
            return if failed.is_empty() { Ok(()) } else { Err((failed, reasons.join("\n"))) };
        }
        let (mut failed, mut reasons, mut first) = (Vec::new(), Vec::new(), true);
        for from in paths {
            if let Err(reason) = file_ops::refuse_busy(window, std::slice::from_ref(&from), false, cx) {
                failed.push(from);
                reasons.push(reason);
                continue;
            }
            match file_ops::move_into(&from, &dir) {
                Ok(to) => {
                    self.moved(&from, &to, std::mem::take(&mut first), cx);
                    self.made.push(Made::Moved { from, to });
                }
                Err(reason) => {
                    failed.push(from);
                    reasons.push(reason);
                }
            }
        }
        if failed.is_empty() { Ok(()) } else { Err((failed, reasons.join("\n"))) }
    }

    /// Cmd-Z in the explorer: takes back the last new item, rename, copy or trashing, for
    /// Shift-Cmd-Z to do again. Says whether there was one.
    pub fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.end_edit(window, cx);
        // Taking back a new item or a copy removes it, which may ask first: the step is spent
        // only when the removal runs, so Cancel leaves it for the next Cmd-Z.
        if let Some(step @ (Made::Created(path) | Made::Duplicated(path))) = self.made.last() {
            let copy = matches!(step, Made::Duplicated(_));
            let path = path.clone();
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            let target = path.clone();
            let _ = self.confirm_unsaved(std::slice::from_ref(&target), format!("Remove \u{201C}{name}\u{201D}?"), "Remove", window, cx, move |this, window, cx| {
                this.made.retain(|step| !matches!(step, Made::Created(made) | Made::Duplicated(made) if *made == path));
                let is_dir = path.is_dir();
                match file_ops::discard(&path) {
                    Ok(kept) => {
                        if let Some(dir) = path.parent() {
                            this.refresh_removed(dir, &path, cx);
                        }
                        this.made.undid(Undone::Removed { path: path.clone(), copy, is_dir, kept });
                        cx.emit(ExplorerEvent::Gone(path));
                    }
                    Err(err) => this.alert("Couldn’t undo", &err, window, cx),
                }
            });
            return true;
        }
        if let Some(Made::Renamed { to, .. } | Made::Moved { to, .. }) = self.made.last() {
            if let Err(why) = file_ops::refuse_busy(window, std::slice::from_ref(to), false, cx) {
                self.alert("Can’t undo it yet", &why, window, cx);
                return true;
            }
        }
        let Some(step) = self.made.pop() else { return false };
        match step {
            Made::Created(_) | Made::Duplicated(_) => {}
            Made::Renamed { from, to } => match file_ops::move_to(&to, &from) {
                Ok(()) => {
                    let in_place = self.moved(&to, &from, true, cx);
                    self.flash_in_place(in_place, cx);
                    self.made.undid(Undone::Renamed { from, to });
                }
                Err(err) => self.alert("Couldn’t undo the rename", &err, window, cx),
            },
            Made::Moved { from, to } => match file_ops::move_to(&to, &from) {
                Ok(()) => {
                    self.moved(&to, &from, true, cx);
                    self.made.undid(Undone::Moved { from, to });
                }
                Err(err) => self.alert("Couldn’t undo the move", &err, window, cx),
            },
            Made::Trashed { original, trashed } => match file_ops::move_to(&trashed, &original) {
                Ok(()) => {
                    self.show_arrived(&original, true, true, cx);
                    self.made.undid(Undone::Trashed(original));
                }
                Err(err) => self.alert("Couldn’t put it back", &err, window, cx),
            },
        }
        true
    }

    /// Shift-Cmd-Z in the explorer: does the last step that Cmd-Z took back again. Says whether
    /// there was one.
    pub fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.end_edit(window, cx);
        match self.made.last_undone() {
            None => return false,
            // Trashing again asks first, as Move to Trash does, when the file was opened and edited
            // since it came back: the step is spent only when it runs, so Cancel leaves it.
            Some(Undone::Trashed(original)) => {
                let path = original.clone();
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                let target = path.clone();
                let _ = self.confirm_unsaved(std::slice::from_ref(&target), format!("Move \u{201C}{name}\u{201D} to the Trash?"), "Move to Trash", window, cx, move |this, window, cx| {
                    this.made.take_undone(|step| matches!(step, Undone::Trashed(undone) if *undone == path));
                    match file_ops::trash(&path) {
                        Ok(trashed) => {
                            if let Some(dir) = path.parent() {
                                this.refresh_removed(dir, &path, cx);
                            }
                            this.made.redone(Made::Trashed { original: path.clone(), trashed });
                            cx.emit(ExplorerEvent::Gone(path));
                        }
                        Err(err) => this.alert(&format!("{} to the Trash", file_ops::failed_title("move", std::slice::from_ref(&path))), &err, window, cx),
                    }
                });
                return true;
            }
            Some(Undone::Renamed { from, .. } | Undone::Moved { from, .. }) => {
                if let Err(why) = file_ops::refuse_busy(window, std::slice::from_ref(from), false, cx) {
                    self.alert("Can’t redo it yet", &why, window, cx);
                    return true;
                }
            }
            Some(Undone::Removed { .. }) => {}
        }
        let Some(step) = self.made.pop_undone() else { return false };
        match step {
            Undone::Trashed(_) => {}
            Undone::Renamed { from, to } => match file_ops::move_to(&from, &to) {
                Ok(()) => {
                    let in_place = self.moved(&from, &to, true, cx);
                    self.flash_in_place(in_place, cx);
                    self.made.redone(Made::Renamed { from, to });
                }
                Err(err) => self.alert("Couldn’t redo the rename", &err, window, cx),
            },
            Undone::Moved { from, to } => match file_ops::move_to(&from, &to) {
                Ok(()) => {
                    // A move out of the tree (Move to… a folder outside it) leaves its row, as the move did.
                    if to.starts_with(&self.root) {
                        self.moved(&from, &to, true, cx);
                    } else {
                        if let Some(parent) = from.parent() {
                            self.refresh_removed(parent, &from, cx);
                        }
                        cx.emit(ExplorerEvent::Renamed { from: from.clone(), to: to.clone() });
                    }
                    self.made.redone(Made::Moved { from, to });
                }
                Err(err) => self.alert("Couldn’t redo the move", &err, window, cx),
            },
            Undone::Removed { path, copy, is_dir, kept } => {
                let back = match &kept {
                    Some(trashed) => file_ops::move_to(trashed, &path),
                    None => file_ops::make_empty(&path, is_dir),
                };
                match back {
                    Ok(()) => {
                        self.show_arrived(&path, true, true, cx);
                        self.made.redone(if copy { Made::Duplicated(path) } else { Made::Created(path) });
                    }
                    Err(err) => self.alert("Couldn’t redo", &err, window, cx),
                }
            }
        }
        true
    }
}

/// `paths` less the ones inside another of them, which go with that folder.
fn outermost(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let inside: Vec<bool> = paths.iter().map(|path| paths.iter().any(|other| other != path && path.starts_with(other))).collect();
    paths.into_iter().zip(inside).filter_map(|(path, inside)| (!inside).then_some(path)).collect()
}

/// The share of the tree's height at each edge where a drag scrolls it fast, and where it scrolls
/// slower, ramping up toward the edge (Zed's 5% and 15%).
const EDGE_FAST_BAND: f32 = 0.05;
const EDGE_SLOW_BAND: f32 = 0.15;
/// Scroll speeds in points per second: at the far side of the slow band, at its near side, and in
/// the fast band.
const EDGE_SLOWEST: f32 = 150.;
const EDGE_SLOW: f32 = 600.;
const EDGE_FAST: f32 = 1400.;

/// How fast a drag at `y` points below the top of a tree `height` tall scrolls it, in points per
/// second: negative up, positive down, 0 outside the bands.
fn edge_speed(y: f32, height: f32) -> f32 {
    if height <= 0. {
        return 0.;
    }
    let (near, sign) = if y < height / 2. { (y, -1.) } else { (height - y, 1.) };
    let depth = near / height;
    let speed = if depth < EDGE_FAST_BAND {
        EDGE_FAST
    } else if depth < EDGE_SLOW_BAND {
        let along = (EDGE_SLOW_BAND - depth) / (EDGE_SLOW_BAND - EDGE_FAST_BAND);
        EDGE_SLOWEST + (EDGE_SLOW - EDGE_SLOWEST) * along
    } else {
        return 0.;
    };
    sign * speed
}

/// The detail of the warning shown before `action` ("Move to Trash" or "Remove") would lose
/// unsaved edits of the files called `names`.
fn unsaved_detail(names: &[String], action: &str) -> String {
    let (it, them) = if action == "Remove" { ("Removing it", "Removing them") } else { ("Moving it to the Trash", "Moving them to the Trash") };
    if let [name] = names {
        return format!("\u{201C}{name}\u{201D} has unsaved changes in an open tab. {it} loses the changes.");
    }
    let mut detail = format!("{} files have unsaved changes in open tabs. {them} loses the changes.", names.len());
    for name in names.iter().take(UNSAVED_NAMES_SHOWN) {
        detail.push_str(&format!("\n{name}"));
    }
    if names.len() > UNSAVED_NAMES_SHOWN {
        detail.push_str(&format!("\nand {} more", names.len() - UNSAVED_NAMES_SHOWN));
    }
    detail
}

#[cfg(test)]
mod steps_tests {
    use std::path::PathBuf;

    use super::{Made, Steps, Undone};
    use crate::tokens::UNDO_STEPS;

    #[test]
    fn two_hundred_steps_keep_the_last_hundred_and_a_clear_empties_the_list() {
        let mut steps = Steps::default();
        for n in 1..=200 {
            steps.push(Made::Created(PathBuf::from(format!("/r/f{n}"))));
        }
        let mut kept = Vec::new();
        while let Some(Made::Created(path)) = steps.pop() {
            kept.push(path);
        }
        assert_eq!(kept.len(), UNDO_STEPS);
        assert_eq!((kept.first(), kept.last()), (Some(&PathBuf::from("/r/f200")), Some(&PathBuf::from("/r/f101"))));
        steps.push(Made::Created(PathBuf::from("/r/g")));
        steps.clear();
        assert!(steps.last().is_none());
    }

    #[test]
    fn a_new_action_drops_the_redo_list_and_a_redo_keeps_it() {
        let path = |name: &str| PathBuf::from(format!("/r/{name}"));
        let mut steps = Steps::default();
        steps.undid(Undone::Trashed(path("a")));
        steps.undid(Undone::Trashed(path("b")));
        // A redo spends its step and leaves the rest of the redo list.
        assert!(matches!(steps.pop_undone(), Some(Undone::Trashed(p)) if p == path("b")));
        steps.redone(Made::Created(path("b")));
        assert!(matches!(steps.last_undone(), Some(Undone::Trashed(p)) if *p == path("a")));
        assert!(matches!(steps.take_undone(|step| matches!(step, Undone::Trashed(p) if *p == path("a"))), Some(Undone::Trashed(_))));
        assert!(steps.last_undone().is_none());
        // A new action drops what was left to redo.
        steps.undid(Undone::Trashed(path("c")));
        steps.push(Made::Created(path("d")));
        assert!(steps.pop_undone().is_none());
        steps.undid(Undone::Trashed(path("e")));
        steps.clear();
        assert!(steps.last_undone().is_none() && steps.last().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::{EDGE_FAST, edge_speed, unsaved_detail};

    fn names(count: usize) -> Vec<String> {
        (1..=count).map(|n| format!("f{n}.txt")).collect()
    }

    #[test]
    fn unsaved_detail_words() {
        assert_eq!(unsaved_detail(&names(1), "Move to Trash"), "\u{201C}f1.txt\u{201D} has unsaved changes in an open tab. Moving it to the Trash loses the changes.");
        assert_eq!(unsaved_detail(&names(1), "Remove"), "\u{201C}f1.txt\u{201D} has unsaved changes in an open tab. Removing it loses the changes.");
        assert_eq!(unsaved_detail(&names(2), "Move to Trash"), "2 files have unsaved changes in open tabs. Moving them to the Trash loses the changes.\nf1.txt\nf2.txt");
        assert_eq!(unsaved_detail(&names(2), "Remove"), "2 files have unsaved changes in open tabs. Removing them loses the changes.\nf1.txt\nf2.txt");
        let six = unsaved_detail(&names(6), "Move to Trash");
        assert!(six.starts_with("6 files have") && six.ends_with("\nf6.txt"));
        let seven = unsaved_detail(&names(7), "Move to Trash");
        assert!(seven.ends_with("\nf6.txt\nand 1 more") && !seven.contains("f7.txt"));
        let forty = unsaved_detail(&names(40), "Move to Trash");
        assert!(forty.starts_with("40 files have") && forty.ends_with("\nf6.txt\nand 34 more"));
        assert_eq!(forty.lines().count(), 8);
    }

    #[test]
    fn edge_speed_bands() {
        assert_eq!(edge_speed(500., 1000.), 0.);
        assert_eq!(edge_speed(200., 1000.), 0.);
        assert_eq!(edge_speed(10., 1000.), -EDGE_FAST);
        assert_eq!(edge_speed(990., 1000.), EDGE_FAST);
        assert!(edge_speed(100., 1000.) < 0. && edge_speed(900., 1000.) > 0.);
        // Faster the nearer the edge, never past the fast band's speed.
        assert!(edge_speed(120., 1000.).abs() < edge_speed(60., 1000.).abs());
        assert!(edge_speed(60., 1000.).abs() < EDGE_FAST);
        assert_eq!(edge_speed(0., 0.), 0.);
    }
}

#[cfg(test)]
mod move_here_tests {
    use std::{fs, path::{Path, PathBuf}};

    use super::gpui::{ClipboardEntry, ClipboardItem, ExternalPaths};

    use super::{files_in, target_folder};
    use crate::file_ops;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from("/tmp/ovn-m94w").join(format!("unit-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_pasteboard_gives_its_files_and_text_gives_none() {
        let paths = vec![PathBuf::from("/tmp/ovn-m94w/a.txt"), PathBuf::from("/tmp/ovn-m94w/b")];
        let item = ClipboardItem { entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(paths.clone().into()))] };
        assert_eq!(files_in(&item), Some(paths));
        assert_eq!(files_in(&ClipboardItem::new_string("a.txt".into())), None);
    }

    #[test]
    fn a_paste_goes_into_a_folder_the_folder_of_a_file_or_the_root() {
        let root = scratch("target");
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/f.txt"), "x").unwrap();
        assert_eq!(target_folder(Some(root.join("sub")), &root), root.join("sub"));
        assert_eq!(target_folder(Some(root.join("sub/f.txt")), &root), root.join("sub"));
        assert_eq!(target_folder(None, &root), root);
    }

    #[test]
    fn moved_files_leave_their_place_a_clash_is_refused_and_undo_puts_them_back() {
        let base = scratch("move");
        let (outside, tree) = (base.join("outside"), base.join("tree"));
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(tree.join("dest")).unwrap();
        fs::write(outside.join("a.txt"), "a").unwrap();
        fs::write(outside.join("b.txt"), "b").unwrap();
        fs::write(tree.join("dest/b.txt"), "taken").unwrap();
        let dest = tree.join("dest");

        // A free name moves: the source is gone, the target is there.
        let moved = file_ops::move_into(&outside.join("a.txt"), &dest).unwrap();
        assert_eq!(moved, dest.join("a.txt"));
        assert!(!outside.join("a.txt").exists() && moved.exists());

        // A taken name is refused and nothing is replaced.
        let clash = file_ops::move_into(&outside.join("b.txt"), &dest).unwrap_err();
        assert!(clash.contains("b.txt"), "{clash}");
        assert_eq!(fs::read_to_string(dest.join("b.txt")).unwrap(), "taken");
        assert!(outside.join("b.txt").exists());

        // Undo is the same step backwards.
        file_ops::move_to(&moved, &outside.join("a.txt")).unwrap();
        assert!(outside.join("a.txt").exists() && !Path::new(&moved).exists());
        let _ = fs::remove_dir_all(&base);
    }
}
