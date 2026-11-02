//! Parts of speech: View > Highlight tints nouns, verbs, adjectives, adverbs or conjunctions in
//! Markdown and text files, one soft hue per class, so the shape of a sentence shows while you write.
//!
//! It is a text-style layer on the shared mark driver (`marks.rs`), right after the focus dim: the
//! tint is `color`, the dim is `fade_out`, so a dimmed word keeps its tint dimmed. Code files and
//! fenced code blocks are never touched, and inline code spans and link targets are blanked before
//! tagging.
//!
//! Tagging is `NLTagger` with the lexical class scheme. NaturalLanguage is `dlopen`ed the first
//! time a paragraph needs tagging (never at launch, never while no class is on), and every job
//! makes its own tagger, since a tagger isn't documented as safe to share. A job runs off the main
//! thread behind the driver's debounce, over the window the driver hands over (the screen and a
//! screen each way), one paragraph (lines up to a blank one) at a time. Answers are cached by the
//! paragraph's text hash, for all five classes at once, so switching a class or scrolling back costs
//! nothing, and at most `MAX_TINTS` words are tinted in all.
//!
//! The editor keeps the marks in step with an edit until the next answer lands, so typing doesn't
//! blink. The word at the caret is left out of the answer that follows an edit, so a half-typed word
//! isn't tinted as the wrong class; it takes its tint when the caret moves on.

use std::{
    collections::HashMap,
    ffi::{c_char, c_void},
    hash::{DefaultHasher, Hash as _, Hasher as _},
    ops::Range,
    sync::{Arc, Mutex, Once},
    time::Duration,
};

use gpui_kit::component::input::{EditorState, Rope, RopeExt as _, TextDecoration};
use gpui_kit::*;
use objc2::{
    Encode, Encoding, RefEncode, class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyClass, AnyObject},
};

use crate::{
    marks::{self, Debounce, MarkLayer, TextSet, View},
    settings, theme, tokens,
};

/// How long the text must rest before a paragraph is tagged.
const WAIT: Duration = Duration::from_millis(300);

/// The most words tinted in one editor, whatever the file.
const MAX_TINTS: usize = 3000;

/// A paragraph is cut after this many bytes and goes on in the next one; a single line longer than
/// this isn't tagged (a minified line of prose, a pasted dump).
const PARAGRAPH_MAX: usize = 16 * 1024;

/// How many rows above the window a Markdown fence scan starts.
const FENCE_LOOKBACK: usize = 3000;

/// The most paragraphs kept answered; a full cache is emptied before the next answer is kept.
const CACHE_MAX: usize = 4096;

/// A class of words View > Highlight can tint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Noun,
    Verb,
    Adjective,
    Adverb,
    Conjunction,
}

impl Class {
    pub const ALL: [Class; 5] = [Class::Noun, Class::Verb, Class::Adjective, Class::Adverb, Class::Conjunction];

    /// The menu item's words.
    pub fn title(self) -> &'static str {
        ["Nouns", "Verbs", "Adjectives", "Adverbs", "Conjunctions"][self as usize]
    }

    /// The name the settings file lists the class by.
    fn key(self) -> &'static str {
        ["nouns", "verbs", "adjectives", "adverbs", "conjunctions"][self as usize]
    }

    /// `NLTag`'s name for the class in the lexical class scheme.
    fn of_tag(tag: &str) -> Option<Class> {
        Some(match tag {
            "Noun" => Class::Noun,
            "Verb" => Class::Verb,
            "Adjective" => Class::Adjective,
            "Adverb" => Class::Adverb,
            "Conjunction" => Class::Conjunction,
            _ => return None,
        })
    }

    fn tint(self, light: bool) -> Hsla {
        rgb(if light { tokens::POS_LIGHT } else { tokens::POS_DARK }[self as usize]).into()
    }
}

/// The classes that are on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Classes(u8);

impl Classes {
    /// The classes a settings list names; a name that isn't one is ignored.
    pub fn of(names: &[String]) -> Self {
        Self(Class::ALL.iter().filter(|class| names.iter().any(|name| name == class.key())).fold(0, |bits, class| bits | 1 << *class as u8))
    }

    pub fn has(self, class: Class) -> bool {
        self.0 & 1 << class as u8 != 0
    }

    fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// View > Highlight's item for one class: flips it in the setting (written once).
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct ToggleHighlight {
    pub class: Class,
}

/// Wires the View > Highlight items.
pub fn install(cx: &mut App) {
    cx.on_action(|action: &ToggleHighlight, cx| {
        let key = action.class.key();
        cx.defer(move |cx| {
            settings::update_later(cx, |settings| {
                let list = &mut settings.editor.highlight;
                if let Some(at) = list.iter().position(|name| name == key) {
                    list.remove(at);
                } else {
                    list.push(key.to_string());
                }
            });
            crate::menu::refresh(cx);
        });
    });
}

/// One tagged word: its bytes inside its paragraph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Word {
    start: u32,
    end: u32,
    class: Class,
}

type Cache = Arc<Mutex<HashMap<u64, Arc<[Word]>>>>;

/// What a run was asked: the text revision, the bytes, the classes, Markdown, light, and the caret
/// whose word it left out (`Some` only after an edit).
type Ran = (u64, Range<usize>, Classes, bool, bool, Option<usize>);

/// The parts of speech layer of one editor.
pub struct Layer {
    classes: Classes,
    light: bool,
    set: std::rc::Rc<std::cell::RefCell<TextSet>>,
    job: Debounce,
    cache: Cache,
    /// The text revision the last wake saw: a wake at another one follows an edit.
    rev: u64,
    /// The collection holds tints that have to be cleared when the layer goes off.
    holds: bool,
    /// The inputs of the last run that finished, to skip a wake that changes none of them.
    ran: std::rc::Rc<std::cell::RefCell<Option<Ran>>>,
    _watch: [Subscription; 2],
}

impl Layer {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let weak = editor.downgrade();
        // The setting (View > Highlight, or the settings file edited by hand) and the theme (light
        // or dark) reach the layer here; neither does anything while a class isn't on.
        let settings_watch = {
            let weak = weak.clone();
            cx.observe_global::<settings::Store>(move |cx| {
                let now = Classes::of(&settings::get(cx).editor.highlight);
                if let Some(editor) = weak.upgrade() {
                    marks::with_layer(&editor, cx, |layer: &mut Layer| ((), std::mem::replace(&mut layer.classes, now) != now));
                }
            })
        };
        let theme_watch = cx.observe_global::<component::Theme>(move |cx| {
            let now = theme::is_light(cx);
            if let Some(editor) = weak.upgrade() {
                marks::with_layer(&editor, cx, |layer: &mut Layer| ((), layer.classes != Classes::default() && std::mem::replace(&mut layer.light, now) != now));
            }
        });
        Self {
            classes: Classes::of(&settings::get(cx).editor.highlight),
            light: theme::is_light(cx),
            set: std::rc::Rc::new(std::cell::RefCell::new(TextSet::new(editor, cx))),
            job: Debounce::default(),
            cache: Cache::default(),
            rev: 0,
            holds: false,
            ran: Default::default(),
            _watch: [settings_watch, theme_watch],
        }
    }
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        !self.classes.is_empty() || self.holds
    }

    fn release(&mut self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = HashMap::new();
        }
    }

    fn held(&self) -> usize {
        self.cache.lock().map_or(0, |cache| cache.values().map(|words| words.len()).sum())
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        let edited = std::mem::replace(&mut self.rev, view.key.rev) != view.key.rev && self.holds;
        if self.classes.is_empty() || !view.prose() || view.text.len() > crate::language::MAX_HIGHLIGHT_BYTES {
            self.job.cancel();
            self.set.borrow_mut().set_if_changed(Vec::new(), cx);
            self.holds = false;
            *self.ran.borrow_mut() = None;
            return;
        }
        self.holds = true;
        self.light = theme::is_light(cx);
        let (classes, light, markdown, cache, set) = (self.classes, self.light, view.lang == "markdown", self.cache.clone(), self.set.clone());
        let (bytes, caret) = (view.bytes.clone(), edited.then_some(view.key.caret));
        let ran: Ran = (view.key.rev, bytes.clone(), classes, markdown, light, caret);
        // Only the caret or selection moved since a finished run that left no word out: the tints are
        // the same, so no task, no wait and no read of the window.
        if unchanged(self.ran.borrow().as_ref(), &ran) {
            return;
        }
        let finished = self.ran.clone();
        self.job.run(
            view,
            WAIT,
            move |text| {
                let mut tagger = None;
                tinted(&text, bytes, markdown, classes, caret, &cache, &mut |paragraph: &str| tagger.get_or_insert_with(Tagger::open).as_ref().map_or_else(Vec::new, |tagger| tagger.tag(paragraph)))
            },
            move |words, cx| {
                let list = words.into_iter().map(|(range, class)| TextDecoration::new(range, HighlightStyle { color: Some(class.tint(light)), ..Default::default() })).collect();
                set.borrow_mut().set_if_changed(list, cx);
                *finished.borrow_mut() = Some(ran);
            },
            cx,
        );
    }
}

/// A wake asking `now` follows a finished run `finished` that left no word out and changes none of its
/// inputs: the caret or the selection moved, and the tints stay.
fn unchanged(finished: Option<&Ran>, now: &Ran) -> bool {
    now.5.is_none() && finished == Some(now)
}

/// The tinted words of the prose in `bytes`: those of an enabled class, in order, at most `MAX_TINTS`,
/// leaving out a word the caret (`Some` after an edit) touches. `tag` answers paragraphs the cache
/// hasn't seen.
fn tinted(text: &Rope, bytes: Range<usize>, markdown: bool, classes: Classes, caret: Option<usize>, cache: &Cache, tag: &mut dyn FnMut(&str) -> Vec<Word>) -> Vec<(Range<usize>, Class)> {
    let mut out = Vec::new();
    for paragraph in paragraphs(text, bytes, markdown) {
        let mut hasher = DefaultHasher::new();
        paragraph.text.hash(&mut hasher);
        let hash = hasher.finish();
        let known = cache.lock().ok().and_then(|cache| cache.get(&hash).cloned());
        let words = known.unwrap_or_else(|| {
            let words: Arc<[Word]> = tag(&paragraph.text).into();
            if let Ok(mut cache) = cache.lock() {
                if cache.len() >= CACHE_MAX {
                    cache.clear();
                }
                cache.insert(hash, words.clone());
            }
            words
        });
        for word in words.iter().filter(|word| classes.has(word.class)) {
            let range = paragraph.at + word.start as usize..paragraph.at + word.end as usize;
            if caret.is_some_and(|caret| range.start <= caret && caret <= range.end) {
                continue;
            }
            out.push((range, word.class));
            if out.len() >= MAX_TINTS {
                return out;
            }
        }
    }
    out
}

/// Prose to tag: where it starts in the text, and its bytes (the lines as they are, with code
/// spans and link targets blanked, so every byte keeps its place).
#[derive(Debug, PartialEq)]
struct Paragraph {
    at: usize,
    text: String,
}

/// The paragraphs (lines up to a blank one) inside `bytes`. In Markdown a fenced block is no prose
/// and ends the paragraph before it; the fence state is read from the top of the file, since the
/// window may begin inside a block.
fn paragraphs(text: &Rope, bytes: Range<usize>, markdown: bool) -> Vec<Paragraph> {
    if text.len() == 0 || bytes.is_empty() {
        return Vec::new();
    }
    let total = text.lines_len();
    let first = text.offset_to_point(bytes.start.min(text.len())).row;
    let last = text.offset_to_point(bytes.end.min(text.len())).row.min(total - 1);
    let mut fence: Option<(char, usize)> = None;
    if markdown {
        // The fence state is read from a bounded stretch above the window, not the whole file.
        for row in first.saturating_sub(FENCE_LOOKBACK)..first {
            fence = fence_after(&text.slice_line(row).chars().take(80).collect::<String>(), fence);
        }
    }
    let mut out = Vec::new();
    let mut current: Option<Paragraph> = None;
    for row in first..=last {
        // One line over the limit is no prose to tag; it ends the paragraph before it.
        if text.line_end_offset(row) - text.line_start_offset(row) > PARAGRAPH_MAX {
            fence = if markdown { fence_after(&text.slice_line(row).chars().take(80).collect::<String>(), fence) } else { None };
            out.extend(current.take());
            continue;
        }
        let line = text.slice_line(row).to_string();
        let was = fence;
        fence = if markdown { fence_after(&line, fence) } else { None };
        if was.is_some() || fence.is_some() || line.chars().all(char::is_whitespace) {
            out.extend(current.take());
            continue;
        }
        if current.as_ref().is_some_and(|paragraph| paragraph.text.len() + line.len() > PARAGRAPH_MAX) {
            out.extend(current.take());
        }
        let paragraph = current.get_or_insert_with(|| Paragraph { at: text.line_start_offset(row), text: String::new() });
        paragraph.text.push_str(&blanked(&line, markdown));
        paragraph.text.push('\n');
    }
    out.extend(current);
    out
}

/// The fence state after `line`: opened by a run of three or more backticks or tildes (up to three
/// spaces in), closed by a run of the same mark at least as long.
fn fence_after(line: &str, state: Option<(char, usize)>) -> Option<(char, usize)> {
    let Some((mark, count)) = fence_run(line) else { return state };
    match state {
        None => Some((mark, count)),
        Some((open, length)) if open == mark && count >= length && line.trim().chars().all(|c| c == mark) => None,
        other => other,
    }
}

/// The mark and length of a fence run at the start of `line`.
fn fence_run(line: &str) -> Option<(char, usize)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let count = rest.chars().take_while(|c| *c == mark).count();
    (count >= 3).then_some((mark, count))
}

/// `line` with inline code spans, link targets (`](...)`) and web addresses turned to spaces;
/// code spans and link targets only in Markdown.
fn blanked(line: &str, markdown: bool) -> String {
    let mut bytes = line.as_bytes().to_vec();
    let blank = |bytes: &mut [u8], range: Range<usize>| bytes[range].fill(b' ');
    if markdown {
        let mut at = 0;
        while let Some(open) = line[at..].find('`').map(|found| at + found) {
            let run = line[open..].bytes().take_while(|b| *b == b'`').count();
            let ticks = "`".repeat(run);
            match line[open + run..].find(&ticks).map(|found| open + run + found) {
                Some(close) => {
                    blank(&mut bytes, open..close + run);
                    at = close + run;
                }
                None => at = open + run,
            }
        }
        let mut at = 0;
        while let Some(open) = line[at..].find("](").map(|found| at + found) {
            let end = line[open..].find(')').map_or(line.len(), |found| open + found + 1);
            blank(&mut bytes, open..end);
            at = end;
        }
    }
    for scheme in ["http://", "https://"] {
        let mut at = 0;
        while let Some(open) = line[at..].find(scheme).map(|found| at + found) {
            let end = line[open..].find(char::is_whitespace).map_or(line.len(), |found| open + found);
            blank(&mut bytes, open..end);
            at = end;
        }
    }
    // Only ASCII delimiters bound a blanked stretch, so whole characters were replaced.
    String::from_utf8(bytes).unwrap_or_default()
}

/// NSRange, for `tagAtIndex:unit:scheme:tokenRange:`.
#[repr(C)]
#[derive(Clone, Copy)]
struct NSRange {
    location: usize,
    length: usize,
}

// SAFETY: matches `struct _NSRange { NSUInteger location; NSUInteger length; }`.
unsafe impl Encode for NSRange {
    const ENCODING: Encoding = Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}

// SAFETY: the same struct, behind a pointer.
unsafe impl RefEncode for NSRange {
    const ENCODING_REF: Encoding = Encoding::Pointer(&<Self as Encode>::ENCODING);
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

/// Loads NaturalLanguage once; false when this Mac has none.
fn load() -> bool {
    static LOAD: Once = Once::new();
    LOAD.call_once(|| {
        // SAFETY: a plain `dlopen` of a system framework by path; the handle is never closed.
        unsafe { dlopen(c"/System/Library/Frameworks/NaturalLanguage.framework/NaturalLanguage".as_ptr(), 1) };
    });
    AnyClass::get(c"NLTagger").is_some()
}

/// An `NLTagger` over the lexical class scheme, for the thread that opened it.
struct Tagger {
    tagger: Retained<AnyObject>,
    scheme: Retained<AnyObject>,
}

impl Tagger {
    fn open() -> Option<Self> {
        if !load() {
            return None;
        }
        autoreleasepool(|_| {
            // SAFETY: NLTagger is loaded; `LexicalClass` is the value of `NLTagSchemeLexicalClass`.
            unsafe {
                let scheme = string("LexicalClass")?;
                let schemes: Retained<AnyObject> = msg_send![class!(NSArray), arrayWithObject: &*scheme];
                let class = AnyClass::get(c"NLTagger")?;
                let tagger: Option<Retained<AnyObject>> = msg_send![msg_send![class, alloc], initWithTagSchemes: &*schemes];
                Some(Self { tagger: tagger?, scheme })
            }
        })
    }

    /// The words of `text` that are one of the five classes, with their byte ranges.
    fn tag(&self, text: &str) -> Vec<Word> {
        autoreleasepool(|_| {
            let Some(string) = string(text) else { return Vec::new() };
            let mut words = Vec::new();
            // SAFETY: `tagger` and `scheme` live as long as `self`; the string outlives the loop; the
            // tag is borrowed (autoreleased) and read at once.
            unsafe {
                let _: () = msg_send![&*self.tagger, setString: &*string];
                let units: usize = text.encode_utf16().count();
                let mut walk = Walk::new(text);
                let mut at = 0;
                while at < units {
                    let mut range = NSRange { location: 0, length: 0 };
                    let tag: *mut AnyObject = msg_send![&*self.tagger, tagAtIndex: at, unit: 0isize, scheme: &*self.scheme, tokenRange: &mut range as *mut NSRange];
                    if range.length == 0 || range.location + range.length <= at {
                        at += 1;
                        continue;
                    }
                    if let Some(class) = (!tag.is_null()).then(|| text_of(&*tag)).as_deref().and_then(Class::of_tag) {
                        let (start, end) = (walk.byte_at(range.location), walk.byte_at(range.location + range.length));
                        words.push(Word { start: start as u32, end: end as u32, class });
                    }
                    at = range.location + range.length;
                }
            }
            words
        })
    }
}

/// An `NSString` of `text`; nothing when it can't be made.
fn string(text: &str) -> Option<Retained<AnyObject>> {
    // SAFETY: the bytes are valid UTF-8 (encoding 4) and copied.
    unsafe { msg_send![msg_send![class!(NSString), alloc], initWithBytes: text.as_ptr() as *const c_void, length: text.len(), encoding: 4usize] }
}

fn text_of(string: &AnyObject) -> String {
    // SAFETY: `UTF8String` is a NUL-terminated copy owned by the autorelease pool.
    unsafe {
        let chars: *const c_char = msg_send![string, UTF8String];
        if chars.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(chars).to_string_lossy().into_owned()
    }
}

/// UTF-16 offsets (what `NSString` counts in) to byte offsets of a string, for offsets that only
/// grow.
struct Walk<'a> {
    chars: std::str::CharIndices<'a>,
    len: usize,
    byte: usize,
    unit: usize,
}

impl<'a> Walk<'a> {
    fn new(text: &'a str) -> Self {
        Self { chars: text.char_indices(), len: text.len(), byte: 0, unit: 0 }
    }

    fn byte_at(&mut self, unit: usize) -> usize {
        while self.unit < unit {
            match self.chars.next() {
                Some((at, c)) => {
                    self.byte = at + c.len_utf8();
                    self.unit += c.len_utf16();
                }
                None => return self.len,
            }
        }
        self.byte
    }
}

#[cfg(test)]
mod tests {
    // Named, not `super::*`: the glob would bring gpui's `test` attribute over the standard one.
    use super::{Cache, Class, Classes, Paragraph, Rope, Tagger, Walk, Word, blanked, paragraphs, tinted};

    fn paragraph_texts(text: &str, markdown: bool) -> Vec<String> {
        let rope = Rope::from(text);
        paragraphs(&rope, 0..text.len(), markdown).into_iter().map(|p| p.text).collect()
    }

    #[test]
    fn fenced_blocks_are_not_prose() {
        let text = "One line.\n```rust\nfn quickly() {}\n\nlet x = 1;\n```\nAfter it.\n";
        assert_eq!(paragraph_texts(text, true), ["One line.\n", "After it.\n"]);
        // Plain text has no fences.
        assert_eq!(paragraph_texts(text, false).len(), 2);
    }

    #[test]
    fn a_window_inside_a_fence_knows_it() {
        let text = "```\nfn quickly() {}\nmore code\n```\nProse here.\n";
        let rope = Rope::from(text);
        let start = text.find("more").unwrap();
        let found = paragraphs(&rope, start..text.len(), true);
        assert_eq!(found, [Paragraph { at: text.find("Prose").unwrap(), text: "Prose here.\n".into() }]);
    }

    #[test]
    fn a_longer_fence_waits_for_its_close() {
        let text = "````\n```\nstill code\n````\nProse.\n";
        assert_eq!(paragraph_texts(text, true), ["Prose.\n"]);
    }

    #[test]
    fn blanking_keeps_every_byte_in_place() {
        let line = "Use `quick code` and [a link](http://x.y/z) see https://a.b now é";
        let out = blanked(line, true);
        assert_eq!(out.len(), line.len());
        assert_eq!(out, format!("Use {} and [a link{} see {} now é", " ".repeat(12), " ".repeat(15), " ".repeat(11)));
        assert_eq!(blanked("`kept` http://a.b", false), format!("`kept` {}", " ".repeat(10)));
    }

    #[test]
    fn utf16_offsets_walk_to_bytes() {
        let mut walk = Walk::new("aé😀b");
        assert_eq!([walk.byte_at(0), walk.byte_at(1), walk.byte_at(2), walk.byte_at(4), walk.byte_at(5)], [0, 1, 3, 7, 8]);
    }

    #[test]
    fn tints_the_enabled_class_skip_the_caret_word_and_cache_by_text() {
        let text = "It ran quickly.\n\nIt ran quickly.\n";
        let rope = Rope::from(text);
        let cache = Cache::default();
        let tagged = std::cell::Cell::new(0);
        let mut tag = |_: &str| {
            tagged.set(tagged.get() + 1);
            vec![Word { start: 3, end: 6, class: Class::Verb }, Word { start: 7, end: 14, class: Class::Adverb }]
        };
        let adverbs = Classes::of(&["adverbs".to_string()]);
        let found = tinted(&rope, 0..text.len(), false, adverbs, None, &cache, &mut tag);
        assert_eq!(found, [(7..14, Class::Adverb), (24..31, Class::Adverb)]);
        // Both paragraphs read the same: one answer, kept.
        assert_eq!(tagged.get(), 1);
        // A caret touching the word leaves it out; another class needs no new answer.
        let found = tinted(&rope, 0..text.len(), false, adverbs, Some(14), &cache, &mut tag);
        assert_eq!(found, [(24..31, Class::Adverb)]);
        let verbs = Classes::of(&["verbs".to_string()]);
        assert_eq!(tinted(&rope, 0..text.len(), false, verbs, None, &cache, &mut tag).len(), 2);
        assert_eq!(tagged.get(), 1);
    }

    #[test]
    fn the_system_tagger_finds_adverbs_at_their_bytes() {
        let tagger = Tagger::open().expect("NaturalLanguage loads");
        let text = "Café ünï: she walked slowly, and the dog ran quickly.";
        let words = tagger.tag(text);
        let adverbs: Vec<&str> = words.iter().filter(|word| word.class == Class::Adverb).map(|word| &text[word.start as usize..word.end as usize]).collect();
        assert_eq!(adverbs, ["slowly", "quickly"]);
        assert!(words.iter().any(|word| word.class == Class::Conjunction && &text[word.start as usize..word.end as usize] == "and"));
    }

    /// 200 arrow-key steps in a 20k-line Markdown file wake the layer 200 times; only the first
    /// (and the one after an edit) runs. Before: 200 runs, each reading the 120-row window and the
    /// 3000 rows of fence look-back above it (3120 lines), 624,000 lines in all.
    #[test]
    fn caret_steps_after_a_finished_run_do_not_run_again() {
        use super::{FENCE_LOOKBACK, unchanged};
        let (window, markdown, light) = (9_000..9_120, true, false);
        let classes = Classes::of(&["adverbs".to_string()]);
        let rev = 7;
        let mut finished = None;
        let (mut runs, mut lines) = (0, 0);
        let mut wake = |finished: &mut Option<_>, caret: Option<usize>| {
            let now = (rev, window.clone(), classes, markdown, light, caret);
            if unchanged(finished.as_ref(), &now) {
                return;
            }
            runs += 1;
            lines += FENCE_LOOKBACK + window.len();
            *finished = Some(now);
        };
        // The wake after an edit leaves the caret's word out; the first move away reruns once.
        wake(&mut finished, Some(5));
        for _ in 0..200 {
            wake(&mut finished, None);
        }
        assert_eq!(runs, 2);
        assert_eq!(lines, 2 * 3120);
        // A different window or class set runs again.
        let other = (rev, 9_040..9_160, classes, markdown, light, None);
        assert!(!unchanged(finished.as_ref(), &other));
    }
}
