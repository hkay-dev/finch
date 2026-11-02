//! Config parse check (finch#03xt): tells when a JSON, TOML or YAML file stops parsing, from the
//! error and missing nodes of the tree the highlighter already holds. It never parses: the tree
//! is read only while it is current for the text (`EditorState::with_current_syntax_tree`), and
//! the walk visits only the branches that hold an error.
//!
//! What the status line calls (see `ConfigWatch`): on every editor change and tab switch, not per
//! frame, `watch.schedule(...)`; it waits about 300 ms after the last edit (so half typed text
//! never nags), checks the tree, sets or clears the faint wavy underline, and tells the status
//! line to redraw only when the answer changed. `watch.error()` is what the pill shows: `None`
//! hides it. Click on the pill: `watch.jump_target()` is the byte offset to put the caret at.
//!
//! The message comes from the error node's kind and neighbours ("expected a comma", "this [ is
//! never closed"); no second parser is involved. Trailing commas in JSON are not an error for
//! JSONC (`lenient_json`).

use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc, time::Duration};

use futures_channel::oneshot;

use gpui_kit::component::{
    ActiveTheme as _,
    input::{EditorState, Rope, RopeExt as _, TextDecoration, TextDecorationCollection},
};
use gpui_kit::*;
use tree_sitter::{Node, Tree};

use crate::language::MAX_HIGHLIGHT_BYTES;

actions!(gpui_editor, [FormatJson]);

/// How long after the last edit the file is checked.
const SETTLE: Duration = Duration::from_millis(300);
/// Most editor notifications a wait for the parse sits through before giving up on a tree that never lands (a parse that is busy
/// notifies every few milliseconds, so this is seconds of waiting, and an idle editor costs nothing while it waits).
const MAX_WAKES: usize = 2000;
/// Most bytes of an error node scanned for an unclosed bracket or quote.
const SCAN_CAP: usize = 256 * 1024;

/// Waits for `probe` to answer from the editor's syntax tree: tries once at once, then again on each of the editor's notifies (it
/// notifies when a parse lands), never on a timer. `None` when the editor is released or `MAX_WAKES` notifies pass without an answer.
pub async fn when_parsed<R>(editor: &Entity<EditorState>, cx: &mut AsyncApp, probe: impl Fn(&mut App) -> Option<R>) -> Option<R> {
    let slot: Rc<RefCell<Option<oneshot::Sender<()>>>> = Rc::default();
    let mut watch = None;
    for _ in 0..MAX_WAKES {
        let (wake, woken) = oneshot::channel();
        let found = cx.update(|cx| {
            if let Some(found) = probe(cx) {
                return Some(found);
            }
            *slot.borrow_mut() = Some(wake);
            if watch.is_none() {
                let (notified, released) = (slot.clone(), slot.clone());
                watch = Some((
                    cx.observe(editor, move |_, _| {
                        if let Some(wake) = notified.borrow_mut().take() {
                            let _ = wake.send(());
                        }
                    }),
                    cx.observe_release(editor, move |_, _| {
                        released.borrow_mut().take();
                    }),
                ));
            }
            None
        });
        if found.is_some() {
            return found;
        }
        if woken.await.is_err() {
            return None;
        }
    }
    None
}

/// The config languages the check knows, by the highlighter's name for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Json,
    Toml,
    Yaml,
}

impl Lang {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "json" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" => Some(Self::Yaml),
            _ => None,
        }
    }
}

/// Whether a JSON file is JSONC (comments are always read; trailing commas are then allowed):
/// by its name, for the files tools read leniently.
pub fn lenient_json(file_name: &str) -> bool {
    let name = file_name.to_ascii_lowercase();
    name.ends_with(".jsonc")
        || matches!(
            name.as_str(),
            "tsconfig.json" | "jsconfig.json" | "devcontainer.json" | ".eslintrc" | ".babelrc" | ".swcrc" | ".jshintrc" | ".stylelintrc" | ".prettierrc" | "pyrightconfig.json"
        )
        || name.starts_with("tsconfig.") && name.ends_with(".json")
}

/// The text a check reads next to the tree: byte ranges and positions.
pub trait Text {
    fn len(&self) -> usize;
    /// The bytes `range` as a string, its ends moved inward to character boundaries.
    fn slice(&self, range: Range<usize>) -> String;
    /// Zero-based line and zero-based column in characters of a byte offset.
    fn position(&self, offset: usize) -> (usize, usize);
}

impl Text for str {
    fn len(&self) -> usize {
        str::len(self)
    }

    fn slice(&self, range: Range<usize>) -> String {
        let mut start = range.start.min(self.len());
        let mut end = range.end.min(self.len());
        while !self.is_char_boundary(start) {
            start += 1;
        }
        while !self.is_char_boundary(end) {
            end -= 1;
        }
        self[start..end.max(start)].to_string()
    }

    fn position(&self, offset: usize) -> (usize, usize) {
        let before = self.slice(0..offset);
        let line = before.matches('\n').count();
        let column = before.rsplit('\n').next().map_or(0, |last| last.chars().count());
        (line, column)
    }
}

impl Text for Rope {
    fn len(&self) -> usize {
        Rope::len(self)
    }

    fn slice(&self, range: Range<usize>) -> String {
        let mut start = range.start.min(self.len());
        let mut end = range.end.min(self.len());
        while start < end && !self.is_char_boundary(start) {
            start += 1;
        }
        while end > start && !self.is_char_boundary(end) {
            end -= 1;
        }
        Rope::slice(self, start..end).to_string()
    }

    fn position(&self, offset: usize) -> (usize, usize) {
        let position = self.offset_to_position(offset.min(self.len()));
        (position.line as usize, position.character as usize)
    }
}

/// Where a file stopped parsing, ready for the pill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// One-based.
    pub line: usize,
    /// One-based, in characters.
    pub column: usize,
    /// Short and lower case after "Line N:", like "expected a comma".
    pub message: String,
    /// The bytes the wavy underline covers (at least one character); `range.start` is where the
    /// pill's click puts the caret.
    pub range: Range<usize>,
}

impl ConfigError {
    /// The pill's words: "Line 12: expected a comma".
    pub fn pill(&self) -> String {
        format!("Line {}: {}", self.line, self.message)
    }

    /// The pill's tooltip.
    pub fn tooltip(&self) -> String {
        format!("Line {}, column {}: {}. Click to go there.", self.line, self.column, self.message)
    }
}

/// The error of the first place `tree` stops parsing `text`, or `None` for a file that parses.
/// `lenient` allows JSON's trailing commas (JSONC).
pub fn check<T: Text + ?Sized>(tree: &Tree, text: &T, lang: Lang, lenient: bool) -> Option<ConfigError> {
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    let node = first_problem(root, text, lang, lenient)?;
    let (spot, message) = describe(node, text, lang);
    let (line, column) = text.position(spot);
    let end = spot + text.slice(spot..spot + 4).chars().next().map_or(0, char::len_utf8).max(1);
    Some(ConfigError { line: line + 1, column: column + 1, message, range: spot..end.min(text.len().max(spot + 1)) })
}

/// The first error or missing node in document order that is not an allowed JSONC comma.
fn first_problem<'t, T: Text + ?Sized>(root: Node<'t>, text: &T, lang: Lang, lenient: bool) -> Option<Node<'t>> {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let flagged = node.is_error() || node.is_missing();
        if flagged && !(lenient && lang == Lang::Json && trailing_comma(node, text)) {
            return Some(node);
        }
        // A missing token of a hidden rule (TOML's line ending) is not a node of its own: its
        // parent is the one that has the error, and none of its children does.
        if !flagged && node.has_error() {
            let mut children = node.walk();
            let culprit = node.children(&mut children).any(|child| child.is_error() || child.is_missing() || child.has_error());
            if !culprit {
                return Some(node);
            }
        }
        if !flagged && node.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return None;
            }
        }
    }
}

/// A comma that closes a JSON object or array: an error node holding just that comma, or a
/// missing value after a comma.
fn trailing_comma<T: Text + ?Sized>(node: Node, text: &T) -> bool {
    let closes = node.next_sibling().is_none_or(|next| matches!(next.kind(), "}" | "]"));
    if node.is_error() {
        closes && text.slice(node.byte_range()).trim() == ","
    } else {
        node.is_missing() && closes && node.prev_sibling().is_some_and(|prev| prev.kind() == ",")
    }
}

/// Where to put the underline, and what to say.
fn describe<T: Text + ?Sized>(node: Node, text: &T, lang: Lang) -> (usize, String) {
    if !node.is_missing() && !node.is_error() {
        let message = if lang == Lang::Toml { "expected a new line" } else { "doesn't parse here" };
        return (after_last_token(text, node.end_byte()), message.to_string());
    }
    if node.is_missing() {
        let kind = node.kind();
        let message = match kind {
            "}" | "]" | ")" => format!("missing a closing {kind}"),
            "," => "expected a comma".to_string(),
            ":" => "expected a colon".to_string(),
            "=" => "expected an =".to_string(),
            "_line_ending_or_eof" => "expected a new line".to_string(),
            "\"" | "'" => "this quote is never closed".to_string(),
            _ if matches!(lang, Lang::Json) && kind != "string_content" || kind.ends_with("value") || kind == "number" => {
                if trailing_comma(node, text) { "trailing comma".to_string() } else { "expected a value".to_string() }
            }
            _ => format!("expected {}", kind.trim_matches('"').trim_start_matches('_').replace('_', " ")),
        };
        return (after_last_token(text, node.start_byte()), message);
    }

    // A bracket or quote left open in the error node: point at that opener.
    // A YAML error stops where the parse gave up; JSON and TOML errors that stop short of the
    // end of the file are about the text inside them.
    let at_end = text.slice(node.end_byte()..node.end_byte() + 4096).trim().is_empty();
    if lang == Lang::Yaml || at_end {
        if let Some((offset, what)) = unclosed(text, node.byte_range(), lang) {
            return (offset, what);
        }
    }

    let spot = node.start_byte();
    let own = text.slice(node.byte_range());
    if lang == Lang::Json || lang == Lang::Toml {
        if own.trim() == "," {
            return (spot, if node.next_sibling().is_none_or(|n| matches!(n.kind(), "}" | "]")) { "trailing comma" } else { "unexpected comma" }.to_string());
        }
    }
    if let Some(parent) = node.parent().filter(|p| lang == Lang::Json && matches!(p.kind(), "object" | "array")) {
        let named: Vec<Node> = {
            let mut cursor = node.walk();
            node.named_children(&mut cursor).collect()
        };
        let next = node.next_named_sibling();
        if let (Some(next), false) = (next, named.is_empty()) {
            return (next.start_byte(), "expected a comma".to_string());
        }
        if next.is_none() && named.len() >= 2 {
            let last = named[named.len() - 1];
            let word = if parent.kind() == "object" { "expected a colon" } else { "expected a comma" };
            return (last.start_byte(), word.to_string());
        }
    }
    if lang == Lang::Toml {
        let line_rest = text.slice(spot..spot + 512);
        let first = line_rest.lines().next().unwrap_or("");
        if let Some(eq) = first.find('=') {
            if first[eq + 1..].trim().is_empty() {
                return (spot + eq, "expected a value after =".to_string());
            }
        }
    }
    // A string left open on the line the parse stopped at.
    if lang != Lang::Yaml {
        let before = text.slice(spot.saturating_sub(512)..spot);
        let line_start = before.rfind('\n').map(|i| spot - (before.len() - i - 1));
        if let Some(start) = line_start.or((spot < 512).then_some(0)) {
            let rest = text.slice(spot..spot + 512);
            let end = spot + rest.find('\n').unwrap_or(rest.len());
            if let Some((offset, what)) = unclosed(text, start..end, lang).filter(|(_, what)| what.contains("quote")) {
                return (offset, what);
            }
        }
    }
    let (spot, token) = if lang == Lang::Yaml && node.end_byte() > node.start_byte() {
        let end = node.end_byte();
        let rest = text.slice(end..end + 40);
        let skipped = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        (end + skipped, rest[skipped..].to_string())
    } else {
        (spot, text.slice(spot..spot + 40))
    };
    let token: String = token.chars().take_while(|c| !c.is_whitespace()).take(12).collect();
    let message = if token.is_empty() { "unexpected end of line".to_string() } else { format!("unexpected {token:?}") };
    (if token.is_empty() { after_last_token(text, spot) } else { spot }, message)
}

/// The start of the last character before `offset` that isn't white space, so a problem found at
/// the end of the file or a line is shown on the text the user wrote.
fn after_last_token<T: Text + ?Sized>(text: &T, offset: usize) -> usize {
    let before = text.slice(offset.saturating_sub(256)..offset);
    let trimmed = before.trim_end();
    match trimmed.chars().next_back() {
        Some(last) => offset.saturating_sub(256).max(offset - before.len()) + trimmed.len() - last.len_utf8(),
        None => offset,
    }
}

/// The innermost bracket or the quote left open in `range` of the text, read the way the
/// language writes strings and comments, with what to say about it.
fn unclosed<T: Text + ?Sized>(text: &T, range: Range<usize>, lang: Lang) -> Option<(usize, String)> {
    if range.len() > SCAN_CAP {
        return None;
    }
    let source = text.slice(range.clone());
    let bytes = source.as_bytes();
    let mut stack: Vec<(usize, u8)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'#' if lang != Lang::Json => {
                // YAML starts a comment only after white space.
                if lang == Lang::Toml || i == 0 || bytes[i - 1].is_ascii_whitespace() {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
            }
            b'/' if lang == Lang::Json && bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if lang == Lang::Json && bytes.get(i + 1) == Some(&b'*') => {
                match source[i + 2..].find("*/") {
                    Some(end) => {
                        i += end + 4;
                        continue;
                    }
                    None => return Some((range.start + i, "this comment is never closed".to_string())),
                }
            }
            b'"' | b'\'' => {
                let opens_scalar = lang != Lang::Yaml || {
                    let before = source[..i].trim_end_matches([' ', '\t']);
                    before.is_empty() || before.ends_with(['\n', ':', '-', ',', '[', '{', '?'])
                };
                if opens_scalar && !(c == b'\'' && lang == Lang::Json) {
                    let triple = lang == Lang::Toml && bytes[i..].starts_with(&[c, c, c]);
                    let multiline = triple || lang == Lang::Yaml;
                    let mut j = i + if triple { 3 } else { 1 };
                    let mut closed = false;
                    while j < bytes.len() {
                        let d = bytes[j];
                        if d == b'\\' && c == b'"' {
                            j += 2;
                            continue;
                        }
                        if d == b'\n' && !multiline {
                            break;
                        }
                        if d == c {
                            if triple {
                                if bytes[j..].starts_with(&[c, c, c]) {
                                    j += 3;
                                    closed = true;
                                    break;
                                }
                            } else if lang == Lang::Yaml && c == b'\'' && bytes.get(j + 1) == Some(&b'\'') {
                                j += 2;
                                continue;
                            } else {
                                j += 1;
                                closed = true;
                                break;
                            }
                        }
                        j += 1;
                    }
                    if !closed {
                        return Some((range.start + i, "this quote is never closed".to_string()));
                    }
                    i = j;
                    continue;
                }
            }
            b'[' | b'{' | b'(' if lang != Lang::Yaml || c != b'(' => stack.push((i, c)),
            b']' | b'}' => match stack.pop() {
                Some((_, open)) if (open == b'[') == (c == b']') => {}
                _ => return None,
            },
            _ => {}
        }
        i += 1;
    }
    let (at, open) = stack.pop()?;
    let open = open as char;
    Some((range.start + at, format!("this {open} is never closed")))
}

/// What the check made of the editor's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Valid,
    Broken(ConfigError),
    /// The tree is behind the text (or there is none): no answer yet.
    Stale,
}

/// Reads the editor's current tree; never parses. Files over `MAX_HIGHLIGHT_BYTES` have no tree.
pub fn verdict(editor: &EditorState, lang: Lang, lenient: bool) -> Verdict {
    match editor.with_current_syntax_tree(|tree, text| {
        tree.downcast_ref::<Tree>().map(|tree| check(tree, text, lang, lenient))
    }) {
        Some(Some(None)) => Verdict::Valid,
        Some(Some(Some(error))) => Verdict::Broken(error),
        _ => Verdict::Stale,
    }
}

/// What an answer is for: the editor, its text revision, the language and whether JSON commas
/// are lenient. An answer is reused only on a full match.
type Key = (EntityId, u64, Option<Lang>, bool);

/// One editor's check: the debounce, the last answer and the underline. The status line holds
/// one and re-points it on a tab switch with `schedule`.
#[derive(Default)]
pub struct ConfigWatch {
    shared: Rc<RefCell<Shared>>,
    task: Option<Task<()>>,
    /// The key a pending check was started for.
    pending: Option<Key>,
}

#[derive(Default)]
struct Shared {
    /// The answer last landed, for the pill.
    error: Option<ConfigError>,
    /// What that answer is for.
    key: Option<Key>,
    /// The one collection of each editor, made when its first underline needs it.
    collections: HashMap<EntityId, TextDecorationCollection>,
    /// The editor that carries the underline now.
    underlined: Option<EntityId>,
}

impl ConfigWatch {
    /// The error to show in the pill, `None` while the file parses (or was not checked).
    pub fn error(&self) -> Option<ConfigError> {
        self.shared.borrow().error.clone()
    }

    /// The byte offset a click on the pill puts the caret at.
    pub fn jump_target(&self) -> Option<usize> {
        self.shared.borrow().error.as_ref().map(|error| error.range.start)
    }

    /// Call on every editor change and tab switch (not per frame; a caret move needs no call).
    /// `lang` is `Lang::from_name` of the file's language (`None` clears the pill and the
    /// underline). Does nothing while the answer for this editor, text revision, language and
    /// leniency is already in. Otherwise it waits `SETTLE` after the last call, then for the tree
    /// if it is behind (the editor notifies when a parse lands), before it sets the answer; the status line `T` is notified
    /// only when the answer changed. Returns whether the answer changed at once (a file with no
    /// config language, or too big).
    pub fn schedule<T: 'static>(&mut self, editor: &Entity<EditorState>, lang: Option<Lang>, lenient: bool, cx: &mut Context<T>) -> bool {
        let key = (editor.entity_id(), editor.read(cx).text_revision(), lang, lenient);
        if self.shared.borrow().key == Some(key) {
            return false;
        }
        let Some(checked) = lang.filter(|_| editor.read(cx).text().len() <= MAX_HIGHLIGHT_BYTES) else {
            self.task = None;
            self.pending = None;
            return Self::land_app(&self.shared, editor, None, key, cx);
        };
        if self.pending == Some(key) && self.task.is_some() {
            return false;
        }
        self.pending = Some(key);
        let shared = self.shared.clone();
        let editor = editor.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let changed = when_parsed(&editor, cx, |cx| {
                let state = editor.read(cx);
                let key = (editor.entity_id(), state.text_revision(), lang, lenient);
                match verdict(state, checked, lenient) {
                    Verdict::Stale => None,
                    Verdict::Valid => Some(Self::land_app(&shared, &editor, None, key, cx)),
                    Verdict::Broken(error) => Some(Self::land_app(&shared, &editor, Some(error), key, cx)),
                }
            })
            .await;
            if changed == Some(true) {
                let _ = this.update(cx, |_, cx| cx.notify());
            }
        }));
        false
    }

    /// Stores the answer and puts the underline where it belongs; whether the pill's answer
    /// changed. An underline left on another editor is cleared; an editor's collection is set
    /// only when what it holds differs from what is wanted (every set notifies the editor).
    fn land_app(shared: &Rc<RefCell<Shared>>, editor: &Entity<EditorState>, error: Option<ConfigError>, key: Key, cx: &mut App) -> bool {
        let id = editor.entity_id();
        let wanted = error.as_ref().map(|error| error.range.clone());
        let (changed, stale, collection) = {
            let mut inner = shared.borrow_mut();
            inner.key = Some(key);
            let changed = inner.error != error;
            inner.error = error;
            let stale = inner.underlined.filter(|other| *other != id).and_then(|other| inner.collections.get(&other).cloned());
            if stale.is_some() || inner.underlined.is_some_and(|other| other != id) {
                inner.underlined = None;
            }
            let collection = match (&wanted, inner.collections.get(&id)) {
                (_, Some(collection)) => Some(collection.clone()),
                (Some(_), None) => None,
                (None, None) => return changed,
            };
            if wanted.is_some() {
                inner.underlined = Some(id);
            } else if inner.underlined == Some(id) {
                inner.underlined = None;
            }
            (changed, stale, collection)
        };
        if let Some(stale) = stale {
            if !stale.get_ranges(cx).is_empty() {
                stale.clear(cx);
            }
        }
        let collection = match collection {
            Some(collection) => collection,
            None => {
                let collection = editor.update(cx, |state, cx| state.create_decorations_collection(Vec::new(), cx));
                shared.borrow_mut().collections.insert(id, collection.clone());
                // The collection goes with its editor, so a closed tab leaves nothing here.
                let hook = shared.clone();
                cx.observe_release(editor, move |_, _| {
                    let mut inner = hook.borrow_mut();
                    inner.collections.remove(&id);
                    if inner.underlined == Some(id) {
                        inner.underlined = None;
                    }
                })
                .detach();
                collection
            }
        };
        let held = collection.get_ranges(cx);
        match wanted {
            Some(range) if held != [range.clone()] => collection.set(vec![TextDecoration::new(range, wavy(cx))], cx),
            None if !held.is_empty() => collection.clear(cx),
            _ => {}
        }
        changed
    }
}

/// The pill's ink: the theme's warning amber, calmed.
pub fn pill_ink(cx: &App) -> Hsla {
    let mut color = cx.theme().warning;
    color.a *= 0.9;
    color
}

/// A click on the pill: puts the caret at `offset` and focuses the editor.
pub fn jump_to(editor: &Entity<EditorState>, offset: usize, window: &mut Window, cx: &mut App) {
    editor.update(cx, |state, cx| {
        let position = state.text().offset_to_position(offset.min(state.text().len()));
        state.set_cursor_position(position, window, cx);
    });
}

/// Edit > Transformations > Format JSON on `editor`: one Undo step, the caret kept where it was
/// (or at the end if the text got shorter). Refuses, changing nothing, when the file does not
/// parse (the pill shows why) or its tree is behind the text; returns whether it formatted.
pub fn format_json_in(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) -> bool {
    let formatted = editor.read(cx).with_current_syntax_tree(|tree, text| {
        let tree = tree.downcast_ref::<Tree>()?;
        let text = text.to_string();
        let indent = indent_for(&text);
        format_json(tree, &text, &indent).filter(|formatted| *formatted != text).map(|formatted| (text.len(), formatted))
    });
    let Some(Some((len, formatted))) = formatted else { return false };
    editor.update(cx, |state, cx| {
        let caret = state.cursor().min(formatted.len());
        state.replace_range(0..len, &formatted, caret, window, cx);
    });
    true
}

/// The indentation Format JSON writes: the file's own (tabs, or its step of spaces), two spaces
/// for a file with no indented line.
fn indent_for(text: &str) -> String {
    if !text.lines().take(1000).any(|line| line.starts_with([' ', '\t'])) {
        return "  ".to_string();
    }
    let tab = crate::indent_detect::detect(text);
    if tab.hard_tabs { "\t".to_string() } else { " ".repeat(tab.tab_size) }
}

/// The faint wavy underline: the warning amber at low strength, no colour or size change to the
/// text, so no run is re-shaped.
fn wavy(cx: &App) -> HighlightStyle {
    let mut color = cx.theme().warning;
    color.a *= 0.7;
    HighlightStyle {
        underline: Some(UnderlineStyle { thickness: px(1.0), color: Some(color), wavy: true }),
        ..Default::default()
    }
}

/// Re-indents JSON text with `indent`, one level per nesting, keeping every comment, string and
/// number exactly as written. `None` when the text does not parse as JSON (an unclosed bracket,
/// a stray character): Format JSON then refuses.
pub fn format_json(tree: &Tree, text: &str, indent: &str) -> Option<String> {
    if tree.root_node().has_error() {
        return None;
    }
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    format_node(tree.root_node(), text, indent, 0, &mut out);
    out.push('\n');
    Some(out)
}

fn format_node(node: Node, text: &str, indent: &str, depth: usize, out: &mut String) {
    match node.kind() {
        "document" => {
            let mut cursor = node.walk();
            let mut first = true;
            for child in node.children(&mut cursor) {
                if !first {
                    out.push('\n');
                }
                first = false;
                format_node(child, text, indent, 0, out);
            }
        }
        "object" | "array" => {
            let mut cursor = node.walk();
            let items: Vec<Node> = node.named_children(&mut cursor).collect();
            if items.is_empty() {
                out.push_str(if node.kind() == "object" { "{}" } else { "[]" });
                return;
            }
            out.push(if node.kind() == "object" { '{' } else { '[' });
            let last_value = items.iter().rposition(|item| item.kind() != "comment");
            for (index, item) in items.iter().enumerate() {
                out.push('\n');
                push_indent(out, indent, depth + 1);
                format_node(*item, text, indent, depth + 1, out);
                if item.kind() != "comment" && Some(index) != last_value {
                    out.push(',');
                }
            }
            out.push('\n');
            push_indent(out, indent, depth);
            out.push(if node.kind() == "object" { '}' } else { ']' });
        }
        "pair" => {
            let key_id = node.child_by_field_name("key").map(|key| key.id());
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                let space = |out: &mut String| {
                    if !out.ends_with(char::is_whitespace) {
                        out.push(' ');
                    }
                };
                if child.kind() == ":" {
                    out.push(':');
                } else if child.kind() == "comment" {
                    space(out);
                    let comment = &text[child.byte_range()];
                    out.push_str(comment);
                    if comment.starts_with("//") {
                        out.push('\n');
                        push_indent(out, indent, depth + 1);
                    }
                } else if Some(child.id()) == key_id {
                    out.push_str(&text[child.byte_range()]);
                } else if child.is_named() {
                    space(out);
                    format_node(child, text, indent, depth, out);
                }
            }
        }
        _ => out.push_str(&text[node.byte_range()]),
    }
}

fn push_indent(out: &mut String, indent: &str, depth: usize) {
    for _ in 0..depth {
        out.push_str(indent);
    }
}

#[cfg(test)]
mod tests {
    use super::{Lang, Tree, check, format_json, lenient_json};
    use tree_sitter::{Language, Parser};

    fn parse(lang: Lang, text: &str) -> Tree {
        let language: Language = match lang {
            Lang::Json => tree_sitter_json::LANGUAGE.into(),
            Lang::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
        };
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(text, None).unwrap()
    }

    /// "line:column message" of the problem, or None.
    fn problem(lang: Lang, text: &str, lenient: bool) -> Option<String> {
        check(&parse(lang, text), text, lang, lenient).map(|e| format!("{}:{} {}", e.line, e.column, e.message))
    }

    #[test]
    fn json_missing_comma_and_brackets() {
        assert_eq!(problem(Lang::Json, "{\n  \"a\": 1\n  \"b\": 2\n}", false).as_deref(), Some("3:3 expected a comma"));
        assert_eq!(problem(Lang::Json, "[1 2]", false).as_deref(), Some("1:4 expected a comma"));
        assert_eq!(problem(Lang::Json, "{\"a\": 1\n", false).as_deref(), Some("1:7 missing a closing }"));
        assert_eq!(problem(Lang::Json, "{\"a\": [1, 2}", false).as_deref(), Some("1:11 missing a closing ]"));
        assert_eq!(problem(Lang::Json, "{\"a\": }", false).as_deref(), Some("1:5 expected a value"));
        assert_eq!(problem(Lang::Json, "{\"a\" 1}", false).as_deref(), Some("1:6 expected a colon"));
        assert_eq!(problem(Lang::Json, "{a: 1}", false).as_deref(), Some("1:1 unexpected \"{a:\""));
    }

    #[test]
    fn json_comments_and_trailing_commas() {
        assert_eq!(problem(Lang::Json, "// note\n{\"a\": 1 /* x */}", false), None);
        assert_eq!(problem(Lang::Json, "{\"a\": 1,}", false).as_deref(), Some("1:8 trailing comma"));
        assert_eq!(problem(Lang::Json, "[1, 2,]", false).as_deref(), Some("1:6 trailing comma"));
        assert_eq!(problem(Lang::Json, "{\"a\": 1,}", true), None);
        assert_eq!(problem(Lang::Json, "[1, 2,]", true), None);
        // A real error after an allowed comma still shows.
        assert!(problem(Lang::Json, "{\"a\": [1,], \"b\": }", true).is_some());
        assert!(lenient_json("tsconfig.json") && lenient_json("x.jsonc") && !lenient_json("package.json"));
    }

    #[test]
    fn toml_dotted_keys_and_tables_are_fine() {
        assert_eq!(problem(Lang::Toml, "a.b.c = 1\n\"x y\".z = 2\n[t.u]\nk = [1, 2]\n[[w]]\nq = { r = 1 }\n", false), None);
        assert_eq!(problem(Lang::Toml, "s = \"\"\"\nmulti\n\"\"\"\n", false), None);
    }

    #[test]
    fn toml_problems() {
        assert_eq!(problem(Lang::Toml, "a = \nb = 1\n", false).as_deref(), Some("1:3 expected a value after ="));
        assert_eq!(problem(Lang::Toml, "a = 1 b = 2\n", false).as_deref(), Some("1:5 expected a new line"));
        assert_eq!(problem(Lang::Toml, "a = [1, 2\n", false).as_deref(), Some("1:5 this [ is never closed"));
        assert_eq!(problem(Lang::Toml, "[x\na = 1\n", false).as_deref(), Some("1:1 this [ is never closed"));
        assert_eq!(problem(Lang::Toml, "a = \"x\nb = 1\n", false).as_deref(), Some("1:5 this quote is never closed"));
    }

    #[test]
    fn yaml_multi_document_and_valid_forms_do_not_cry_wolf() {
        assert_eq!(problem(Lang::Yaml, "a: 1\n---\nb: 2\n---\nc:\n  - d\n  - e: [1, 2]\n  - {f: g}\n", false), None);
        assert_eq!(problem(Lang::Yaml, "a: &x 1\nb: *x\nc: !!str 3\nd: |\n  text\n  more\ne: >-\n  folded\n? complex\n: v\n", false), None);
        assert_eq!(problem(Lang::Yaml, "# only a comment\n", false), None);
        assert_eq!(problem(Lang::Yaml, "", false), None);
    }

    #[test]
    fn yaml_problems() {
        assert_eq!(problem(Lang::Yaml, "a: [1, 2\nb: 3\n", false).as_deref(), Some("1:4 this [ is never closed"));
        assert_eq!(problem(Lang::Yaml, "a: {b: 1\n", false).as_deref(), Some("1:4 this { is never closed"));
        assert_eq!(problem(Lang::Yaml, "a: 'x\nb: 1\n", false).as_deref(), Some("1:4 this quote is never closed"));
        assert_eq!(problem(Lang::Yaml, "a: b: c\n", false).as_deref(), Some("1:5 unexpected \":\""));
        assert_eq!(problem(Lang::Yaml, "it's: fine\nb: don't\n", false), None);
    }

    #[test]
    fn format_json_reindents_and_keeps_comments() {
        let text = "// top\n{\"a\":1,\"b\":[1,2,{\"c\":null}],\"d\":{},\"e\":[ ]  /* c */}";
        let formatted = format_json(&parse(Lang::Json, text), text, "  ").unwrap();
        assert_eq!(
            formatted,
            "// top\n{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2,\n    {\n      \"c\": null\n    }\n  ],\n  \"d\": {},\n  \"e\": []\n  /* c */\n}\n"
        );
        // Formatting twice changes nothing.
        assert_eq!(format_json(&parse(Lang::Json, &formatted), &formatted, "  ").unwrap(), formatted);
        assert_eq!(format_json(&parse(Lang::Json, "{\"a\": 1"), "{\"a\": 1", "  "), None);
        // A comment between the key and the value stays.
        let text = "{\"a\" /* k */ : /* v */ 1, \"b\": // line\n 2}";
        let formatted = format_json(&parse(Lang::Json, text), text, "  ").unwrap();
        assert!(formatted.contains("/* k */") && formatted.contains("/* v */") && formatted.contains("// line"), "{formatted}");
        assert_eq!(format_json(&parse(Lang::Json, &formatted), &formatted, "  ").unwrap(), formatted);
    }
}
