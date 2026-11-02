//! One open file: its path, its editor and whether it has unsaved changes.
//!
//! A file over `BIG_FILE_LINES` opens with soft wrap off: the vendored editor
//! wraps a long file a few milliseconds a frame, but all of it still takes
//! about 16 us a line (measured on large files). The
//! View menu's Soft wrap check shows it, and Option-Z turns wrap back on, at the
//! window's width.
//!
//! A document tells its window when its unsaved state, path or wrap changes,
//! so the title, the window's edited dot and the tree's dots follow it.

use std::{
    borrow::Cow,
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use gpui_kit::component::input::{EditorState, InputEvent, Position, Rope, RopeExt as _, WrappingIndent};
use gpui_kit::*;

use crate::{
    compare::Comparison,
    compare_view::{CompareEvent, CompareView},
    drafts,
    encoding::{self, Decoded, Endings, GrowBase, Tail},
    file_ops, language, menu,
    undo_slider::{Close, UndoSlider},
    preview::Preview,
    settings, smart_punct,
    start::Caret,
};
use encoding_rs::Encoding;
use futures_channel::oneshot;

/// A draft is written this long after typing stops.
const DRAFT_DELAY: Duration = Duration::from_millis(500);
/// Typing that never pauses for `DRAFT_DELAY` still writes the draft this long after its oldest unwritten edit.
const DRAFT_MAX_WAIT: Duration = Duration::from_secs(5);
/// The longest tab title an Untitled page takes from its first line, in characters, before the ellipsis.
const DRAFT_TITLE_CHARS: usize = 40;
/// How many characters of one line are looked at for a title or a file name: a line is never read whole.
const FIRST_LINE_SCAN: usize = 200;
/// How many characters at the top of a page are looked at to find its title past blank lines and front matter.
const PAGE_SCAN: usize = 2000;
/// The longest file name stem Save offers from the first line, in characters.
const SAVE_NAME_CHARS: usize = 60;

/// A file with more lines than this opens with soft wrap off.
const BIG_FILE_LINES: usize = 50_000;

/// The document that built each editor, so the pane that only holds the editor can reach its
/// document (minimal mode).
#[derive(Default)]
struct Builders(std::collections::HashMap<EntityId, WeakEntity<Document>>);

impl Global for Builders {}

impl Builders {
    fn prune(&mut self) {
        self.0.retain(|_, kept| kept.upgrade().is_some());
    }

    fn insert(&mut self, editor: EntityId, document: WeakEntity<Document>) {
        self.prune();
        self.0.insert(editor, document);
    }
}

/// Puts minimal mode on or off for `editor` and the document that built it: the gutter's numbers
/// hidden and the reading column forced on, or both back as the settings say. The same value does
/// nothing, so a pane may call it for every editor it shows.
pub fn set_minimal(editor: &Entity<EditorState>, on: bool, cx: &mut App) {
    let document = cx.try_global::<Builders>().and_then(|builders| builders.0.get(&editor.entity_id())).and_then(|document| document.upgrade());
    match document {
        Some(document) => document.update(cx, |document, cx| document.set_minimal(on, cx)),
        None => editor.update(cx, |state, cx| state.set_line_numbers_hidden(on, cx)),
    }
}

/// Gives every built editor the rainbow-bracket and semantic palettes now in effect (`theme::code_palettes`), after
/// View > Code Colors, a settings edit or an appearance change. An editor built later takes them at build.
pub fn apply_code_palettes(cx: &mut App) {
    let palettes = crate::theme::code_palettes(cx);
    if !cx.has_global::<Builders>() {
        return;
    }
    cx.global_mut::<Builders>().prune();
    let documents: Vec<Entity<Document>> = cx.global::<Builders>().0.values().filter_map(WeakEntity::upgrade).collect();
    for document in documents {
        let Body::Built { editor, .. } = &document.read(cx).body else { continue };
        let editor = editor.clone();
        editor.update(cx, |state, cx| {
            state.set_rainbow_brackets(palettes.brackets.clone(), cx);
            state.set_semantic_palette(palettes.names.clone(), cx);
        });
    }
}

/// Sent when a document's unsaved state or path changed.
pub struct DocumentChanged;

/// Sent when the document needs its tab in front of the user (a save that has to ask).
pub struct ShowTab;

/// Sent when the document's tab should go without asking (its file is gone and the user said
/// Discard and close).
pub struct DropTab;

/// A text's length and a hash of its bytes: enough to know the text again without holding it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    len: usize,
    hash: u64,
}

impl Fingerprint {
    fn of(text: &Rope) -> Self {
        use std::hash::Hasher as _;
        // Bytes are fed through `write`, which streams, so chunk boundaries don't change the hash.
        let mut hasher = std::hash::DefaultHasher::new();
        for chunk in text.chunks() {
            hasher.write(chunk.as_bytes());
        }
        Self { len: text.len(), hash: hasher.finish() }
    }

    /// Whether `text` is the text this was taken from; the hash is only read when the lengths agree.
    fn matches(&self, text: &Rope) -> bool {
        text.len() == self.len && Self::of(text) == *self
    }
}

/// What Format > Line endings replaced: the style before, and the length and hash of the text before
/// and after with the style it was given (`crlf`, never Mixed). Undo and Redo bring those texts back,
/// so the style goes with them. No text is kept: the editor's history holds the replaced stretch, and a
/// second copy of a big file here would cost as much as the file.
struct EndingsUndo {
    before: Fingerprint,
    before_style: (bool, bool),
    after: Fingerprint,
    crlf: bool,
}

/// What a document holds: nothing but its path (a clean file restored at launch: its file is read
/// when it first shows), the text alone until it first shows or is edited, then its editor.
/// A tab restored at launch stays unbuilt, so the first frame doesn't wait on its editor.
enum Body {
    Unread,
    Unbuilt(String),
    Built { editor: Entity<EditorState>, _changes: Subscription },
}

/// An open Compare view and the disk's time as it was read, which Use These takes as the file's time.
struct OpenCompare {
    view: Entity<CompareView>,
    _subscription: Subscription,
    mtime: Option<SystemTime>,
    /// The document's edit count (`Saves::edits`) when the compared text was taken.
    edits: u64,
    /// Reading the disk changed bytes (`Decoded::replaced`).
    replaced: bool,
    /// The saved version being compared (its id and time) instead of the disk: Use These takes
    /// neither the disk's time nor its bytes, and leaves the tab unsaved.
    version: Option<(String, SystemTime)>,
}

pub struct Document {
    pub path: Option<PathBuf>,
    /// The folder the window's tree is rooted at (the window hands it over), where Save As starts
    /// for a page with no file.
    pub tree_root: Option<PathBuf>,
    body: Body,
    /// A large file keeps its identity while its empty editor waits for permission to read.
    awaiting_open: bool,
    pending_caret: Option<Caret>,
    pub unsaved: bool,
    /// The file's modification time as of our last read or write of it: a file whose time is
    /// different from this was changed by someone else since.
    saved_mtime: Option<SystemTime>,
    /// Unsaved edits, and the file changed on disk since `saved_mtime`: a save would overwrite
    /// the other change. Found when the window sees the file change, and again at save.
    conflict: bool,
    /// The file is gone from disk (deleted or moved) while this tab holds unsaved edits.
    disk_gone: bool,
    /// The file has no write permission: its tab shows a lock and Save asks where to save a copy.
    read_only: bool,
    /// The encoding the file was read in and is saved back in; UTF-8 for Untitled pages and drafts.
    encoding: &'static Encoding,
    /// The encoding of the bytes on disk now: what the file was read in, or what our last save wrote.
    /// Revert, Discard edits and Compare read the file in it, so a pending Convert to is undone by them.
    disk_encoding: &'static Encoding,
    /// The file began with a byte order mark, which a save writes again.
    has_bom: bool,
    /// Every line break in the file was CRLF when it was read (and there was one): a save writes
    /// each bare LF, from Return or a paste, as CRLF so the file keeps one ending.
    crlf: bool,
    /// The file has both CRLF and bare LF breaks, so it is saved as it is until Format > Line
    /// endings picks one; the status line says Mixed.
    mixed: bool,
    /// After Format > Line endings: the text before and after it, and the style each goes with. An
    /// Undo or Redo that brings back either exact text brings its style back; any other edit
    /// leaves the style alone. Dropped where the undo history goes (a reload that changed the text).
    endings_undo: Option<EndingsUndo>,
    /// Opening or reopening changed bytes that a Save must not silently replace, even if the
    /// replacement characters are edited out before saving.
    replaced: bool,
    /// The text the file held when this document last opened, reloaded or saved it, to tell when
    /// scrubbing back through the edits is at the saved text again.
    saved_text: Option<Rope>,
    /// An Undo history slider is open over the editor (`begin_scrub`).
    scrubbing: bool,
    /// The text changed while the slider was open.
    scrub_dirty: bool,
    slider: Option<WeakEntity<UndoSlider>>,
    /// The warm parsed state of a markdown file; `None` for any other, and until the editor is built.
    pub preview: Option<Preview>,
    /// Whether long lines wrap now: the setting, but off for a big file.
    soft_wrap: bool,
    /// Whether wrapped lines run to the window's width, past the reading column (Option-Z, this file only).
    wrap_full: bool,
    /// The editor's text length after the last change smart punctuation looked at (it tells a quote the editor closed itself from a lone key).
    punct_len: usize,
    /// Whether the editor had undo history after the last change: Edit > Undo history follows it, and an already unsaved restored draft raises no `DocumentChanged` to refresh the menu.
    had_history: bool,
    /// An Untitled page's language by its first line (`language::for_untitled`): Diff for a patch,
    /// Markdown for a heading, front matter or a Markdown template, else text (`detect_untitled_lang`).
    untitled_lang: &'static str,
    /// The page came from a Markdown template (`set_markdown_template`); emptying it lets go.
    markdown_template: bool,
    /// The scrap draft file backing an Untitled page, once it has a character.
    draft: Option<PathBuf>,
    /// The draft has edits not yet written.
    draft_waiting: bool,
    /// The last write of the draft failed.
    draft_failed: bool,
    /// Writes that failed and told the user why, counted for the tab bar.
    write_errors: u32,
    /// Counts edits, so only the last one of a burst writes the draft.
    draft_edits: u64,
    /// The draft's file shared with its background writes.
    draft_slot: Option<drafts::Shared>,
    /// The task waiting to write the draft, alive while `draft_due` is set.
    draft_timer: Option<Task<()>>,
    /// When the waiting task writes: `DRAFT_DELAY` after the latest edit. `None` while no task
    /// waits; an edit moves it and the task sleeps on, so a burst of keys spawns once.
    draft_due: Option<Instant>,
    /// When the oldest edit not yet handed to a write was made; the waiting task never sleeps
    /// past `DRAFT_MAX_WAIT` after it.
    draft_first: Option<Instant>,
    /// The tab title an Untitled page takes from its first line, set `DRAFT_DELAY` after typing stops; none while the first line is empty.
    draft_title: Option<String>,
    /// The task waiting to set `draft_title`, alive while `title_due` is set.
    title_timer: Option<Task<()>>,
    /// When the title task sets it: `DRAFT_DELAY` after the latest edit.
    title_due: Option<Instant>,
    /// The number of the latest write try whose result the page has taken.
    draft_settled: u64,
    /// The edit count and the one write of the file in flight, if any.
    saves: Saves,
    /// Saves asked for while one writes; each wakes when it lands.
    save_waiters: Vec<oneshot::Sender<()>>,
    /// The Compare view over this tab's editor, while one is open.
    compare: Option<OpenCompare>,
    /// The last read of the file was a growth (only the bytes it gained were read), so reloads are paced.
    growing: bool,
    /// A growth read could not be applied; the window reads the file again.
    retry: bool,
    /// The device and inode of the file as of the last whole compare of its old text with the
    /// disk (`read_tail`); the bounded growth check holds the file to it.
    grow_ident: Option<(u64, u64)>,
    /// Minimal mode is on for this document's editor (`set_minimal`).
    minimal: bool,
}

impl EventEmitter<DocumentChanged> for Document {}
impl EventEmitter<ShowTab> for Document {}
impl EventEmitter<DropTab> for Document {}

impl Document {
    /// Opens `text` in a new editor and titles the window for it.
    pub fn new(path: Option<PathBuf>, decoded: Decoded, saved_mtime: Option<SystemTime>, wrap_setting: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::unbuilt(path, decoded, saved_mtime, wrap_setting);
        let editor = this.editor(window, cx);
        editor.update(cx, |state, cx| state.focus(window, cx));
        window.set_window_title(&this.title());
        this
    }

    /// A document holding `decoded` that builds its editor on first need (`editor`), for a tab
    /// that comes back at launch and isn't showing. It is never dirty. `saved_mtime` is the
    /// file's modification time taken before the text was read.
    pub fn unbuilt(path: Option<PathBuf>, decoded: Decoded, saved_mtime: Option<SystemTime>, wrap_setting: bool) -> Self {
        let Decoded { text, encoding, has_bom, replaced } = decoded;
        let endings = encoding::endings(&text);
        let soft_wrap = wrap_setting && text.lines().take(BIG_FILE_LINES + 1).count() <= BIG_FILE_LINES;
        let read_only = path.as_deref().map(probe).is_some_and(|disk| disk.read_only);
        Self {
            path,
            tree_root: None,
            body: Body::Unbuilt(text),
            awaiting_open: false,
            pending_caret: None,
            encoding,
            disk_encoding: encoding,
            has_bom,
            crlf: endings == Endings::Crlf,
            mixed: endings == Endings::Mixed,
            endings_undo: None,
            replaced,
            saved_text: None,
            scrubbing: false,
            scrub_dirty: false,
            slider: None,
            unsaved: false,
            saved_mtime,
            conflict: false,
            disk_gone: false,
            read_only,
            preview: None,
            soft_wrap,
            wrap_full: false,
            punct_len: 0,
            had_history: false,
            untitled_lang: "text",
            markdown_template: false,
            draft: None,
            draft_waiting: false,
            draft_failed: false,
            write_errors: 0,
            draft_edits: 0,
            draft_slot: None,
            draft_timer: None,
            draft_due: None,
            draft_first: None,
            draft_title: None,
            title_timer: None,
            title_due: None,
            saves: Saves::default(),
            save_waiters: Vec::new(),
            compare: None,
            growing: false,
            retry: false,
            grow_ident: None,
            minimal: false,
            draft_settled: 0,
        }
    }

    /// A clean file restored at launch and not showing: it keeps only its path (`saved_mtime` is
    /// the file's time now) and reads the file, with its encoding, when it first shows (`editor`).
    pub fn unread(path: PathBuf, saved_mtime: Option<SystemTime>, wrap_setting: bool) -> Self {
        let mut this = Self::unbuilt(Some(path), Decoded::utf8(String::new()), saved_mtime, wrap_setting);
        this.body = Body::Unread;
        this
    }

    /// Reads the file of an unread document through the normal open path (`encoding::read_file`)
    /// and takes its encoding, wrap and time as an open would. A file that can't be read now (gone
    /// since launch, or no text) says why like a failed open and sends its tab away; the editor
    /// that is built meanwhile is empty.
    fn read_now(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        let Some(path) = self.path.clone() else { return String::new() };
        // The time before the text, so a write in between shows as a change later.
        let mtime = path_mtime(Some(&path));
        match encoding::read_file(&path) {
            Ok(Decoded { text, encoding, has_bom, replaced }) => {
                self.encoding = encoding;
                self.disk_encoding = encoding;
                self.has_bom = has_bom;
                self.read_endings(&text);
                self.replaced = replaced;
                self.saved_mtime = mtime;
                self.soft_wrap = self.soft_wrap && text.lines().take(BIG_FILE_LINES + 1).count() <= BIG_FILE_LINES;
                text
            }
            Err(err) if err.kind() == io::ErrorKind::FileTooLarge => {
                let (title, detail) = encoding::large_file_question(&path);
                let answer = window.prompt(PromptLevel::Warning, &title, Some(&detail), &["Open", "Cancel"], cx);
                self.awaiting_open = true;
                cx.spawn_in(window, async move |this, cx| {
                    if answer.await.ok() != Some(0) {
                        this.update(cx, |_, cx| cx.emit(DropTab)).ok();
                        return;
                    }
                    let read = {
                        let path = path.clone();
                        cx.background_executor().spawn(async move {
                            let mtime = path_mtime(Some(&path));
                            (mtime, encoding::read_large_file(&path))
                        })
                    };
                    let (mtime, read) = read.await;
                    this.update_in(cx, |this, window, cx| {
                        match read {
                            Ok(decoded) => {
                                this.awaiting_open = false;
                                this.soft_wrap = this.soft_wrap && decoded.text.lines().take(BIG_FILE_LINES + 1).count() <= BIG_FILE_LINES;
                                // Build with the real language, indentation and wrap choice, not the placeholder's.
                                let focused = this.built_editor().is_some_and(|editor| editor.read(cx).focus_handle(cx).is_focused(window));
                                this.body = Body::Unbuilt(String::new());
                                this.reload(decoded, mtime, true, window, cx);
                                let editor = this.editor(window, cx);
                                if focused {
                                    editor.update(cx, |state, cx| state.focus(window, cx));
                                }
                                if let Some(caret) = this.pending_caret.take() {
                                    this.go_to(caret, window, cx);
                                }
                                cx.emit(DocumentChanged);
                            }
                            Err(err) => {
                                let detail = file_ops::explain("open", &path, &err);
                                let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("open", &[path.clone()]), Some(&detail), &["OK"], cx);
                                cx.emit(DropTab);
                            }
                        }
                    }).ok();
                }).detach();
                String::new()
            }
            Err(err) => {
                let detail = file_ops::explain("open", &path, &err);
                let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("open", &[path.clone()]), Some(&detail), &["OK"], cx);
                // No file to write back to: a Save must never put this empty page over it. The drop
                // goes out on the next turn, after a pane that is still being set up listens.
                self.path = None;
                cx.spawn(async move |this, cx| {
                    this.update(cx, |_, cx| cx.emit(DropTab)).ok();
                })
                .detach();
                String::new()
            }
        }
    }

    /// An Untitled page that comes back from its draft file, `text` being what it holds. It
    /// builds its editor on first need.
    pub fn unbuilt_draft(draft: PathBuf, text: String, wrap_setting: bool) -> Self {
        let title = first_line_title(text.chars());
        let mut this = Self::unbuilt(None, Decoded::utf8(text), None, wrap_setting);
        this.set_draft(draft);
        this.draft_title = title;
        this
    }

    /// The editor, built now from the stored text if this is the first need. It doesn't take the
    /// keyboard.
    pub fn editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditorState> {
        let text = match &mut self.body {
            Body::Built { editor, .. } => return editor.clone(),
            Body::Unbuilt(text) => std::mem::take(text),
            Body::Unread => self.read_now(window, cx),
        };
        // A restored Untitled draft builds in its first line's language (`detect_untitled_lang` keeps it so).
        let lang = if self.path.is_none() {
            self.untitled_lang = language::for_untitled(language::clip(text.lines().next().unwrap_or("")), self.markdown_template);
            self.untitled_lang
        } else {
            language::for_text(self.path.as_deref(), &text)
        };
        let soft_wrap = self.soft_wrap;
        let cold = Preview::starts_cold(text.len(), cx);
        self.preview = (lang == "markdown").then(|| Preview::new(if cold { "" } else { &text }, cx));
        let tab = crate::indent_detect::detect(&text);
        // The preview and status line keep the real language; only the highlighter is capped by size.
        let highlighter = language::for_highlighter(lang, text.len());
        self.punct_len = text.len();
        // Wrapped rows start at the text's left edge: no hang, no carried indent (finch#8ezp).
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(highlighter)
                .tab_size(tab)
                .soft_wrap(soft_wrap)
                .wrapping_indent(WrappingIndent::None)
                .default_value(text)
        });
        crate::note_links::editor::attach(&editor, lang == "markdown", cx);
        // A press that moves inside the selection drags its text (the explorer's folders take it as a new file).
        let doc = cx.weak_entity();
        editor.update(cx, |editor, _| {
            editor.on_text_drag(std::rc::Rc::new(move |text, window, cx| {
                let path = doc.read_with(cx, |doc, _| doc.path.clone()).ok().flatten();
                crate::text_drag::start(text, path.as_deref(), window, cx);
            }));
            // A right click in the gutter (numbers shown or hidden) turns the numbers on or off, as View > Show line numbers.
            editor.on_gutter_menu(std::rc::Rc::new(move |_, window, cx| {
                let shown = settings::get(cx).editor.line_numbers;
                window.on_next_frame(move |window, _| {
                    window.on_next_frame(move |window, cx| {
                        window
                            .spawn(cx, async move |cx| {
                                if crate::native_menu::gutter_menu(shown) {
                                    cx.update(|window, cx| window.dispatch_action(Box::new(crate::app::ToggleLineNumbers), cx)).ok();
                                }
                            })
                            .detach();
                    });
                });
            }));
        });
        let changes = Self::watch_editor(&editor, window, cx);
        self.body = Body::Built { editor: editor.clone(), _changes: changes };
        self.set_reading(lang, cx);
        let palettes = crate::theme::code_palettes(cx);
        editor.update(cx, |state, cx| {
            state.set_line_numbers_hidden(self.minimal, cx);
            state.set_rainbow_brackets(palettes.brackets, cx);
            state.set_semantic_palette(palettes.names, cx);
        });
        let me = cx.weak_entity();
        cx.default_global::<Builders>().insert(editor.entity_id(), me);
        crate::status_line::own(&editor, cx.weak_entity(), cx);
        self.saved_text = Some(editor.read(cx).text().clone());
        crate::marks::attach(&editor, lang, cx);
        crate::patch_view::attach(&editor, lang, cx);
        crate::log_follow::attach(&editor, self.path.as_deref().and_then(|path| path.extension()).is_some_and(|ext| ext.eq_ignore_ascii_case("log")), cx);
        if let Some(preview) = self.preview.as_mut().filter(|_| cold) {
            preview.cool(&editor, cx);
        }
        // The empty placeholder of a large file takes no typing: the file replaces it when it is read.
        if self.awaiting_open {
            editor.update(cx, |state, cx| state.set_readonly(true, cx));
        }
        editor
    }

    /// Watches `editor` for edits: the preview, the draft, the saves and the unsaved dot follow.
    fn watch_editor(editor: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(editor, window, |this, editor, event: &InputEvent, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            // Typing, Undo or Redo lets go of a followed end.
            editor.update(cx, |state, cx| state.stop_following(cx));
            // A typed edit leaves a step; only the others (Undo, Redo, a paste) read the counts.
            let has_history = editor.read(cx).edit_was_typed() || {
                let (applied, waiting) = editor.read(cx).history_counts();
                applied + waiting > 0
            };
            if std::mem::replace(&mut this.had_history, has_history) != has_history {
                cx.defer(menu::refresh);
            }
            this.detect_untitled_lang(editor, cx);
            this.smart_punctuate(editor, window, cx);
            if let Some(preview) = this.preview.as_mut() {
                preview.schedule(editor.clone(), cx);
            }
            // Typing at the newest stop (no steps waiting) is a plain edit; only a scrubbed-back
            // text waits for the slider to close.
            if this.scrubbing && (this.path.is_some() || editor.read(cx).history_waiting() > 0) {
                this.scrub_dirty = true;
                // The text on screen is scrubbed: a draft write armed by typing would take it.
                this.draft_due = None;
                this.draft_timer = None;
            } else if this.path.is_none() {
                this.draft_changed(cx);
                if this.scrubbing {
                    // Typed at the newest stop with the slider open: copied now, before a jump back.
                    this.draft_due = None;
                    this.draft_timer = None;
                    this.write_draft_in_background(cx);
                }
            }
            this.saves.edited();
            // An Undo or Redo of Format > Line endings brings back the text it saw: the saved style goes with it.
            // A typed edit is never an Undo or Redo, so it skips the hash.
            let style = this.endings_undo.as_ref().filter(|_| !editor.read(cx).edit_was_typed()).and_then(|undo| {
                let text = editor.read(cx).text();
                if undo.before.matches(text) {
                    Some(undo.before_style)
                } else if undo.after.matches(text) {
                    Some((undo.crlf, false))
                } else {
                    None
                }
            });
            if let Some(style) = style.filter(|&style| style != (this.crlf, this.mixed)) {
                (this.crlf, this.mixed) = style;
                cx.emit(DocumentChanged);
                cx.defer(menu::refresh);
            }
            if this.scrubbing && this.path.is_some() && this.saved_text.is_some() {
                // A jump may land on the saved text again: the dot follows the text, not the event.
                this.scrub_settled(cx);
            } else if !this.unsaved {
                this.unsaved = true;
                cx.emit(DocumentChanged);
            }
        })
    }

    /// The document moves to `window` (its tab was dragged out of another window): the editor's
    /// window subscriptions and this document's watch of the editor are made again there, so
    /// edits and drafts go on once the old window is gone.
    pub fn rehome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Body::Built { editor, .. } = &self.body else { return };
        let editor = editor.clone();
        editor.update(cx, |state, cx| state.rehome(window, cx));
        let changes = Self::watch_editor(&editor, window, cx);
        self.body = Body::Built { editor, _changes: changes };
        if let Some(open) = self.compare.as_mut() {
            open._subscription = cx.subscribe_in(&open.view, window, |this, _, event: &CompareEvent, window, cx| this.compare_event(event, window, cx));
        }
    }

    /// Smart punctuation: after a key that added one character (two for a quote the editor closed
    /// itself), reads the caret's line and, when the character is a quote, a dot or a dash that the
    /// rules turn into curly punctuation, swaps it as its own undo step (so Cmd-Z gives back exactly
    /// what was typed). Only an edit the editor marks as typed counts, so Undo, Redo, pastes and
    /// reloads pass; `punct_len` tells one added character from a quote plus its closer.
    fn smart_punctuate(&mut self, editor: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) {
        let (len, typed) = {
            let state = editor.read(cx);
            (state.text().len(), state.edit_was_typed())
        };
        let grew = len.checked_sub(self.punct_len);
        self.punct_len = len;
        if !typed {
            return;
        }
        let paired = grew == Some(2);
        if !(paired || grew == Some(1)) || !settings::get(cx).editor.smart_punctuation {
            return;
        }
        let lang = if self.path.is_none() { self.untitled_lang } else { language::for_path(self.path.as_deref()) };
        let Some(kind) = smart_punct::kind(self.path.as_deref(), lang) else { return };
        let (range, text, caret) = {
            let state = editor.read(cx);
            let cursor = state.cursor();
            let rope = state.text();
            if state.selected_ranges().count() != 1 || state.selected_range().start != cursor || cursor == 0 {
                return;
            }
            let Some(_) = rope.chars_at(cursor).reversed().next().filter(|&ch| smart_punct::is_trigger(ch)) else { return };
            let point = rope.offset_to_point(cursor);
            let line_start = cursor - point.column;
            let line = rope.slice(line_start..cursor).to_string();
            let after = rope.slice(cursor..).chars().next();
            let Some(edit) = smart_punct::replacement(kind, &line, after, paired) else { return };
            if point.row > smart_punct::LINES_ABOVE_MAX {
                return;
            }
            let above = rope.iter_lines().take(point.row).map(|row| smart_punct::LinePrefix::of(row.len(), |at| row.chunk(at).0));
            if smart_punct::in_front_matter_or_fence(kind, above, &line) {
                return;
            }
            (line_start + edit.range.start..line_start + edit.range.end, edit.text, line_start + edit.caret)
        };
        editor.update(cx, |state, cx| {
            state.replace_range(range, text, caret, window, cx);
        });
    }

    /// Whether the editor is built yet.
    pub fn is_built(&self) -> bool {
        matches!(self.body, Body::Built { .. })
    }

    /// Whether the tab never showed: it holds only its path and reads the file when it does.
    pub fn is_unread(&self) -> bool {
        matches!(self.body, Body::Unread)
    }

    /// Whether the preview can show: a built markdown file has it, an unbuilt one will.
    pub fn has_preview(&self) -> bool {
        match &self.body {
            Body::Built { .. } => self.preview.is_some(),
            Body::Unbuilt(text) if self.path.is_none() => language::for_untitled(language::clip(text.lines().next().unwrap_or("")), self.markdown_template) == "markdown",
            Body::Unbuilt(text) => language::for_text(self.path.as_deref(), text) == "markdown",
            // By its name alone: a `#!` line or modeline is only read when it shows.
            Body::Unread => language::for_path(self.path.as_deref()) == "markdown",
        }
    }

    /// Keeps the parsed preview (`warm`) or lets it go: a tab past the few kept warm holds no
    /// parsed tree. The text stays with the editor, and the preview parses it again when it
    /// comes back.
    pub fn keep_preview_warm(&mut self, warm: bool, cx: &mut Context<Self>) {
        let Body::Built { editor, .. } = &self.body else { return };
        let editor = editor.clone();
        if let Some(preview) = self.preview.as_mut() {
            if warm {
                preview.warm(&editor, cx);
            } else {
                preview.cool(&editor, cx);
            }
        }
    }

    /// The highlighter language now: the path's, else the `#!` line's or a modeline's in the text
    /// already loaded. An Untitled page has its first line's (`language::for_untitled`).
    pub fn language(&self, cx: &App) -> &'static str {
        self.language_at(self.path.as_deref(), cx)
    }

    /// The same for `path`, which a rename or Save As is about to give this document.
    fn language_at(&self, path: Option<&Path>, cx: &App) -> &'static str {
        match &self.body {
            Body::Unbuilt(text) if path.is_none() => language::for_untitled(language::clip(text.lines().next().unwrap_or("")), self.markdown_template),
            Body::Unbuilt(text) => language::for_text(path, text),
            Body::Unread => language::for_path(path),
            Body::Built { editor, .. } => {
                if path.is_none() {
                    return language::for_untitled(&Self::first_line(editor.read(cx).text()), self.markdown_template);
                }
                let by_path = language::for_path(path);
                if by_path != "text" {
                    return by_path;
                }
                let rope = editor.read(cx).text();
                let rows = rope.lines_len();
                let line = |row: usize| rope.slice_line(row).chars().take(language::LINE_CHARS).collect::<String>();
                let first: Vec<String> = (0..rows.min(language::EDGE_LINES)).map(line).collect();
                let last: Vec<String> = (rows.saturating_sub(language::EDGE_LINES)..rows).map(line).collect();
                let first: Vec<&str> = first.iter().map(String::as_str).collect();
                let last: Vec<&str> = last.iter().map(String::as_str).collect();
                language::for_lines(path, &first, &last)
            }
        }
    }

    /// The editor if it is built; `None` for a document still holding only its text.
    pub fn built_editor(&self) -> Option<&Entity<EditorState>> {
        match &self.body {
            Body::Built { editor, .. } => Some(editor),
            Body::Unbuilt(_) | Body::Unread => None,
        }
    }

    /// Puts the caret on `caret` (counted from 1; a line or column past the end lands on the last)
    /// and scrolls it into view. A document that hasn't drawn has no layout to scroll by, so the
    /// move is asked again two frames on, if the caret is still where it was put.
    pub fn go_to(&mut self, caret: Caret, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor(window, cx);
        if self.awaiting_open {
            self.pending_caret = Some(caret);
            return;
        }
        let offset = editor.update(cx, |state, cx| {
            let rope = state.text();
            let line = caret.line.saturating_sub(1).min(rope.lines_len().saturating_sub(1) as u32);
            let offset = rope.position_to_offset(&Position::new(line, caret.column.saturating_sub(1)));
            // A line inside a fold opens it; the caret would stop at the fold's first line otherwise.
            state.unfold_at(Position::new(line, 0), cx);
            state.set_selected_range(offset..offset, cx);
            state.cursor()
        });
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, cx| {
                editor.update(cx, |state, cx| {
                    if state.cursor() == offset {
                        state.set_selected_range(offset..offset, cx);
                    }
                });
            });
        });
    }

    /// Where the caret is, counted from 1 as `go_to` takes it; `None` for a document not built
    /// yet, whose caret is at the start.
    pub fn caret(&self, cx: &App) -> Option<Caret> {
        if self.awaiting_open {
            return self.pending_caret;
        }
        let editor = self.built_editor()?;
        let at = editor.read(cx).cursor_position();
        Some(Caret { line: at.line + 1, column: at.character + 1 })
    }

    /// The text now, from the editor or, until it is built, as stored.
    fn text(&self, cx: &App) -> String {
        match &self.body {
            Body::Built { editor, .. } => editor.read(cx).text().to_string(),
            Body::Unbuilt(text) => text.clone(),
            // Clean, so what the file holds; nothing reads this on a tab that hasn't shown.
            Body::Unread => self.path.as_deref().and_then(|path| encoding::read_file(path).ok()).map(|decoded| decoded.text).unwrap_or_default(),
        }
    }

    /// The text now as a rope, from the editor or, until it is built, as stored.
    fn rope(&self, cx: &App) -> Rope {
        match &self.body {
            Body::Built { editor, .. } => editor.read(cx).text().clone(),
            Body::Unbuilt(text) => Rope::from(text.as_str()),
            Body::Unread => Rope::from(self.text(cx).as_str()),
        }
    }

    /// Holds the editor's wrapped text to a reading column (View > Reading width, `reading_width`
    /// in the settings): 70 characters in Markdown and plain text, and 100 in other files once the
    /// pane is wider than 1,400 points. The setting off puts both back. The column starts at the
    /// gutter, so soft wrap never moves the text right (finch#cbys). Minimal mode forces the
    /// column on whatever the setting says, centered: 70 for prose and 100 for code in a pane of
    /// any width. Option-Z's window width (`wrap_full`) drops the column for this file, Minimal
    /// mode too. An unbuilt document picks it up at build.
    fn set_reading(&self, lang: &str, cx: &mut Context<Self>) {
        let Body::Built { editor, .. } = &self.body else { return };
        let on = settings::get(cx).editor.reading_width;
        let prose = matches!(lang, "markdown" | "text");
        let (always, wide) = match (on, prose) {
            _ if self.wrap_full => (None, None),
            _ if self.minimal && prose => (Some(crate::tokens::READING_MEASURE_CHARS), None),
            _ if self.minimal => (Some(crate::tokens::READING_MEASURE_CODE_CHARS), None),
            (false, _) => (None, None),
            (true, true) => (Some(crate::tokens::READING_MEASURE_CHARS), None),
            (true, false) => (None, Some(crate::tokens::READING_MEASURE_CODE_CHARS)),
        };
        let centered = self.minimal;
        editor.update(cx, |state, cx| {
            state.set_reading_measure(always, cx);
            state.set_reading_measure_wide(wide, cx);
            state.set_reading_centered(centered, cx);
        });
    }

    /// The same for the language the document has now, after the setting changed.
    pub fn apply_reading_width(&self, cx: &mut Context<Self>) {
        let lang = self.language(cx);
        self.set_reading(lang, cx);
    }

    /// Minimal mode on or off for this document's editor: the reading column forced on (or back to
    /// what the setting says) and the gutter's numbers hidden (or back), both before the next
    /// prepaint so the editor wraps once.
    fn set_minimal(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.minimal == on {
            return;
        }
        self.minimal = on;
        if let Body::Built { editor, .. } = &self.body {
            editor.update(cx, |state, cx| state.set_line_numbers_hidden(on, cx));
        }
        self.apply_reading_width(cx);
    }

    /// Sets the editor's highlighter after a rename; an unbuilt document picks it up at build. A
    /// file past `language::MAX_HIGHLIGHT_BYTES` gets none, whatever `lang` is. `eased` is for a
    /// save or rename: when it really changes the language and the new one has colours, they ease
    /// in over `FADE_QUICK` instead of swapping in one frame (a re-set to the same language,
    /// typing and the size cap never fade).
    fn set_highlighter(&self, lang: &'static str, eased: bool, cx: &mut Context<Self>) {
        let changed = eased && self.language(cx) != lang;
        if let Body::Built { editor, .. } = &self.body {
            let capped = language::for_highlighter(lang, editor.read(cx).text().len());
            editor.update(cx, |state, cx| {
                state.set_highlighter(capped, cx);
                if changed && capped != "text" {
                    state.fade_syntax(crate::tokens::FADE_QUICK, crate::motion::ease_out, cx);
                }
            });
            crate::marks::set_lang(editor, lang, cx);
            crate::patch_view::set_lang(editor, lang, cx);
        }
        self.set_reading(lang, cx);
    }

    /// After the text changed from `before` bytes: when it crossed `language::MAX_HIGHLIGHT_BYTES`
    /// either way, the highlighter is set again for the file's language (`set_highlighter` applies the cap).
    fn recheck_highlighter_size(&self, before: usize, cx: &mut Context<Self>) {
        let Body::Built { editor, .. } = &self.body else { return };
        let now = editor.read(cx).text().len();
        if (before > language::MAX_HIGHLIGHT_BYTES) != (now > language::MAX_HIGHLIGHT_BYTES) {
            let lang = self.language_at(self.path.as_deref(), cx);
            self.set_highlighter(lang, false, cx);
        }
    }

    /// An Untitled page reads as its first line says while it has no name (finch#9s91, finch#x6xz):
    /// a patch (`git diff | finch -` types the piped text into one) as Diff, a heading or front
    /// matter as Markdown, with its preview and styled marks. One line is looked at.
    fn detect_untitled_lang(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        if self.path.is_some() {
            return;
        }
        let rope = editor.read(cx).text();
        // An emptied page is a fresh one: the template it came from no longer says what it is.
        if rope.len() == 0 {
            self.markdown_template = false;
        }
        let lang = language::for_untitled(&Self::first_line(rope), self.markdown_template);
        if lang != std::mem::replace(&mut self.untitled_lang, lang) {
            self.set_highlighter(lang, false, cx);
            self.sync_preview(lang, cx);
            cx.emit(DocumentChanged);
        }
    }

    /// File > New from template: whether the template it came from is Markdown. A Markdown one
    /// makes the Untitled page Markdown whatever its first line says, until it is emptied.
    pub fn set_markdown_template(&mut self, markdown: bool, cx: &mut Context<Self>) {
        if self.path.is_some() {
            return;
        }
        self.markdown_template = markdown;
        if let Some(editor) = self.built_editor().cloned() {
            self.detect_untitled_lang(&editor, cx);
        }
    }

    /// The text's first line, cut to `language::LINE_CHARS`.
    fn first_line(rope: &Rope) -> String {
        rope.slice_line(0).chars().take(language::LINE_CHARS).collect()
    }

    /// The file at `from` (or a folder holding this one) was renamed to `to`: the document
    /// follows it. Text, undo history and unsaved state stay; nothing is read or written. A
    /// draft whose file is renamed keeps being that Untitled page, backed by the renamed file.
    pub fn retarget(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        let mut moved = false;
        if let Some(path) = self.path.as_deref().and_then(|path| file_ops::moved_path(path, from, to)) {
            let lang = self.language_at(Some(&path), cx);
            self.set_highlighter(lang, true, cx);
            self.sync_preview(lang, cx);
            self.path = Some(path);
            moved = true;
        }
        if let Some(draft) = self.draft.as_deref().and_then(|draft| file_ops::moved_path(draft, from, to)) {
            self.draft = Some(draft.clone());
            if let Some(slot) = &self.draft_slot {
                drafts::lock(slot).set_path(draft.clone());
                // Nothing is written at the new path yet: the next flush puts the text there.
                self.draft_waiting = true;
            }
            if !drafts::is_draft(&draft) {
                // Moved out of the drafts folder: it is a file now, so a later Save As or an
                // emptied page can't delete it. Unsaved text goes to it first.
                self.flush_draft(cx);
                let lang = self.language_at(Some(&draft), cx);
                self.set_highlighter(lang, true, cx);
                self.sync_preview(lang, cx);
                self.path = Some(draft);
                self.saved_mtime = path_mtime(self.path.as_deref());
                self.draft = None;
                if let Some(slot) = self.draft_slot.take() {
                    slot.close();
                }
                self.draft_waiting = false;
                // Scrubbed text was not flushed above: it isn't on disk, so the page stays unsaved.
                self.unsaved = self.draft_failed || (self.scrubbing && self.scrub_dirty);
                self.draft_failed = false;
                // The flush above wrote the text: that is the file's text now, not the one the editor was built with.
                self.saved_text = (!self.unsaved).then(|| self.rope(cx));
            }
            moved = true;
        }
        if moved {
            cx.emit(DocumentChanged);
        }
    }

    /// Counts the edits as saved, for a tab that closes because its file is gone (the user was
    /// asked). A draft is forgotten without deleting its file, so no later write brings it back.
    pub fn discard_edits(&mut self) {
        self.unsaved = false;
        self.conflict = false;
        self.disk_gone = false;
        self.draft = None;
        if let Some(slot) = self.draft_slot.take() {
            slot.close();
        }
        self.draft_waiting = false;
        self.draft_failed = false;
    }

    /// Whether long lines wrap in this file now (the View menu's check).
    pub fn soft_wrap(&self) -> bool {
        self.soft_wrap
    }

    /// Whether this file wraps at the window's width, past the reading column (the View menu's check).
    pub fn wraps_full(&self) -> bool {
        self.soft_wrap && self.wrap_full
    }

    /// View > Soft wrap: wraps long lines or stops wrapping them, for this file only.
    pub fn toggle_soft_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_wrap(!self.soft_wrap, self.wrap_full, window, cx);
    }

    /// Option-Z: a wrapped file flips between the reading column and the window's width; a file that
    /// doesn't wrap starts wrapping at the window's width.
    pub fn cycle_wrap_width(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let full = !self.soft_wrap || !self.wrap_full;
        self.set_wrap(true, full, window, cx);
    }

    fn set_wrap(&mut self, wrap: bool, full: bool, window: &mut Window, cx: &mut Context<Self>) {
        let column_changed = self.wrap_full != full;
        self.soft_wrap = wrap;
        self.wrap_full = full;
        if let Body::Built { editor, .. } = &self.body {
            editor.update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
        }
        if column_changed {
            self.apply_reading_width(cx);
        }
        cx.emit(DocumentChanged);
    }

    /// Whether the document holds edits the disk doesn't: unsaved, or scrubbed through the history
    /// (the text may differ from what the dot says until the slider closes).
    pub fn dirty(&self) -> bool {
        self.unsaved || self.scrubbing
    }

    /// An Undo history slider opens over this document's editor: its jumps send `Change` events
    /// that mustn't write drafts, and a reload ends it first. A draft write already waiting goes
    /// out now, before any scrubbed text exists.
    pub fn begin_scrub(&mut self, slider: WeakEntity<UndoSlider>, cx: &mut Context<Self>) {
        self.draft_due = None;
        self.draft_timer = None;
        self.flush_draft(cx);
        self.scrubbing = true;
        self.scrub_dirty = false;
        self.slider = Some(slider);
    }

    /// The slider is closed: when its jumps changed the text, the draft is written once now.
    pub fn end_scrub(&mut self, cx: &mut Context<Self>) {
        self.slider = None;
        if !std::mem::take(&mut self.scrubbing) {
            return;
        }
        self.scrub_settled(cx);
        if std::mem::take(&mut self.scrub_dirty) && self.path.is_none() {
            self.draft_changed(cx);
        }
    }

    /// Folds a live Undo history slider shut (Keep), as Done does, so an edit made now isn't refused.
    fn close_scrub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(slider) = self.slider.as_ref().and_then(|slider| slider.upgrade()) {
            slider.update(cx, |slider, cx| slider.shut(Close::Keep, window, cx, false));
            self.end_scrub(cx);
        }
    }

    /// The slider let go: the dot is right for where the text is. A file whose text is the text it
    /// was last opened, reloaded or saved with, and whose file is still there unchanged, is saved;
    /// any other text is unsaved.
    pub fn scrub_settled(&mut self, cx: &mut Context<Self>) {
        let Body::Built { editor, .. } = &self.body else { return };
        let Some(saved) = self.saved_text.as_ref().filter(|_| self.path.is_some()) else { return };
        // A file that is gone or changed on disk no longer holds the saved text.
        let on_disk = !self.conflict && !self.disk_gone;
        let text = editor.read(cx).text();
        let same = on_disk && text.len() == saved.len() && *text == *saved;
        if same != !self.unsaved {
            self.unsaved = !same;
            cx.emit(DocumentChanged);
        }
    }

    /// Shows `decoded`, what the file holds on disk now (last changed at `mtime`), in place of the
    /// editor's text, keeping the cursor line and the scroll; the file's encoding is taken from it
    /// too. Does nothing while the document has unsaved edits (they're never thrown away) unless
    /// `force` says to drop them, as Discard edits does. The editor is left alone when the text is
    /// already the same, as it is right after our own save.
    pub fn reload(&mut self, decoded: Decoded, mtime: Option<SystemTime>, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty() && !force {
            return;
        }
        let (was, shown) = self.adopt(decoded, mtime);
        if was {
            cx.emit(DocumentChanged);
        }
        let Some((editor, text)) = shown else { return };
        if let Some(slider) = self.slider.as_ref().and_then(|slider| slider.upgrade()) {
            // The slider's edits and the reload would fight over the history: it ends first.
            slider.update(cx, |slider, cx| slider.shut(Close::Keep, window, cx, false));
            self.end_scrub(cx);
        }
        let changed = editor.update(cx, |state, cx| {
            let cursor = state.cursor_position();
            let scroll = state.scroll_offset();
            // Replaces only the changed span and sends no Change event, so the
            // document stays saved.
            if !state.apply_external_text(&text, window, cx) {
                return false;
            }
            // Not `set_cursor_position`: it focuses the editor, and this one
            // may not be the one showing.
            let offset = state.text().position_to_offset(&cursor);
            state.set_selected_range(offset..offset, cx);
            state.set_scroll_offset(scroll, cx);
            true
        });
        self.saved_text = Some(editor.read(cx).text().clone());
        if !changed {
            return;
        }
        self.punct_len = editor.read(cx).text().len();
        // The editing history went with the old text, and the snapshots of Format > Line endings with it.
        self.endings_undo = None;
        if let Some(preview) = self.preview.as_mut() {
            // The editor washes what changed (after a quiet spell); the preview washes its blocks.
            preview.note_reload(editor.read(cx).external_wash_span(), self.punct_len, cx);
            preview.feed(&editor, cx);
        }
    }

    /// Takes `decoded` as the file's state: the text counts as saved, the file as unchanged since
    /// `mtime`, the edits and any question about the disk as settled. Returns whether the tab's
    /// marks changed, and for a document with an editor, that editor and the new text (still to
    /// be put in it); an unbuilt one keeps the text for when it is built.
    fn adopt(&mut self, decoded: Decoded, mtime: Option<SystemTime>) -> (bool, Option<(Entity<EditorState>, String)>) {
        // The text changes under any save in flight, so its landing mustn't count the new text as saved.
        self.saves.edited();
        // A Compare view of the old text has nothing left to compare.
        self.compare = None;
        let Decoded { text, encoding, has_bom, replaced } = decoded;
        self.encoding = encoding;
        self.disk_encoding = encoding;
        self.has_bom = has_bom;
        self.read_endings(&text);
        self.replaced = replaced;
        let was = self.unsaved || self.conflict || self.disk_gone;
        self.unsaved = false;
        self.conflict = false;
        self.disk_gone = false;
        self.saved_mtime = mtime;
        self.growing = false;
        match &mut self.body {
            Body::Built { editor, .. } => (was, Some((editor.clone(), text))),
            Body::Unbuilt(stored) => {
                *stored = text;
                self.endings_undo = None;
                (was, None)
            }
            // Stays unread: the file is read when it shows.
            Body::Unread => {
                self.endings_undo = None;
                (was, None)
            }
        }
    }

    /// Whether File > Revert to saved has anything to do: a file on disk with edits not yet saved and no save writing it.
    pub fn can_revert(&self) -> bool {
        self.path.is_some() && self.unsaved && !self.is_saving()
    }

    /// File > Revert to saved: asks once (Cancel is the default, so Return keeps the edits), then
    /// puts the file's text from disk back in place of the edits, keeping the caret line and the
    /// scroll. The undo history goes with them. Does nothing for a page with no file or one
    /// without edits.
    pub fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone().filter(|_| self.can_revert()) else { return };
        cx.emit(ShowTab);
        let name = self.title();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Revert to the saved version of \u{201C}{name}\u{201D}?"),
            Some("Your changes will be lost."),
            &[PromptButton::cancel("Cancel"), PromptButton::destructive("Revert")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(1) {
                return;
            }
            // A save started while the alert was up is writing the file in place: wait for it, so
            // the read never sees half of it. If it saved everything, there is nothing to revert.
            let Ok(landed) = this.update_in(cx, |this, window, cx| this.save_landed(window, cx)) else { return };
            landed.await;
            this.update_in(cx, |this, window, cx| {
                if this.can_revert() {
                    this.discard_for_disk(&path, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Whether File > Reopen with encoding has anything to do: any page with a file on disk.
    pub fn can_reopen(&self) -> bool {
        self.path.is_some()
    }

    /// File > Reopen with encoding: reads the file from disk again decoded as `encoding` (a wrong
    /// guess fixed), puts that text in place of the current one and saves in that encoding from now
    /// on. With unsaved edits it asks once first, as Revert does (Cancel is the default); a save in
    /// flight lands first. Bytes `encoding` has no character for show as U+FFFD, and a later Save
    /// asks before writing them. Does nothing for a page with no file.
    pub fn reopen_as(&mut self, encoding: &'static Encoding, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        let ask = self.unsaved.then(|| {
            cx.emit(ShowTab);
            let name = self.title();
            window.prompt(
                PromptLevel::Warning,
                &format!("Reopen \u{201C}{name}\u{201D} as {}?", encoding::label(encoding)),
                Some("Your changes will be lost."),
                &[PromptButton::cancel("Cancel"), PromptButton::destructive("Reopen")],
                cx,
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = match ask {
                Some(answer) => {
                    if answer.await.ok() != Some(1) {
                        return;
                    }
                    true
                }
                None => false,
            };
            // A save started meanwhile is writing the file in place: wait for it, so the read never sees half of it.
            let Ok(landed) = this.update_in(cx, |this, window, cx| this.save_landed(window, cx)) else { return };
            landed.await;
            this.update_in(cx, |this, window, cx| {
                // Edits typed while it waited, with no say-so to drop them, stay.
                if confirmed || !this.unsaved {
                    this.reread_as(&path, encoding, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes the file as the disk has it, decoded as `encoding`, dropping the unsaved edits.
    fn reread_as(&mut self, path: &Path, encoding: &'static Encoding, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.to_path_buf();
        let edits = self.saves.edits;
        cx.spawn_in(window, async move |this, cx| {
            let read_path = path.clone();
            let (mtime, read) = cx
                .background_executor()
                .spawn(async move {
                    let mtime = path_mtime(Some(&path));
                    (mtime, encoding::read_file_as(&path, encoding))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                // The tab was saved as or renamed meanwhile: that read is of another file.
                if this.path.as_deref() != Some(read_path.as_path()) {
                    return;
                }
                // Typed over, or a save writing, while it read: the text stays and the file is not taken.
                if this.saves.edits != edits || this.is_saving() {
                    return;
                }
                match read {
                    Ok(decoded) => {
                        this.reload(decoded, mtime, true, window, cx);
                        cx.emit(DocumentChanged);
                        cx.defer(menu::refresh);
                    }
                    Err(err) => {
                        let detail = file_ops::explain("read", &read_path, &err);
                        let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("read", &[read_path.clone()]), Some(&detail), &["OK"], cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// What the disk holds of this file now, as the window found it: `text` is the file's text
    /// when the document is clean and it was read. Returns whether the tab should close, which it
    /// does for a file that is gone while nothing is lost by it (no unsaved edits, or an empty
    /// page). A gone file under unsaved edits stays, marked; a file that comes back or changes
    /// reloads when clean and is checked for a conflict when not.
    pub fn on_disk(&mut self, disk: Disk, text: Option<Read>, epoch: u64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // The large-file question is up: the placeholder is empty and the file is read once it is answered.
        if self.awaiting_open {
            return false;
        }
        // What the disk shows while a save writes is that save, half done; its landing looks again.
        if self.saves.stale(epoch) {
            if self.saves.busy {
                self.saves.missed = true;
            } else {
                self.recheck_disk(window, cx);
            }
            return false;
        }
        if disk.gone {
            self.growing = false;
            if !self.dirty() || self.rope(cx).len() == 0 {
                return true;
            }
            let was = std::mem::replace(&mut self.disk_gone, true);
            let conflict = std::mem::replace(&mut self.conflict, false);
            if !was || conflict {
                cx.emit(DocumentChanged);
            }
            return false;
        }
        if std::mem::replace(&mut self.read_only, disk.read_only) != disk.read_only {
            cx.emit(DocumentChanged);
        }
        // A tab that never showed holds no text to reload; it reads the file when it does, so only the time is taken.
        if matches!(self.body, Body::Unread) {
            self.saved_mtime = disk.mtime;
            return false;
        }
        match text {
            Some(Read::Full(decoded)) => {
                if std::env::var_os("FINCH_FOLLOW_TRACE").is_some() {
                    eprintln!("finch: follow full {} bytes", decoded.text.len());
                }
                let before = self.rope(cx).len();
                let grew = decoded.text.len() > before;
                self.reload(decoded, disk.mtime, false, window, cx);
                self.recheck_highlighter_size(before, cx);
                // A log that keeps growing is paced like a tail read, even when this read took it whole.
                self.growing = grew && !self.dirty();
                // Whatever file this was read from is unverified; the next tail read compares it whole.
                self.grow_ident = None;
            }
            Some(Read::Tail(tail)) => self.append_tail(tail, disk.mtime, window, cx),
            None => {
                let conflict = has_conflict(self.dirty(), self.saved_mtime, disk.mtime);
                let was_gone = std::mem::replace(&mut self.disk_gone, false);
                if std::mem::replace(&mut self.conflict, conflict) != conflict || was_gone {
                    cx.emit(DocumentChanged);
                }
            }
        }
        false
    }

    /// What a read of only the bytes this file gained needs to start from: the text as the disk has
    /// it (a cheap clone, compared with the file's bytes off the main thread). `None` unless the
    /// editor holds exactly what the disk held at the last read: a built editor, no unsaved edits,
    /// no save writing, UTF-8 with nothing replaced. Every old byte is compared unless the file
    /// keeps growing, is over `GROW_FULL_MAX` and was whole-compared or read before (`grow_ident`).
    pub fn grow_base(&self, cx: &App) -> Option<GrowBase> {
        let Body::Built { editor, .. } = &self.body else { return None };
        if self.awaiting_open || self.unsaved || self.replaced || self.saves.busy || self.path.is_none() || self.encoding != encoding_rs::UTF_8 {
            return None;
        }
        let text = editor.read(cx).text();
        let full = !self.growing || text.len() <= encoding::GROW_FULL_MAX || self.grow_ident.is_none();
        Some(GrowBase { len: text.len(), bom: if self.has_bom { 3 } else { 0 }, text: text.clone(), full, ident: self.grow_ident })
    }

    /// Whether the last thing read of the file was a growth, which paces its reloads.
    pub fn growing(&self) -> bool {
        self.growing
    }

    /// Whether a growth could not be applied to the text it was read against (typed over meanwhile):
    /// the window reads again. Reading it clears it.
    pub fn take_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry)
    }

    /// The file only grew: puts `tail` after the editor's text, keeping the history and the
    /// caret's place, and takes the file as read as of `mtime`. Does nothing while the document has
    /// unsaved edits (they're never thrown over); a tail read against text that has since changed
    /// is dropped and asked for again (`take_retry`).
    fn append_tail(&mut self, tail: Tail, mtime: Option<SystemTime>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty() {
            return;
        }
        let Body::Built { editor, .. } = &self.body else {
            self.retry = true;
            return;
        };
        let editor = editor.clone();
        let before = editor.read(cx).text().len();
        if before != tail.base {
            self.retry = true;
            return;
        }
        if std::env::var_os("FINCH_FOLLOW_TRACE").is_some() {
            eprintln!("finch: follow tail {} bytes read, {} added, disk {}", tail.read, tail.text.len(), tail.disk_len);
        }
        // The text changes under any save in flight, and a Compare view of the old text has nothing left to compare.
        self.saves.edited();
        self.compare = None;
        let was = self.conflict || self.disk_gone;
        self.conflict = false;
        self.disk_gone = false;
        self.saved_mtime = mtime;
        self.grow_ident = Some(tail.ident);
        self.growing = true;
        if was {
            cx.emit(DocumentChanged);
        }
        if tail.text.is_empty() {
            return;
        }
        // Endings: the tail has bare LF or CRLF breaks only (`read_tail` takes anything else whole).
        let breaks = encoding::endings(&tail.text);
        if tail.text.contains('\n') {
            let had_break = editor.read(cx).text().lines_len() > 1;
            let tail_crlf = breaks == Endings::Crlf;
            if breaks == Endings::Mixed || (had_break && !self.mixed && self.crlf != tail_crlf) {
                self.mixed = true;
                self.crlf = false;
            } else if !had_break {
                self.crlf = tail_crlf;
            }
        }
        editor.update(cx, |state, cx| state.append_external_text(&tail.text, window, cx));
        let len = editor.read(cx).text().len();
        self.saved_text = Some(editor.read(cx).text().clone());
        self.recheck_highlighter_size(before, cx);
        self.punct_len = len;
        if let Some(preview) = self.preview.as_mut() {
            preview.note_reload(editor.read(cx).external_wash_span(), len, cx);
            preview.feed(&editor, cx);
        }
    }

    /// The encoding the file is read and saved in.
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// File > Convert to: the next save writes `encoding`, and the tab is unsaved now. The text is
    /// not touched and nothing is written until Save; a character the new encoding can't hold is
    /// caught by the lossy-save question then, as for any save. The byte order mark: UTF-16 from
    /// any other encoding always has one, UTF-16 to UTF-16 and to UTF-8 keep the file's choice,
    /// any other encoding has none. An Untitled page converts too: it
    /// has no file to read again, but an encoding to save in. Nothing changes when the page is
    /// in that encoding already.
    pub fn convert_to(&mut self, encoding: &'static Encoding, cx: &mut Context<Self>) {
        if encoding == self.encoding {
            return;
        }
        if self.read_only {
            crate::preview::render::beep();
            return;
        }
        if matches!(self.body, Body::Unread) {
            return;
        }
        let utf16 = |encoding: &'static Encoding| encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE;
        self.has_bom = if utf16(encoding) {
            // UTF-16 without a mark doesn't read back reliably: from anything but UTF-16 it always gets one.
            !utf16(self.encoding) || self.has_bom
        } else {
            // UTF-8 keeps the file's own choice (a non-Unicode file has none to keep); the others have none.
            encoding == encoding_rs::UTF_8 && self.has_bom
        };
        self.encoding = encoding;
        self.saves.edited();
        self.unsaved = true;
        self.saved_text = None;
        cx.emit(DocumentChanged);
        cx.notify();
    }

    /// The line breaks the file is saved with: CRLF when every break was one at open (or after
    /// Format > Line endings), Mixed when it had both kinds and nothing was picked, otherwise LF.
    pub fn endings(&self) -> Endings {
        if self.mixed {
            Endings::Mixed
        } else if self.crlf {
            Endings::Crlf
        } else {
            Endings::Lf
        }
    }

    /// Takes the saved line-ending style from text just read from disk.
    fn read_endings(&mut self, text: &str) {
        let endings = encoding::endings(text);
        self.crlf = endings == Endings::Crlf;
        self.mixed = endings == Endings::Mixed;
    }

    /// Format > Line endings: rewrites every break to CRLF (`crlf`) or LF as one undoable edit,
    /// marks the tab unsaved and makes that the style a save writes and Return and paste follow.
    /// Nothing changes when the file already has that style (a Mixed file has neither). The text
    /// is converted in one pass and enters the editor as one replacement of the stretch from the
    /// first break to the last. Nothing is written until Save.
    pub fn set_line_endings(&mut self, crlf: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.endings() == if crlf { Endings::Crlf } else { Endings::Lf } {
            return;
        }
        if self.read_only {
            crate::preview::render::beep();
            return;
        }
        // The conversion is an ordinary edit: a slider scrubbed back would refuse it, so it folds first.
        self.close_scrub(window, cx);
        let convert = |text: &str| -> Option<String> {
            match if crlf { encoding::to_crlf(text) } else { encoding::to_lf(text) } {
                Cow::Owned(converted) => Some(converted),
                Cow::Borrowed(_) => None,
            }
        };
        let before_style = (self.crlf, self.mixed);
        let undo = match &mut self.body {
            Body::Built { editor, .. } => editor.clone().update(cx, |state, cx| {
                let text = state.text().to_string();
                let converted = convert(&text)?;
                let before = Fingerprint::of(state.text());
                let (range, scroll) = (state.selected_range(), state.scroll_offset());
                let new_len = converted.len();
                // Where an offset lands once the breaks before it are rewritten.
                let moved = |at: usize| {
                    let bytes = &text.as_bytes()[..at];
                    let to = if crlf {
                        at + (0..at).filter(|&i| bytes[i] == b'\n' && (i == 0 || bytes[i - 1] != b'\r')).count()
                    } else {
                        at - bytes.windows(2).filter(|pair| *pair == b"\r\n").count()
                    };
                    to.min(new_len)
                };
                state.replace_all(converted, window, cx);
                state.set_selected_range(moved(range.start)..moved(range.end), cx);
                state.set_scroll_offset(scroll, cx);
                Some(EndingsUndo { before, before_style, after: Fingerprint::of(state.text()), crlf })
            }),
            Body::Unbuilt(text) => {
                if let Some(converted) = convert(text) {
                    *text = converted;
                }
                None
            }
            // Not read yet, so not showing: no menu or field reaches it.
            Body::Unread => return,
        };
        self.crlf = crlf;
        self.mixed = false;
        self.endings_undo = undo;
        // A conversion of a text with no break to change has no edit to say so.
        self.saves.edited();
        self.unsaved = true;
        cx.emit(DocumentChanged);
        cx.notify();
    }

    /// The file's time as of our last read or write of it.
    pub fn saved_mtime(&self) -> Option<SystemTime> {
        self.saved_mtime
    }

    /// Whether the tab shows the file changed on disk under unsaved edits.
    pub fn in_conflict(&self) -> bool {
        self.conflict
    }

    /// Whether the tab shows the file deleted from disk under unsaved edits.
    pub fn is_gone(&self) -> bool {
        self.disk_gone
    }

    /// Whether the file can't be written, as of the last time we looked at it.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// File > Duplicate of a tab with unsaved edits: `None` for a clean tab or a page with no
    /// file, which copy the file on disk. Otherwise a refusal now (text its encoding can't
    /// keep, so no file is made), or the job that makes `name copy.ext` next to the file from
    /// a snapshot of the tab's text, in its encoding, BOM and line endings. The job writes and
    /// syncs, so it runs off the main thread; it leaves the original file and the tab's edits alone.
    pub fn duplicate_edits(&self, cx: &App) -> Option<Result<Box<dyn FnOnce() -> Result<PathBuf, String> + Send>, String>> {
        let from = self.path.clone().filter(|_| self.unsaved)?;
        let (encoding, has_bom, crlf) = (self.encoding, self.has_bom, self.crlf);
        if self.replaced {
            return Some(Err(format!("The text has characters {} can’t keep. Save the file in another encoding first, then duplicate it.", encoding.name())));
        }
        let text = self.rope(cx);
        // Counted from now, on the main thread, so quitting waits for the copy.
        let guard = WriteGuard::new();
        Some(Ok(Box::new(move || {
            let _guard = guard;
            file_ops::duplicate_text(&from, |to| write_copy(to, &text, encoding, has_bom, crlf))
        })))
    }

    /// After a save under a new name, or an Untitled page's first line changing what it reads as:
    /// a markdown page gets a warm preview, any other loses it.
    fn sync_preview(&mut self, lang: &str, cx: &mut Context<Self>) {
        // An unbuilt document makes its preview when it builds.
        if !self.is_built() {
            return;
        }
        if lang != "markdown" {
            self.preview = None;
        } else if self.preview.is_none() {
            let text = self.text(cx);
            let cold = Preview::starts_cold(text.len(), cx);
            let mut preview = Preview::new(if cold { "" } else { &text }, cx);
            if let Some(editor) = self.built_editor().cloned().filter(|_| cold) {
                preview.cool(&editor, cx);
            }
            self.preview = Some(preview);
        }
        if let Some(editor) = self.built_editor() {
            crate::note_links::editor::attach(editor, lang == "markdown", cx);
        }
    }

    /// The window title: the file's name, or an Untitled page's first line once it has settled.
    pub fn title(&self) -> String {
        match (&self.path, &self.draft_title) {
            (None, Some(line)) => line.clone(),
            _ => title(self.path.as_deref()),
        }
    }

    /// The title an Untitled page's first line gives its tab, `None` for a file or an empty first line.
    pub fn draft_title(&self) -> Option<&str> {
        self.draft_title.as_deref().filter(|_| self.path.is_none())
    }

    /// The file an Untitled page's text is kept in, `None` for a file or a page not yet written.
    pub fn draft_path(&self) -> Option<&Path> {
        self.draft.as_deref()
    }

    /// The title the top of the page's text gives (see `page_title`).
    fn first_line_title(&self, cx: &App) -> Option<String> {
        match &self.body {
            Body::Built { editor, .. } => first_line_title(editor.read(cx).text().chars()),
            Body::Unbuilt(text) => first_line_title(text.chars()),
            Body::Unread => None,
        }
    }

    /// Sets the title from the first line `DRAFT_DELAY` after the latest edit: one task per burst, each edit moves the deadline.
    fn title_pause(&mut self, cx: &mut Context<Self>) {
        if self.title_due.replace(Instant::now() + DRAFT_DELAY).is_some() {
            return;
        }
        self.title_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(due)) = this.update(cx, |this, _| this.title_due) else { return };
                let now = Instant::now();
                if due > now {
                    cx.background_executor().timer(due - now).await;
                    continue;
                }
                this.update(cx, |this, cx| {
                    this.title_due = None;
                    let title = this.first_line_title(cx);
                    if title != this.draft_title {
                        this.draft_title = title;
                        cx.emit(DocumentChanged);
                    }
                })
                .ok();
                return;
            }
        }));
    }

    /// The name Save offers: the page's title (see `page_title`) made safe for a file name, `.md` when the page reads as Markdown (a heading, front matter or a Markdown template), else `.txt`; `Untitled.txt` (or `.md`) when nothing names it.
    fn save_name(&self, cx: &App) -> String {
        let name = match &self.body {
            Body::Built { editor, .. } => save_name_of(editor.read(cx).text().chars()),
            Body::Unbuilt(text) => save_name_of(text.chars()),
            Body::Unread => "Untitled.txt".to_owned(),
        };
        // A page made from a Markdown template saves as one, so it stays Markdown.
        match name.strip_suffix(".txt") {
            Some(stem) if self.path.is_none() && self.language(cx) == "markdown" => format!("{stem}.md"),
            _ => name,
        }
    }

    /// Whether the tab and the window show the unsaved dot: a file with edits not yet saved.
    /// An Untitled page never does while its draft is being written; one whose draft write
    /// failed does, since its text is nowhere else.
    pub fn shows_dot(&self) -> bool {
        self.unsaved && (self.path.is_some() || self.draft_failed)
    }

    /// A page with no file, no draft and no edits: nothing leads back to it.
    pub fn is_blank(&self) -> bool {
        !self.awaiting_open && self.path.is_none() && self.draft.is_none() && !self.unsaved
    }

    /// The file that stands for this tab across a relaunch: its path, or its draft's.
    pub fn tab_file(&self) -> Option<PathBuf> {
        self.path.clone().or_else(|| self.draft.clone())
    }

    /// An Untitled page changed: the first character makes its draft file at once, any
    /// later edit is written `DRAFT_DELAY` after typing stops, and a page with no text left
    /// loses its file (the next character makes a new one).
    fn draft_changed(&mut self, cx: &mut Context<Self>) {
        self.draft_edits += 1;
        let empty = match &self.body {
            Body::Built { editor, .. } => editor.read(cx).text().len() == 0,
            Body::Unbuilt(text) => text.is_empty(),
            Body::Unread => false,
        };
        if empty {
            self.drop_draft(cx);
            cx.emit(DocumentChanged);
            return;
        }
        self.title_pause(cx);
        if self.draft.is_none() {
            let path = drafts::new_path(&drafts::folder(), &drafts::stamp_now());
            // Held until its first write lands, so a second page can't take the same name meanwhile.
            drafts::claim(&path);
            self.set_draft(path);
            self.draft_waiting = true;
            self.write_draft_in_background(cx);
            cx.emit(DocumentChanged);
            return;
        }
        self.draft_waiting = true;
        // One task per burst: each edit moves the deadline, and the task sleeps on to it, but
        // never past `DRAFT_MAX_WAIT` after the oldest edit not yet written, so steady typing
        // still reaches the disk.
        let now = Instant::now();
        let first = *self.draft_first.get_or_insert(now);
        if self.draft_due.replace((now + DRAFT_DELAY).min(first + DRAFT_MAX_WAIT)).is_some() {
            return;
        }
        self.draft_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(due)) = this.update(cx, |this, _| this.draft_due) else { return };
                let now = Instant::now();
                if due > now {
                    cx.background_executor().timer(due - now).await;
                    continue;
                }
                this.update(cx, |this, cx| {
                    this.draft_due = None;
                    this.write_draft_in_background(cx);
                })
                .ok();
                return;
            }
        }));
    }

    /// Makes `path` this page's draft, with a fresh slot for its writes.
    fn set_draft(&mut self, path: PathBuf) {
        self.draft_slot = Some(drafts::Slot::shared(path.clone()));
        self.draft = Some(path);
        self.draft_settled = 0;
    }

    /// Writes the draft off the main thread: the text now, tagged with the edit count. The
    /// slot runs one write at a time and skips one older than what landed, so a slow write
    /// can't put an old copy over a newer one.
    fn write_draft_in_background(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.draft_slot.clone() else { return };
        let tag = self.draft_edits;
        self.draft_first = None;
        let text = self.rope(cx);
        cx.spawn(async move |this, cx| {
            let written = slot.clone();
            let (attempt, result) = cx
                .background_executor()
                .spawn(async move { drafts::lock(&written).write(tag, &text) })
                .await;
            this.update(cx, |this, cx| {
                // Not the page's draft any more, or a later try already reported.
                if this.draft_slot.as_ref().is_some_and(|current| current.same(&slot)) && attempt > this.draft_settled {
                    this.draft_settled = attempt;
                    this.draft_result(tag, result, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Writes the draft now if it has edits not yet on disk, and waits for a write in flight:
    /// when the tab closes, the window closes, the app quits or the window loses focus.
    pub fn flush_draft(&mut self, cx: &mut Context<Self>) {
        // Scrubbed text is never a draft: the slider's close writes it once, and the newest
        // text was already written when it was typed.
        if self.scrubbing && self.scrub_dirty {
            // Wait for a write in flight, without writing the scrubbed text.
            if let Some(slot) = &self.draft_slot {
                drop(drafts::lock(slot));
            }
            return;
        }
        let Some(slot) = self.draft_slot.clone().filter(|_| self.draft_waiting) else { return };
        let tag = self.draft_edits;
        self.draft_first = None;
        let text = self.rope(cx);
        let (attempt, result) = drafts::lock(&slot).write(tag, &text);
        self.draft_settled = attempt;
        self.draft_result(tag, result, cx);
    }

    /// A write of the text at edit `tag` finished.
    fn draft_result(&mut self, tag: u64, result: std::io::Result<()>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                // The file exists now: its name is held by the file, not by the claim.
                if let Some(draft) = &self.draft {
                    drafts::release(draft);
                }
                if tag == self.draft_edits {
                    self.draft_waiting = false;
                }
                if std::mem::take(&mut self.draft_failed) {
                    cx.emit(DocumentChanged);
                }
            }
            Err(err) => {
                let name = self.draft.as_deref().map_or_else(String::new, |draft| draft.display().to_string());
                eprintln!("finch: can't write the draft {name}: {err}");
                // The text isn't on disk, so the page now shows the dot and asks like a file.
                if !std::mem::replace(&mut self.draft_failed, true) {
                    cx.emit(DocumentChanged);
                }
            }
        }
    }

    /// Forgets the draft and deletes its file: the page was saved somewhere, or has no text left.
    /// The delete waits for a write in flight on a background thread, so the page doesn't.
    fn drop_draft(&mut self, cx: &mut Context<Self>) {
        let name = self.draft.clone();
        if let Some(slot) = self.forget_draft() {
            // The name stays claimed until the delete has run, so no new page takes it meanwhile.
            if let Some(name) = &name {
                drafts::claim(name);
            }
            // Counted from now and handed straight to the background, so quitting waits for the delete.
            let guard = WriteGuard::new();
            cx.background_executor()
                .spawn(async move {
                    let _guard = guard;
                    if let Err(err) = drafts::lock(&slot).delete() {
                        eprintln!("finch: can't delete the draft: {err}");
                    }
                    if let Some(name) = name {
                        drafts::release(&name);
                    }
                })
                .detach();
        }
    }

    /// The page's draft file was saved over by another page's Save As: this page lets that file go
    /// untouched and moves its text to a fresh draft, after a write still running has landed.
    pub fn leave_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(slot) = self.forget_draft() {
            drop(drafts::lock(&slot));
        }
        self.draft_changed(cx);
    }

    /// Lets the page's draft go without touching its file, and returns its slot, closed first
    /// so no write starts after a delete is asked for. Save As onto the draft's own file lands
    /// here: that file is the saved text now.
    fn forget_draft(&mut self) -> Option<drafts::Shared> {
        self.draft_timer = None;
        self.title_timer = None;
        self.title_due = None;
        self.draft_title = None;
        self.draft_due = None;
        self.draft_waiting = false;
        self.draft_failed = false;
        if let Some(draft) = self.draft.take() {
            drafts::release(&draft);
        }
        self.draft_first = None;
        let slot = self.draft_slot.take()?;
        slot.close();
        Some(slot)
    }

    /// Saves to the file's path, or asks for one when it has none or can't be written to (a
    /// read-only file asks first, `ask_read_only`). A file that changed on disk under unsaved edits
    /// asks first (`ask_overwrite`), and one that is gone asks too (`ask_recreate`). The task says
    /// whether the text reached the disk.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        // A tab that never showed has nothing unsaved to write.
        if matches!(self.body, Body::Unread) {
            return Task::ready(true);
        }
        // A save is writing: the file's time is its own, so look again once it has landed.
        if self.saves.busy || self.saves.asking {
            return self.after_save(window, cx, |this, window, cx| this.save(window, cx));
        }
        if let Some(path) = self.path.clone() {
            let disk = probe(&path);
            if disk.gone {
                return self.ask_recreate(path, window, cx);
            }
            if std::mem::replace(&mut self.read_only, disk.read_only) != disk.read_only {
                cx.emit(DocumentChanged);
            }
            if disk.read_only {
                // The guide is read-only by design: its copy goes straight to the panel.
                if crate::app::guide_dir().is_some_and(|guide| path.starts_with(guide)) {
                    return self.save_as(window, cx);
                }
                return self.ask_read_only(window, cx);
            }
            if has_conflict(self.unsaved, self.saved_mtime, disk.mtime) {
                return self.ask_overwrite(path, window, cx);
            }
            // Nothing unsaved and the file moved on: another program wrote it, and this copy is the
            // older one. Showing the disk's text is the save; writing would replace theirs.
            if !self.dirty() && moved_on_disk(self.saved_mtime, disk.mtime) {
                self.recheck_disk(window, cx);
                return Task::ready(true);
            }
            return self.write(&path, true, window, cx);
        }
        self.save_as(window, cx)
    }

    /// The file can't be written: says so and asks Cancel / Save as… with this tab in front, and
    /// Unlock between them when the file is the user's own on a volume that can be written (the
    /// Finder lock and the mode bits are theirs to undo). Cancel comes first so Return and Escape
    /// leave the file and the tab alone. Unlock takes the lock off and then runs the save that was
    /// asked for. The task says whether the text reached the disk.
    fn ask_read_only(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        cx.emit(ShowTab);
        let unlock_path = self.path.clone().filter(|path| unlockable(path));
        let (title, detail) = read_only_question(&self.title(), unlock_path.is_some());
        let buttons: &[&str] = if unlock_path.is_some() { &["Cancel", "Unlock", "Save as…"] } else { &["Cancel", "Save as…"] };
        let answer = window.prompt(PromptLevel::Warning, &title, Some(detail), buttons, cx);
        self.saves.asking = true;
        cx.spawn_in(window, async move |this, cx| {
            let choice = answer.await.ok();
            let task = this.update_in(cx, |this, window, cx| {
                this.saves.asking = false;
                match (choice, unlock_path) {
                    (Some(1), Some(path)) => {
                        let failed = match unlock(&path) {
                            // Something besides the lock (an ACL, say) can still refuse the write:
                            // saving would only bring the same question back.
                            Ok(()) if writable(&path) => return this.save(window, cx),
                            Ok(()) => format!("Something other than the lock keeps \u{201C}{}\u{201D} read-only. Use Save as… to keep your changes in a copy.", this.title()),
                            Err(err) => file_ops::explain("unlock", &path, &err),
                        };
                        let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("unlock", &[path.clone()]), Some(&failed), &["OK"], cx);
                        Task::ready(false)
                    }
                    (Some(1), None) | (Some(2), Some(_)) => this.save_as(window, cx),
                    _ => Task::ready(false),
                }
            });
            let saved = match task {
                Ok(task) => task.await,
                Err(_) => false,
            };
            // Saves asked for while the question was open go once it is answered, as after a write.
            this.update(cx, |this, _| {
                for waiter in this.save_waiters.drain(..) {
                    let _ = waiter.send(());
                }
            })
            .ok();
            saved
        })
    }

    /// Asks where to save (in the file's folder under its name when it has a
    /// path, else in the tree's folder) and saves there; the document then belongs to the new path. The
    /// task says whether the text reached the disk.
    pub fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let named = self.path.as_deref().and_then(|path| Some((path.parent()?.to_path_buf(), path.file_name()?.to_string_lossy().into_owned())));
        // The guide's folder is inside the app, which the next install replaces: a copy goes elsewhere.
        let named = named.map(|(dir, name)| match crate::app::guide_dir() {
            Some(guide) if dir.starts_with(guide) => (untitled_start(self.tree_root.as_deref(), std::env::current_dir().ok(), std::env::home_dir()), name),
            _ => (dir, name),
        });
        let (dir, name) = named.unwrap_or_else(|| {
            // A page with no file starts in the tree's folder. Launched from Finder the working
            // directory is `/`; the home folder stands in then.
            (untitled_start(self.tree_root.as_deref(), std::env::current_dir().ok(), std::env::home_dir()), self.save_name(cx))
        });
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return false };
            let Ok(save) = this.update_in(cx, |this, window, cx| this.save_to(path, window, cx)) else { return false };
            save.await
        })
    }

    /// Saves to `path` as Save As does once the panel has answered; the control socket's
    /// `save-as` verb comes here too, since a native panel can't be driven.
    pub fn save_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        cx.spawn_in(window, async move |this, cx| {
            let Ok(write) = this.update_in(cx, |this, window, cx| {
                // Saving onto the page's own draft: a draft write still running must land first,
                // or it would rename an older copy over the saved file.
                let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if let (Some(slot), Some(draft)) = (&this.draft_slot, this.draft.as_deref()) {
                    if real(draft) == real(&path) {
                        drop(drafts::lock(slot));
                    }
                }
                this.write(&path, false, window, cx)
            }) else {
                return false;
            };
            let saved = write.await;
            this.update_in(cx, |this, _window, cx| {
                let untitled = this.path.is_none();
                // A failed Save As leaves the document where it was; a draft always does, since
                // it is the only copy.
                if saved || (untitled && this.draft.is_none()) {
                    let lang = this.language_at(Some(&path), cx);
                    this.set_highlighter(lang, true, cx);
                    this.path = Some(path.clone());
                    this.sync_preview(lang, cx);
                    if !saved {
                        // Nothing was written there: no text is the saved one.
                        this.saved_text = None;
                    }
                }
                if saved {
                    let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                    if this.draft.as_deref().is_some_and(|draft| real(draft) == real(&path)) {
                        // Saved onto the page's own draft file: that file is the saved text now.
                        this.forget_draft();
                    } else {
                        this.drop_draft(cx);
                    }
                    // A file saved under a new name is one he has opened, for Open Recent.
                    crate::app::note_recent(&path, cx);
                    // Another tab on this file (any window) can't stay beside this one. After this
                    // update, so the windows can be read.
                    let (keeper, path) = (cx.entity(), path.clone());
                    cx.spawn(async move |_, cx| {
                        cx.update(|cx| crate::workspace::drop_twin_tabs(&keeper, &path, cx));
                    })
                    .detach();
                }
                cx.emit(DocumentChanged);
                saved
            })
            .unwrap_or(false)
        })
    }

    /// Save As landed on this file from another tab: its unsaved edits can't be saved over that
    /// text, so asks Cancel / Discard and close with this tab in front. Cancel keeps both tabs.
    pub fn ask_replaced(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ShowTab);
        let name = self.title();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("\u{201C}{name}\u{201D} was just replaced by another tab"),
            Some("Another tab saved over this file. Discard and close closes this tab and discards its unsaved changes. Cancel keeps both tabs open."),
            &[PromptButton::cancel("Cancel"), PromptButton::destructive("Discard and close")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(1) {
                this.update(cx, |this, cx| {
                    this.discard_edits();
                    cx.emit(DropTab);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Applies the save settings to the text (`normalize`), then writes it (`write_now`). When
    /// the edit changed the text, the write waits one turn so the editor's change event has
    /// been taken before the write's snapshot; otherwise the event would count as an edit made
    /// during the save, and the tab would keep its dot. `offer_save_as` is false for a path just
    /// chosen in Save As, so a failed recovery ends at its error instead of asking again.
    fn write(&mut self, path: &Path, offer_save_as: bool, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if !self.normalize(window, cx) {
            return self.write_now(path, offer_save_as, window, cx);
        }
        let path = path.to_path_buf();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(write) = this.update_in(cx, |this, window, cx| this.write_now(&path, offer_save_as, window, cx)) else { return false };
            write.await
        })
    }

    /// Before a file is written: strips trailing spaces and tabs and ends the text with a newline
    /// when the settings ask for it, as one undo step with the caret and the scroll where they
    /// were. Returns whether the text changed. Never run for a draft, and not while a save is
    /// writing.
    fn normalize(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(slider) = self.slider.as_ref().and_then(|slider| slider.upgrade()) {
            // A Save mid-scrub folds like Done, so the edit below isn't refused.
            slider.update(cx, |slider, cx| slider.shut(Close::Keep, window, cx, false));
            self.end_scrub(cx);
        }
        let (trim, newline) = {
            let editor = &settings::get(cx).editor;
            (editor.trim_trailing_whitespace, editor.insert_final_newline)
        };
        let Body::Built { editor, .. } = &self.body else { return false };
        let markdown = self.preview.is_some();
        if (!trim && !newline) || self.saves.busy || self.saves.asking {
            return false;
        }
        editor.clone().update(cx, |state, cx| {
            let (before, scroll) = (state.text().len(), state.scroll_offset());
            state.normalize_for_save(trim, newline, markdown, window, cx);
            state.set_scroll_offset(scroll, cx);
            state.text().len() != before
        })
    }

    /// Writes the text to `path` off the main thread, from a snapshot of it taken now: a plain
    /// UTF-8 file streams the rope out as it is, any other encoding (or UTF-8 with a BOM) is
    /// encoded whole and written the same way, in place. Text the encoding can't hold asks first
    /// (`ask_lossy`). A write asked for while another is in flight waits for it, then writes the
    /// text as it is by then. The task says whether the text reached the disk.
    fn write_now(&mut self, path: &Path, offer_save_as: bool, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if self.awaiting_open {
            return Task::ready(false);
        }
        let path = path.to_path_buf();
        let Some(snapshot) = self.saves.begin() else {
            return self.after_save(window, cx, move |this, window, cx| this.write(&path, offer_save_as, window, cx));
        };
        // A save still writing `SAVE_SLOW` after it began marks itself slow, so the tab's dot breathes until it lands. Dropped with the save.
        self.saves.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(crate::tokens::SAVE_SLOW)).await;
            this.update(cx, |this, cx| {
                this.saves.slow = true;
                cx.emit(DocumentChanged);
            })
            .ok();
        }));
        let text = self.rope(cx);
        let kept = text.clone();
        let (encoding, has_bom, replaced, crlf) = (self.encoding, self.has_bom, self.replaced, self.crlf);
        let began = self.path.clone();
        let expected = self.saved_mtime;
        // The old saved text, kept as a version before the file is overwritten: a write cut short
        // leaves a mix, and this is then the only copy of the old text. Only for the tab's own file.
        let old = self.saved_text.clone().filter(|old| !replaced && self.path.as_deref() == Some(path.as_path()) && old.len() as u64 <= crate::versions::MAX_FILE_BYTES);
        let settings_file = settings::Store::default_path();
        // Counted from now, on the main thread, and handed to the background executor right here,
        // not from a task of this thread: quit waits for the count on the main thread, where such a
        // task would never run. Dropped when the write ends, panic or not.
        let guard = WriteGuard::new();
        let executor = cx.background_executor().clone();
        let target = path.clone();
        let write = cx.background_executor().spawn(async move {
            let _guard = guard;
            // Bytes a Reopen with encoding couldn't read, or that this encoding writes differently,
            // would change for good on a write: it asks first, whatever the text now says.
            if replaced {
                return (Written::Lossy(None), None);
            }
            // One writer of a file at a time, whichever tab asks.
            let _turn = PathTurn::wait_for(&target);
            if let Some(old) = old {
                crate::versions::record(&target, &old, SystemTime::now());
            }
            // The settings file is also written by the settings store: take turns with it.
            let same = |a: &Path, b: &Path| matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b);
            let _settings = same(&target, &settings_file).then(settings::hold_writes);
            let before = probe(&target).mtime;
            let written = write_out(&target, &text, encoding, has_bom, crlf);
            drop(_settings);
            let mut failed = None;
            match &written {
                Written::Done(Ok(_)) if text.len() as u64 <= crate::versions::MAX_FILE_BYTES => {
                    // Counted before this write's own count ends, so quit waits for the version too.
                    // Never awaited: a version can't delay or fail a save.
                    let version_guard = WriteGuard::new();
                    let (path, text) = (target.clone(), text.clone());
                    executor
                        .spawn(async move {
                            let _guard = version_guard;
                            crate::versions::record(&path, &text, SystemTime::now())
                        })
                        .detach();
                }
                Written::Done(Err(_)) if before == expected => failed = Some(probe(&target)),
                _ => {}
            }
            (written, failed)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (written, failed) = write.await;
            let landed_ok = match &written {
                Written::Done(Ok(disk)) => Some(*disk),
                _ => None,
            };
            let Ok(next) = this.update_in(cx, |this, window, cx| {
                let unchanged = this.saves.finish(snapshot);
                // The tab was renamed or moved while the text was on its way: the tab's file
                // isn't the one written, so the tab keeps its dot.
                let moved = this.path != began;
                let task = match written {
                    Written::Lossy(culprit) => {
                        this.saves.asking = true;
                        this.ask_lossy(path, culprit, offer_save_as, window, cx)
                    }
                    Written::Done(result) => {
                        if result.is_ok() && !moved {
                            this.saved_text = Some(kept.clone());
                        }
                        // A failed write can leave old and new bytes mixed: the file no longer holds
                        // the saved text. Its new time is taken (unless someone else changed the file
                        // before the write) so the next save rewrites it instead of asking "changed
                        // on disk".
                        if result.is_err() && !moved && this.path.as_deref() == Some(path.as_path()) {
                            this.saved_text = None;
                            if let Some(disk) = failed.filter(|disk| !disk.gone) {
                                this.saved_mtime = disk.mtime;
                            }
                        }
                        this.landed(&path, result, encoding, unchanged && !(moved && began.is_some()), moved, offer_save_as, window, cx)
                    }
                };
                // A disk check that came in during the write was dropped: look again now.
                if std::mem::take(&mut this.saves.missed) {
                    this.recheck_disk(window, cx);
                }
                task
            }) else {
                // The tab has no window now (dragged out of one that closed): the save still ends,
                // or it would stay busy for good and Cmd-S, close and Quit would wait on it. A write
                // that reached the disk is still recorded, so the tab doesn't ask about its own file.
                this.update(cx, |this, cx| {
                    let unchanged = this.saves.finish(snapshot);
                    if let (Some(disk), true) = (landed_ok, this.path == began) {
                        this.disk_encoding = encoding;
                        this.saved_text = Some(kept.clone());
                        if unchanged {
                            this.unsaved = false;
                        } else {
                            this.scrub_settled(cx);
                        }
                        this.saved_mtime = disk.mtime;
                        this.read_only = disk.read_only;
                        this.conflict = false;
                        this.disk_gone = false;
                        cx.emit(DocumentChanged);
                    }
                    this.saves.asking = false;
                    for waiter in this.save_waiters.drain(..) {
                        let _ = waiter.send(());
                    }
                })
                .ok();
                return false;
            };
            let saved = next.await;
            // Waiting saves go once the question, if any, has been answered.
            this.update(cx, |this, _| {
                this.saves.asking = false;
                for waiter in this.save_waiters.drain(..) {
                    let _ = waiter.send(());
                }
            })
            .ok();
            saved
        })
    }

    /// Runs `then` once the save in flight has landed.
    fn after_save(&mut self, window: &mut Window, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) -> Task<bool> + 'static) -> Task<bool> {
        let (sender, receiver) = oneshot::channel();
        self.save_waiters.push(sender);
        cx.spawn_in(window, async move |this, cx| {
            if receiver.await.is_err() {
                return false;
            }
            let Ok(next) = this.update_in(cx, then) else { return false };
            next.await
        })
    }

    /// How many writes have failed with an error shown, so a save can tell a refused write from a cancelled question.
    pub fn write_errors(&self) -> u32 {
        self.write_errors
    }

    /// Takes the result of writing `path`: a saved document is clean (unless it was edited while
    /// the write ran, when it stays dirty) and notes the file's new time, a failed write says why.
    /// `moved`: the tab was renamed or moved during the write, so the file's time comes from where
    /// it is now. Returns whether the text reached the disk.
    fn landed(&mut self, path: &Path, result: io::Result<Disk>, written_in: &'static Encoding, unchanged: bool, moved: bool, offer_save_as: bool, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        match result {
            Ok(disk) => {
                // The disk holds the bytes just written, in the encoding they were written in.
                self.disk_encoding = written_in;
                if unchanged {
                    self.unsaved = false;
                }
                let disk = match self.path.as_deref() {
                    Some(now) if moved => probe(now),
                    _ => disk,
                };
                self.conflict = false;
                self.disk_gone = false;
                if !unchanged && !moved {
                    // The text changed during the write (a scrub, or typing): the dot follows the text against what is saved now.
                    self.scrub_settled(cx);
                }
                self.saved_mtime = disk.mtime;
                self.read_only = disk.read_only;
                cx.emit(DocumentChanged);
                // Settings.toml saved from an editor window: the store takes the edit
                // instead of writing its old values back over it later.
                settings::reload(cx, path);
                cx.defer(menu::refresh);
                Task::ready(true)
            }
            Err(err) => {
                let detail = file_ops::explain("save", path, &err);
                self.write_errors += 1;
                cx.emit(ShowTab);
                let buttons: &[&str] = if offer_save_as { &["Save as…", "Cancel"] } else { &["OK"] };
                let answer = window.prompt(PromptLevel::Critical, &file_ops::failed_title("save", &[path.to_path_buf()]), Some(&detail), buttons, cx);
                if !offer_save_as {
                    return Task::ready(false);
                }
                self.saves.asking = true;
                cx.spawn_in(window, async move |this, cx| {
                    if answer.await.ok() != Some(0) {
                        return false;
                    }
                    let Ok(save) = this.update_in(cx, |this, window, cx| {
                        this.saves.asking = false;
                        this.save_as(window, cx)
                    }) else { return false };
                    save.await
                })
            }
        }
    }

    /// Whether a save is writing, or asking about characters its encoding can't hold: the app
    /// mustn't exit until it lands.
    pub fn is_saving(&self) -> bool {
        self.saves.busy || self.saves.asking
    }

    /// When the save writing this file began, once it has run longer than `SAVE_SLOW`; none for a quicker one or when none is writing.
    pub fn slow_save(&self) -> Option<Instant> {
        self.saves.began.filter(|_| self.saves.slow)
    }

    /// Whether the Open this big file? question is up: the tab holds an empty placeholder and no text is read for it.
    pub fn awaiting_open(&self) -> bool {
        self.awaiting_open
    }

    /// The save epoch a look at the disk should note before it reads the file, and hand back to `on_disk`.
    pub fn save_epoch(&self) -> u64 {
        self.saves.epoch
    }

    /// Resolves once no save is writing: at once when none is. The task says true.
    pub fn save_landed(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if !self.is_saving() {
            return Task::ready(true);
        }
        self.after_save(window, cx, |this, window, cx| this.save_landed(window, cx))
    }

    /// Looks at the file again in the background and takes the result as `on_disk` does: a check
    /// that came in during a save was dropped, and a clean tab may hold half of a write.
    fn recheck_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        let (unsaved, encoding, epoch) = (self.dirty(), self.disk_encoding, self.saves.epoch);
        cx.spawn_in(window, async move |this, cx| {
            let read_path = path.clone();
            let (disk, text) = cx
                .background_executor()
                .spawn(async move {
                    let disk = probe(&path);
                    let text = if unsaved || disk.gone { None } else { encoding::reread_file(&path, encoding).ok().map(Read::Full) };
                    (disk, text)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                // The tab was saved as or renamed meanwhile: that read is of another file. Look at the new one.
                if this.path.as_deref() != Some(read_path.as_path()) {
                    this.recheck_disk(window, cx);
                    return;
                }
                if this.on_disk(disk, text, epoch, window, cx) {
                    // Nothing is lost: no unsaved edits, or an empty page.
                    this.discard_edits();
                    cx.emit(DropTab);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The file changed on disk since we last read or wrote it, and the edits here would overwrite
    /// that: asks Cancel / Compare… / Save as… / Discard edits / Overwrite, with this tab in front. Cancel comes
    /// first because the first button is the one Return presses, and Return must not overwrite. The
    /// task says whether the text reached the disk (Overwrite and a Save as… that lands do);
    /// Compare… opens the Compare view and says false.
    fn ask_overwrite(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if !self.conflict {
            self.conflict = true;
            cx.emit(DocumentChanged);
        }
        cx.emit(ShowTab);
        let (title, detail) = changed_question(&self.title());
        let buttons = [
            PromptButton::cancel("Cancel"),
            PromptButton::new("Compare…"),
            PromptButton::new("Save as…"),
            PromptButton::destructive("Discard edits"),
            PromptButton::destructive("Overwrite"),
        ];
        let answer = window.prompt(PromptLevel::Warning, &title, Some(detail), &buttons, cx);
        cx.spawn_in(window, async move |this, cx| {
            let choice = answer.await.ok();
            let task = this.update_in(cx, |this, window, cx| match choice {
                Some(4) => this.write(&path, true, window, cx),
                Some(3) => {
                    this.discard_for_disk(&path, window, cx);
                    Task::ready(false)
                }
                Some(2) => this.save_as(window, cx),
                Some(1) => {
                    this.open_compare(&path, window, cx);
                    Task::ready(false)
                }
                _ => Task::ready(false),
            });
            match task {
                Ok(task) => task.await,
                Err(_) => false,
            }
        })
    }

    /// Discard edits: takes the file as the disk has it, dropping the unsaved edits.
    fn discard_for_disk(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let path = path.to_path_buf();
        let (encoding, edits) = (self.disk_encoding, self.saves.edits);
        cx.spawn_in(window, async move |this, cx| {
            let read_path = path.clone();
            let (mtime, read) = cx
                .background_executor()
                .spawn(async move {
                    let mtime = path_mtime(Some(&path));
                    (mtime, encoding::reread_file(&path, encoding))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                // The tab was saved as or renamed meanwhile: that read is of another file.
                if this.path.as_deref() != Some(read_path.as_path()) {
                    return;
                }
                // Typed over, or a save writing, while it read: the text stays and the file is not taken.
                if this.saves.edits != edits || this.saves.busy {
                    return;
                }
                match read {
                    Ok(decoded) => this.reload(decoded, mtime, true, window, cx),
                    Err(err) => {
                        let detail = file_ops::explain("read", &read_path, &err);
                        let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("read", &[read_path.clone()]), Some(&detail), &["OK"], cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Compare…: reads the file as the disk has it now, in this document's encoding and off the
    /// main thread, and opens the Compare view of the unsaved text against it. Nothing is written;
    /// the view's buttons decide what happens to the text.
    fn open_compare(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.built_editor().cloned() else { return };
        let line_height = editor.read(cx).line_height();
        // The text and its edit count are read together, so a keystroke while the diff runs shows.
        let (text, edits, path, encoding) = (self.text(cx), self.saves.edits, path.to_path_buf(), self.disk_encoding);
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_executor()
                .spawn(async move {
                    // The time first, so a change landing during the read still shows as a conflict later.
                    let mtime = path_mtime(Some(&path));
                    match encoding::reread_file(&path, encoding) {
                        Ok(decoded) => {
                            let comparison = Comparison::new(&text, &decoded.text);
                            Ok((mtime, decoded, comparison, text))
                        }
                        Err(err) => Err((path, err)),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| match made {
                Ok((mtime, decoded, mut comparison, mut text)) => {
                    let mut edits = edits;
                    if this.saves.edits != edits {
                        // Typed while the diff ran: compare the text as it is now.
                        text = this.text(cx);
                        edits = this.saves.edits;
                        comparison = Comparison::new(&text, &decoded.text);
                    }
                    let final_newline = text.ends_with('\n') || (text.is_empty() && decoded.text.ends_with('\n'));
                    let name = this.title();
                    let view = cx.new(|cx| CompareView::new(name, comparison, final_newline, line_height, window, cx));
                    if let Some(width) = this.built_editor().map(|editor| editor.read(cx).input_bounds().size.width) {
                        view.read(cx).seed_width(width);
                    }
                    let subscription = cx.subscribe_in(&view, window, |this, _, event: &CompareEvent, window, cx| this.compare_event(event, window, cx));
                    this.compare = Some(OpenCompare { view, _subscription: subscription, mtime, edits, replaced: decoded.replaced, version: None });
                    cx.emit(ShowTab);
                    cx.emit(DocumentChanged);
                }
                Err((path, err)) => {
                    let detail = file_ops::explain("read", &path, &err);
                    let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("read", &[path.clone()]), Some(&detail), &["OK"], cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Browse versions…: reads the saved version `id` of this file off the main thread and opens
    /// the Compare view of the unsaved text against it, titled by `time`. Use these puts the
    /// merged text in the buffer as one Undo and leaves the tab unsaved, or as it was when the picks match the text.
    pub fn open_version(&mut self, id: String, time: SystemTime, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(editor), Some(path)) = (self.built_editor().cloned(), self.path.clone()) else { return };
        let line_height = editor.read(cx).line_height();
        let (text, edits) = (self.text(cx), self.saves.edits);
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_executor()
                .spawn(async move {
                    let kept = crate::versions::read(&path, &id);
                    (id, path, kept.map(|kept| (Comparison::new(&text, &kept), text, kept)))
                })
                .await;
            this.update_in(cx, |this, window, cx| match made {
                (id, _, Ok((mut comparison, mut text, kept))) => {
                    let mut edits = edits;
                    if this.saves.edits != edits {
                        // Typed while the diff ran: compare the text as it is now.
                        text = this.text(cx);
                        edits = this.saves.edits;
                        comparison = Comparison::new(&text, &kept);
                    }
                    let final_newline = text.ends_with('\n') || (text.is_empty() && kept.ends_with('\n'));
                    let when = crate::versions_list::friendly(time, SystemTime::now());
                    let name = this.title();
                    let view = cx.new(|cx| CompareView::old_version(name, when, comparison, final_newline, line_height, window, cx));
                    if let Some(width) = this.built_editor().map(|editor| editor.read(cx).input_bounds().size.width) {
                        view.read(cx).seed_width(width);
                    }
                    let subscription = cx.subscribe_in(&view, window, |this, _, event: &CompareEvent, window, cx| this.compare_event(event, window, cx));
                    this.compare = Some(OpenCompare { view, _subscription: subscription, mtime: None, edits, replaced: false, version: Some((id, time)) });
                    cx.emit(ShowTab);
                    cx.emit(DocumentChanged);
                }
                (id, path, Err(err)) => {
                    let detail = format!("{}\n\nVersion {id}: {err}", path.display());
                    let _ = window.prompt(PromptLevel::Critical, "Couldn’t read that version", Some(&detail), &["OK"], cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Gives the keyboard to what this tab shows: its Compare view while one is open, else the editor.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor(window, cx);
        match &self.compare {
            Some(open) => window.focus(&open.view.read(cx).focus_handle(cx), cx),
            None => editor.update(cx, |state, cx| state.focus(window, cx)),
        }
    }

    /// The Compare view, while this document shows one over its editor.
    pub fn compare_view(&self) -> Option<&Entity<CompareView>> {
        self.compare.as_ref().map(|open| &open.view)
    }

    /// The view's answer: Cancel leaves it, Save as… leaves it and saves the buffer as it is (all
    /// of yours), Use These puts the merged text in the buffer.
    fn compare_event(&mut self, event: &CompareEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.compare.take() else { return };
        if let Some(editor) = self.built_editor().cloned() {
            editor.update(cx, |editor, cx| editor.focus(window, cx));
        }
        match event {
            CompareEvent::Cancel => {}
            CompareEvent::SaveAs => self.save_as(window, cx).detach(),
            CompareEvent::Use(text, from_disk) => {
                // The text moved on since the comparison was drawn: show it again from the text as it is.
                if self.saves.edits != open.edits {
                    match (self.path.clone(), open.version.clone()) {
                        (Some(path), None) => self.open_compare(&path, window, cx),
                        (_, Some((id, time))) => self.open_version(id, time, window, cx),
                        _ => {}
                    }
                    cx.emit(DocumentChanged);
                    return;
                }
                // The merge is an ordinary edit: a slider scrubbed back would refuse it, so it folds first.
                self.close_scrub(window, cx);
                // The chosen text is the buffer as it is: nothing to write back, so no undo step and no unsaved mark.
                let unchanged = self.rope(cx) == *text;
                if let Body::Built { editor, .. } = &self.body
                    && !unchanged
                {
                    // One undo step over the changed span; the caret stays on its line, the scroll where it was.
                    editor.clone().update(cx, |state, cx| {
                        let scroll = state.scroll_offset();
                        state.replace_all_in_place(text.clone(), window, cx);
                        state.set_scroll_offset(scroll, cx);
                    });
                }
                // A saved version isn't the disk: nothing was seen of it and nothing is marked saved.
                if open.version.is_none() {
                    // The user has seen the disk: the next Save writes without asking again.
                    self.conflict = false;
                    if open.mtime.is_some() {
                        self.saved_mtime = open.mtime;
                    }
                    // Text taken from a disk read that changed bytes must still ask before a save writes them.
                    if *from_disk {
                        self.replaced |= open.replaced;
                    }
                }
            }
        }
        cx.emit(DocumentChanged);
        cx.defer(menu::refresh);
    }

    /// The file is gone from disk and the edits here are all that is left of it: asks Save (writes
    /// it again) / Discard and close (closes the tab without saving) / Cancel, with this tab in
    /// front. The task says whether the text reached the disk, or the tab went with the user's
    /// say-so.
    fn ask_recreate(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if !std::mem::replace(&mut self.disk_gone, true) {
            self.conflict = false;
            cx.emit(DocumentChanged);
        }
        cx.emit(ShowTab);
        let (title, detail) = deleted_question(&self.title());
        let buttons = [PromptButton::new("Save"), PromptButton::destructive("Discard and close"), PromptButton::cancel("Cancel")];
        let answer = window.prompt(PromptLevel::Warning, &title, Some(detail), &buttons, cx);
        cx.spawn_in(window, async move |this, cx| {
            let choice = answer.await.ok();
            let task = this.update_in(cx, |this, window, cx| match choice {
                Some(0) if path.parent().is_none_or(|parent| !parent.exists()) => this.save_as(window, cx),
                Some(0) => this.write(&path, true, window, cx),
                Some(1) => {
                    // Counted as saved first, so nothing asks again as the tab goes.
                    this.discard_edits();
                    cx.emit(DropTab);
                    Task::ready(true)
                }
                _ => Task::ready(false),
            });
            match task {
                Ok(task) => task.await,
                Err(_) => false,
            }
        })
    }

    /// The text has characters the file's encoding can't hold: asks Save as UTF-8 (the document
    /// becomes UTF-8 without a BOM, then saves) / Cancel, with this tab in front. The task says
    /// whether the text reached the disk.
    fn ask_lossy(&mut self, path: PathBuf, culprit: Option<char>, offer_save_as: bool, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        cx.emit(ShowTab);
        // "Read as" names what the disk has; "can't be saved in" names what the save writes.
        let label = encoding::label(if self.replaced { self.disk_encoding } else { self.encoding });
        let (title, detail) = encoding::lossy_question(&self.title(), &label, self.replaced, culprit);
        let answer = window.prompt(PromptLevel::Warning, &title, Some(&detail), &["Save as UTF-8", "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            let choice = answer.await.ok();
            let task = this.update_in(cx, |this, window, cx| match choice {
                Some(0) => {
                    if this.encoding != encoding_rs::UTF_8 {
                        this.has_bom = false;
                    }
                    this.encoding = encoding_rs::UTF_8;
                    this.replaced = false;
                    this.write(&path, offer_save_as, window, cx)
                }
                _ => Task::ready(false),
            });
            match task {
                Ok(task) => task.await,
                Err(_) => false,
            }
        })
    }
}

/// Where Save As starts for a page with no file: the tree's folder while it exists, else the
/// working directory (launched from Finder that is `/`, which doesn't count), else home.
fn untitled_start(tree_root: Option<&Path>, cwd: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    tree_root
        .filter(|dir| dir.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| cwd.filter(|dir| dir != Path::new("/")))
        .or(home)
        .unwrap_or_default()
}

/// The read-only question's title and detail for the file called `name`; `unlock` is whether
/// Unlock is one of its buttons, which the detail then explains with the others.
fn read_only_question(name: &str, unlock: bool) -> (String, &'static str) {
    let detail = if unlock {
        "Unlock lets you save in this file. Save as… saves a copy somewhere else. Cancel leaves the file alone."
    } else {
        "Save a copy somewhere else?"
    };
    (format!("\u{201C}{name}\u{201D} is read-only"), detail)
}

/// The changed-on-disk question's title and detail for the file called `name`; the detail
/// explains each of its buttons.
fn changed_question(name: &str) -> (String, &'static str) {
    (
        format!("\u{201C}{name}\u{201D} changed on disk"),
        "It was changed by something else since you opened or last saved it. Compare… shows both versions side by side so you can keep parts of each. Overwrite replaces that version with yours. Save as… keeps both: yours goes to a new file. Discard edits drops your changes and shows the file as it is now. Cancel keeps your changes and leaves the file alone.",
    )
}

/// The deleted-from-disk question's title and detail for the file called `name`; the detail
/// explains each of its buttons.
fn deleted_question(name: &str) -> (String, &'static str) {
    (
        format!("\u{201C}{name}\u{201D} was deleted from disk"),
        "It was deleted or moved since you opened it. Save puts it back with your text. Discard and close closes the tab and drops your changes. Cancel keeps the tab open.",
    )
}

/// The file's modification time, `None` for no path or a file that can't be read.
pub fn path_mtime(path: Option<&Path>) -> Option<SystemTime> {
    fs::metadata(path?).and_then(|meta| meta.modified()).ok()
}

/// Whether a save would overwrite someone else's change: there are unsaved edits and the file is
/// different on disk than at our last read or write, including an older copy restored over it.
/// Without both times there is nothing to compare, which is no conflict.
fn has_conflict(unsaved: bool, saved: Option<SystemTime>, disk: Option<SystemTime>) -> bool {
    unsaved && moved_on_disk(saved, disk)
}

/// The file's time on disk differs from the one we last read or wrote. Without both times there
/// is nothing to compare, which is no change.
fn moved_on_disk(saved: Option<SystemTime>, disk: Option<SystemTime>) -> bool {
    matches!((saved, disk), (Some(saved), Some(disk)) if disk != saved)
}

/// What the disk says about a file now.
#[derive(Clone, Copy)]
pub struct Disk {
    /// When it was last written; `None` when the time can't be read.
    pub mtime: Option<SystemTime>,
    /// There is no file at the path (deleted, or moved away).
    pub gone: bool,
    /// This process can't write the file.
    pub read_only: bool,
}

/// What a read of a changed file brought: the whole text, or, when the file only grew, just the
/// bytes it gained (`encoding::read_tail`).
pub enum Read {
    Full(Decoded),
    Tail(Tail),
}

/// Looks at the file at `path`. Only "not found" counts as gone: a file that can't be looked at for
/// another reason is left to the read or write that follows, which reports why.
pub fn probe(path: &Path) -> Disk {
    match fs::metadata(path) {
        Ok(meta) => Disk { mtime: meta.modified().ok(), gone: false, read_only: !writable(path) },
        Err(err) => Disk { mtime: None, gone: err.kind() == io::ErrorKind::NotFound, read_only: false },
    }
}

unsafe extern "C" {
    fn access(path: *const std::ffi::c_char, mode: std::ffi::c_int) -> std::ffi::c_int;
}

/// Whether this process may write the file at `path`, as `access(2)` with `W_OK` answers: it takes
/// the owner, the mode bits, a Finder lock and a read-only volume into account, which the mode
/// bits alone (`Permissions::readonly`) don't. The guide's pages are never writable: Help opens
/// them, and a save there would write into the app bundle (or the repo's docs).
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    const W_OK: std::ffi::c_int = 2;
    if crate::app::guide_dir().is_some_and(|guide| path.starts_with(guide)) {
        return false;
    }
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return true };
    // SAFETY: `path` is a NUL-terminated string that lives through the call.
    unsafe { access(path.as_ptr(), W_OK) == 0 }
}

unsafe extern "C" {
    fn chflags(path: *const std::ffi::c_char, flags: u32) -> std::ffi::c_int;
    fn geteuid() -> u32;
}

/// `UF_IMMUTABLE`: the flag Finder's Locked checkbox sets.
const UF_IMMUTABLE: u32 = 0x2;

/// The user this process runs as.
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    unsafe { geteuid() }
}

/// Sets the file's BSD flags to `flags`, as `chflags(2)` does.
fn set_flags(path: &Path, flags: u32) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `path` is a NUL-terminated string that lives through the call.
    if unsafe { chflags(path.as_ptr(), flags) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Whether Unlock may be offered: the file is the user's own, its volume isn't read-only, and
/// there is a Finder lock or a missing owner write bit to undo, with no flag or ACL-proof state
/// that only root can lift (system immutable, append-only).
fn can_unlock(file_owner: u32, user: u32, volume_read_only: bool, flags: u32, mode: u32) -> bool {
    const UF_APPEND: u32 = 0x4;
    const SF_IMMUTABLE: u32 = 0x2_0000;
    const SF_APPEND: u32 = 0x4_0000;
    let something_to_undo = flags & UF_IMMUTABLE != 0 || mode & 0o200 == 0;
    file_owner == user && !volume_read_only && something_to_undo && flags & (UF_APPEND | SF_IMMUTABLE | SF_APPEND) == 0
}

/// `can_unlock` for the file at `path` as it is now. The guide's pages are never unlocked.
fn unlockable(path: &Path) -> bool {
    use std::os::{macos::fs::MetadataExt as _, unix::{ffi::OsStrExt as _, fs::{MetadataExt as _, PermissionsExt as _}}};
    const EROFS: i32 = 30;
    if crate::app::guide_dir().is_some_and(|guide| path.starts_with(guide)) {
        return false;
    }
    let (Ok(meta), Ok(c_path)) = (fs::metadata(path), std::ffi::CString::new(path.as_os_str().as_bytes())) else { return false };
    // SAFETY: `c_path` is a NUL-terminated string that lives through the call. A read-only
    // volume answers EROFS before it looks at the lock or the mode.
    let volume_read_only = unsafe { access(c_path.as_ptr(), 2) } != 0 && io::Error::last_os_error().raw_os_error() == Some(EROFS);
    can_unlock(meta.uid(), effective_uid(), volume_read_only, meta.st_flags(), meta.permissions().mode())
}

/// Takes the Finder lock off the file and gives its owner write, so a save can go through.
/// The lock goes first: a locked file refuses a mode change.
fn unlock(path: &Path) -> io::Result<()> {
    use std::os::{macos::fs::MetadataExt as _, unix::fs::PermissionsExt as _};
    let meta = fs::metadata(path)?;
    if meta.st_flags() & UF_IMMUTABLE != 0 {
        set_flags(path, meta.st_flags() & !UF_IMMUTABLE)?;
    }
    let mode = meta.permissions().mode();
    if mode & 0o200 == 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o200))?;
    }
    Ok(())
}

/// The window title for a file: its name, or Untitled.
pub fn title(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_name())
        .map_or_else(|| "Untitled".into(), |name| name.to_string_lossy().into_owned())
}

/// What the top of an Untitled page says it is called, and whether that reads as Markdown.
pub(crate) struct PageTitle {
    pub text: String,
    pub markdown: bool,
}

/// The name a page's text gives it, from the first `PAGE_SCAN` characters of `chars`: leading blank lines are skipped; a closed `---` front matter block is skipped and its `title:` value is used when it has one, else the first non-empty line after the block; a `#` line is a heading (its `#`s and spaces dropped), a `#!` line is not (taken as written). `None` when that line is empty. Front matter and headings read as Markdown.
pub(crate) fn page_title(chars: impl Iterator<Item = char>) -> Option<PageTitle> {
    let head: String = chars.take(PAGE_SCAN).collect();
    let head = head.strip_prefix('\u{feff}').unwrap_or(&head);
    let lines: Vec<&str> = head.lines().collect();
    let first = lines.iter().position(|line| !line.trim().is_empty())?;
    let mut rest = &lines[first..];
    let mut front_matter = false;
    let mut named = None;
    if rest[0].trim_end() == "---"
        && let Some(close) = rest[1..].iter().position(|line| matches!(line.trim_end(), "---" | "..."))
    {
        front_matter = true;
        named = rest[1..=close]
            .iter()
            .find_map(|line| line.strip_prefix("title:"))
            .map(|value| value.trim().trim_matches(['"', '\'']).trim())
            .filter(|value| !value.is_empty());
        rest = &rest[close + 2..];
    }
    let (text, heading) = match named {
        Some(value) => (value.to_owned(), false),
        None => {
            let line = rest.iter().map(|line| line.trim()).find(|line| !line.is_empty()).unwrap_or("");
            let line: String = line.chars().take(FIRST_LINE_SCAN).collect();
            if line.starts_with("#!") {
                (line, false)
            } else {
                let heading = line.starts_with('#');
                (line.trim_start_matches('#').trim().to_owned(), heading)
            }
        }
    };
    let text: String = text.chars().take(FIRST_LINE_SCAN).collect();
    (!text.is_empty()).then_some(PageTitle { text, markdown: front_matter || heading })
}

/// The tab title the top of `chars` gives an Untitled page (see `page_title`), cut to `DRAFT_TITLE_CHARS` characters with an ellipsis; `None` when there is no line to take it from.
fn first_line_title(chars: impl Iterator<Item = char>) -> Option<String> {
    let line = page_title(chars)?.text;
    match line.char_indices().nth(DRAFT_TITLE_CHARS) {
        Some((end, _)) => Some(format!("{}\u{2026}", line[..end].trim_end())),
        None => Some(line),
    }
}

/// The extension a `#!` first line gives a script (after a BOM): the interpreter is the last path segment, or the first word after `env` that isn't a flag or a `NAME=value`, with a version suffix dropped (`python3.12` is `python`). `None` for no `#!` line or an interpreter not known. Reads at most `SHEBANG_SCAN` characters.
fn shebang_extension(chars: impl Iterator<Item = char>) -> Option<&'static str> {
    const SHEBANG_SCAN: usize = 200;
    let line: String = chars.take(SHEBANG_SCAN).skip_while(|&c| c == '\u{feff}').take_while(|&c| c != '\n' && c != '\r').collect();
    let mut words = line.strip_prefix("#!")?.split_whitespace();
    let base = |word: &str| word.rsplit('/').next().unwrap_or(word).to_owned();
    let mut interpreter = base(words.next()?);
    if interpreter == "env" {
        interpreter = base(words.find(|word| !word.starts_with('-') && !word.contains('='))?);
    }
    match interpreter.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.') {
        "bash" | "sh" | "zsh" | "fish" => Some("sh"),
        "python" => Some("py"),
        "node" | "deno" | "bun" => Some("js"),
        "ruby" => Some("rb"),
        "perl" => Some("pl"),
        "php" => Some("php"),
        "lua" => Some("lua"),
        _ => None,
    }
}

/// The name Save offers for the top of `chars` (see `page_title`): `Untitled.<ext>` when it starts with a `#!` line of a known interpreter (see `shebang_extension`); else the title made safe for a file name, with `.md` when it reads as Markdown, else `.txt`; `Untitled.txt` when nothing is left of it.
fn save_name_of(chars: impl Iterator<Item = char> + Clone) -> String {
    if let Some(ext) = shebang_extension(chars.clone()) {
        return format!("Untitled.{ext}");
    }
    let Some(PageTitle { text, markdown }) = page_title(chars) else {
        return "Untitled.txt".to_owned();
    };
    let safe: String = text.chars().map(|c| if c == '/' || c == ':' || c.is_control() { ' ' } else { c }).take(SAVE_NAME_CHARS).collect();
    let stem = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    let stem = stem.trim_start_matches(|c: char| c == '.' || c.is_whitespace()).trim_end();
    if stem.is_empty() {
        return "Untitled.txt".to_owned();
    }
    format!("{stem}.{}", if markdown { "md" } else { "txt" })
}

/// Writes in place so hard links, Finder tags and permissions survive. Opening never truncates:
/// growth gets a reservation attempt before touching the text, and shortening waits until every
/// byte is flushed. This avoids emptying the old file first, but it is not an atomic save.
/// A kill or I/O error during an overwrite can still leave a mix of old and new bytes.
fn save_file(path: &Path, text: &Rope, crlf: bool) -> io::Result<()> {
    let len = text.len() as u64 + if crlf { bare_lf_count(text) } else { 0 };
    let file = write_rope(prepare_save(path, len)?, text, crlf)?;
    file.set_len(len)?;
    full_sync(&file)
}

/// Encoded text uses the same in-place reservation, delayed shortening, flush and sync.
fn save_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let len = bytes.len() as u64;
    let mut file = prepare_save(path, len)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.set_len(len)?;
    full_sync(&file)
}

/// Flushes `file` to the disk as far as the volume allows: `F_FULLFSYNC` (all `sync_all` does on
/// a Mac), else plain `fsync` where the volume has no such request (SMB answers ENOTSUP; others
/// ENOTTY or EINVAL), as SQLite and libuv do. Any other error is a real failure.
pub(crate) fn full_sync(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    unsafe extern "C" {
        fn fcntl(fd: std::ffi::c_int, command: std::ffi::c_int, ...) -> std::ffi::c_int;
        fn fsync(fd: std::ffi::c_int) -> std::ffi::c_int;
    }
    // Darwin fcntl.h: F_FULLFSYNC = 51.
    loop {
        // SAFETY: the file descriptor is live and F_FULLFSYNC takes no argument.
        if unsafe { fcntl(file.as_raw_fd(), 51) } != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(4) => continue,
            // ENOTSUP 45, ENOTTY 25, EINVAL 22.
            Some(45 | 25 | 22) => break,
            _ => return Err(err),
        }
    }
    loop {
        // SAFETY: as above.
        if unsafe { fsync(file.as_raw_fd()) } != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(4) {
            return Err(err);
        }
    }
}

/// Opens without truncation and tries to reserve growth before the first write. Space and size
/// failures stop the save; unsupported reservations fall through to writing in place. Existing
/// allocated blocks need no further reservation. Shared APFS extents can still need space when
/// overwritten, so even a successful reservation does not make the overwrite atomic.
fn prepare_save(path: &Path, len: u64) -> io::Result<File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;

    let file = File::options().write(true).create(true).truncate(false).open(path)?;
    let meta = file.metadata()?;
    let old_len = meta.len();
    if len > old_len && len > meta.blocks().saturating_mul(512) {
        // Darwin fcntl.h: F_PREALLOCATE = 42, F_ALLOCATECONTIG = 2, F_ALLOCATEALL = 4,
        // F_PEOFPOSMODE = 3 (allocate from the physical EOF, without changing the logical EOF).
        #[repr(C)]
        struct Fstore {
            flags: u32,
            posmode: i32,
            offset: i64,
            length: i64,
            allocated: i64,
        }
        unsafe extern "C" {
            fn fcntl(fd: std::ffi::c_int, command: std::ffi::c_int, ...) -> std::ffi::c_int;
        }
        let length = i64::try_from(len - old_len).map_err(|_| io::Error::from_raw_os_error(27))?;
        let mut store = Fstore { flags: 2 | 4, posmode: 3, offset: 0, length, allocated: 0 };
        let reserve = |store: &mut Fstore| {
            // SAFETY: the file descriptor is live and `store` has Darwin's fstore_t layout.
            if unsafe { fcntl(file.as_raw_fd(), 42, store as *mut Fstore) } != -1 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
            // SAFETY: as above; retry an interrupted reservation once.
            if unsafe { fcntl(file.as_raw_fd(), 42, store as *mut Fstore) } == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        };
        if reserve(&mut store).is_err() {
            store.flags = 4;
            store.allocated = 0;
            if let Err(err) = reserve(&mut store) {
                reservation_error(err)?;
            }
        }
    }
    Ok(file)
}

/// A reservation is advisory unless the disk reports too little space, quota or file-size room.
fn reservation_error(err: io::Error) -> io::Result<()> {
    if matches!(err.raw_os_error(), Some(28 | 69 | 27)) {
        Err(err)
    } else {
        Ok(())
    }
}

/// Writes `text` through a buffer into `writer` and hands the writer back once the buffer is flushed.
fn write_rope<W: Write>(writer: W, text: &Rope, crlf: bool) -> io::Result<W> {
    let mut buffered = BufWriter::new(writer);
    if crlf {
        write_crlf(&mut buffered, text)?;
    } else {
        text.write_to(&mut buffered)?;
    }
    buffered.into_inner().map_err(io::IntoInnerError::into_error)
}

/// How many LFs in `text` have no CR before them, found chunk by chunk without a copy.
fn bare_lf_count(text: &Rope) -> u64 {
    let mut count = 0;
    let mut last = 0u8;
    for chunk in text.chunks() {
        let bytes = chunk.as_bytes();
        let Some(&end) = bytes.last() else { continue };
        count += bytes.iter().enumerate().filter(|&(i, &b)| b == b'\n' && (if i == 0 { last } else { bytes[i - 1] }) != b'\r').count() as u64;
        last = end;
    }
    count
}

/// Writes `text` with every bare LF as CRLF, a CRLF already there left as one, streaming the
/// rope's chunks. A CR at the end of one chunk and its LF at the start of the next stay a CRLF.
fn write_crlf<W: Write>(out: &mut W, text: &Rope) -> io::Result<()> {
    let mut last = 0u8;
    for chunk in text.chunks() {
        let bytes = chunk.as_bytes();
        let Some(&end) = bytes.last() else { continue };
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' && (if i == 0 { last } else { bytes[i - 1] }) != b'\r' {
                out.write_all(&bytes[start..i])?;
                out.write_all(b"\r")?;
                start = i;
            }
        }
        out.write_all(&bytes[start..])?;
        last = end;
    }
    Ok(())
}

/// How many background writes of files are running, in every window, closed tabs included.
static WRITES: AtomicUsize = AtomicUsize::new(0);

/// Counts one background write while it lives; dropping it, however the write ends, takes it off.
struct WriteGuard;

impl WriteGuard {
    fn new() -> Self {
        WRITES.fetch_add(1, Ordering::SeqCst);
        WriteGuard
    }
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        WRITES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The real paths being written now, so two writers of one file (a save and a Save as onto it, from
/// two tabs) take turns instead of overwriting each other's bytes in place.
static TURNS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static TURN_OVER: Condvar = Condvar::new();

/// The turn to write one file; dropping it lets the next writer of that file go.
struct PathTurn(PathBuf);

impl PathTurn {
    /// Waits (on a background thread) until no other write of the file at `path` is running.
    fn wait_for(path: &Path) -> Self {
        let real = fs::canonicalize(path).unwrap_or_else(|_| match (path.parent().and_then(|dir| fs::canonicalize(dir).ok()), path.file_name()) {
            (Some(dir), Some(name)) => dir.join(name),
            _ => path.to_path_buf(),
        });
        let mut busy = TURNS.lock().unwrap_or_else(PoisonError::into_inner);
        while busy.contains(&real) {
            busy = TURN_OVER.wait(busy).unwrap_or_else(PoisonError::into_inner);
        }
        busy.push(real.clone());
        PathTurn(real)
    }
}

impl Drop for PathTurn {
    fn drop(&mut self) {
        TURNS.lock().unwrap_or_else(PoisonError::into_inner).retain(|path| *path != self.0);
        TURN_OVER.notify_all();
    }
}

/// The number of background file writes running now. The app mustn't exit while it isn't zero,
/// whether or not the tab that asked for the write is still open.
pub fn writes_in_flight() -> usize {
    WRITES.load(Ordering::SeqCst)
}

/// How a background write ended.
enum Written {
    /// The write finished or failed; on success, the file as the disk has it right after the sync.
    Done(io::Result<Disk>),
    /// The text has characters the encoding can't hold, so nothing was written.
    Lossy(Option<char>),
}

/// Encodes and writes `text` to `path`, then looks at the file: a plain UTF-8 file streams the
/// rope out as it is, any other encoding (or UTF-8 with a BOM) is encoded whole and written the
/// same way, in place. Runs off the main thread, on a snapshot of the text.
fn write_out(path: &Path, text: &Rope, encoding: &'static Encoding, has_bom: bool, crlf: bool) -> Written {
    let result = if encoding == encoding_rs::UTF_8 && !has_bom {
        save_file(path, text, crlf)
    } else {
        let string = text.to_string();
        let string = if crlf { encoding::to_crlf(&string) } else { Cow::Borrowed(string.as_str()) };
        let (bytes, lossy) = encoding::encode(&string, encoding, has_bom);
        if lossy {
            return Written::Lossy(encoding::first_unmappable(&string, encoding));
        }
        save_bytes(path, &bytes)
    };
    // Taken after the sync, so our own write is never later than what we noted.
    Written::Done(result.map(|()| probe(path)))
}

/// Writes `text` into the new, empty file `to` as a save of that text would (same encoding, BOM
/// and line endings, same in-place write), for File > Duplicate of a tab with unsaved edits. The
/// words to show when it can't.
fn write_copy(to: &Path, text: &Rope, encoding: &'static Encoding, has_bom: bool, crlf: bool) -> Result<(), String> {
    match write_out(to, text, encoding, has_bom, crlf) {
        Written::Done(Ok(_)) => Ok(()),
        Written::Done(Err(err)) => Err(file_ops::explain("copy", to, &err)),
        Written::Lossy(_) => Err(format!("The text has characters {} can’t keep. Save the file in another encoding first, then duplicate it.", encoding.name())),
    }
}

/// Counts a document's edits and lets one save at a time write its file.
#[derive(Default)]
struct Saves {
    edits: u64,
    busy: bool,
    /// A save question is open: nothing writes, but a save asked for now waits for the answer.
    asking: bool,
    /// When the write in flight began, and whether it has outlasted `SAVE_SLOW`; the timer that says so lives as long as the write.
    began: Option<Instant>,
    slow: bool,
    timer: Option<Task<()>>,
    /// Moves at the start and the end of every write, so a disk check that began before one is known.
    epoch: u64,
    /// A disk check came in while a write ran and was dropped: look again when it lands.
    missed: bool,
}

impl Saves {
    fn edited(&mut self) {
        self.edits += 1;
    }

    /// Starts a save of the text as it is now, returning its snapshot's number; `None` while one is in flight.
    fn begin(&mut self) -> Option<u64> {
        if std::mem::replace(&mut self.busy, true) {
            return None;
        }
        self.epoch += 1;
        (self.began, self.slow) = (Some(Instant::now()), false);
        Some(self.edits)
    }

    /// The save of `snapshot` ended. Returns whether the text is still what that save held.
    fn finish(&mut self, snapshot: u64) -> bool {
        self.busy = false;
        self.epoch += 1;
        (self.began, self.slow, self.timer) = (None, false, None);
        self.edits == snapshot
    }

    /// Whether a look at the disk that began at `epoch` saw the file before, during or between
    /// our writes, so what it read may be half a save.
    fn stale(&self, epoch: u64) -> bool {
        self.busy || epoch != self.epoch
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        time::Duration,
    };

    use super::{Rope, Saves, Written, can_unlock, changed_question, deleted_question, first_line_title, has_conflict, moved_on_disk, path_mtime, read_only_question, save_bytes, save_file, save_name_of, unlock, write_copy, write_out, write_rope};

    #[test]
    fn a_first_line_gives_a_short_tab_title_and_a_safe_file_name() {
        assert_eq!(first_line_title("# Grocery ideas\nmilk".chars()).as_deref(), Some("Grocery ideas"));
        assert_eq!(first_line_title("###   Plain  \n".chars()).as_deref(), Some("Plain"));
        assert_eq!(first_line_title("\nsecond line".chars()).as_deref(), Some("second line"));
        assert_eq!(first_line_title("#  \n".chars()), None);
        let long = first_line_title("word ".repeat(30).chars()).unwrap();
        assert!(long.ends_with('\u{2026}') && long.chars().count() <= 41);
        assert_eq!(save_name_of("# Grocery ideas\nmilk".chars()), "Grocery ideas.md");
        assert_eq!(save_name_of("a/b: c".chars()), "a b c.txt");
        assert_eq!(save_name_of("..hidden".chars()), "hidden.txt");
        assert_eq!(save_name_of("../../etc".chars()), "etc.txt");
        assert_eq!(save_name_of("".chars()), "Untitled.txt");
    }

    #[test]
    fn front_matter_blank_lines_and_shebangs_title_a_page_by_one_rule() {
        let title = |text: &str| first_line_title(text.chars());
        assert_eq!(title("---\ntitle: x\n---").as_deref(), Some("x"));
        assert_eq!(save_name_of("---\ntitle: x\n---".chars()), "x.md");
        assert_eq!(title("---\ntitle: \"Quoted one\"\n---\nbody").as_deref(), Some("Quoted one"));
        assert_eq!(title("---\ntitle: \n---\n\nHello").as_deref(), Some("Hello"));
        assert_eq!(save_name_of("---\ntitle: \n---\n\nHello".chars()), "Hello.md");
        assert_eq!(title("---\ntags: a\n---\n# Plan\n").as_deref(), Some("Plan"));
        assert_eq!(title("---\n---\n").as_deref(), None);
        assert_eq!(title("---\nnever closed\n").as_deref(), Some("---"));
        assert_eq!(title("#!/bin/sh").as_deref(), Some("#!/bin/sh"));
        assert_eq!(save_name_of("#!/usr/bin/env bash\nset -e".chars()), "Untitled.sh");
        assert_eq!(save_name_of("#!/bin/bash".chars()), "Untitled.sh");
        assert_eq!(save_name_of("\u{feff}#!/usr/bin/env python3\nprint()".chars()), "Untitled.py");
        assert_eq!(save_name_of("#!/usr/bin/env python3.12".chars()), "Untitled.py");
        assert_eq!(save_name_of("#!/usr/bin/env -S deno run".chars()), "Untitled.js");
        assert_eq!(save_name_of("#!/opt/homebrew/bin/fish".chars()), "Untitled.sh");
        assert_eq!(save_name_of("#! /usr/bin/perl -w".chars()), "Untitled.pl");
        assert_eq!(save_name_of("#!/usr/bin/weird".chars()), "#! usr bin weird.txt");
        assert_eq!(save_name_of("#!/usr/bin/env".chars()), "#! usr bin env.txt");
        assert_eq!(save_name_of("\n#!/bin/bash".chars()), "#! bin bash.txt");
        assert_eq!(title("\n\nhello").as_deref(), Some("hello"));
        assert_eq!(save_name_of("\n\nhello".chars()), "hello.txt");
        assert_eq!(title("# Heading").as_deref(), Some("Heading"));
        assert_eq!(save_name_of("# Heading".chars()), "Heading.md");
    }

    #[test]
    fn the_disk_alerts_explain_every_button() {
        let (title, detail) = changed_question("a.txt");
        assert_eq!(title, "\u{201C}a.txt\u{201D} changed on disk");
        for button in ["Compare…", "Overwrite", "Discard edits", "Save as…", "Cancel"] {
            assert!(detail.contains(button), "{detail}");
        }
        let (title, detail) = deleted_question("a.txt");
        assert_eq!(title, "\u{201C}a.txt\u{201D} was deleted from disk");
        for button in ["Save", "Discard and close", "Cancel"] {
            assert!(detail.contains(button), "{detail}");
        }
    }

    #[test]
    fn the_read_only_question_names_the_file_and_offers_a_copy() {
        let (title, detail) = read_only_question("notes.txt", false);
        assert_eq!(title, "\u{201C}notes.txt\u{201D} is read-only");
        assert_eq!(detail, "Save a copy somewhere else?");
        let (_, detail) = read_only_question("notes.txt", true);
        for button in ["Unlock", "Save as…"] {
            assert!(detail.contains(button), "{detail}");
        }
    }

    #[test]
    fn unlock_is_offered_for_your_own_file_on_a_volume_you_can_write() {
        const LOCKED: u32 = super::UF_IMMUTABLE;
        assert!(can_unlock(501, 501, false, LOCKED, 0o644));
        assert!(can_unlock(501, 501, false, 0, 0o444), "no owner write bit");
        assert!(!can_unlock(0, 501, false, LOCKED, 0o644), "someone else's file");
        assert!(!can_unlock(501, 501, true, LOCKED, 0o644), "a read-only volume");
        assert!(!can_unlock(501, 501, false, 0, 0o644), "nothing to undo: an ACL or the like refuses");
        assert!(!can_unlock(501, 501, false, LOCKED | 0x2_0000, 0o644), "system immutable needs root");
        assert!(!can_unlock(501, 501, false, 0x4, 0o444), "append-only");
    }

    #[test]
    fn unlock_clears_the_finder_lock_and_gives_the_owner_write() {
        use std::os::{macos::fs::MetadataExt as _, unix::fs::PermissionsExt as _};
        let dir = std::env::temp_dir().join(format!("finch-unlock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("locked.txt");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        super::set_flags(&file, super::UF_IMMUTABLE).unwrap();
        assert!(!super::writable(&file));
        assert!(can_unlock(std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&file).unwrap()), super::effective_uid(), false, super::UF_IMMUTABLE, 0o444));
        unlock(&file).unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        assert_eq!(meta.st_flags() & super::UF_IMMUTABLE, 0);
        assert_ne!(meta.permissions().mode() & 0o200, 0);
        assert!(super::writable(&file));
        save_file(&file, &rope("new"), false).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_of_edits_keeps_encoding_bom_and_line_endings() {
        let dir = std::env::temp_dir().join(format!("finch-copy-edits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("a copy.txt");
        std::fs::File::create(&copy).unwrap();
        write_copy(&copy, &rope("one\ntwo\n"), encoding_rs::UTF_8, true, true).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"\xEF\xBB\xBFone\r\ntwo\r\n");
        let latin = dir.join("b copy.txt");
        std::fs::File::create(&latin).unwrap();
        write_copy(&latin, &rope("caf\u{e9}"), encoding_rs::WINDOWS_1252, false, false).unwrap();
        assert_eq!(std::fs::read(&latin).unwrap(), b"caf\xE9");
        let narrow = dir.join("c copy.txt");
        std::fs::File::create(&narrow).unwrap();
        let said = write_copy(&narrow, &rope("\u{4e2d}"), encoding_rs::WINDOWS_1252, false, false).unwrap_err();
        assert!(said.contains("can’t keep"), "{said}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_writers_of_one_file_take_turns_and_full_sync_works_on_a_local_file() {
        use super::{PathTurn, full_sync};
        let dir = std::env::temp_dir().join(format!("finch-save-turns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        full_sync(&std::fs::File::open(&file).unwrap()).unwrap();
        let first = PathTurn::wait_for(&file);
        let (sender, receiver) = std::sync::mpsc::channel();
        let other = file.clone();
        let waiter = std::thread::spawn(move || {
            let _second = PathTurn::wait_for(&other);
            sender.send(()).unwrap();
        });
        assert!(receiver.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "the second writer started while the first held the file");
        drop(first);
        receiver.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_write_count_returns_to_zero_when_a_write_task_ends_even_by_panic() {
        use super::{WriteGuard, writes_in_flight};
        assert_eq!(writes_in_flight(), 0);
        let guard = WriteGuard::new();
        assert_eq!(writes_in_flight(), 1);
        let other = WriteGuard::new();
        assert_eq!(writes_in_flight(), 2);
        let ended = std::thread::spawn(move || {
            let _guard = other;
            let dir = std::env::temp_dir().join("finch-no-such-dir-for-count").join("a.txt");
            // A write that fails ends the task normally.
            assert!(matches!(write_out(&dir, &rope("x"), encoding_rs::UTF_8, false, false), Written::Done(Err(_))));
        });
        ended.join().unwrap();
        assert_eq!(writes_in_flight(), 1);
        let panicked = std::thread::spawn(move || {
            let _guard = guard;
            panic!("the write task blew up");
        });
        assert!(panicked.join().is_err());
        assert_eq!(writes_in_flight(), 0);
    }

    /// Accepts `room` bytes, then fails every write and flush.
    #[derive(Debug)]
    struct Full {
        taken: Vec<u8>,
        room: usize,
    }

    impl Write for Full {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.taken.len() >= self.room {
                return Err(io::Error::other("disk full"));
            }
            let n = buf.len().min(self.room - self.taken.len());
            self.taken.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn rope(text: &str) -> Rope {
        Rope::from(text)
    }

    #[test]
    fn a_fingerprint_knows_its_text_however_the_rope_is_split() {
        use super::Fingerprint;
        let text = "one\r\ntwo\r\nthree\r\n".repeat(2000);
        let whole = rope(&text);
        use gpui_kit::component::input::RopeExt as _;
        let mut pieced = rope("");
        for piece in text.as_bytes().chunks(777) {
            pieced.replace(pieced.len()..pieced.len(), std::str::from_utf8(piece).unwrap());
        }
        let print = Fingerprint::of(&whole);
        assert!(print.matches(&pieced));
        // One byte different, a longer text and an empty one are other texts.
        assert!(!print.matches(&rope(&text.replacen("one", "ONE", 1))));
        assert!(!print.matches(&rope(&format!("{text}x"))));
        assert!(!print.matches(&rope("")));
    }

    #[test]
    fn save_as_on_a_page_with_no_file_starts_in_the_trees_folder() {
        use super::untitled_start;
        use std::path::{Path, PathBuf};
        let tree = std::env::temp_dir();
        let cwd = Some(PathBuf::from("/usr"));
        let home = Some(PathBuf::from("/Users/someone"));
        assert_eq!(untitled_start(Some(&tree), cwd.clone(), home.clone()), tree);
        assert_eq!(untitled_start(Some(Path::new("/no/such/folder/here")), cwd.clone(), home.clone()), PathBuf::from("/usr"));
        assert_eq!(untitled_start(None, Some(PathBuf::from("/")), home.clone()), PathBuf::from("/Users/someone"));
    }

    #[test]
    fn revert_puts_the_disk_text_back_and_leaves_the_tab_clean() {
        use super::{Body, Document};
        use crate::encoding::{Decoded, reread_file};
        let dir = std::env::temp_dir().join(format!("finch-revert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, "saved text\n").unwrap();
        let mut doc = Document::unbuilt(Some(path.clone()), Decoded::utf8("saved text\n".into()), path_mtime(Some(&path)), false);
        assert!(!doc.can_revert(), "a clean tab has nothing to revert");
        doc.body = Body::Unbuilt("edited text\n".into());
        doc.unsaved = true;
        doc.conflict = true;
        assert!(doc.can_revert());
        let (was, shown) = doc.adopt(reread_file(&path, encoding_rs::UTF_8).unwrap(), path_mtime(Some(&path)));
        assert!(was, "the tab's marks changed");
        assert!(shown.is_none(), "an unbuilt document has no editor to fill");
        assert!(matches!(&doc.body, Body::Unbuilt(text) if text == "saved text\n"));
        assert!(!doc.unsaved && !doc.conflict && !doc.can_revert());
        let untitled = Document::unbuilt(None, Decoded::utf8(String::new()), None, false);
        assert!(!untitled.can_revert());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopening_with_the_right_encoding_fixes_a_wrong_guess_and_saves_back_identical() {
        use super::{Body, Document};
        use crate::encoding::{self, Decoded, decode_as, read_file};
        let dir = std::env::temp_dir().join(format!("finch-reopen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latin1.txt");
        let bytes = b"\xcf\xf0\xe8\xe2\xe5\xf2 \xec\xe8\xf0, caf\xe9\n".to_vec();
        std::fs::write(&path, &bytes).unwrap();
        let guessed = read_file(&path).unwrap();
        assert_ne!(guessed.encoding, encoding_rs::WINDOWS_1252, "the guess is the wrong one");
        let mut doc = Document::unbuilt(Some(path.clone()), guessed, path_mtime(Some(&path)), false);
        assert!(doc.can_reopen());
        let decoded = decode_as(&std::fs::read(&path).unwrap(), encoding_rs::WINDOWS_1252);
        doc.adopt(decoded, path_mtime(Some(&path)));
        assert_eq!(doc.encoding(), encoding_rs::WINDOWS_1252);
        assert_eq!(encoding::label(doc.encoding()), "Western (Windows Latin 1)");
        assert!(!doc.replaced && !doc.unsaved);
        let Body::Unbuilt(text) = &doc.body else { panic!("unbuilt") };
        assert_eq!(text, "\u{cf}\u{f0}\u{e8}\u{e2}\u{e5}\u{f2} \u{ec}\u{e8}\u{f0}, caf\u{e9}\n");
        let Written::Done(Ok(_)) = write_out(&path, &rope(text), doc.encoding, doc.has_bom, false) else { panic!("save failed") };
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "saved back byte for byte");
        // As UTF-8 the same file has bytes with no character: they flag the tab.
        let decoded = decode_as(&bytes, encoding_rs::UTF_8);
        assert!(decoded.replaced && decoded.text.contains('\u{FFFD}'));
        assert!(!Document::unbuilt(None, Decoded::utf8(String::new()), None, false).can_reopen(), "an Untitled page has no file to reread");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_lands_every_byte_of_a_file_bigger_than_the_buffer() {
        let dir = std::env::temp_dir().join(format!("finch-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        std::fs::write(&path, "old and much longer ".repeat(20_000)).unwrap();
        let text: String = (0..30_000).map(|i| format!("line {i} \u{e9}\n")).collect();
        save_file(&path, &rope(&text), false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rope_and_encoded_saves_grow_and_shorten_to_exact_bytes_without_replacing_the_inode() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = scratch("save-in-place");
        let path = dir.join("text.txt");
        let link = dir.join("linked.txt");
        std::fs::write(&path, "old text with a longer tail").unwrap();
        std::fs::hard_link(&path, &link).unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();
        for text in ["short", "a much longer replacement that grows the file", ""] {
            save_file(&path, &rope(text), false).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
            assert_eq!(std::fs::read(&link).unwrap(), text.as_bytes());
            assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        }
        for bytes in [b"\xff\xfeh\0i\0\n\0".as_slice(), b"\xff\xfe", b"\xef\xbb\xbfencoded bytes that grow the file\n"] {
            save_bytes(&path, bytes).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(std::fs::read(&link).unwrap(), bytes);
            assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unsupported_reservations_fall_through_but_space_and_size_errors_stop_the_save() {
        for errno in [22, 45, 4] {
            assert!(super::reservation_error(io::Error::from_raw_os_error(errno)).is_ok());
        }
        for errno in [28, 69, 27] {
            assert_eq!(super::reservation_error(io::Error::from_raw_os_error(errno)).unwrap_err().raw_os_error(), Some(errno));
        }
    }

    #[test]
    #[ignore = "requires a dedicated tiny mounted volume in GPUI_EDITOR_TEST_FULL_VOLUME"]
    fn a_full_volume_save_keeps_the_old_bytes_and_length() {
        let dir = std::path::PathBuf::from(std::env::var_os("GPUI_EDITOR_TEST_FULL_VOLUME").expect("mount a dedicated tiny test volume"));
        let utf8 = dir.join("rope.txt");
        let encoded = dir.join("encoded.txt");
        let old = b"the old bytes must stay intact\n";
        std::fs::write(&utf8, old).unwrap();
        std::fs::write(&encoded, old).unwrap();
        let filler = dir.join("filler");
        let mut fill = std::fs::File::create(&filler).unwrap();
        loop {
            if let Err(err) = fill.write_all(&[b'x'; 4096]) {
                assert_eq!(err.raw_os_error(), Some(28));
                break;
            }
        }
        drop(fill);
        let text = "new text ".repeat(128 * 1024);
        assert_eq!(save_file(&utf8, &rope(&text), false).unwrap_err().raw_os_error(), Some(28));
        assert_eq!(save_bytes(&encoded, text.as_bytes()).unwrap_err().raw_os_error(), Some(28));
        assert_eq!(std::fs::read(&utf8).unwrap(), old);
        assert_eq!(std::fs::read(&encoded).unwrap(), old);
        std::fs::remove_file(&filler).unwrap();
        std::fs::remove_file(&utf8).unwrap();
        std::fs::remove_file(&encoded).unwrap();
    }

    #[test]
    fn save_returns_the_error_of_the_final_flush() {
        // Small enough to sit whole in the buffer, so only the closing flush hits the error.
        let err = write_rope(Full { taken: Vec::new(), room: 0 }, &rope("tail of the file"), false).unwrap_err();
        assert_eq!(err.to_string(), "disk full");
    }

    #[test]
    fn save_returns_an_error_from_the_middle_of_a_big_write() {
        let text = "x".repeat(100_000);
        assert!(write_rope(Full { taken: Vec::new(), room: 50_000 }, &rope(&text), false).is_err());
    }

    #[test]
    fn a_save_over_a_file_changed_on_disk_under_edits_waits_for_the_answer() {
        let dir = std::env::temp_dir().join(format!("finch-conflict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, "one").unwrap();
        let opened = path_mtime(Some(&path));
        // Someone else writes the file after we opened it.
        let theirs = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        theirs.set_modified(opened.unwrap() + Duration::from_secs(5)).unwrap();
        let disk = path_mtime(Some(&path));
        // With edits, the save must ask; without, the file just reloads.
        assert!(has_conflict(true, opened, disk));
        assert!(!has_conflict(false, opened, disk));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clean_tab_whose_file_moved_on_reloads_instead_of_writing() {
        let epoch = std::time::SystemTime::UNIX_EPOCH;
        let (saved, disk) = (Some(epoch), Some(epoch + Duration::from_secs(5)));
        assert!(moved_on_disk(saved, disk));
        assert!(!moved_on_disk(saved, saved));
        assert!(!moved_on_disk(None, disk));
        assert!(!moved_on_disk(saved, None));
    }

    #[test]
    fn an_older_copy_restored_over_a_file_conflicts_only_with_unsaved_edits() {
        let dir = scratch("older-conflict");
        let path = dir.join("notes.txt");
        std::fs::write(&path, "current").unwrap();
        let saved = path_mtime(Some(&path));
        let restored = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        restored.set_modified(saved.unwrap() - Duration::from_secs(5)).unwrap();
        let disk = path_mtime(Some(&path));
        assert!(disk < saved);
        assert!(has_conflict(true, saved, disk));
        assert!(!has_conflict(false, saved, disk));
        assert!(!has_conflict(true, saved, saved));
        assert!(!has_conflict(true, None, disk));
        assert!(!has_conflict(true, saved, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_disk_check_that_overlapped_a_save_is_stale() {
        let mut saves = Saves::default();
        let before = saves.epoch;
        assert!(!saves.stale(before));
        // Begun before the save, arriving during it.
        let snapshot = saves.begin().unwrap();
        assert!(saves.stale(before));
        assert!(saves.stale(saves.epoch), "nothing read during a write counts");
        assert!(saves.finish(snapshot));
        // Begun before the save, arriving after it: still stale; begun after it: fresh.
        assert!(saves.stale(before));
        assert!(!saves.stale(saves.epoch));
    }

    #[test]
    fn our_own_save_followed_by_another_never_conflicts() {
        let dir = std::env::temp_dir().join(format!("finch-own-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, "one").unwrap();
        let mut noted = path_mtime(Some(&path));
        for text in ["two", "three", "four"] {
            assert!(!has_conflict(true, noted, path_mtime(Some(&path))));
            save_file(&path, &rope(text), false).unwrap();
            noted = path_mtime(Some(&path));
        }
        assert!(!has_conflict(true, noted, path_mtime(Some(&path))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latin1_and_utf16_files_save_back_to_the_bytes_they_were_read_from() {
        let dir = std::env::temp_dir().join(format!("finch-encodings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (latin1, _, _) = encoding_rs::WINDOWS_1252.encode("Café crème brûlée, déjà vu: l'été à Genève était très agréable.\n");
        let (utf16, _) = crate::encoding::encode("fn main() {}\nlet café = \"日本\";\n", encoding_rs::UTF_16LE, true);
        for (name, bytes) in [("latin1.txt", latin1.into_owned()), ("utf16.txt", utf16.into_owned())] {
            let path = dir.join(name);
            std::fs::write(&path, &bytes).unwrap();
            let decoded = crate::encoding::read_file(&path).unwrap();
            // What Save does for a file that isn't plain UTF-8.
            let (out, lossy) = crate::encoding::encode(&decoded.text, decoded.encoding, decoded.has_bom);
            assert!(!lossy);
            save_bytes(&path, &out).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("finch-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_background_save_lands_the_snapshots_bytes_and_the_file_time() {
        let dir = scratch("snapshot");
        let utf8 = dir.join("a.txt");
        let latin1 = dir.join("b.txt");
        std::fs::write(&utf8, "old").unwrap();
        let snapshot = rope("caf\u{e9} one\ntwo\n");
        let Written::Done(Ok(disk)) = write_out(&utf8, &snapshot, encoding_rs::UTF_8, false, false) else { panic!("utf-8 save failed") };
        assert_eq!(std::fs::read(&utf8).unwrap(), "caf\u{e9} one\ntwo\n".as_bytes());
        assert_eq!(disk.mtime, path_mtime(Some(&utf8)));
        assert!(!has_conflict(true, disk.mtime, path_mtime(Some(&utf8))), "our own background save records the new time");
        assert!(!disk.gone);
        let Written::Done(Ok(_)) = write_out(&latin1, &snapshot, encoding_rs::WINDOWS_1252, false, false) else { panic!("latin1 save failed") };
        assert_eq!(std::fs::read(&latin1).unwrap(), b"caf\xe9 one\ntwo\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_background_save_of_text_the_encoding_cant_hold_writes_nothing() {
        let dir = scratch("lossy");
        let path = dir.join("a.txt");
        std::fs::write(&path, "old").unwrap();
        let written = write_out(&path, &rope("caf\u{e9} \u{65e5}"), encoding_rs::WINDOWS_1252, false, false);
        assert!(matches!(written, Written::Lossy(Some('\u{65e5}'))));
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edit_during_a_save_leaves_the_document_dirty() {
        let mut saves = Saves::default();
        let snapshot = saves.begin().unwrap();
        saves.edited();
        assert!(!saves.finish(snapshot), "the text moved on, so the tab stays dirty");
        // Without an edit the save leaves the tab clean.
        let snapshot = saves.begin().unwrap();
        assert!(saves.finish(snapshot));
    }

    #[test]
    fn a_save_knows_when_it_began_and_forgets_it_when_it_lands() {
        let mut saves = Saves::default();
        assert!(saves.began.is_none() && !saves.slow);
        let snapshot = saves.begin().unwrap();
        assert!(saves.began.is_some() && !saves.slow, "a save is not slow until its timer says so");
        saves.slow = true;
        assert!(saves.finish(snapshot));
        assert!(saves.began.is_none() && !saves.slow && saves.timer.is_none(), "a landed save leaves nothing to breathe for");
        // The next save starts quick again.
        saves.begin().unwrap();
        assert!(!saves.slow);
    }

    #[test]
    fn two_quick_saves_never_write_at_once_and_leave_the_latest_text_on_disk() {
        let dir = scratch("quick");
        let path = dir.join("a.txt");
        let mut saves = Saves::default();
        let mut text = rope("first");
        // The first save snapshots "first"; the user types; a second save is asked for while it runs.
        let one = saves.begin().unwrap();
        let snapshot_one = text.clone();
        text = rope("first and second");
        saves.edited();
        assert!(saves.begin().is_none(), "one writer at a time");
        assert!(matches!(write_out(&path, &snapshot_one, encoding_rs::UTF_8, false, false), Written::Done(Ok(_))));
        assert!(!saves.finish(one));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        // The waiting save starts once the first lands, from the text as it is by then.
        let two = saves.begin().unwrap();
        assert!(matches!(write_out(&path, &text.clone(), encoding_rs::UTF_8, false, false), Written::Done(Ok(_))));
        assert!(saves.finish(two));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first and second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_restored_tabs_hold_no_text_before_they_show_and_a_draft_keeps_its_text() {
        use super::{Body, Document};
        let dir = scratch("unread-tabs");
        let tabs: Vec<Document> = (0..8)
            .map(|n| {
                let path = dir.join(format!("file{n}.txt"));
                std::fs::write(&path, format!("text of file {n}\n")).unwrap();
                Document::unread(path.clone(), path_mtime(Some(&path)), true)
            })
            .collect();
        for tab in &tabs {
            assert!(matches!(tab.body, Body::Unread), "no text kept");
            assert!(!tab.is_built() && !tab.unsaved && !tab.shows_dot());
            assert!(tab.path.is_some() && !tab.title().is_empty(), "path and title are all it needs");
            assert!(tab.tab_file().is_some(), "it still leads back after a relaunch");
            assert!(!tab.has_preview());
        }
        let draft = Document::unbuilt_draft(dir.join("Untitled 1.txt"), "typed text".into(), true);
        assert!(matches!(&draft.body, Body::Unbuilt(text) if text == "typed text"), "a draft is the only copy of its text");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_file_waiting_to_open_keeps_its_tab_identity() {
        use super::Document;
        let path = std::path::PathBuf::from("/large.txt");
        let mut doc = Document::unread(path.clone(), None, true);
        doc.awaiting_open = true;
        assert_eq!(doc.path.as_ref(), Some(&path));
        assert_eq!(doc.tab_file(), Some(path));
        assert!(!doc.is_blank());
        doc.path = None;
        assert!(!doc.is_blank(), "a waiting editor is never a replaceable blank page");
    }

    #[test]
    fn a_crlf_file_saves_with_only_crlf_and_lf_or_mixed_files_save_as_they_are() {
        use super::Document;
        use crate::encoding::Decoded;
        let dir = scratch("crlf");
        let path = dir.join("a.txt");
        // (bytes at open, the text after an inserted LF line, bytes it saves as)
        let cases: [(&str, &str, &str); 3] = [
            ("one\r\ntwo\r\n", "one\r\nnew\ntwo\r\n", "one\r\nnew\r\ntwo\r\n"),
            ("one\ntwo\n", "one\nnew\ntwo\n", "one\nnew\ntwo\n"),
            ("one\r\ntwo\n", "one\r\nnew\ntwo\n", "one\r\nnew\ntwo\n"),
        ];
        for (opened, edited, saved) in cases {
            let doc = Document::unbuilt(None, Decoded::utf8(opened.into()), None, false);
            let all_crlf = opened == "one\r\ntwo\r\n";
            assert_eq!(doc.crlf, all_crlf, "{opened:?}");
            for (encoding, bom) in [(encoding_rs::UTF_8, false), (encoding_rs::UTF_8, true), (encoding_rs::UTF_16LE, true)] {
                let Written::Done(Ok(_)) = write_out(&path, &rope(edited), encoding, bom, doc.crlf) else { panic!("save failed") };
                let (want, _) = crate::encoding::encode(saved, encoding, bom);
                assert_eq!(std::fs::read(&path).unwrap(), want.as_ref(), "{opened:?} as {}", encoding.name());
            }
        }
        // A CR and its LF split across rope chunks stay one CRLF.
        let long = format!("{}\r\n{}\n", "x".repeat(70_000), "y".repeat(70_000));
        let Written::Done(Ok(_)) = write_out(&path, &rope(&long), encoding_rs::UTF_8, false, true) else { panic!("save failed") };
        assert_eq!(std::fs::read(&path).unwrap(), long.replace('\n', "\r\n").replace("\r\r", "\r").as_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_and_adopting_keep_the_lossy_decode_guard() {
        use super::Document;
        use crate::encoding::{Decoded, decode};
        let broken = || decode(b"\xff\xfeh\0i\0x".to_vec()).unwrap();
        let mut doc = Document::unbuilt(None, broken(), None, false);
        assert!(doc.replaced);
        doc.adopt(Decoded::utf8("clean".into()), None);
        assert!(!doc.replaced);
        doc.adopt(broken(), None);
        assert!(doc.replaced);
    }
}
