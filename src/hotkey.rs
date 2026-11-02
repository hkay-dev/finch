//! The scratch window's global shortcut (finch#wjr7): Carbon's `RegisterEventHotKey`, which needs
//! no Accessibility permission. Carbon loads with `dlopen` on the first registration, so a launch
//! with the shortcut off loads nothing. The handler runs on the main thread and only sets a flag
//! and wakes the foreground task (`Press`); the task opens or dismisses the window.
//!
//! A background launch (`GPUI_EDITOR_BACKGROUND`) never registers one, so a test launch can't take
//! the real shortcut, unless `GPUI_EDITOR_HOTKEY` is set too.

use std::{
    cell::RefCell,
    ffi::{c_char, c_int, c_void},
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};

use gpui_kit::*;

use crate::{scratch, settings};

/// A key and its Carbon modifier mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    key: u32,
    modifiers: u32,
}

const CMD: u32 = 0x100;
const SHIFT: u32 = 0x200;
const OPTION: u32 = 0x800;
const CONTROL: u32 = 0x1000;

/// The chord a settings string names, `Ok(None)` for "off" (or nothing). Words joined by `-` or `+`
/// in any order: `ctrl`, `alt` (`option`, `opt`), `cmd` (`command`), `shift` and one key: a letter,
/// a digit, `space`, `return`, `tab`, `escape`, `delete`, `f1` to `f12`, or `minus`, `equal`,
/// `comma`, `period`, `slash`, `semicolon`, `quote`, `backslash`, `grave`, `lbracket`, `rbracket`.
/// It must have `ctrl`, `alt` or `cmd`, so no bare key is taken.
pub fn parse(text: &str) -> Result<Option<Chord>, String> {
    let text = text.trim().to_lowercase();
    if text.is_empty() || matches!(text.as_str(), "off" | "none" | "false") {
        return Ok(None);
    }
    let (mut modifiers, mut key) = (0, None);
    for word in text.split(['-', '+']).map(str::trim).filter(|word| !word.is_empty()) {
        match word {
            "ctrl" | "control" => modifiers |= CONTROL,
            "alt" | "option" | "opt" => modifiers |= OPTION,
            "cmd" | "command" => modifiers |= CMD,
            "shift" => modifiers |= SHIFT,
            _ => {
                let code = key_code(word).ok_or_else(|| format!("unknown key '{word}'"))?;
                if key.replace(code).is_some() {
                    return Err("more than one key".to_owned());
                }
            }
        }
    }
    let key = key.ok_or("no key")?;
    if modifiers & (CONTROL | OPTION | CMD) == 0 {
        return Err("needs ctrl, alt or cmd".to_owned());
    }
    Ok(Some(Chord { key, modifiers }))
}

/// The shortcut a settings string names as Mac glyphs in the ⌃⌥⇧⌘ order with the key named in capitals, like `⌃⌥Space`; `None` for
/// "off", empty or text `parse` refuses. The field keeps the typed words; this is the hint beside them.
pub fn glyphs(text: &str) -> Option<String> {
    let chord = parse(text).ok()??;
    let mut out = String::new();
    for (mask, glyph) in [(CONTROL, '⌃'), (OPTION, '⌥'), (SHIFT, '⇧'), (CMD, '⌘')] {
        if chord.modifiers & mask != 0 {
            out.push(glyph);
        }
    }
    let text = text.trim().to_lowercase();
    let word = text.split(['-', '+']).map(str::trim).find(|word| !matches!(*word, "" | "ctrl" | "control" | "alt" | "option" | "opt" | "cmd" | "command" | "shift"))?;
    let word = if word == "enter" { "return" } else { word };
    let mut letters = word.chars();
    out.extend(letters.next().into_iter().flat_map(char::to_uppercase));
    out.extend(letters);
    Some(out)
}

/// A key's ANSI virtual key code.
fn key_code(word: &str) -> Option<u32> {
    Some(match word {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "equal" => 24,
        "9" => 25,
        "7" => 26,
        "minus" => 27,
        "8" => 28,
        "0" => 29,
        "rbracket" => 30,
        "o" => 31,
        "u" => 32,
        "lbracket" => 33,
        "i" => 34,
        "p" => 35,
        "return" | "enter" => 36,
        "l" => 37,
        "j" => 38,
        "quote" => 39,
        "k" => 40,
        "semicolon" => 41,
        "backslash" => 42,
        "comma" => 43,
        "slash" => 44,
        "n" => 45,
        "m" => 46,
        "period" => 47,
        "tab" => 48,
        "space" => 49,
        "grave" => 50,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        _ => return None,
    })
}

/// `EventHotKeyID`.
#[repr(C)]
#[derive(Clone, Copy)]
struct HotKeyId {
    signature: u32,
    id: u32,
}

/// `EventTypeSpec`.
#[repr(C)]
struct EventType {
    class: u32,
    kind: u32,
}

type Handler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> c_int;

/// The Carbon calls, found with `dlsym` once the framework is loaded.
struct Carbon {
    target: unsafe extern "C" fn() -> *mut c_void,
    install: unsafe extern "C" fn(*mut c_void, Handler, u32, *const EventType, *mut c_void, *mut *mut c_void) -> c_int,
    register: unsafe extern "C" fn(u32, u32, HotKeyId, *mut c_void, u32, *mut *mut c_void) -> c_int,
    unregister: unsafe extern "C" fn(*mut c_void) -> c_int,
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

impl Carbon {
    fn load() -> Option<Self> {
        // SAFETY: a plain `dlopen` of a system framework by path (the handle is never closed) and
        // `dlsym` of Carbon's documented entry points, cast to their C signatures.
        unsafe {
            let handle = dlopen(c"/System/Library/Frameworks/Carbon.framework/Carbon".as_ptr(), 1);
            if handle.is_null() {
                return None;
            }
            let find = |name: &std::ffi::CStr| {
                let symbol = dlsym(handle, name.as_ptr());
                (!symbol.is_null()).then_some(symbol)
            };
            Some(Self {
                target: std::mem::transmute::<*mut c_void, _>(find(c"GetApplicationEventTarget")?),
                install: std::mem::transmute::<*mut c_void, _>(find(c"InstallEventHandler")?),
                register: std::mem::transmute::<*mut c_void, _>(find(c"RegisterEventHotKey")?),
                unregister: std::mem::transmute::<*mut c_void, _>(find(c"UnregisterEventHotKey")?),
            })
        }
    }
}

/// A press the handler saw and the task hasn't taken, and the task's waker.
static PRESSED: AtomicBool = AtomicBool::new(false);
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// Carbon's callback, on the main thread: records the press and wakes the task. Nothing else.
extern "C" fn pressed(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> c_int {
    PRESSED.store(true, Ordering::Release);
    if let Some(waker) = WAKER.lock().unwrap_or_else(PoisonError::into_inner).take() {
        waker.wake();
    }
    0
}

/// Resolves on the next press.
struct Press;

impl Future for Press {
    type Output = ();

    fn poll(self: Pin<&mut Self>, task: &mut Context<'_>) -> Poll<()> {
        *WAKER.lock().unwrap_or_else(PoisonError::into_inner) = Some(task.waker().clone());
        if PRESSED.swap(false, Ordering::Acquire) { Poll::Ready(()) } else { Poll::Pending }
    }
}

/// What this launch registered: the Carbon calls (loaded once) and the chord now held.
#[derive(Default)]
struct Registered {
    carbon: Option<Carbon>,
    handler_installed: bool,
    held: Option<(Chord, *mut c_void)>,
    /// The settings text last tried, so a refused or mistaken chord isn't tried and logged again on every settings change.
    tried: Option<String>,
}

impl Registered {
    /// Holds `chord` (none releases the one held). A refused registration (another app has the
    /// chord) says so on stderr and holds nothing.
    fn set(&mut self, chord: Option<Chord>) {
        if self.held.map(|(held, _)| held) == chord {
            return;
        }
        if let (Some((_, reference)), Some(carbon)) = (self.held.take(), &self.carbon) {
            // SAFETY: a reference `RegisterEventHotKey` returned, released once.
            unsafe { (carbon.unregister)(reference) };
        }
        let Some(chord) = chord else { return };
        if self.carbon.is_none() {
            self.carbon = Carbon::load();
        }
        let Some(carbon) = &self.carbon else {
            eprintln!("finch: can't load Carbon for the scratch shortcut");
            return;
        };
        if !self.handler_installed {
            // 'keyb' keyboard events, kEventHotKeyPressed.
            let types = [EventType { class: 0x6B65_7962, kind: 5 }];
            let mut handler = std::ptr::null_mut();
            // SAFETY: the application event target, a handler with Carbon's signature that lives for
            // the process, and a `types` array of the one length passed.
            let status = unsafe { (carbon.install)((carbon.target)(), pressed, 1, types.as_ptr(), std::ptr::null_mut(), &mut handler) };
            if status != 0 {
                eprintln!("finch: can't listen for the scratch shortcut (error {status})");
                return;
            }
            self.handler_installed = true;
        }
        // 'fnch'.
        let id = HotKeyId { signature: 0x666E_6368, id: 1 };
        let mut reference = std::ptr::null_mut();
        // SAFETY: a key code and modifier mask, an id by value, the application event target and
        // room for the returned reference.
        let status = unsafe { (carbon.register)(chord.key, chord.modifiers, id, (carbon.target)(), 0, &mut reference) };
        if status == 0 {
            self.held = Some((chord, reference));
        } else {
            eprintln!("finch: can't register the scratch shortcut (error {status}); another app may hold it");
        }
    }
}

impl Registered {
    /// Follows the settings: does nothing when the shortcut text is the one last tried; a mistake
    /// in it says so on stderr and counts as off.
    fn follow(&mut self, cx: &App) {
        let text = &settings::get(cx).scratch.shortcut;
        if self.tried.as_ref() == Some(text) {
            return;
        }
        self.tried = Some(text.clone());
        let chord = parse(text).unwrap_or_else(|why| {
            eprintln!("finch: scratch.shortcut \"{text}\": {why}");
            None
        });
        self.set(chord);
    }
}

/// Registers the shortcut once after launch, on the main thread, and follows the settings file
/// (a hand edit changes or turns off the chord at once). Does nothing in a background launch.
pub fn install(background: bool, cx: &mut App) {
    if background && std::env::var_os("GPUI_EDITOR_HOTKEY").is_none() {
        return;
    }
    let state = Rc::new(RefCell::new(Registered::default()));
    state.borrow_mut().follow(cx);
    cx.observe_global::<settings::Store>({
        let state = state.clone();
        move |cx| state.borrow_mut().follow(cx)
    })
    .detach();
    cx.spawn(async move |cx| {
        loop {
            Press.await;
            cx.update(|cx| scratch::toggle(background, cx));
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{CMD, CONTROL, Chord, OPTION, SHIFT, glyphs, parse};

    #[test]
    fn the_usual_chord_parses_in_any_order_and_spelling() {
        let usual = Some(Chord { key: 49, modifiers: CONTROL | OPTION });
        assert_eq!(parse("ctrl-alt-space"), Ok(usual));
        assert_eq!(parse("Option+Control+Space"), Ok(usual));
        assert_eq!(parse("cmd-shift-j"), Ok(Some(Chord { key: 38, modifiers: CMD | SHIFT })));
    }

    #[test]
    fn off_and_empty_turn_it_off_and_mistakes_are_named() {
        assert_eq!(parse(""), Ok(None));
        assert_eq!(parse(" Off "), Ok(None));
        assert!(parse("space").is_err());
        assert!(parse("shift-space").is_err());
        assert!(parse("ctrl-alt").is_err());
        assert!(parse("ctrl-a-b").is_err());
        assert!(parse("ctrl-banana").is_err());
    }

    #[test]
    fn the_hint_shows_glyphs_in_the_menu_order() {
        assert_eq!(glyphs("ctrl-alt-space").as_deref(), Some("⌃⌥Space"));
        assert_eq!(glyphs("Cmd+Shift+J").as_deref(), Some("⇧⌘J"));
        assert_eq!(glyphs("option-control-enter").as_deref(), Some("⌃⌥Return"));
        assert_eq!(glyphs("off"), None);
        assert_eq!(glyphs("ctrl-alt"), None);
    }
}
