//! Plain style check: View > Style Check underlines, very faintly, the words and phrases of a list
//! the writer keeps (filler such as "very" and "really") and every sentence over a length in words
//! (`style_sentence_words`, 30 at first). Markdown and plain text only; code files are never
//! touched. Off at first, remembered as the `style_check` setting.
//!
//! It is a text-style layer on the shared mark driver (`marks.rs`), after the focus dim. The
//! underline is the tertiary ink at hairline weight, so no run is re-shaped by size or weight. The
//! scan works on the driver's window of bytes only, off the main thread through `Debounce` (about
//! 300 ms after the last edit, dropped when the text moved on). A word inside a long sentence is
//! not marked on its own: the sentence mark already runs under it, and the two never overlap.
//!
//! The word being typed (one that touches the caret) carries no mark: it is dropped from the list
//! when the caret arrives, and at once after an edit from the marks the editor already holds.
//!
//! The list is a plain text file beside the settings file (`style-words.txt`): one word or phrase
//! per line, `#` starts a comment, case ignored. It is created from `SEED` the first time the check
//! is on (not by a launch that must not write), and read again when the file changes (the settings
//! watcher's folder watch, started while the check is on). A file that can't be read leaves the
//! seed list in force.
//!
//! Sentences come from the same breaker Focus uses (`focus::sentence_at`).

use std::{collections::HashMap, ops::Range, path::PathBuf, sync::Arc, time::Duration};

use gpui_kit::component::input::{EditorState, Rope, TextDecoration};
use gpui_kit::*;

use crate::{
    focus::sentence_at,
    marks::{self, Debounce, MarkLayer, TextSet, View},
    settings, theme, tokens,
};

/// What the list file holds when it is first made.
const SEED: &str = "# Words and phrases the Style Check underlines, one per line.\n\
# A line starting with # is a note. Case doesn't matter.\n\
delve\ncrucial\npivotal\ntapestry\nrobust\nseamless\nrealm\nuseful\nworth\nhonestly\nleverage\nutilize\nvery\nreally\njust\nactually\nbasically\n";

/// How long after the last edit the scan starts.
const WAIT: Duration = Duration::from_millis(300);

/// The most of the window one scan reads (a window over one huge line stays cheap).
const SCAN_BYTES: usize = 64 * 1024;

/// The first look at a sentence reads this far; a sentence that reaches the end of it is looked
/// at again with twice as much.
const CHUNK: usize = 4 * 1024;

/// The list: each entry is its words, lowercase, found by its first word.
#[derive(Default)]
pub struct Words {
    by_first: HashMap<String, Vec<Vec<String>>>,
}

impl Words {
    /// One word or phrase per line; blank lines and `#` lines are skipped.
    fn parse(text: &str) -> Self {
        let mut by_first: HashMap<String, Vec<Vec<String>>> = HashMap::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let entry: Vec<String> = line.split_whitespace().map(str::to_lowercase).collect();
            by_first.entry(entry[0].clone()).or_default().push(entry);
        }
        for entries in by_first.values_mut() {
            // The longest phrase first, so "in order to" wins over "in".
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.len()));
        }
        Self { by_first }
    }
}

/// What a mark says: a listed word, or a sentence of this many words.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mark {
    Word,
    Sentence(usize),
}

/// What one scan found: marks in absolute bytes, each a listed word or a long sentence, for the window `bytes` of the text at revision `rev`, under list and limit `version`.
struct Found {
    rev: u64,
    bytes: Range<usize>,
    version: u64,
    marks: Vec<(Range<usize>, Mark)>,
}

/// The one list every layer shares, made while the check is on: parsed once, followed by one
/// watcher, and dropped (watcher and threads with it) when the check goes off.
struct Shared {
    path: PathBuf,
    words: Arc<Words>,
    /// The editors to wake when the list changes.
    editors: Vec<WeakEntity<EditorState>>,
    _watching: Option<Task<()>>,
}

impl Global for Shared {}

/// The style check layer of one editor.
pub struct Layer {
    on: bool,
    limit: usize,
    words: Arc<Words>,
    /// The list file's path, once read.
    path: Option<PathBuf>,
    /// Counts changes of the list or the limit, so an older scan is not taken for current.
    version: u64,
    set: TextSet,
    /// What each decoration of the set list marks, in order.
    kinds: Vec<Mark>,
    holds: bool,
    scan: Debounce,
    found: Option<Found>,
    /// The (revision, window, version) a scan is waiting or running for.
    pending: Option<(u64, Range<usize>, u64)>,
    _settings: Subscription,
}

impl Layer {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let weak = editor.downgrade();
        let subscription = cx.observe_global::<settings::Store>(move |cx| {
            let Some(editor) = weak.upgrade() else { return };
            let (on, limit) = {
                let editor_settings = &settings::get(cx).editor;
                (editor_settings.style_check, editor_settings.style_sentence_words.max(1))
            };
            marks::with_layer(&editor, cx, |layer: &mut Layer| {
                let changed = layer.on != on || layer.limit != limit;
                layer.on = on;
                layer.limit = limit;
                if changed {
                    layer.version += 1;
                }
                if !on {
                    layer.path = None;
                }
                ((), changed)
            });
            if !on && cx.has_global::<Shared>() {
                // The list and its watcher end with the check, whether or not a layer still runs.
                cx.remove_global::<Shared>();
            }
        });
        let editor_settings = &settings::get(cx).editor;
        Self {
            on: editor_settings.style_check,
            limit: editor_settings.style_sentence_words.max(1),
            words: Arc::new(Words::default()),
            path: None,
            version: 0,
            set: TextSet::new(editor, cx),
            kinds: Vec::new(),
            holds: false,
            scan: Debounce::default(),
            found: None,
            pending: None,
            _settings: subscription,
        }
    }

    /// Takes the shared list (made, read and followed by the first layer to ask) and joins the
    /// editors the watcher tells.
    fn load(&mut self, editor: &Entity<EditorState>, cx: &mut App) {
        if !cx.has_global::<Shared>() {
            let (path, may_write) = settings::style_words_file(cx);
            if may_write {
                seed_words_file(&path);
            }
            let words = Arc::new(read_words(&path));
            let watching = crate::settings_watch::start(&path).map(|(watch, mut events)| {
                let path = path.clone();
                cx.spawn(async move |cx| {
                    let _watch = watch;
                    while events.recv().await.is_ok() {
                        let Some((words, editors)) = cx.update(|cx| {
                            let words = Arc::new(read_words(&path));
                            if !cx.has_global::<Shared>() {
                                return None;
                            }
                            let shared = cx.global_mut::<Shared>();
                            shared.words = words.clone();
                            shared.editors.retain(|editor| editor.upgrade().is_some());
                            Some((words, shared.editors.iter().filter_map(|editor| editor.upgrade()).collect::<Vec<_>>()))
                        }) else {
                            return;
                        };
                        cx.update(|cx| {
                            for editor in editors {
                                marks::with_layer(&editor, cx, |layer: &mut Layer| {
                                    layer.reload(&words);
                                    ((), true)
                                });
                            }
                        });
                    }
                })
            });
            cx.set_global(Shared { path, words, editors: Vec::new(), _watching: watching });
        }
        let shared = cx.global_mut::<Shared>();
        shared.editors.retain(|held| held.upgrade().is_some());
        let id = editor.entity_id();
        if !shared.editors.iter().any(|held| held.entity_id() == id) {
            shared.editors.push(editor.downgrade());
        }
        self.words = shared.words.clone();
        self.path = Some(shared.path.clone());
        self.version += 1;
    }

    /// The list file changed: take the new shared list.
    fn reload(&mut self, words: &Arc<Words>) {
        if self.path.is_some() {
            self.words = words.clone();
            self.version += 1;
        }
    }

    /// Takes every mark away and lets go of what the layer holds.
    fn clear(&mut self, cx: &mut App) {
        self.scan.cancel();
        self.found = None;
        self.pending = None;
        self.kinds.clear();
        self.set.set_if_changed(Vec::new(), cx);
        self.holds = false;
    }
}

/// Makes the word list file from `SEED` when it isn't there; a file that exists is never touched.
pub(crate) fn seed_words_file(path: &std::path::Path) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        let _ = file.write_all(SEED.as_bytes());
    }
}

fn read_words(path: &PathBuf) -> Words {
    Words::parse(&std::fs::read_to_string(path).unwrap_or_else(|_| SEED.to_string()))
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        self.on || self.holds
    }

    fn release(&mut self) {
        self.found = None;
    }

    fn held(&self) -> usize {
        self.found.as_ref().map_or(0, |found| found.marks.len())
    }

    fn hover(&self, offset: usize, cx: &App) -> Option<SharedString> {
        if !self.on {
            return None;
        }
        let held = self.set.ranges(cx);
        if held.len() != self.kinds.len() {
            return None;
        }
        let (_, kind) = held.into_iter().zip(self.kinds.iter()).find(|(range, _)| range.start <= offset && offset < range.end)?;
        Some(match kind {
            Mark::Word => "On your list".into(),
            Mark::Sentence(count) => format!("Sentence is {count} words").into(),
        })
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        if !self.on {
            self.path = None;
            self.clear(cx);
            return;
        }
        if !view.prose() {
            self.clear(cx);
            return;
        }
        if self.path.is_none() {
            self.load(view.editor, cx);
        }
        let caret = view.key.caret;
        let current = self.found.as_ref().is_some_and(|found| found.rev == view.key.rev && found.bytes == view.bytes && found.version == self.version);
        if current {
            let found = self.found.as_ref().expect("checked just above");
            let shown: Vec<(Range<usize>, Mark)> = found.marks.iter().filter(|(range, word)| !(*word == Mark::Word && touches(range, caret))).cloned().collect();
            self.apply(shown, cx);
            return;
        }
        // Waiting for a scan: the marks the editor holds move with the edits; the one at the caret goes now.
        let held: Vec<Range<usize>> = self.set.held(cx).iter().map(|decoration| decoration.range.clone()).collect();
        if held.len() == self.kinds.len() && held.iter().all(|range| range.start != usize::MAX) {
            let kept: Vec<(Range<usize>, Mark)> = held.into_iter().zip(self.kinds.iter().copied()).filter(|(range, word)| !(*word == Mark::Word && touches(range, caret))).collect();
            self.apply(kept, cx);
        }
        let want = (view.key.rev, view.bytes.clone(), self.version);
        if self.pending.as_ref() == Some(&want) {
            return;
        }
        self.pending = Some(want);
        let (words, limit, bytes, rev, version) = (self.words.clone(), self.limit, view.bytes.clone(), view.key.rev, self.version);
        let window = bytes.clone();
        self.scan.run(
            view,
            WAIT,
            move |text: Rope| {
                let end = window.end.min(window.start + SCAN_BYTES);
                let mut end = end;
                while end > window.start && !text.is_char_boundary(end) {
                    end -= 1;
                }
                let found = scan(&text.slice(window.start..end).to_string(), &words, limit);
                found.into_iter().map(|(range, word)| (window.start + range.start..window.start + range.end, word)).collect::<Vec<_>>()
            },
            {
                let editor = view.editor.clone();
                move |found, cx| {
                    marks::with_layer(&editor, cx, |layer: &mut Layer| {
                        layer.found = Some(Found { rev, bytes, version, marks: found });
                        layer.pending = None;
                        ((), true)
                    });
                }
            },
            cx,
        );
    }
}

impl Layer {
    /// Sets `marks` as the underlines, unless the editor already shows them.
    fn apply(&mut self, marks: Vec<(Range<usize>, Mark)>, cx: &mut App) {
        let style = HighlightStyle {
            underline: Some(UnderlineStyle { thickness: px(1.0), color: Some(theme::ink(tokens::ALPHA_TERTIARY, cx)), wavy: false }),
            ..Default::default()
        };
        let list: Vec<TextDecoration> = marks.iter().map(|(range, _)| TextDecoration::new(range.clone(), style)).collect();
        self.set.set_if_changed(list, cx);
        self.kinds = marks.into_iter().filter(|(range, _)| !range.is_empty()).map(|(_, kind)| kind).collect();
        self.holds = !self.kinds.is_empty();
    }
}

/// The caret is in or at the end of `range`: the word is being typed.
fn touches(range: &Range<usize>, caret: usize) -> bool {
    range.start <= caret && caret <= range.end
}

/// The marks of `text`, as byte ranges in it sorted by start: each listed word or phrase (`true`)
/// outside a long sentence, and each sentence of more than `limit` words (`false`). Fenced code
/// blocks are skipped.
fn scan(text: &str, words: &Words, limit: usize) -> Vec<(Range<usize>, Mark)> {
    let text = blank_fences(text);
    let mut long: Vec<(Range<usize>, usize)> = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let found = next_sentence(&text[at..]);
        if found.end == 0 {
            break;
        }
        let sentence = &text[at + found.start..at + found.end];
        let trimmed = sentence.trim();
        let count = trimmed.split_whitespace().filter(|piece| piece.chars().any(char::is_alphanumeric)).count();
        if count > limit {
            let start = at + found.start + (sentence.len() - sentence.trim_start().len());
            long.push((start..start + trimmed.len(), count));
        }
        at += found.end;
    }
    let mut marks: Vec<(Range<usize>, Mark)> = long.iter().cloned().map(|(range, count)| (range, Mark::Sentence(count))).collect();
    marks.extend(listed(&text, words).into_iter().filter(|word| !long.iter().any(|(range, _)| range.start <= word.start && word.end <= range.end)).map(|range| (range, Mark::Word)));
    marks.sort_by_key(|(range, _)| range.start);
    marks
}

/// The sentence at the start of `rest`, read from a chunk that doubles while the sentence
/// reaches its end.
fn next_sentence(rest: &str) -> Range<usize> {
    let mut size = CHUNK;
    loop {
        let mut end = size.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let found = sentence_at(&rest[..end], 0);
        if found.end < end || end == rest.len() {
            return found;
        }
        size *= 2;
    }
}

/// `text` with the lines of fenced code blocks (and their fences) turned to spaces, the same
/// length, so offsets stay.
fn blank_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let fence = body.trim_start().starts_with("```") || body.trim_start().starts_with("~~~");
        if fence {
            fenced = !fenced;
        }
        if fence || fenced {
            out.extend(body.chars().map(|c| if c.is_whitespace() { c } else { ' ' }));
            out.push_str(&line[body.len()..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Where the list's words and phrases are in `text`, whole words only, case ignored.
fn listed(text: &str, words: &Words) -> Vec<Range<usize>> {
    if words.by_first.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut tokens: Vec<Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &(at, c)) in chars.iter().enumerate() {
        let inside = c.is_alphanumeric() || (matches!(c, '\'' | '\u{2019}') && start.is_some() && chars.get(i + 1).is_some_and(|(_, next)| next.is_alphanumeric()));
        match (inside, start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                tokens.push(from..at);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        tokens.push(from..text.len());
    }
    let lower: Vec<String> = tokens.iter().map(|token| text[token.clone()].to_lowercase()).collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let hit = words.by_first.get(&lower[i]).and_then(|entries| {
            entries.iter().find(|entry| {
                i + entry.len() <= tokens.len()
                    && entry.iter().enumerate().all(|(k, word)| lower[i + k] == *word && (k == 0 || text[tokens[i + k - 1].end..tokens[i + k].start].chars().all(char::is_whitespace)))
            })
        });
        match hit {
            Some(entry) => {
                found.push(tokens[i].start..tokens[i + entry.len() - 1].end);
                i += entry.len();
            }
            None => i += 1,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{Mark, Words, blank_fences, listed, scan};

    #[test]
    fn list_skips_comments_and_matches_whole_words_and_phrases() {
        let words = Words::parse("# note\n\nVery\nin order to\n");
        let text = "A very good, Very! overly. We did it in order   to win.";
        let found: Vec<&str> = listed(text, &words).into_iter().map(|range| &text[range]).collect();
        assert_eq!(found, ["very", "Very", "in order   to"]);
    }

    #[test]
    fn long_sentences_swallow_their_listed_words_and_fences_are_skipped() {
        let words = Words::parse("really\n");
        let text = "It is really short. One two three four five six seven eight nine ten really.\n```\nreally really\n```\n";
        let marks = scan(text, &words, 5);
        let shown: Vec<(&str, Mark)> = marks.iter().map(|(range, mark)| (&text[range.clone()], *mark)).collect();
        assert_eq!(shown, [("really", Mark::Word), ("One two three four five six seven eight nine ten really.", Mark::Sentence(11))]);
        assert_eq!(blank_fences(text).len(), text.len());
    }
}
