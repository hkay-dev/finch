//! What the Mac decides about an editor's look: the gutter's type, its ink, and
//! the selection's color (the system accent, following System Settings).

use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicBool, Ordering},
};

use gpui::{Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Hsla, Pixels, Rgba, Window, px};

/// Set by the app's debug socket in a background launch: the editor draws as if its window were key.
static DRAW_AS_KEY: AtomicBool = AtomicBool::new(false);

/// Makes every `window_key` read true (or back to the window's own state).
pub fn set_draw_as_key(on: bool) {
    DRAW_AS_KEY.store(on, Ordering::Relaxed);
}

/// Whether `window` draws as the key window: it is, or `set_draw_as_key` says so. One atomic load.
pub fn window_key(window: &Window) -> bool {
    window.is_window_active() || DRAW_AS_KEY.load(Ordering::Relaxed)
}

/// A line number is `subheadline` (ui-standards, Type: meta is 11).
pub(super) const GUTTER_SIZE: Pixels = px(11.);

/// The system face with tabular figures: digits of one width, so a column of
/// numbers stays flush as they change.
pub(super) fn gutter_font() -> Font {
    static FONT: LazyLock<Font> = LazyLock::new(|| Font {
        family: ".SystemUIFont".into(),
        features: FontFeatures(Arc::new(vec![("tnum".into(), 1)])),
        fallbacks: None::<FontFallbacks>,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    });
    FONT.clone()
}

/// A text ink at `alpha` over the theme's text color, on AppKit's label greys
/// (ui-standards, Dark and light themes): dark lifts the alpha by `1 - (1 - a)^1.25`;
/// light takes the text color 0.55 toward black first, then `1 - (1 - a)^1.85`.
pub(super) fn ink(foreground: Hsla, alpha: f32, dark: bool) -> Hsla {
    if dark {
        Hsla {
            a: foreground.a * (1. - (1. - alpha).powf(1.25)), // mirrors tokens::INK_GAMMA_DARK
            ..foreground
        }
    } else {
        Hsla {
            l: foreground.l * (1. - 0.55), // mirrors tokens::INK_TOWARD_BLACK
            a: foreground.a * (1. - (1. - alpha).powf(1.85)), // mirrors tokens::INK_GAMMA_LIGHT
            ..foreground
        }
    }
}

/// The text selection's fill. In the key window it is the system accent at 0.4
/// (dark) or 0.3 (light); anywhere else the unemphasized selection, grey in the
/// text's ink at 0.07 (dark) or 0.098 (light, the fill scaled by 1.4), so it
/// stays visible. `fallback` stands in where there is no system accent.
pub(super) fn selection_fill(foreground: Hsla, fallback: Hsla, dark: bool, key: bool) -> Hsla {
    if !key {
        return crate::theme_tokens::unemphasized_fill(foreground, dark);
    }
    Hsla {
        a: if dark {
            0.4 // mirrors tokens::ALPHA_TEXT_SELECTION
        } else {
            0.3 // mirrors tokens::ALPHA_TEXT_SELECTION_LIGHT
        },
        ..system_accent().unwrap_or(fallback)
    }
}

/// `NSColor.controlAccentColor` in sRGB, read each time it is asked for, so a
/// change in System Settings shows at the next draw.
#[cfg(target_os = "macos")]
fn system_accent() -> Option<Hsla> {
    use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};

    // SAFETY: plain AppKit getters on the main thread (paint runs there); each
    // reply is checked for nil before the next message.
    unsafe {
        let accent: Option<Retained<AnyObject>> = msg_send![class!(NSColor), controlAccentColor];
        let space: Option<Retained<AnyObject>> = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let accent: Option<Retained<AnyObject>> =
            msg_send![&*accent?, colorUsingColorSpace: &*space?];
        let accent = accent?;
        let (mut red, mut green, mut blue, mut alpha) = (0f64, 0f64, 0f64, 0f64);
        let _: () = msg_send![
            &*accent,
            getRed: &mut red,
            green: &mut green,
            blue: &mut blue,
            alpha: &mut alpha
        ];
        Some(
            Rgba {
                r: red as f32,
                g: green as f32,
                b: blue as f32,
                a: 1.,
            }
            .into(),
        )
    }
}

#[cfg(not(target_os = "macos"))]
fn system_accent() -> Option<Hsla> {
    None
}
