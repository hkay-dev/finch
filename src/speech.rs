//! Edit > Speech > Start Speaking: reads the selection, or from the caret to the end, in the Mac's
//! voice, with each word lit as it is spoken.
//!
//! The speech is `AVSpeechSynthesizer` from AVFoundation, `dlopen`ed on the first Start Speaking
//! (nothing is loaded at launch) and driven through the Objective-C runtime without typed
//! bindings, as `camera_text.rs` does. A delegate class tells us which range of the spoken string
//! is being said (`willSpeakRangeOfSpeechString`) and when the speech ends or is cancelled; each
//! call is handed to the main thread's executor before it touches the app, so it never runs
//! inside another borrow of it.
//!
//! The spoken string is a copy of the text read, so its UTF-16 ranges map to byte offsets by a
//! cursor that only walks forward, added to where reading started. The word is a range fill (the
//! `Layer` below, the last of the mark layers) in a soft wash of the accent. The view follows it
//! with the editor's own reveal (`EditorState::reveal_offset`), the caret and the selection stay
//! where they were, and in Focus highlighting the lit unit follows the voice (`focus::follow`).
//!
//! It ends when the speech does, on Stop Speaking, Escape, a click in the text, or any edit (the
//! text revision moved, so the ranges no longer mean what they did): the wash clears, the focus
//! unit goes back to the caret, and nothing asks for another frame.

use std::{
    cell::RefCell,
    ffi::{CString, c_char, c_void},
    ops::Range,
    sync::Once,
};

use gpui_kit::component::input::{EditorState, RangeDecoration, RangeDecorationStyle, Rope};
use gpui_kit::{App, AsyncApp, Entity, Subscription, WeakEntity, actions};
use objc2::{
    ClassType, Encode, Encoding, define_class, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool, NSObject},
};

use crate::{
    focus,
    marks::{self, MarkLayer, RangeSet, View},
};

actions!(gpui_editor, [StartSpeaking, StopSpeaking]);

/// How much of the accent the lit word is washed with.
const WASH: f32 = 0.28;

/// `AVSpeechBoundaryImmediate`.
const IMMEDIATE: isize = 0;

/// An `NSRange`.
#[repr(C)]
#[derive(Clone, Copy)]
struct NSRange {
    location: usize,
    length: usize,
}

// SAFETY: `NSRange` is two `NSUInteger`s, as the struct's encoding says.
unsafe impl Encode for NSRange {
    const ENCODING: Encoding = Encoding::Struct("_NSRange", &[usize::ENCODING, usize::ENCODING]);
}

enum Event {
    /// The utterance (by address) is saying this range of its string, in UTF-16 units.
    Word(usize, Range<usize>),
    /// The utterance finished or was cancelled.
    End(usize),
}

thread_local! {
    /// The app, for the delegate, which AVFoundation calls with no `App` borrowed.
    static APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
    static VOICE: RefCell<Option<Voice>> = const { RefCell::new(None) };
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// The synthesizer and its delegate (which it holds weakly), made at the first Start Speaking.
struct Voice {
    synth: Retained<AnyObject>,
    _delegate: Retained<AnyObject>,
}

/// The reading under way.
struct Session {
    editor: WeakEntity<EditorState>,
    /// The utterance's address: events of any other one are an earlier reading's.
    utterance: usize,
    _utterance: Retained<AnyObject>,
    /// Where in the document the spoken string starts, and the revision it was cut from.
    base: usize,
    rev: u64,
    /// The UTF-16 and byte position the last range ended at.
    at: (usize, usize),
    _watch: Subscription,
}

impl Session {
    /// The byte offset in the spoken text (which is the editor's text from `base` on, read
    /// from `rope` at the revision the speech was cut from) of UTF-16 offset `utf16`. Ranges
    /// arrive in order, so the walk goes on from the last one; a range before it starts over.
    fn byte_of(&mut self, rope: &Rope, utf16: usize) -> usize {
        if utf16 < self.at.0 {
            self.at = (0, 0);
        }
        let start = self.at.1;
        advance(&mut self.at, rope.chars_at(self.base + start), utf16)
    }
}

/// Walks `chars` (the text from the byte `at.1`, which stands at UTF-16 offset `at.0`) up to
/// UTF-16 offset `utf16`, leaves `at` there and gives its byte offset.
fn advance(at: &mut (usize, usize), chars: impl Iterator<Item = char>, utf16: usize) -> usize {
    let (mut units, mut bytes) = *at;
    for c in chars {
        if units >= utf16 {
            break;
        }
        units += c.len_utf16();
        bytes += c.len_utf8();
    }
    *at = (units, bytes);
    bytes
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "FinchSpeechDelegate"]
    struct Delegate;

    impl Delegate {
        #[unsafe(method(speechSynthesizer:willSpeakRangeOfSpeechString:utterance:))]
        fn will_speak(&self, _synth: &AnyObject, range: NSRange, utterance: &AnyObject) {
            deliver(Event::Word(address(utterance), range.location..range.location + range.length));
        }

        #[unsafe(method(speechSynthesizer:didFinishSpeechUtterance:))]
        fn did_finish(&self, _synth: &AnyObject, utterance: &AnyObject) {
            deliver(Event::End(address(utterance)));
        }

        #[unsafe(method(speechSynthesizer:didCancelSpeechUtterance:))]
        fn did_cancel(&self, _synth: &AnyObject, utterance: &AnyObject) {
            deliver(Event::End(address(utterance)));
        }
    }
);

fn address(object: &AnyObject) -> usize {
    object as *const AnyObject as usize
}

/// Hands an event to the main executor, which runs it with the app.
fn deliver(event: Event) {
    let Some(app) = APP.with_borrow(Clone::clone) else { return };
    app.spawn(async move |cx| {
        cx.update(|cx| handle(event, cx));
    })
    .detach();
}

/// Wires Edit > Speech to the key window's editor. Call once at launch; it loads nothing.
pub fn install(cx: &mut App) {
    APP.with_borrow_mut(|app| *app = Some(cx.to_async()));
    cx.on_action(|_: &StartSpeaking, cx| cx.defer(start));
    cx.on_action(|_: &StopSpeaking, cx| {
        cx.defer(|cx| {
            stop(cx);
        })
    });
}

/// Whether anything is being read (Edit > Speech > Stop Speaking is grey when not).
pub fn speaking() -> bool {
    SESSION.with_borrow(Option::is_some)
}

/// Ends the reading if it is in `editor` (Escape, a click in its text); says whether it did.
pub fn end(editor: &Entity<EditorState>, cx: &mut App) -> bool {
    let ours = SESSION.with_borrow(|session| session.as_ref().is_some_and(|session| session.editor.upgrade().as_ref() == Some(editor)));
    ours && stop(cx)
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

/// Loads AVFoundation once; false when this Mac has none.
fn load() -> bool {
    static LOAD: Once = Once::new();
    LOAD.call_once(|| {
        // SAFETY: a plain `dlopen` of a system framework by path; the handle is never closed.
        unsafe { dlopen(c"/System/Library/Frameworks/AVFoundation.framework/AVFoundation".as_ptr(), 1) };
    });
    AnyClass::get(c"AVSpeechSynthesizer").is_some() && AnyClass::get(c"AVSpeechUtterance").is_some()
}

fn string(text: &str) -> Retained<AnyObject> {
    let text = CString::new(text).unwrap_or_default();
    unsafe { msg_send![objc2::class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

fn make_voice() -> Voice {
    // SAFETY: both classes are loaded (`load`); the delegate is kept alive beside the synthesizer.
    unsafe {
        let delegate: Retained<AnyObject> = msg_send![Delegate::class(), new];
        let class = AnyClass::get(c"AVSpeechSynthesizer").expect("AVFoundation is loaded");
        let synth: Retained<AnyObject> = msg_send![msg_send![class, alloc], init];
        let _: () = msg_send![&*synth, setDelegate: &*delegate];
        Voice { synth, _delegate: delegate }
    }
}

/// Start Speaking: the selection, or from the caret to the end of the text.
fn start(cx: &mut App) {
    let Some(editor) = crate::app::key_editor(cx) else {
        crate::preview::render::beep();
        return;
    };
    let (text, base, rev) = {
        let state = editor.read(cx);
        let (selection, rope): (Range<usize>, &Rope) = (state.selected_range(), state.text());
        let range = if selection.is_empty() { selection.start..rope.len() } else { selection };
        (rope.slice(range.clone()).to_string().replace('\0', " "), range.start, state.text_revision())
    };
    if text.trim().is_empty() || !load() {
        crate::preview::render::beep();
        return;
    }
    stop(cx);
    let synth = VOICE.with_borrow_mut(|voice| voice.get_or_insert_with(make_voice).synth.clone());
    // SAFETY: the class is loaded (`load`) and the string is a live `NSString`.
    let utterance: Retained<AnyObject> = unsafe {
        let class = AnyClass::get(c"AVSpeechUtterance").expect("AVFoundation is loaded");
        msg_send![class, speechUtteranceWithString: &*string(&text)]
    };
    let watch = cx.observe(&editor, |editor, cx| {
        let edited = SESSION.with_borrow(|session| session.as_ref().is_some_and(|session| session.rev != editor.read(cx).text_revision()));
        if edited {
            cx.defer(|cx| {
                stop(cx);
            });
        }
    });
    SESSION.with_borrow_mut(|session| {
        *session = Some(Session { editor: editor.downgrade(), utterance: address(&utterance), _utterance: utterance.clone(), base, rev, at: (0, 0), _watch: watch });
    });
    // SAFETY: a plain call on the main thread with a live utterance.
    unsafe {
        let _: () = msg_send![&*synth, speakUtterance: &*utterance];
    }
    cx.defer(crate::menu::refresh);
}

/// Ends the reading: the voice stops, the wash clears and the focus unit goes back to the caret.
/// Says whether there was one.
fn stop(cx: &mut App) -> bool {
    let Some(session) = SESSION.with_borrow_mut(Option::take) else { return false };
    if let Some(synth) = VOICE.with_borrow(|voice| voice.as_ref().map(|voice| voice.synth.clone())) {
        // SAFETY: a plain call on the main thread; the cancel's own event finds no session.
        unsafe {
            let _: Bool = msg_send![&*synth, stopSpeakingAtBoundary: IMMEDIATE];
        }
    }
    clear(&session, cx);
    true
}

/// The speech ended by itself.
fn finished(cx: &mut App) {
    if let Some(session) = SESSION.with_borrow_mut(Option::take) {
        clear(&session, cx);
    }
}

fn clear(session: &Session, cx: &mut App) {
    if let Some(editor) = session.editor.upgrade() {
        marks::with_layer(&editor, cx, |layer: &mut Layer| ((), layer.set_word(None)));
        focus::follow(&editor, None, cx);
    }
    cx.defer(crate::menu::refresh);
}

fn handle(event: Event, cx: &mut App) {
    match event {
        Event::End(id) => {
            if SESSION.with_borrow(|session| session.as_ref().is_some_and(|session| session.utterance == id)) {
                finished(cx);
            }
        }
        Event::Word(id, range) => {
            // `Some(None)`: the text changed or the editor is gone, so the reading ends.
            let step = SESSION.with_borrow_mut(|slot| {
                let session = slot.as_mut().filter(|session| session.utterance == id)?;
                let editor = session.editor.upgrade().filter(|editor| editor.read(cx).text_revision() == session.rev);
                Some(editor.map(|editor| {
                    let rope = editor.read(cx).text();
                    let (start, end) = (session.byte_of(rope, range.start), session.byte_of(rope, range.end));
                    (editor, session.base + start..session.base + end)
                }))
            });
            match step {
                None => {}
                Some(None) => {
                    stop(cx);
                }
                Some(Some((editor, word))) => light(&editor, word, cx),
            }
        }
    }
}

/// Lights `word`, brings it into view and moves the focus unit to it.
fn light(editor: &Entity<EditorState>, word: Range<usize>, cx: &mut App) {
    let start = word.start;
    marks::with_layer(editor, cx, |layer: &mut Layer| ((), layer.set_word(Some(word))));
    focus::follow(editor, Some(start), cx);
    editor.update(cx, |state, cx| state.reveal_offset(start, cx));
}

/// The speech layer of one editor: one range fill, the word being read.
pub struct Layer {
    set: RangeSet,
    word: Option<Range<usize>>,
    /// The collection holds a fill that has to be cleared.
    holds: bool,
}

impl Layer {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        Self { set: RangeSet::new(editor, cx), word: None, holds: false }
    }

    /// Whether the word changed.
    fn set_word(&mut self, word: Option<Range<usize>>) -> bool {
        if self.word == word {
            return false;
        }
        self.word = word;
        true
    }
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        self.word.is_some() || self.holds
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        let list = match &self.word {
            Some(word) if word.end <= view.text.len() => {
                vec![RangeDecoration::new(word.clone()).with_style(RangeDecorationStyle::Fill).with_color(crate::explorer::icons::accent().opacity(WASH))]
            }
            _ => Vec::new(),
        };
        self.holds = !list.is_empty();
        self.set.set_if_changed(list, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::advance;

    /// The offsets the walk over a `String` gave before the rope was read in place.
    fn expected(text: &str, utf16: usize) -> usize {
        let mut units = 0;
        for (at, ch) in text.char_indices() {
            if units >= utf16 {
                return at;
            }
            units += ch.len_utf16();
        }
        text.len()
    }

    #[test]
    fn words_land_on_the_bytes_the_speech_names() {
        // Two-byte, three-byte and surrogate-pair characters: UTF-16 and byte offsets part ways.
        for text in ["caf\u{e9} au lait", "\u{65e5}\u{672c}\u{8a9e} text", "a \u{1f600} b \u{1f44d}\u{1f3fd} c", "plain words here"] {
            let units: usize = text.chars().map(char::len_utf16).sum();
            // Ranges arrive in order, each word's start then end; the walk goes on from the last one.
            let mut at = (0, 0);
            for utf16 in 0..=units {
                // A step that splits a pair stops after the whole character, as before.
                let from = at.1;
                let got = advance(&mut at, text[from..].chars(), utf16);
                assert_eq!(got, expected(text, utf16), "{text:?} at {utf16}");
            }
            // A range before the last one starts over.
            at = (0, 0);
            let end = advance(&mut at, text.chars(), units);
            assert_eq!(end, text.len());
            at = (0, 0);
            assert_eq!(advance(&mut at, text.chars(), 2), expected(text, 2));
        }
    }
}
