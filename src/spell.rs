//! Spell check: the Mac's red wavy underline under misspelled words, only where you write words.
//! View > Check spelling turns it on and off (on at first, remembered as `editor.check_spelling`).
//!
//! It is a layer on the shared mark driver (`marks.rs`): one text-style collection of wavy
//! underlines. Prose (Markdown and plain text) is checked everywhere except Markdown's code
//! fences, inline code, link targets and tags; in code only comments and strings are checked, found
//! from the syntax roles of the file's own highlighter (a capture named `comment*` or `string*`),
//! so a name in code is never looked at. Inside those, a chunk that isn't plainly a word (it has
//! digits, `_`, `/`, `.`, a capital inside it or is all capitals) is left alone, so `fn_nmae`,
//! paths, URLs, camelCase and acronyms never show up.
//!
//! Cost is bounded the way the risk brief asks: only the rows the driver hands over (the screen and
//! a screen each way, cut to `WINDOW_BYTES` around the caret), lines over `MAX_LINE` bytes are
//! skipped, nothing is checked in a file over `MAX_FILE`, and code is only parsed under
//! `MAX_CODE`. The scan runs on the background executor after a 300 ms pause and is dropped when
//! the text moved on (`marks::Debounce`). The word at the caret is skipped while text is being typed
//! (its edit would flash it red at every letter) and looked at again once the caret leaves it or
//! moves without typing.
//!
//! The Mac's `NSSpellChecker` is asked on first need (nothing at launch), never synchronously for a
//! whole window: one `requestCheckingOfString:...completionHandler:` for the words the layer hasn't
//! heard an answer for yet, the answers kept in a per-layer word cache that the Learn and Ignore
//! items, and turning the check on, empty. The completion block is a hand-built Objective-C block
//! (no `block2` here) that hands the answer to the waiting task over a oneshot channel. Language
//! is the Mac's: the checker's own language with automatic detection off, as one word has no
//! sentence to detect from. Learned words and Ignore Spelling are the checker's.
//!
//! The right-click menu (`panes::show_editor_menu`) asks `misspelling_at_caret` for the underlined
//! word at the caret and offers the Mac's guesses, Learn Spelling and Ignore Spelling.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::{CString, c_char, c_void},
    ops::Range,
    rc::Rc,
    sync::Once,
    time::Duration,
};

use futures_channel::oneshot;
use gpui_kit::component::{
    ActiveTheme as _,
    highlighter::SyntaxHighlighter,
    input::{EditorState, HighlightStyleResolver, Rope, RopeExt as _, TextDecoration},
};
use gpui_kit::*;
use objc2::{
    Encode, Encoding, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool},
};

use crate::{
    marks::{self, Debounce, MarkLayer, Slot, Step, TextSet, View},
    settings,
};
use tree_sitter::Tree;

/// The pause after the last change before the window is scanned.
const WAIT: Duration = Duration::from_millis(300);

/// The most bytes of text one scan looks at, around the caret.
const WINDOW_BYTES: usize = 160 * 1024;

/// A line longer than this is not looked at.
const MAX_LINE: usize = 10_000;

/// A file longer than this is not checked at all.
const MAX_FILE: usize = 4 * 1024 * 1024;

/// Code longer than this is not parsed for its comments and strings.
const MAX_CODE: usize = 1024 * 1024;

/// A run of characters between spaces longer than this is no word.
const MAX_CHUNK: usize = 64;

/// The most words one request to the checker carries.
const MAX_ASK: usize = 2000;

/// The answers the layer remembers before it starts over.
const MAX_CACHE: usize = 50_000;

/// How many rows above the window a Markdown fence is looked for.
const FENCE_LOOKBACK: usize = 3000;

/// How many guesses the menu shows.
const GUESSES: usize = 5;

/// Wired from `menu::install`: View > Check spelling.
pub fn install(cx: &mut App) {
    cx.on_action(|_: &ToggleSpelling, cx| cx.defer(toggle));
    // A change to the setting (this menu, or the file edited by hand) reaches every open editor.
    let seen = Rc::new(Cell::new(settings::get(cx).editor.check_spelling));
    cx.observe_global::<settings::Store>(move |cx| {
        let on = settings::get(cx).editor.check_spelling;
        if seen.replace(on) != on {
            cx.defer(move |cx| {
                each_editor(cx, |editor, cx| {
                    marks::with_layer(editor, cx, |layer: &mut Layer| ((), layer.set_on(on)));
                });
            });
        }
    })
    .detach();
}

/// View > Check spelling.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct ToggleSpelling;

/// Every editor with a spell layer, so a toggle or a Learn reaches them all.
#[derive(Default)]
struct Editors(Vec<WeakEntity<EditorState>>);

impl Global for Editors {}

fn each_editor(cx: &mut App, mut each: impl FnMut(&Entity<EditorState>, &mut App)) {
    let editors: Vec<_> = cx.default_global::<Editors>().0.iter().filter_map(|editor| editor.upgrade()).collect();
    for editor in &editors {
        each(editor, cx);
    }
}

fn toggle(cx: &mut App) {
    let on = !settings::get(cx).editor.check_spelling;
    settings::update_later(cx, |settings| settings.editor.check_spelling = on);
    crate::menu::refresh(cx);
}

/// The underlined word at the caret, with the Mac's guesses for it.
pub struct Misspelling {
    pub range: Range<usize>,
    pub word: String,
    pub guesses: Vec<String>,
}

/// The underlined word the caret is in or at the end of, if there is one.
pub fn misspelling_at_caret(editor: &Entity<EditorState>, cx: &mut App) -> Option<Misspelling> {
    let caret = editor.read(cx).cursor();
    let set = marks::with_layer(editor, cx, |layer: &mut Layer| (layer.set.clone(), false))?;
    let held = set.borrow_mut().held(cx).to_vec();
    let range = held.into_iter().map(|decoration| decoration.range).find(|range| range.start <= caret && caret <= range.end)?;
    let word = editor.read(cx).text().slice(range.clone()).to_string();
    let mut guesses = guesses(&word);
    guesses.truncate(GUESSES);
    Some(Misspelling { range, word, guesses })
}

/// Puts `guess` in place of the misspelled word, as one undo step.
pub fn replace(editor: &Entity<EditorState>, found: &Misspelling, guess: &str, window: &mut Window, cx: &mut App) {
    editor.update(cx, |state, cx| {
        let text = state.text();
        let same = found.range.end <= text.len()
            && text.is_char_boundary(found.range.start)
            && text.is_char_boundary(found.range.end)
            && text.slice(found.range.clone()).to_string() == found.word;
        if same {
            state.replace_range(found.range.clone(), guess, found.range.start + guess.len(), window, cx);
        }
    });
}

/// Adds `word` to the Mac's learned words; every editor checks again.
pub fn learn(word: &str, cx: &mut App) {
    if let Some(checker) = checker() {
        let word = string(word);
        let _: () = unsafe { msg_send![&*checker, learnWord: &*word] };
    }
    check_again(cx);
}

/// Stops flagging `word` for this run of the app; every editor checks again.
pub fn ignore(word: &str, cx: &mut App) {
    if let Some(checker) = checker() {
        let word = string(word);
        let _: () = unsafe { msg_send![&*checker, ignoreWord: &*word, inSpellDocumentWithTag: tag()] };
    }
    check_again(cx);
}

fn check_again(cx: &mut App) {
    each_editor(cx, |editor, cx| {
        marks::with_layer(editor, cx, |layer: &mut Layer| ((), layer.forget()));
    });
}

/// What one scan found: each word with where it is.
type Words = Vec<(Range<usize>, String)>;

/// The spell layer of one editor.
pub struct Layer {
    on: bool,
    set: Rc<RefCell<TextSet>>,
    debounce: Debounce,
    /// The scan of a code window waiting for the editor's parse, then running.
    code: Option<Task<()>>,
    /// The newest code scan and its deadline; one task in `code` sleeps on it.
    pending: Rc<RefCell<Slot<CodeJob>>>,
    shared: Rc<RefCell<Shared>>,
    /// The revision the last wake saw, to tell typing from a caret move.
    rev: Option<u64>,
    /// The last scan: the revision, language and window it was made for, and its words.
    scan: Rc<RefCell<Option<(u64, &'static str, Range<usize>, Rc<Words>)>>>,
    /// The collection may hold underlines that have to be cleared when the check goes off.
    holds: bool,
}

#[derive(Default)]
struct Shared {
    /// What the checker said of each word: true when it is misspelled.
    cache: HashMap<String, bool>,
    /// The request to the checker that is waiting for its answer.
    request: Option<Task<()>>,
}

impl Layer {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let on = settings::get(cx).editor.check_spelling;
        let editors = &mut cx.default_global::<Editors>().0;
        editors.retain(|editor| editor.upgrade().is_some());
        editors.push(editor.downgrade());
        Self { on, set: Rc::new(RefCell::new(TextSet::new(editor, cx))), debounce: Debounce::default(), code: None, pending: Rc::default(), shared: Rc::default(), rev: None, scan: Rc::default(), holds: false }
    }

    /// Whether the state changed.
    fn set_on(&mut self, on: bool) -> bool {
        if self.on == on {
            return false;
        }
        self.on = on;
        if on {
            self.shared.borrow_mut().cache.clear();
        }
        true
    }

    /// Forgets what the checker said (a word was learned or ignored); true when it is checking.
    fn forget(&mut self) -> bool {
        let mut shared = self.shared.borrow_mut();
        shared.cache.clear();
        shared.request = None;
        self.on
    }

    /// Drops the code scan waiting or running.
    fn drop_code(&mut self) {
        self.code = None;
        self.pending.borrow_mut().clear();
    }

    fn clear(&mut self, cx: &mut App) {
        self.debounce.cancel();
        self.drop_code();
        self.shared.borrow_mut().request = None;
        self.set.borrow_mut().set_if_changed(Vec::new(), cx);
        self.holds = false;
    }
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        self.on || self.holds
    }

    fn release(&mut self) {
        self.scan.borrow_mut().take();
        self.shared.borrow_mut().cache.clear();
    }

    fn held(&self) -> usize {
        self.scan.borrow().as_ref().map_or(0, |scan| scan.3.len()) + self.shared.borrow().cache.len()
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        let typed = self.rev.replace(view.key.rev).is_some_and(|before| before != view.key.rev);
        let rows = capped_rows(view.text, view.key.rows.clone(), view.key.caret);
        let bytes = if rows.is_empty() { 0..0 } else { view.text.line_start_offset(rows.start)..view.text.line_end_offset(rows.end - 1) };
        let too_big = view.text.len() > MAX_FILE || bytes.len() > WINDOW_BYTES || (!view.prose() && view.text.len() > MAX_CODE);
        if !self.on || too_big || bytes.is_empty() {
            self.clear(cx);
            return;
        }
        self.holds = true;
        self.shared.borrow_mut().request = None;
        let (lang, markdown, prose) = (view.lang, view.lang == "markdown", view.prose());
        let (editor, rev, caret) = (view.editor.clone(), view.key.rev, view.key.caret);
        let (set, shared) = (self.set.clone(), self.shared.clone());
        let known = self.scan.borrow().as_ref().filter(|(was, was_lang, window, _)| *was == rev && *was_lang == lang && *window == bytes).map(|scan| scan.3.clone());
        if let Some(words) = known {
            // Only the caret moved: the words are the same, so no parse and no wait.
            self.debounce.cancel();
            self.drop_code();
            ask(&editor, rev, caret, typed, words, &set, &shared, cx);
            return;
        }
        let (window, last) = (bytes.clone(), self.scan.clone());
        let done = move |words: Words, cx: &mut App| {
            let words = Rc::new(words);
            *last.borrow_mut() = Some((rev, lang, bytes, words.clone()));
            ask(&editor, rev, caret, typed, words, &set, &shared, cx)
        };
        if prose {
            self.drop_code();
            self.debounce.run(view, WAIT, move |text| prose_scan(&text, window, markdown), done, cx);
            return;
        }
        // Code: the comments and strings come from the parse the editor's highlighter holds, read once the pause
        // is over. A parse that is still behind the text is waited for (the editor notifies when one lands), never polled.
        // A burst of keys moves one deadline and swaps in the newest job; the one task sleeping on it wakes at the
        // last key's end and runs only that job, so typing costs one task, not one per key.
        self.debounce.cancel();
        let job = CodeJob { editor: view.editor.clone(), rev, lang, window, done: Box::new(done) };
        let deadline = cx.background_executor().now() + WAIT;
        if !self.pending.borrow_mut().push(deadline, job) {
            return;
        }
        let pending = self.pending.clone();
        self.code = Some(cx.spawn(async move |cx| {
            let CodeJob { editor, rev, lang, window, done } = loop {
                let step = pending.borrow_mut().next(cx.background_executor().now());
                match step {
                    Step::Sleep(left) => cx.background_executor().timer(left).await,
                    Step::Run(Some(job)) => break job,
                    Step::Run(None) => return,
                }
            };
            let (landed, waiting) = oneshot::channel();
            let landed = Rc::new(RefCell::new(Some(landed)));
            let (snapshot, watch) = cx.update(|cx| {
                if let Some(snapshot) = current_tree(&editor, rev, cx) {
                    return (Some(snapshot), None);
                }
                let watch = cx.observe(&editor, move |editor, cx| {
                    if current_tree(&editor, rev, cx).is_some() {
                        if let Some(landed) = landed.borrow_mut().take() {
                            let _ = landed.send(());
                        }
                    }
                });
                (None, Some(watch))
            });
            let snapshot = match snapshot {
                Some(snapshot) => snapshot,
                None => {
                    let woke = waiting.await;
                    drop(watch);
                    if woke.is_err() {
                        return;
                    }
                    let Some(snapshot) = cx.update(|cx| current_tree(&editor, rev, cx)) else { return };
                    snapshot
                }
            };
            let Some((tree, text)) = snapshot else { return };
            let words = cx.background_executor().spawn(async move { code_scan(&tree, &text, window, lang) }).await;
            cx.update(|cx| {
                if editor.read(cx).text_revision() == rev {
                    done(words, cx);
                }
            });
        }));
    }
}

/// A code window's scan waiting out the pause: what to scan and who to hand the words to.
struct CodeJob {
    editor: Entity<EditorState>,
    rev: u64,
    lang: &'static str,
    window: Range<usize>,
    done: Box<dyn FnOnce(Words, &mut App)>,
}

/// The editor's parse tree and the text it parsed when it is current for `rev`; `Some(None)` when the
/// text has moved on from `rev`, `None` while the parse is behind.
fn current_tree(editor: &Entity<EditorState>, rev: u64, cx: &App) -> Option<Option<(Tree, Rope)>> {
    let state = editor.read(cx);
    if state.text_revision() != rev {
        return Some(None);
    }
    state.with_current_syntax_tree(|tree, text| tree.downcast_ref::<Tree>().map(|tree| (tree.clone(), text.clone()))).flatten().map(Some)
}

/// The rows cut to what `WINDOW_BYTES` holds around the caret, dropping from the far end.
fn capped_rows(text: &Rope, mut rows: Range<usize>, caret: usize) -> Range<usize> {
    rows.end = rows.end.min(text.lines_len());
    while rows.len() > 1 {
        let (first, last) = (text.line_start_offset(rows.start), text.line_end_offset(rows.end - 1));
        if last.saturating_sub(first) <= WINDOW_BYTES {
            break;
        }
        if caret.saturating_sub(first) > last.saturating_sub(caret) { rows.start += 1 } else { rows.end -= 1 }
    }
    rows
}

/// The words of a Markdown or plain text window.
fn prose_scan(text: &Rope, window: Range<usize>, markdown: bool) -> Words {
    let mut out = Vec::new();
    let fenced = markdown && window.start > 0 && {
        let from = text.line_start_offset(text.offset_to_point(window.start).row.saturating_sub(FENCE_LOOKBACK));
        fence_open(&text.slice(from..window.start).to_string())
    };
    prose_words(&text.slice(window.clone()).to_string(), window.start, markdown, fenced, &mut out);
    out
}

/// The words in the comments and strings of a code window, found from the highlighter's captures over the
/// editor's own parse of `text` (never a new parse of the file).
fn code_scan(tree: &Tree, text: &Rope, window: Range<usize>, lang: &'static str) -> Words {
    code_words(text, SyntaxHighlighter::new(lang).styles_for_tree(tree, text, &window, &Roles))
}

/// The words of the comment and string runs among `styles`, the merged styles `Roles` made.
fn code_words(text: &Rope, styles: Vec<(Range<usize>, HighlightStyle)>) -> Words {
    let mut segments: Vec<Range<usize>> = Vec::new();
    for (range, style) in styles {
        if style.background_color.is_none() || style.color.is_some() || range.is_empty() {
            continue;
        }
        match segments.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => segments.push(range),
        }
    }
    let mut out = Vec::new();
    for segment in segments {
        if (text.offset_to_point(segment.start).row..=text.offset_to_point(segment.end).row).any(|row| text.line_len(row) > MAX_LINE) {
            continue;
        }
        plain_words(&text.slice(segment.clone()).to_string(), segment.start, &mut out);
    }
    out
}

/// Marks comments and strings by a background color and every other capture by a color, which the
/// layer reads back from the merged styles.
struct Roles;

impl HighlightStyleResolver for Roles {
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        if name == "spell" {
            return None;
        }
        if name.starts_with("comment") || name.starts_with("string") {
            return Some(HighlightStyle { background_color: Some(hsla(0., 0., 0., 0.5)), ..Default::default() });
        }
        // Any other capture inside a string (a JSON key, an interpolation) is code, not prose.
        Some(HighlightStyle { color: Some(hsla(0., 0., 0., 1.)), ..Default::default() })
    }
}

/// The scan is in: asks the checker about the words it hasn't heard of, then underlines.
#[allow(clippy::too_many_arguments)]
fn ask(editor: &Entity<EditorState>, rev: u64, caret: usize, typed: bool, words: Rc<Words>, set: &Rc<RefCell<TextSet>>, shared: &Rc<RefCell<Shared>>, cx: &mut App) {
    let mut unknown: Vec<String> = Vec::new();
    {
        let shared = shared.borrow();
        for (_, word) in words.iter() {
            if !shared.cache.contains_key(word) && !unknown.contains(word) && unknown.len() < MAX_ASK {
                unknown.push(word.clone());
            }
        }
    }
    if unknown.is_empty() {
        underline(editor, rev, caret, typed, &words, set, shared, cx);
        return;
    }
    let Some(answer) = check(&unknown) else { return };
    let (editor, set, held) = (editor.clone(), set.clone(), shared.clone());
    let task = cx.spawn(async move |cx| {
        let Ok(misspelled) = answer.await else { return };
        cx.update(|cx| {
            {
                let mut shared = held.borrow_mut();
                if shared.cache.len() + unknown.len() > MAX_CACHE {
                    shared.cache.clear();
                }
                shared.cache.extend(unknown.into_iter().zip(misspelled));
            }
            // Words past `MAX_ASK` are asked next.
            ask(&editor, rev, caret, typed, words, &set, &held, cx);
        });
    });
    shared.borrow_mut().request = Some(task);
}

#[allow(clippy::too_many_arguments)]
fn underline(editor: &Entity<EditorState>, rev: u64, caret: usize, typed: bool, words: &Words, set: &Rc<RefCell<TextSet>>, shared: &Rc<RefCell<Shared>>, cx: &mut App) {
    if editor.read(cx).text_revision() != rev {
        return;
    }
    let mut color = cx.theme().danger;
    color.a = 0.9;
    let style = HighlightStyle { underline: Some(UnderlineStyle { thickness: px(1.0), color: Some(color), wavy: true }), ..Default::default() };
    let shared = shared.borrow();
    let list = words
        .iter()
        .filter(|(range, word)| shared.cache.get(word) == Some(&true) && !(typed && range.start <= caret && caret <= range.end))
        .map(|(range, _)| TextDecoration::new(range.clone(), style))
        .collect();
    set.borrow_mut().set_if_changed(list, cx);
}

/// Whether `prefix` ends inside a fenced code block of Markdown.
fn fence_open(prefix: &str) -> bool {
    prefix.lines().filter(|line| is_fence(line)).count() % 2 == 1
}

fn is_fence(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("```") || line.starts_with("~~~")
}

/// The words of `text` (which starts at byte `base`), line by line; Markdown leaves out fenced
/// code (`fenced`: the text starts inside a fence), inline code, link targets, tags and entities.
fn prose_words(text: &str, base: usize, markdown: bool, mut fenced: bool, out: &mut Words) {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let body = line.trim_end_matches(['\n', '\r']);
        if markdown {
            if is_fence(body) {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
        }
        if body.len() > MAX_LINE {
            continue;
        }
        if markdown {
            plain_words(&mask_markdown(body), base + start, out);
        } else {
            plain_words(body, base + start, out);
        }
    }
}

/// `line` with the parts of Markdown that are not prose turned to spaces, byte for byte.
fn mask_markdown(line: &str) -> String {
    let mut bytes = line.as_bytes().to_vec();
    let find = |from: usize, close: u8, within: usize| line.as_bytes()[from..].iter().take(within).position(|byte| *byte == close).map(|at| from + at);
    let mut i = 0;
    while i < bytes.len() {
        let end = match bytes[i] {
            b'`' => find(i + 1, b'`', usize::MAX),
            b']' if bytes.get(i + 1) == Some(&b'(') => find(i + 2, b')', usize::MAX),
            b'<' => find(i + 1, b'>', 300),
            b'&' => find(i + 1, b';', 10).filter(|end| line.as_bytes()[i + 1..*end].iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'#')),
            _ => None,
        };
        match end {
            Some(end) => {
                bytes[i..=end].fill(b' ');
                i = end + 1;
            }
            None => i += 1,
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}

/// The words of `text` (which starts at byte `base`): each space-separated chunk that is nothing
/// but letters, apostrophes and hyphens once the punctuation at its ends is off, split at hyphens.
fn plain_words(text: &str, base: usize, out: &mut Words) {
    let mut chunk: Option<usize> = None;
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        if c.is_whitespace() {
            if let Some(start) = chunk.take() {
                chunk_words(&text[start..i], base + start, out);
            }
        } else if chunk.is_none() {
            chunk = Some(i);
        }
    }
}

fn chunk_words(chunk: &str, base: usize, out: &mut Words) {
    if chunk.len() > MAX_CHUNK {
        return;
    }
    let lead = chunk.len() - chunk.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
    let trimmed = chunk[lead..].trim_end_matches(|c: char| !c.is_alphanumeric());
    if !trimmed.chars().all(|c| c.is_alphabetic() || matches!(c, '\'' | '\u{2019}' | '-')) {
        return;
    }
    let mut at = base + lead;
    for part in trimmed.split('-') {
        if word_like(part) {
            out.push((at..at + part.len(), part.to_string()));
        }
        at += part.len() + 1;
    }
}

/// A word worth asking about: three letters or more, no capital past the first (so no camelCase
/// and no acronyms).
fn word_like(word: &str) -> bool {
    word.chars().count() >= 3 && !word.chars().skip(1).any(char::is_uppercase)
}

// The Mac's checker, written against the Objective-C runtime like `native_menu.rs`. AppKit is
// already in the process, so the class is looked up by name and nothing loads at launch.

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRange {
    location: usize,
    length: usize,
}

// SAFETY: `NSRange` is two `NSUInteger`s, as `_NSRange` is.
unsafe impl Encode for NSRange {
    const ENCODING: Encoding = Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}

fn string(text: &str) -> Retained<AnyObject> {
    let text = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { msg_send![objc2::class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

fn text(string: &AnyObject) -> String {
    let chars: *const c_char = unsafe { msg_send![string, UTF8String] };
    if chars.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(chars) }.to_string_lossy().into_owned()
}

fn checker() -> Option<Retained<AnyObject>> {
    static SETUP: Once = Once::new();
    let class = AnyClass::get(c"NSSpellChecker")?;
    let checker: Option<Retained<AnyObject>> = unsafe { msg_send![class, sharedSpellChecker] };
    let checker = checker?;
    SETUP.call_once(|| {
        // One word has no sentence to detect a language from: use the Mac's own.
        let _: () = unsafe { msg_send![&*checker, setAutomaticallyIdentifiesLanguages: Bool::NO] };
    });
    Some(checker)
}

/// The checker's tag for this app's one document, which is what Ignore Spelling is kept under.
fn tag() -> isize {
    thread_local!(static TAG: Cell<Option<isize>> = const { Cell::new(None) });
    TAG.with(|tag| {
        if tag.get().is_none() {
            tag.set(Some(unsafe { msg_send![objc2::class!(NSSpellChecker), uniqueSpellDocumentTag] }));
        }
        tag.get().unwrap_or(0)
    })
}

fn guesses(word: &str) -> Vec<String> {
    let Some(checker) = checker() else { return Vec::new() };
    let (shown, range) = (string(word), NSRange { location: 0, length: word.encode_utf16().count() });
    let list: Option<Retained<AnyObject>> = unsafe { msg_send![&*checker, guessesForWordRange: range, inString: &*shown, language: None::<&AnyObject>, inSpellDocumentWithTag: tag()] };
    let Some(list) = list else { return Vec::new() };
    let count: usize = unsafe { msg_send![&*list, count] };
    (0..count)
        .map(|index| {
            let guess: Retained<AnyObject> = unsafe { msg_send![&*list, objectAtIndex: index] };
            text(&guess)
        })
        .collect()
}

/// What the completion block needs: where to send the answer and where each word starts in the
/// string that was checked (in UTF-16 units, the checker's).
struct Reply {
    tx: oneshot::Sender<Vec<bool>>,
    starts: Vec<usize>,
}

#[repr(C)]
struct Descriptor {
    reserved: usize,
    size: usize,
}

/// An Objective-C block with one captured pointer, laid out as the blocks ABI says: the stack
/// kind, which the checker copies to the heap when it keeps it.
#[repr(C)]
struct RawBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut RawBlock, isize, *mut AnyObject, *mut AnyObject, isize),
    descriptor: *const Descriptor,
    reply: *mut Reply,
}

static DESCRIPTOR: Descriptor = Descriptor { reserved: 0, size: std::mem::size_of::<RawBlock>() };

unsafe extern "C" {
    static _NSConcreteStackBlock: c_void;
}

/// The completion handler: the checker calls it once with the results (`NSTextCheckingResult`s),
/// whose ranges say which of the words are misspelled.
unsafe extern "C" fn answered(block: *mut RawBlock, _sequence: isize, results: *mut AnyObject, _orthography: *mut AnyObject, _words: isize) {
    // SAFETY: the block is the one `check` built, so `reply` is the box it leaked, taken back once.
    let reply = unsafe { Box::from_raw((*block).reply) };
    let mut misspelled = vec![false; reply.starts.len()];
    if !results.is_null() {
        let count: usize = unsafe { msg_send![&*results, count] };
        for index in 0..count {
            let result: Retained<AnyObject> = unsafe { msg_send![&*results, objectAtIndex: index] };
            let range: NSRange = unsafe { msg_send![&*result, range] };
            let at = reply.starts.partition_point(|start| *start <= range.location);
            if at > 0 {
                misspelled[at - 1] = true;
            }
        }
    }
    let _ = reply.tx.send(misspelled);
}

/// Asks the checker, asynchronously, which of `words` are misspelled: one answer per word, in
/// order. `None` when the Mac has no checker. The words go in one string, a line each.
fn check(words: &[String]) -> Option<oneshot::Receiver<Vec<bool>>> {
    let checker = checker()?;
    let (mut joined, mut starts, mut units) = (String::new(), Vec::with_capacity(words.len()), 0);
    for word in words {
        starts.push(units);
        joined.push_str(word);
        joined.push('\n');
        units += word.encode_utf16().count() + 1;
    }
    let (tx, rx) = oneshot::channel();
    let reply = Box::into_raw(Box::new(Reply { tx, starts }));
    let mut block = RawBlock { isa: &raw const _NSConcreteStackBlock, flags: 0, reserved: 0, invoke: answered, descriptor: &DESCRIPTOR, reply };
    let shown = string(&joined);
    let range = NSRange { location: 0, length: units };
    // NSTextCheckingTypeSpelling is 1 << 1.
    let _: isize = unsafe {
        msg_send![&*checker, requestCheckingOfString: &*shown, range: range, types: 2u64, options: None::<&AnyObject>, inSpellDocumentWithTag: tag(), completionHandler: (&raw mut block).cast::<AnyObject>()]
    };
    Some(rx)
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::{highlighter::SyntaxHighlighter, input::Rope};
    use std::time::{Duration, Instant};

    use super::{MAX_LINE, Roles, WAIT, code_scan, code_words, fence_open, plain_words, prose_words};
    use crate::marks::{Slot, Step};

    /// 200 keys, 20 ms apart: one task is started (before: 200) and, after the pause that follows the
    /// last key, it runs exactly the newest job once.
    #[test]
    fn a_burst_of_keys_in_code_starts_one_task_and_checks_the_newest_revision_once() {
        let (gap, start) = (Duration::from_millis(20), Instant::now());
        let mut pending: Slot<usize> = Slot::default();
        let mut spawned = 0;
        for key in 0..200 {
            spawned += usize::from(pending.push(start + gap * key as u32 + WAIT, key));
        }
        assert_eq!(spawned, 1);
        let last = start + gap * 199;
        assert!(matches!(pending.next(start + WAIT), Step::Sleep(left) if left == gap * 199), "the first wake finds the deadline moved");
        assert!(matches!(pending.next(last + WAIT), Step::Run(Some(199))), "the newest revision runs, once");
        assert!(matches!(pending.next(last + WAIT), Step::Run(None)));
        assert!(pending.push(last + WAIT * 2, 200), "the next key after the pause starts a task again");
    }

    /// The tree the editor holds is the one the old scan made by parsing: same words, same places.
    #[test]
    fn code_words_from_a_given_tree_match_a_fresh_parse() {
        for (lang, source) in [
            ("rust", "// a wrold here\nfn main() {\n    let s = \"teh string\"; /* blokc */\n    foo(\"x\"); // dones\n}\n/// Docs wiht typo\nstruct A;\n"),
            ("json", "{\"keey\": \"vlaue here\", \"n\": 1}\n"),
        ] {
            let text = Rope::from(source);
            let window = 0..text.len();
            let mut fresh = SyntaxHighlighter::new(lang);
            fresh.update(None, &text, None);
            let old = code_words(&text, fresh.styles(&window, &Roles));
            let tree = fresh.tree().expect("parsed").clone();
            let new = code_scan(&tree, &text, window, lang);
            assert!(!old.is_empty(), "{lang} fixture has words");
            assert_eq!(old, new, "{lang}");
        }
    }

    fn found(text: &str, markdown: bool) -> Vec<String> {
        let mut out = Vec::new();
        prose_words(text, 0, markdown, false, &mut out);
        out.into_iter().map(|(_, word)| word).collect()
    }

    #[test]
    fn plain_prose_gives_words_with_their_offsets() {
        let mut out = Vec::new();
        plain_words("Hello, wrold! it's well-known.", 10, &mut out);
        let words: Vec<_> = out.iter().map(|(range, word)| (range.clone(), word.as_str())).collect();
        assert_eq!(words, vec![(10..15, "Hello"), (17..22, "wrold"), (24..28, "it's"), (29..33, "well"), (34..39, "known")]);
    }

    #[test]
    fn names_numbers_paths_and_acronyms_are_not_words() {
        assert_eq!(found("fn_nmae camelCase HTTP v2 /usr/bin a.b e.g. user@host https://x.y word", false), vec!["word"]);
    }

    #[test]
    fn markdown_leaves_out_code_links_and_fences() {
        let text = "Some `codee` and [a link](http://exmple.com/teh) here\n```\nfenced wrods\n```\n<span class=xx>text</span> done &amp; dusted\n";
        assert_eq!(found(text, true), vec!["Some", "and", "link", "here", "text", "done", "dusted"]);
    }

    #[test]
    fn a_window_that_starts_inside_a_fence_stays_in_it() {
        assert!(fence_open("a\n```rust\nlet x\n"));
        assert!(!fence_open("a\n```\nb\n```\n"));
        let mut out = Vec::new();
        prose_words("fenced wrods\n```\nafter\n", 0, true, true, &mut out);
        assert_eq!(out.into_iter().map(|(_, word)| word).collect::<Vec<_>>(), vec!["after"]);
    }

    #[test]
    fn long_lines_are_skipped() {
        let long = "word ".repeat(MAX_LINE / 4);
        assert!(long.len() > MAX_LINE);
        assert_eq!(found(&format!("{long}\nshort"), false), vec!["short"]);
    }
}
