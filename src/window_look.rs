//! What GPUI doesn't set on the native window: frame pacing on its Metal
//! layer, and, for background test launches, drawing while covered.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui_kit::{App, Window};
use objc2::{
    Encode, Encoding, class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyClass, AnyObject, Bool, Imp, Sel},
    sel,
};

/// GPUI's view (the `NSView` whose backing layer is the window's
/// `CAMetalLayer`).
fn native_view(window: &Window) -> Option<*mut AnyObject> {
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let raw_window_handle::RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    Some(handle.ns_view.as_ptr().cast())
}

/// `NSWindowToolbarStyleUnified`.
const TOOLBAR_UNIFIED: isize = 3;
/// `NSTitlebarSeparatorStyleNone`.
const SEPARATOR_NONE: isize = 1;
/// `NSWindowTitleHidden`.
const TITLE_HIDDEN: isize = 1;
/// `NSWindowTabbingModeDisallowed`.
const TABBING_DISALLOWED: isize = 2;

/// Called once when a window opens. An empty unified `NSToolbar` gives the
/// title bar its roomier row with no title text, no separator and no
/// tabbing, and the system centers the traffic lights in it, so they stay put
/// on resize and a tab bar can sit on their line. Controls are drawn in GPUI,
/// never as toolbar items. Harrier's `window_chrome::attach` (harrier#t2z5).
pub fn unified_title_bar(window: &Window) {
    let Some(view) = native_view(window) else { return };
    // SAFETY: NSToolbar and NSWindow setters on the window GPUI just opened,
    // on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let identifier: Option<Retained<AnyObject>> = msg_send![class!(NSString), stringWithUTF8String: c"GPUIEditorWindow".as_ptr()];
        let Some(identifier) = identifier else { return };
        let toolbar: Allocated<AnyObject> = msg_send![class!(NSToolbar), alloc];
        let toolbar: Retained<AnyObject> = msg_send![toolbar, initWithIdentifier: &*identifier];
        let _: () = msg_send![&*native, setToolbar: &*toolbar];
        let _: () = msg_send![&*native, setToolbarStyle: TOOLBAR_UNIFIED];
        let _: () = msg_send![&*native, setTitlebarSeparatorStyle: SEPARATOR_NONE];
        let _: () = msg_send![&*native, setTitleVisibility: TITLE_HIDDEN];
        let _: () = msg_send![&*native, setTabbingMode: TABBING_DISALLOWED];
    }
}

/// In native full screen AppKit makes a 52 pt overlay for any window with a visible toolbar, and
/// it slides down over the tabs and buttons: the toolbar is hidden there and shown again outside.
/// Called on every bounds change, which covers entering and leaving full screen. Harrier's
/// `window_chrome::native::sync_full_screen`.
pub fn sync_full_screen(window: &Window) {
    let Some(view) = native_view(window) else { return };
    let full = window.is_fullscreen();
    // SAFETY: NSWindow's `toolbar` and NSToolbar's `isVisible`/`setVisible:`, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let toolbar: Option<Retained<AnyObject>> = msg_send![&*native, toolbar];
        let Some(toolbar) = toolbar else { return };
        let visible: Bool = msg_send![&*toolbar, isVisible];
        if visible.as_bool() == full {
            let _: () = msg_send![&*toolbar, setVisible: Bool::new(!full)];
        }
    }
}

/// Called once when a window opens. Turns off display sync on the window's
/// Metal layer, so a frame never waits in `nextDrawable` for a vsync to free a
/// drawable (skill gpui, frame pacing rule 1); WindowServer still composites
/// on vsync. With `background` (a test launch) the window also keeps drawing
/// while it's covered and sits behind every other window. It also gives the
/// window its unified title bar.
pub fn free_frame_pacing(window: &Window, background: bool) {
    unified_title_bar(window);
    stamp_first_frame(window);
    let Some(view) = native_view(window) else { return };
    // SAFETY: GPUI's view and its layer, on the main thread; the setter is
    // only sent when the layer answers to it.
    unsafe {
        let layer: Option<Retained<AnyObject>> = msg_send![view, layer];
        if let Some(layer) = layer {
            let responds: Bool = msg_send![&*layer, respondsToSelector: sel!(setDisplaySyncEnabled:)];
            if responds.as_bool() {
                let _: () = msg_send![&*layer, setDisplaySyncEnabled: false];
            }
        }
    }
    if background {
        draw_when_covered(view);
        // The window opens unshown (`app::open_window`), so nothing orders it front: this puts
        // it on screen at the back, drawing while covered, under every other app's windows.
        // SAFETY: GPUI's view and its window, on the main thread.
        unsafe {
            let native: Option<Retained<AnyObject>> = msg_send![view, window];
            if let Some(native) = native {
                keep_under_everything(&native);
            }
        }
        order_back(view);
    }
}

/// `kCGMinimumWindowLevelKey`: the lowest level, under the desktop picture.
const MINIMUM_WINDOW_LEVEL_KEY: i32 = 1;
/// `canJoinAllSpaces | transient | ignoresCycle | fullScreenAuxiliary`.
const ALL_SPACES_UNSEEN: usize = 1 | 1 << 3 | 1 << 6 | 1 << 8;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowLevelForKey(key: i32) -> i32;
}

/// Puts a test launch's window (or panel) at the lowest window level, on every Space, full-screen
/// ones too, before it's ordered in. Ordering back only works among normal windows: an accessory
/// app's window ordered in on another app's full-screen Space drew in front of the user's full-screen
/// video (finch#jxb8). At the lowest level nothing can raise it over another app, and since it's on
/// the showing Space already, nothing switches Spaces to show it. `screencapture -l` still reads it.
/// Ramble's focus-safe `park` does the same (kata:ramble#nma5).
///
/// # Safety
/// `window` is an `NSWindow`, on the main thread.
pub unsafe fn keep_under_everything(window: &AnyObject) {
    // SAFETY: the caller's.
    unsafe {
        let level = CGWindowLevelForKey(MINIMUM_WINDOW_LEVEL_KEY) as isize;
        let _: () = msg_send![window, setLevel: level];
        let _: () = msg_send![window, setCollectionBehavior: ALL_SPACES_UNSEEN];
    }
}

/// How long a window's fade in runs, in seconds.
const WINDOW_REVEAL: f32 = 0.16;
/// How long a window waits for its first frame before it shows at full alpha anyway.
const REVEAL_GIVE_UP: Duration = Duration::from_millis(600);

/// Called once when a window opens unshown (`show: false`), for a window that comes in front: GPUI
/// would order it front before its first frame, 60 to 75 ms of empty window. This sets its alpha
/// to 0 and orders it in, key, so it draws at once; after its first frame AppKit fades the alpha
/// to 1 on the compositor, no GPUI frames. At full alpha after 0.6 s if no frame came.
pub fn fade_in(window: &Window, cx: &mut App) {
    let Some(view) = native_view(window) else { return };
    set_alpha(view, 0., 0., c"easeOut");
    // SAFETY: GPUI's view and its window, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        if let Some(native) = native {
            let none: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![&*native, makeKeyAndOrderFront: none];
        }
    }
    window.on_next_frame(|window, _| {
        if let Some(view) = native_view(window) {
            set_alpha(view, 1., WINDOW_REVEAL, c"easeOut");
        }
    });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(REVEAL_GIVE_UP).await;
        cx.update(|cx| {
            handle
                .update(cx, |_, window, _| {
                    if let Some(view) = native_view(window) {
                        set_alpha(view, 1., 0., c"easeOut");
                    }
                })
                .ok();
        });
    })
    .detach();
}

/// How long a window's fade out runs, in seconds.
const WINDOW_HIDE: f32 = 0.14;

thread_local! {
    /// Windows whose close fade is running, so a second close (the red button pressed twice) never
    /// stacks another animation or another removal.
    static CLOSING: RefCell<Vec<gpui_kit::WindowId>> = const { RefCell::new(Vec::new()) };
}

/// Whether the window's close fade is running: it's on its way out, so it asks nothing more and
/// takes no files.
pub fn is_closing(id: gpui_kit::WindowId) -> bool {
    CLOSING.with_borrow(|closing| closing.contains(&id))
}

/// Closes a window whose close is confirmed: its alpha eases to 0 over 0.14 s, then the window is
/// removed (a test launch too: it never takes focus). A window with no native view is removed
/// at once. Called after the save path, never before it; a window already fading is left to its
/// fade. Not for quit, which never comes through here.
pub fn fade_out(window: &mut Window, cx: &mut App) {
    leave(window, false, cx);
}

/// `fade_out` for the scratch window: it also slides up off the top of its screen as it fades,
/// the way it came in (`slide_in`).
pub fn slide_out(window: &mut Window, cx: &mut App) {
    leave(window, true, cx);
}

fn leave(window: &mut Window, slide: bool, cx: &mut App) {
    let id = window.window_handle().window_id();
    if is_closing(id) {
        return;
    }
    let Some(view) = native_view(window) else {
        window.remove_window();
        return;
    };
    fade_trace("close fade begins");
    CLOSING.with_borrow_mut(|closing| closing.push(id));
    // The keyboard goes first: keys typed during the fade would edit a window that is about to go.
    window.blur(cx);
    set_alpha(view, 0., WINDOW_HIDE, c"easeIn");
    if slide {
        slide_frame(view, SCRATCH_ABOVE, WINDOW_HIDE, c"easeIn");
    }
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs_f32(WINDOW_HIDE)).await;
        cx.update(|cx| {
            handle.update(cx, |_, window, _| window.remove_window()).ok();
            CLOSING.with_borrow_mut(|closing| closing.retain(|closing| *closing != id));
        });
    })
    .detach();
}

/// How far above its resting place the scratch window waits, in points: more than its height, so
/// all of it is off the top of the screen.
const SCRATCH_ABOVE: f64 = 400.;
/// How long the scratch window's slide down takes, in seconds.
const SCRATCH_SLIDE: f32 = 0.22;
/// `NSWindowCollectionBehaviorMoveToActiveSpace | FullScreenAuxiliary`: it opens on the Space the
/// pointer is on, a full-screen one too, and never switches Spaces.
const SCRATCH_SPACES: usize = 1 << 1 | 1 << 8;

/// Called once when the scratch window opens (not in a test launch): no traffic lights, and it comes
/// up on the Space that is showing.
pub fn scratch_style(window: &Window) {
    let Some(view) = native_view(window) else { return };
    // SAFETY: NSWindow's `standardWindowButton:` and `setCollectionBehavior:`, NSButton's `setHidden:`, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        for kind in 0isize..3 {
            let button: Option<Retained<AnyObject>> = msg_send![&*native, standardWindowButton: kind];
            if let Some(button) = button {
                let _: () = msg_send![&*button, setHidden: Bool::YES];
            }
        }
        let _: () = msg_send![&*native, setCollectionBehavior: SCRATCH_SPACES];
    }
}

thread_local! {
    /// Whether each window's traffic lights are meant to show, so a hide's delayed `setHidden:` never lands after a show.
    static LIGHTS: RefCell<HashMap<gpui_kit::WindowId, (bool, u64)>> = RefCell::new(HashMap::new());
}

/// Drops a released window's traffic-light entry (a window that never showed them has none).
pub fn forget_lights(id: gpui_kit::WindowId) {
    LIGHTS.with_borrow_mut(|lights| lights.remove(&id));
}

/// Minimal mode: eases the traffic lights in or out over `seconds` on AppKit's animator (no GPUI frames; at once for 0). Once
/// faded out they are also hidden, so the corner takes no pointer; shown again, they unhide first and then fade in.
pub fn window_buttons(window: &Window, shown: bool, seconds: f32, cx: &mut App) {
    let Some(view) = native_view(window) else { return };
    let id = window.window_handle().window_id();
    let generation = LIGHTS.with_borrow_mut(|lights| {
        let next = lights.get(&id).map_or(0, |(_, at)| at + 1);
        lights.insert(id, (shown, next));
        next
    });
    // SAFETY: NSWindow's `standardWindowButton:`, NSButton's `setHidden:` and its animator's `setAlphaValue:` inside an
    // NSAnimationContext group, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let ease = ns_string(c"easeInEaseOut");
        for kind in 0isize..3 {
            let button: Option<Retained<AnyObject>> = msg_send![&*native, standardWindowButton: kind];
            let Some(button) = button else { continue };
            if shown {
                let _: () = msg_send![&*button, setHidden: Bool::NO];
            }
            let to = if shown { 1.0f64 } else { 0.0f64 };
            match ease.as_ref().filter(|_| seconds > 0.) {
                Some(ease) => {
                    let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
                    let context: Retained<AnyObject> = msg_send![class!(NSAnimationContext), currentContext];
                    let _: () = msg_send![&*context, setDuration: f64::from(seconds)];
                    let curve: Retained<AnyObject> = msg_send![class!(CAMediaTimingFunction), functionWithName: &**ease];
                    let _: () = msg_send![&*context, setTimingFunction: &*curve];
                    let animator: Retained<AnyObject> = msg_send![&*button, animator];
                    let _: () = msg_send![&*animator, setAlphaValue: to];
                    let _: () = msg_send![class!(NSAnimationContext), endGrouping];
                }
                None => {
                    let _: () = msg_send![&*button, setAlphaValue: to];
                }
            }
        }
    }
    if !shown {
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs_f32(seconds)).await;
            cx.update(|cx| {
                // Hide only if no later call came; a window that is gone forgets its entry.
                if LIGHTS.with_borrow(|lights| lights.get(&id).is_some_and(|(_, at)| *at == generation)) {
                    if handle.update(cx, |_, window, _| set_lights_hidden(window)).is_err() {
                        LIGHTS.with_borrow_mut(|lights| lights.remove(&id));
                    }
                }
            });
        })
        .detach();
    }
}

/// Hides the three traffic lights (`window_buttons` after their fade out).
fn set_lights_hidden(window: &Window) {
    let Some(view) = native_view(window) else { return };
    // SAFETY: NSWindow's `standardWindowButton:` and NSButton's `setHidden:`, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        for kind in 0isize..3 {
            let button: Option<Retained<AnyObject>> = msg_send![&*native, standardWindowButton: kind];
            if let Some(button) = button {
                let _: () = msg_send![&*button, setHidden: Bool::YES];
            }
        }
    }
}

/// Called once when the scratch window opens unshown: like `fade_in` (alpha 0, ordered in, key,
/// eased to 1 after the first frame), and the window also starts above the top of its screen and
/// glides down to the place it was made for. AppKit's animator does both on the compositor, so
/// frames come only for the first draw. At rest after 0.6 s if no frame came.
pub fn slide_in(window: &Window, cx: &mut App) {
    let Some(view) = native_view(window) else { return };
    set_alpha(view, 0., 0., c"easeOut");
    // SAFETY: NSWindow's `frame`, `makeKeyAndOrderFront:` and `setFrame:display:`, on the main thread. The
    // move up comes after the window is ordered in: ordering a window in pulls one that is off the top back under the menu bar.
    let rest = unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let rest: Rect = msg_send![&*native, frame];
        let none: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![&*native, makeKeyAndOrderFront: none];
        let above = Rect { origin: Point { x: rest.origin.x, y: rest.origin.y + SCRATCH_ABOVE }, size: rest.size };
        let _: () = msg_send![&*native, setFrame: above, display: Bool::NO];
        rest
    };
    window.on_next_frame(move |window, _| {
        if let Some(view) = native_view(window) {
            set_alpha(view, 1., SCRATCH_SLIDE, c"easeOut");
            animate_frame(view, rest, SCRATCH_SLIDE, c"easeOut");
        }
    });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(REVEAL_GIVE_UP).await;
        cx.update(|cx| {
            handle
                .update(cx, |_, window, _| {
                    if let Some(view) = native_view(window).filter(|_| !is_closing(window.window_handle().window_id())) {
                        set_alpha(view, 1., 0., c"easeOut");
                        animate_frame(view, rest, 0., c"easeOut");
                    }
                })
                .ok();
        });
    })
    .detach();
}

/// Moves the window up by `rise` points over `seconds`, eased along the named curve.
fn slide_frame(view: *mut AnyObject, rise: f64, seconds: f32, curve: &std::ffi::CStr) {
    // SAFETY: NSWindow's `frame`, on the main thread.
    let to = unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let now: Rect = msg_send![&*native, frame];
        Rect { origin: Point { x: now.origin.x, y: now.origin.y + rise }, size: now.size }
    };
    animate_frame(view, to, seconds, curve);
}

/// Sets the window's frame to `to`, eased along the named curve over `seconds` (at once for 0) by
/// AppKit's animator.
fn animate_frame(view: *mut AnyObject, to: Rect, seconds: f32, curve: &std::ffi::CStr) {
    // SAFETY: NSWindow's `setFrame:display:`, and its frame through the animator inside an
    // NSAnimationContext group, and CAMediaTimingFunction's named curve, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        if seconds <= 0. {
            let _: () = msg_send![&*native, setFrame: to, display: Bool::NO];
            return;
        }
        let Some(ease) = ns_string(curve) else { return };
        let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
        let context: Retained<AnyObject> = msg_send![class!(NSAnimationContext), currentContext];
        let _: () = msg_send![&*context, setDuration: f64::from(seconds)];
        let timing: Retained<AnyObject> = msg_send![class!(CAMediaTimingFunction), functionWithName: &*ease];
        let _: () = msg_send![&*context, setTimingFunction: &*timing];
        let animator: Retained<AnyObject> = msg_send![&*native, animator];
        let _: () = msg_send![&*animator, setFrame: to, display: Bool::NO];
        let _: () = msg_send![class!(NSAnimationContext), endGrouping];
    }
}

/// Sets the window's alpha to `to`, eased along the named curve over `seconds` (at once for 0) by
/// AppKit's animator. Harrier's `native_reveal::alpha`.
fn set_alpha(view: *mut AnyObject, to: f64, seconds: f32, curve: &std::ffi::CStr) {
    // SAFETY: NSWindow's `alphaValue` through its animator inside an NSAnimationContext group, and
    // CAMediaTimingFunction's named curve, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        if seconds <= 0. {
            let _: () = msg_send![&*native, setAlphaValue: to];
            return;
        }
        let Some(ease) = ns_string(curve) else { return };
        let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
        let context: Retained<AnyObject> = msg_send![class!(NSAnimationContext), currentContext];
        let _: () = msg_send![&*context, setDuration: f64::from(seconds)];
        let curve: Retained<AnyObject> = msg_send![class!(CAMediaTimingFunction), functionWithName: &*ease];
        let _: () = msg_send![&*context, setTimingFunction: &*curve];
        let animator: Retained<AnyObject> = msg_send![&*native, animator];
        let _: () = msg_send![&*animator, setAlphaValue: to];
        let _: () = msg_send![class!(NSAnimationContext), endGrouping];
    }
}

/// How long a torn-off window's growth from the card to its full size runs, in seconds.
const WINDOW_GROW: f32 = 0.25;

thread_local! {
    /// Windows growing from a card: their size is in flux, so nothing saves it as the user's window size.
    static GROWING: RefCell<Vec<gpui_kit::WindowId>> = const { RefCell::new(Vec::new()) };
}

/// Whether the window is growing from a card (`grow_from`).
pub fn is_growing(id: gpui_kit::WindowId) -> bool {
    GROWING.with_borrow(|growing| growing.contains(&id))
}

/// A window just opened by a dropped tab starts as the card: `card` is its place (the left, the top and the size, in points) in the
/// window's own frame. It then grows to the full frame over `WINDOW_GROW`, eased by AppKit's animator on the compositor, so the
/// frames come only while it grows. The minimum size is lifted meanwhile and put back after. A window with no native view is left.
pub fn grow_from(window: &Window, card: [f64; 4], cx: &mut App) {
    let Some(view) = native_view(window) else { return };
    let id = window.window_handle().window_id();
    // SAFETY: NSWindow's `frame`, `contentMinSize` and `setFrame:display:`, and its frame through the animator inside an
    // NSAnimationContext group, on the main thread.
    let min = unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let Some(ease) = ns_string(c"easeOut") else { return };
        let full: Rect = msg_send![&*native, frame];
        let min: Size = msg_send![&*native, contentMinSize];
        let top = full.origin.y + full.size.height;
        let from = Rect { origin: Point { x: full.origin.x + card[0], y: top - card[1] - card[3] }, size: Size { width: card[2], height: card[3] } };
        let _: () = msg_send![&*native, setContentMinSize: Size { width: 1., height: 1. }];
        let _: () = msg_send![&*native, setFrame: from, display: Bool::NO];
        let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
        let context: Retained<AnyObject> = msg_send![class!(NSAnimationContext), currentContext];
        let _: () = msg_send![&*context, setDuration: f64::from(WINDOW_GROW)];
        let curve: Retained<AnyObject> = msg_send![class!(CAMediaTimingFunction), functionWithName: &*ease];
        let _: () = msg_send![&*context, setTimingFunction: &*curve];
        let animator: Retained<AnyObject> = msg_send![&*native, animator];
        let _: () = msg_send![&*animator, setFrame: full, display: Bool::YES];
        let _: () = msg_send![class!(NSAnimationContext), endGrouping];
        min
    };
    GROWING.with_borrow_mut(|growing| growing.push(id));
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs_f32(WINDOW_GROW + 0.05)).await;
        cx.update(|cx| {
            handle
                .update(cx, |_, window, _| {
                    if let Some(view) = native_view(window) {
                        // SAFETY: NSWindow's `setContentMinSize:`, on the main thread.
                        unsafe {
                            let native: Option<Retained<AnyObject>> = msg_send![view, window];
                            if let Some(native) = native {
                                let _: () = msg_send![&*native, setContentMinSize: min];
                            }
                        }
                    }
                })
                .ok();
            GROWING.with_borrow_mut(|growing| growing.retain(|growing| *growing != id));
        });
    })
    .detach();
}

/// Milliseconds from process exec to the first window's first frame, stamped once.
static EXEC_TO_FIRST_FRAME_MS: OnceLock<f64> = OnceLock::new();

unsafe extern "C" {
    fn sysctl(name: *mut i32, namelen: u32, oldp: *mut u8, oldlenp: *mut usize, newp: *mut u8, newlen: usize) -> i32;
}

/// When this process started, in seconds since the epoch: `kp_proc.p_starttime`
/// (a `timeval`, the first field of `kinfo_proc`) from `KERN_PROC_PID`.
fn process_start_secs() -> Option<f64> {
    // Room for a `kinfo_proc` (648 bytes on arm64), 8-aligned for the `timeval` read.
    let mut info = [0u64; 128];
    let mut len = std::mem::size_of_val(&info);
    // CTL_KERN, KERN_PROC, KERN_PROC_PID, pid
    let mut mib = [1, 14, 1, std::process::id() as i32];
    // SAFETY: a read-only sysctl into a buffer of the length passed.
    let status = unsafe { sysctl(mib.as_mut_ptr(), 4, info.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if status != 0 || len < 16 {
        return None;
    }
    // tv_sec (i64), then tv_usec (i32) at byte 8.
    let secs = info[0] as i64;
    let micros = (info[1] & 0xffff_ffff) as i32;
    Some(secs as f64 + micros as f64 / 1e6)
}

/// Stamps the wall-clock time of this window's first frame, if no window has yet.
/// Costs one clock read and a sysctl, once per process, inside the frame callback.
pub fn stamp_first_frame(window: &Window) {
    window.on_next_frame(|_, _| {
        EXEC_TO_FIRST_FRAME_MS.get_or_init(|| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            process_start_secs().map_or(f64::NAN, |start| (now - start) * 1000.0)
        });
    });
}

/// `exec_to_first_frame_ms`, or None until a frame has drawn.
pub fn exec_to_first_frame_ms() -> Option<f64> {
    EXEC_TO_FIRST_FRAME_MS.get().copied().filter(|ms| ms.is_finite())
}

/// Puts a test window behind every other window on its Space. Not activating isn't enough:
/// GPUI orders the window front, and a Space swipe makes the owner of the Space's topmost
/// window the front app, so the user's typing would go to the test window (gpui-editor#pcck).
/// Harrier's `native::reveal` does the same.
fn order_back(view: *mut AnyObject) {
    // SAFETY: GPUI's view and its window, on the main thread.
    unsafe {
        let window: Option<Retained<AnyObject>> = msg_send![view, window];
        if let Some(window) = window {
            let none: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![&*window, orderBack: none];
        }
    }
}

/// Hands the keyboard back to whichever app had it. A background launch calls this when its window
/// turns active (something asked the app forward), so it never stays the front app (gpui-editor#a6gn).
pub fn deactivate_app() {
    // SAFETY: `NSApp`'s `deactivate`, on the main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, deactivate];
    }
}

/// `NSWindowOcclusionStateVisible`.
const VISIBLE: usize = 1 << 1;

extern "C-unwind" fn always_visible(_: *mut AnyObject, _: Sel) -> usize {
    VISIBLE
}

/// GPUI stops a window's display link while `occlusionState` says it isn't
/// visible, so a test window behind the user's apps draws nothing: no scroll
/// frames to measure, no picture to capture.
/// This gives GPUI's window class an `occlusionState` that always answers
/// visible, then replays the occlusion change so a window that opened covered
/// starts its link. Harrier's `window_look::draw_when_covered` (harrier#fskr).
fn draw_when_covered(view: *mut AnyObject) {
    // SAFETY: GPUI's view, its window and the window's delegate, on the main
    // thread. The added method has the getter's signature (receiver, selector,
    // NSUInteger), and `class_addMethod` leaves a class that already defines it
    // alone.
    unsafe {
        let window: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(window) = window else { return };
        let class = (*window).class() as *const AnyClass as *mut AnyClass;
        let imp = std::mem::transmute::<extern "C-unwind" fn(*mut AnyObject, Sel) -> usize, Imp>(always_visible);
        objc2::ffi::class_addMethod(class, sel!(occlusionState), imp, c"Q@:".as_ptr());
        let delegate: Option<Retained<AnyObject>> = msg_send![&*window, delegate];
        let Some(delegate) = delegate else { return };
        let responds: Bool = msg_send![&*delegate, respondsToSelector: sel!(windowDidChangeOcclusionState:)];
        if responds.as_bool() {
            let none: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![&*delegate, windowDidChangeOcclusionState: none];
        }
    }
}

/// `NSPoint`, `NSSize` and `NSRect` as AppKit encodes them, so `setFrame:` and
/// `bounds` type-check without a Foundation crate.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Size {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub origin: Point,
    pub size: Size,
}

// SAFETY: each matches CoreGraphics' CGPoint, CGSize and CGRect layout.
unsafe impl Encode for Point {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for Size {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for Rect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Size::ENCODING]);
}

/// `NSViewHeightSizable`: the column keeps its width and follows the window's height.
const TALL: usize = 16;
/// `NSVisualEffectMaterialSidebar`, `NSVisualEffectBlendingModeBehindWindow`,
/// `NSVisualEffectStateFollowsWindowActiveState`.
const MATERIAL_SIDEBAR: isize = 7;
const BEHIND_WINDOW: isize = 0;
const FOLLOWS_ACTIVE: isize = 0;
/// `NSWindowBelow`.
const BELOW: isize = -1;

thread_local! {
    /// Each window's material view and the column width and appearance last
    /// applied to it, so a repaint of the same column costs no AppKit calls.
    static MATERIAL: RefCell<HashMap<isize, (Retained<AnyObject>, u32, Option<bool>)>> = RefCell::default();
}

/// A real `NSVisualEffectView` behind a `width` column, full height so it also
/// sits under the title bar: the sidebar material, blending with what is
/// behind the window, active only while the window is key. A `width` of 0
/// hides it. A pinned theme gives the window that appearance so the material is the right one;
/// following the Mac leaves the window on the system's. Harrier's `window_look::native::sidebar_material`.
pub fn sidebar_material(window: &Window, width: f32, pin: Option<bool>) {
    let Some(view) = native_view(window) else { return };
    let key = width.ceil().max(0.) as u32;
    // SAFETY: plain AppKit getters and setters on the window GPUI owns, on the
    // main thread. `view` is GPUI's live content view.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return };
        let number: isize = msg_send![&*native, windowNumber];
        if MATERIAL.with(|all| all.borrow().get(&number).is_some_and(|(_, w, p)| *w == key && *p == pin)) {
            return;
        }
        // Pinned: the window takes that appearance. Following the Mac: none, so the window (its
        // material, menus and sheets) tracks the system and tells GPUI when it changes.
        let wanted: Option<Retained<AnyObject>> = match pin {
            Some(light) => {
                let name = if light { c"NSAppearanceNameAqua" } else { c"NSAppearanceNameDarkAqua" };
                let name: Option<Retained<AnyObject>> = msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
                name.and_then(|name| msg_send![class!(NSAppearance), appearanceNamed: &*name])
            }
            None => None,
        };
        let _: () = msg_send![&*native, setAppearance: wanted.as_deref()];
        let content: Option<Retained<AnyObject>> = msg_send![&*native, contentView];
        let frame: Option<Retained<AnyObject>> = content.as_ref().and_then(|content| msg_send![&**content, superview]);
        let Some(frame) = frame else { return };
        let existing = MATERIAL.with(|all| all.borrow().get(&number).map(|(view, ..)| view.clone()));
        let material = match existing {
            Some(material) => material,
            None => {
                let Some(class) = AnyClass::get(c"NSVisualEffectView") else { return };
                let material: Retained<AnyObject> = msg_send![class, new];
                let _: () = msg_send![&*material, setMaterial: MATERIAL_SIDEBAR];
                let _: () = msg_send![&*material, setBlendingMode: BEHIND_WINDOW];
                let _: () = msg_send![&*material, setState: FOLLOWS_ACTIVE];
                let _: () = msg_send![&*material, setAutoresizingMask: TALL];
                // Below the content view, so GPUI's drawing stays over the material.
                let _: () = msg_send![&*frame, addSubview: &*material, positioned: BELOW, relativeTo: content.as_deref()];
                material
            }
        };
        let bounds: Rect = msg_send![&*frame, bounds];
        let column = Rect { origin: bounds.origin, size: Size { width: f64::from(key), height: bounds.size.height } };
        let _: () = msg_send![&*material, setFrame: column];
        let _: () = msg_send![&*material, setHidden: Bool::new(key == 0)];
        MATERIAL.with(|all| all.borrow_mut().insert(number, (material, key, pin)));
    }
}

/// `CGColorRef` as AppKit encodes the argument of `setBackgroundColor:`.
#[repr(transparent)]
struct CgColor(*mut std::ffi::c_void);

// SAFETY: matches the encoding of a CoreGraphics color reference, `^{CGColor=}`.
unsafe impl Encode for CgColor {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorCreateSRGB(red: f64, green: f64, blue: f64, alpha: f64) -> *mut std::ffi::c_void;
}

unsafe extern "C" {
    fn CFRelease(object: *const std::ffi::c_void);
    fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
}

/// `CGWindowListCreateImage(rect, option, window, imageOption)`.
type CaptureWindow = unsafe extern "C" fn(Rect, u32, u32, u32) -> *mut std::ffi::c_void;

/// `kCGWindowListOptionIncludingWindow`.
const LIST_INCLUDING_WINDOW: u32 = 1 << 3;
/// `kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution`: the window without its shadow, at the screen's pixels.
const IMAGE_BEST_NO_FRAME: u32 = 1 | (1 << 3);

/// Looks `CGWindowListCreateImage` up when called, not at link time: a system that has dropped it
/// gets no fade instead of an app that won't start.
fn capture_window() -> Option<CaptureWindow> {
    // SAFETY: `RTLD_DEFAULT` (-2) with a NUL-terminated name; a found symbol of that name has the
    // `CGWindowListCreateImage` signature.
    unsafe {
        let symbol = dlsym(-2isize as *mut std::ffi::c_void, c"CGWindowListCreateImage".as_ptr());
        (!symbol.is_null()).then(|| std::mem::transmute::<*mut std::ffi::c_void, CaptureWindow>(symbol))
    }
}

/// `CGRectNull`: the window's own bounds.
const RECT_NULL: Rect = Rect { origin: Point { x: f64::INFINITY, y: f64::INFINITY }, size: Size { width: 0., height: 0. } };

/// The window's `windowNumber`, which is also its `CGWindowID`.
fn window_number(window: &Window) -> Option<(*mut AnyObject, isize)> {
    let view = native_view(window)?;
    // SAFETY: plain AppKit getters on the window GPUI owns, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let number: isize = msg_send![&*native?, windowNumber];
        Some((view, number))
    }
}

/// The window's `windowNumber`, for `forget` to name it after the window is gone.
pub fn number(window: &Window) -> Option<isize> {
    window_number(window).map(|(_, number)| number)
}

/// Drops a closed window's sidebar material view and any theme-fade layer, so neither outlives
/// the window or answers for a window that macOS later gives the same number.
pub fn forget(number: isize) {
    MATERIAL.with(|all| all.borrow_mut().remove(&number));
    FADING_WINDOWS.with_borrow_mut(|all| all.retain(|n| *n != number));
    PRIOR.with(|all| all.borrow_mut().remove(&number));
    fade_end_number(number, None);
}

/// Called when a window opens. The first `CGWindowListCreateImage` of a process takes long
/// enough to stall a frame, so the first capture is made here, off the main thread, and dropped.
/// Only that first capture is slow, so later windows skip it.
pub fn warm_capture(window: &Window) {
    static WARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some((_, number)) = window_number(window) else { return };
    FADING_WINDOWS.with_borrow_mut(|all| {
        if !all.contains(&number) {
            all.push(number);
        }
    });
    let Some(capture) = capture_window() else { return };
    if WARMED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || {
        // SAFETY: CoreGraphics' window image call is thread-safe; the image is released at once.
        unsafe {
            let image = capture(RECT_NULL, LIST_INCLUDING_WINDOW, number as u32, IMAGE_BEST_NO_FRAME);
            if !image.is_null() {
                CFRelease(image);
            }
        }
    });
}

/// A `CGImage` this module owns, released when dropped.
struct Picture(*mut std::ffi::c_void);

impl Picture {
    /// The image, for a caller that releases it.
    fn into_raw(self) -> *mut std::ffi::c_void {
        let image = self.0;
        std::mem::forget(self);
        image
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: a CoreGraphics image this picture owns.
        unsafe { CFRelease(self.0) };
    }
}

/// How long a picture taken as the Mac starts to change is still the one to fade from.
const PRIOR_FRESH: Duration = Duration::from_secs(1);

/// Whether the setting follows the Mac, so its appearance change is worth taking pictures for.
static FOLLOWS_MAC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

thread_local! {
    /// The numbers of the windows that fade (they opened through `warm_capture`).
    static FADING_WINDOWS: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    /// Each window's picture taken just before the Mac's appearance change lands, and when:
    /// `fade_begin` runs after AppKit has switched the title bar and the sidebar material, so a
    /// picture taken then would hold the new ones.
    static PRIOR: RefCell<HashMap<isize, (std::time::Instant, Picture)>> = RefCell::default();
}

/// Called when the setting changes: whether it follows the Mac. Pictures taken for a change
/// that no window will fade for are let go.
pub fn follow_mac(on: bool) {
    FOLLOWS_MAC.store(on, std::sync::atomic::Ordering::Relaxed);
    if !on {
        PRIOR.with(|all| all.borrow_mut().clear());
    }
}

/// The picture `store_prior_pictures` took of the window, if it is fresh.
fn take_prior(number: isize) -> Option<Picture> {
    let (taken, picture) = PRIOR.with(|all| all.borrow_mut().remove(&number))?;
    (taken.elapsed() < PRIOR_FRESH).then_some(picture)
}

/// A picture of every window that fades, kept for its `fade_begin`; at most one per window.
fn store_prior_pictures() {
    if !FOLLOWS_MAC.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(capture) = capture_window() else { return };
    // SAFETY: plain AppKit getters on `NSApp` and its windows, and a CoreGraphics image of each
    // visible one, on the main thread.
    unsafe {
        let app: Option<Retained<AnyObject>> = msg_send![class!(NSApplication), sharedApplication];
        let windows: Option<Retained<AnyObject>> = app.and_then(|app| msg_send![&*app, windows]);
        let Some(windows) = windows else { return };
        let count: usize = msg_send![&*windows, count];
        for ix in 0..count {
            let native: Option<Retained<AnyObject>> = msg_send![&*windows, objectAtIndex: ix];
            let Some(native) = native else { continue };
            let number: isize = msg_send![&*native, windowNumber];
            let visible: Bool = msg_send![&*native, isVisible];
            if !visible.as_bool() || !FADING_WINDOWS.with_borrow(|all| all.contains(&number)) {
                continue;
            }
            let image = capture(RECT_NULL, LIST_INCLUDING_WINDOW, number as u32, IMAGE_BEST_NO_FRAME);
            if !image.is_null() {
                PRIOR.with(|all| all.borrow_mut().insert(number, (std::time::Instant::now(), Picture(image))));
            }
        }
    }
}

/// KVO's call on `NSApp.effectiveAppearance`, which comes twice for a change: before (flagged
/// `notificationIsPrior`) and after. Only the first takes pictures.
extern "C-unwind" fn appearance_will_change(
    _this: *mut AnyObject,
    _sel: Sel,
    _path: *mut AnyObject,
    _object: *mut AnyObject,
    change: *mut AnyObject,
    _context: *mut std::ffi::c_void,
) {
    let Some(key) = ns_string(c"notificationIsPrior") else { return };
    // SAFETY: KVO hands over its change dictionary.
    let prior: Option<Retained<AnyObject>> = unsafe { msg_send![change, objectForKey: &*key] };
    if prior.is_some() {
        store_prior_pictures();
    }
}

/// Called once when a window opens: observes the app's appearance with KVO's `prior` option, so
/// the pictures of the theme fade (`fade_begin`) are taken before AppKit switches the title bar
/// and the sidebar material, not after. Only the first call acts.
pub fn watch_appearance() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(mut builder) = objc2::runtime::ClassBuilder::new(c"FinchAppearanceWatch", class!(NSObject)) else { return };
        // SAFETY: the method has KVO's observer signature; the observer lives as long as the app.
        unsafe {
            builder.add_method(
                sel!(observeValueForKeyPath:ofObject:change:context:),
                appearance_will_change
                    as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject, *mut std::ffi::c_void),
            );
            let class = builder.register();
            let observer: Retained<AnyObject> = msg_send![class, new];
            let app: Option<Retained<AnyObject>> = msg_send![class!(NSApplication), sharedApplication];
            let (Some(app), Some(key)) = (app, ns_string(c"effectiveAppearance")) else { return };
            // `NSKeyValueObservingOptionPrior`.
            let _: () = msg_send![&*app, addObserver: &*observer, forKeyPath: &*key, options: 4usize, context: std::ptr::null_mut::<std::ffi::c_void>()];
            std::mem::forget(observer);
        }
    });
}

thread_local! {
    /// Each window's picture of its old look while a theme fade runs, with the serial of the
    /// switch that made it: a switch's timer takes off its own picture only.
    static FADES: RefCell<HashMap<isize, (u64, Retained<AnyObject>)>> = RefCell::default();
    static FADE_SERIAL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn ns_string(text: &std::ffi::CStr) -> Option<Retained<AnyObject>> {
    // SAFETY: `NSString` from a NUL-terminated string.
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

/// The control points of `motion::ease_out` (SwiftUI's ease-out, (0, 0, 0.58, 1)), for the CA fade, which can't call it.
const EASE_OUT_POINTS: [f32; 4] = [0., 0., 0.58, 1.];

/// Whether the window is on screen to be seen: the app isn't hidden and the window isn't minimised
/// or ordered out. A covered window still counts (it draws while covered, see `draw_when_covered`).
fn shows_now(window: &Window) -> bool {
    let Some(view) = native_view(window) else { return false };
    // SAFETY: plain AppKit getters on the window GPUI owns and on `NSApp`, on the main thread.
    unsafe {
        let app: Option<Retained<AnyObject>> = msg_send![class!(NSApplication), sharedApplication];
        let hidden: bool = app.is_some_and(|app| msg_send![&*app, isHidden]);
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let Some(native) = native else { return false };
        let mini: bool = msg_send![&*native, isMiniaturized];
        let visible: bool = msg_send![&*native, isVisible];
        !hidden && !mini && visible
    }
}

/// Debug only: with `GPUI_EDITOR_FADE_LOG` set, a theme fade's steps go to stderr with a clock.
pub fn fade_trace(what: &str) {
    if std::env::var_os("GPUI_EDITOR_FADE_LOG").is_some() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0., |d| d.as_secs_f64());
        eprintln!("[fade {now:.3}] {what}");
    }
}

/// The first half of a theme switch, before the new colors are set: a `CGWindowListCreateImage`
/// of the window (the one taken as the Mac's change began, if there is a fresh one) goes into a
/// layer over it, at full opacity, backed by the old chrome ground
/// `(r, g, b)` so a see-through part of the window can't dip to grey. A CATransition can't fade
/// GPUI's Metal layer, so the new theme is drawn once underneath and `fade_run` fades the picture.
/// Returns the picture's serial, which `fade_end` takes; a picture still up from an earlier
/// switch goes off here, as the new one is taken over it.
pub fn fade_begin(window: &Window, ground: (f32, f32, f32)) -> Option<u64> {
    // Taken first, so a window that skips its fade lets go of its picture too.
    let prior = window_number(window).and_then(|(_, number)| take_prior(number));
    // A window already in its close fade is going away; a picture over it would never be taken off.
    if is_closing(window.window_handle().window_id()) {
        return None;
    }
    if !shows_now(window) {
        fade_trace("skipped, window hidden");
        return None;
    }
    let (view, number) = window_number(window)?;
    let capture = capture_window()?;
    fade_end_number(number, None);
    // SAFETY: a CoreGraphics image and plain CoreAnimation calls on the main thread; the image and
    // the color are released once the layer holds them.
    unsafe {
        let image = match prior {
            Some(picture) => picture.into_raw(),
            None => capture(RECT_NULL, LIST_INCLUDING_WINDOW, number as u32, IMAGE_BEST_NO_FRAME),
        };
        if image.is_null() {
            return None;
        }
        let host: Option<Retained<AnyObject>> = msg_send![view, layer];
        let (Some(host), Some(gravity)) = (host, ns_string(c"resize")) else {
            CFRelease(image);
            return None;
        };
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let scale: f64 = native.map_or(2., |native| msg_send![&*native, backingScaleFactor]);
        let bounds: Rect = msg_send![view, bounds];
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
        let layer: Retained<AnyObject> = msg_send![class!(CALayer), new];
        let _: () = msg_send![&*layer, setFrame: bounds];
        let _: () = msg_send![&*layer, setContents: image as *mut AnyObject];
        let _: () = msg_send![&*layer, setContentsGravity: &*gravity];
        let _: () = msg_send![&*layer, setContentsScale: scale];
        let color = CGColorCreateSRGB(f64::from(ground.0), f64::from(ground.1), f64::from(ground.2), 1.);
        let _: () = msg_send![&*layer, setBackgroundColor: CgColor(color)];
        let _: () = msg_send![&*layer, setZPosition: 1000.0f64];
        let _: () = msg_send![&*host, addSublayer: &*layer];
        let _: () = msg_send![class!(CATransaction), commit];
        CFRelease(image);
        CFRelease(color);
        let serial = FADE_SERIAL.with(|next| {
            next.set(next.get() + 1);
            next.get()
        });
        FADES.with(|all| all.borrow_mut().insert(number, (serial, layer)));
        fade_trace(&format!("captured, serial {serial}"));
        Some(serial)
    }
}

/// The second half: fades the window's picture out over `seconds`, to be called once GPUI has
/// drawn the new theme. The layer stays (at zero opacity) until `fade_end`.
pub fn fade_run(window: &Window, seconds: f32) {
    let Some((_, number)) = window_number(window) else { return };
    let Some(layer) = FADES.with(|all| all.borrow().get(&number).map(|(_, layer)| layer.clone())) else { return };
    fade_trace("fade runs");
    let (Some(key), Some(forwards)) = (ns_string(c"opacity"), ns_string(c"forwards")) else { return };
    // SAFETY: plain CoreAnimation calls on the main thread.
    unsafe {
        let animation: Retained<AnyObject> = msg_send![class!(CABasicAnimation), animationWithKeyPath: &*key];
        let from: Retained<AnyObject> = msg_send![class!(NSNumber), numberWithDouble: 1.0f64];
        let to: Retained<AnyObject> = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
        let [x1, y1, x2, y2] = EASE_OUT_POINTS;
        let timing: *mut AnyObject = objc2::runtime::MessageReceiver::send_message(
            class!(CAMediaTimingFunction),
            sel!(functionWithControlPoints::::),
            (x1, y1, x2, y2),
        );
        let Some(timing) = Retained::retain(timing) else { return };
        let _: () = msg_send![&*animation, setFromValue: &*from];
        let _: () = msg_send![&*animation, setToValue: &*to];
        let _: () = msg_send![&*animation, setDuration: f64::from(seconds)];
        let _: () = msg_send![&*animation, setTimingFunction: &*timing];
        let _: () = msg_send![&*animation, setFillMode: &*forwards];
        let _: () = msg_send![&*animation, setRemovedOnCompletion: Bool::NO];
        let _: () = msg_send![&*layer, addAnimation: &*animation, forKey: &*key];
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
        let _: () = msg_send![&*layer, setOpacity: 0.0f32];
        let _: () = msg_send![class!(CATransaction), commit];
    }
}

/// Takes the window's picture layer off once its fade has run, if it is still the one `serial`
/// named: a later switch's picture is left to that switch's own timer.
pub fn fade_end(window: &Window, serial: u64) {
    if let Some((_, number)) = window_number(window) {
        fade_end_number(number, Some(serial));
    }
}

fn fade_end_number(number: isize, only: Option<u64>) {
    let taken = FADES.with(|all| {
        let mut all = all.borrow_mut();
        let held = all.get(&number).map(|(serial, _)| *serial);
        if only.is_some_and(|only| held != Some(only)) {
            return None;
        }
        all.remove(&number).map(|(_, layer)| layer)
    });
    if let Some(layer) = taken {
        // SAFETY: a CoreAnimation layer this module added, on the main thread.
        unsafe {
            let _: () = msg_send![&*layer, removeFromSuperlayer];
        }
        fade_trace(&format!("picture off, window {number}, {}", if only.is_some() { "at fade end" } else { "early (new switch or window gone)" }));
    }
}

/// Debug only: gives the window `light` or dark as its own appearance, as if the Mac had switched
/// while the setting follows the system, so the observer path can be exercised without touching
/// the Mac's setting.
pub fn force_appearance(window: &Window, light: bool) {
    let Some(view) = native_view(window) else { return };
    let name = if light { c"NSAppearanceNameAqua" } else { c"NSAppearanceNameDarkAqua" };
    // The Mac's change takes its pictures first (`watch_appearance`); this stands in for it.
    store_prior_pictures();
    // SAFETY: plain AppKit getters and setters on the window GPUI owns, on the main thread.
    unsafe {
        let native: Option<Retained<AnyObject>> = msg_send![view, window];
        let name = ns_string(name);
        let wanted: Option<Retained<AnyObject>> = name.and_then(|name| msg_send![class!(NSAppearance), appearanceNamed: &*name]);
        if let (Some(native), Some(wanted)) = (native, wanted) {
            let _: () = msg_send![&*native, setAppearance: &*wanted];
        }
    }
}
