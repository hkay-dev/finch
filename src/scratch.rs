//! The scratch window (finch#wjr7): the shortcut slides a small Untitled window down from the top
//! of the screen the pointer is on, ready to type; the shortcut again, Escape or Cmd-W slides it
//! away and gives the keyboard back to the app that was in front. Its text is a draft like any
//! Untitled page (`Document`'s draft), so nothing here saves.
//!
//! The window is made on demand and removed when it goes, never kept hidden, and only one exists at a
//! time. It is a `Workspace` flagged `scratch`, so a window geometry, tab list or panel width
//! it would write never reaches `settings.toml`. In a test launch (`GPUI_EDITOR_BACKGROUND`) it opens
//! behind everything, without activating the app or sliding, and closes at once.

use std::{cell::RefCell, ffi::c_void, path::Path};

use gpui_kit::*;
use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};

use crate::{
    app::display_frame,
    settings, start, theme, window_look,
    workspace::{self, Workspace},
};

/// The window's size in points.
const SIZE: (f32, f32) = (560., 280.);
/// The least it is resized to.
const SIZE_MIN: (f32, f32) = (320., 160.);
/// The gap between the top of the usable screen (under the menu bar) and the window.
const TOP_GAP: f32 = 8.;

struct Open {
    window: AnyWindowHandle,
    /// The app that had the keyboard when the shortcut was pressed, to give it back; none when that was Finch.
    previous: Option<Retained<AnyObject>>,
}

thread_local! {
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
}

/// Whether `window` is the scratch window.
pub fn is_scratch(window: &Window) -> bool {
    let id = window.window_handle().window_id();
    OPEN.with_borrow(|open| open.as_ref().is_some_and(|open| open.window.window_id() == id))
}

/// Whether the scratch window is open: one that went some other way (a quit, a tab dragged out)
/// is forgotten here.
pub fn is_open(cx: &App) -> bool {
    OPEN.with_borrow_mut(|open| {
        if open.as_ref().is_some_and(|held| !cx.windows().contains(&held.window)) {
            *open = None;
        }
        open.is_some()
    })
}

/// Opens the scratch window, or, when it is open, dismisses it: the shortcut and the control verb.
/// Says whether it is open after.
pub fn toggle(background: bool, cx: &mut App) -> bool {
    if is_open(cx) {
        dismiss(cx);
        false
    } else {
        open(background, cx);
        true
    }
}

/// Closes the scratch window the way Cmd-Shift-W closes any window: drafts written first, a failed
/// draft asks, then it slides away (`finish_close`). Nothing when none is open.
pub fn dismiss(cx: &mut App) {
    let Some(handle) = OPEN.with_borrow(|open| open.as_ref().map(|open| open.window)) else { return };
    // Deferred: the shortcut and Escape can come while the window is being updated.
    cx.defer(move |cx| {
        let Some(workspace) = workspace::open_workspaces(cx).into_iter().find(|(open, _)| *open == handle).map(|(_, workspace)| workspace) else {
            // The window is already going or gone.
            OPEN.with_borrow_mut(|open| *open = None);
            return;
        };
        handle.update(cx, |_, window, cx| workspace::close(workspace, window, cx)).ok();
    });
}

/// The scratch window is leaving (`workspace::finish_close`): hands the keyboard back to the app
/// that was in front, and forgets the window so the shortcut can open the next one.
pub fn leaving(window: &mut Window, cx: &mut App) {
    let Some(open) = OPEN.with_borrow_mut(Option::take) else {
        window_look::fade_out(window, cx);
        return;
    };
    // File > Show scratch window becomes Show again.
    cx.defer(crate::menu::refresh);
    window_look::slide_out(window, cx);
    if let Some(app) = open.previous {
        // SAFETY: NSRunningApplication's `activateWithOptions:` (0: no options), on the main thread.
        unsafe {
            let _: bool = msg_send![&*app, activateWithOptions: 0usize];
        }
    }
}

fn open(background: bool, cx: &mut App) {
    let previous = if background { None } else { frontmost_other_app() };
    let pointer = pointer();
    let display = cx.displays().into_iter().find(|display| display_frame(display.id()).contains(&pointer)).or_else(|| cx.primary_display());
    let visible = display.as_ref().map(|display| display.visible_bounds()).unwrap_or_else(|| Bounds::centered(None, size(px(1280.), px(800.)), cx));
    let window_size = size(px(SIZE.0).min(visible.size.width), px(SIZE.1).min(visible.size.height));
    let origin = point(visible.origin.x + (visible.size.width - window_size.width) / 2., visible.origin.y + px(TOP_GAP));
    let mut start = start::resolve(None, Path::new("/"), &settings::get(cx).explorer);
    start.file = None;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size: window_size })),
        display_id: display.map(|display| display.id()),
        titlebar: Some(TitlebarOptions { title: Some("Scratch".into()), appears_transparent: true, traffic_light_position: None }),
        app_owns_titlebar_drag: true,
        window_background: WindowBackgroundAppearance::Transparent,
        window_min_size: Some(size(px(SIZE_MIN.0), px(SIZE_MIN.1))),
        focus: !background,
        // Unshown, as every window is (`app::open_with`): `slide_in` orders it in.
        show: false,
        inactive_frame_interval: None,
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        window_look::free_frame_pacing(window, background);
        if !background {
            window_look::scratch_style(window);
            window_look::slide_in(window, cx);
        }
        theme::watch(window);
        cx.new(|cx| Workspace::new_scratch(start, window, cx))
    });
    match opened {
        Ok((handle, _)) => {
            OPEN.with_borrow_mut(|open| *open = Some(Open { window: handle.into(), previous }));
            // File > Show scratch window becomes Hide.
            cx.defer(crate::menu::refresh);
            if !background {
                cx.activate(true);
            }
        }
        Err(err) => eprintln!("finch: can't open the scratch window: {err}"),
    }
}

/// The pointer's place in global screen points, top-left origin, the space `display_frame` is in.
fn pointer() -> Point<Pixels> {
    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
    }
    // SAFETY: a CGEvent made with no source, read for its location and released, once.
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return point(px(0.), px(0.));
        }
        let at = CGEventGetLocation(event);
        CFRelease(event);
        point(px(at.x as f32), px(at.y as f32))
    }
}

/// The app that has the keyboard now, unless that is Finch.
fn frontmost_other_app() -> Option<Retained<AnyObject>> {
    // SAFETY: NSWorkspace's `sharedWorkspace`, `frontmostApplication` and NSRunningApplication's
    // `processIdentifier`, on the main thread.
    unsafe {
        let workspace: Option<Retained<AnyObject>> = msg_send![class!(NSWorkspace), sharedWorkspace];
        let front: Option<Retained<AnyObject>> = msg_send![&*workspace?, frontmostApplication];
        let front = front?;
        let pid: i32 = msg_send![&*front, processIdentifier];
        (pid as u32 != std::process::id()).then_some(front)
    }
}
