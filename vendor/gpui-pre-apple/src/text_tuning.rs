//! Text rendering knobs the app sets once at launch (gpui-editor#1sp6).
//!
//! The thickening level is read for every glyph and the gamma and contrast for
//! every frame, so they're plain relaxed atomics. Until the app sets them, text
//! draws as upstream does.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};

/// Thickening picked from the text color's brightness, as upstream does: white
/// text gets level 4 and black text none.
pub const THICKEN_FROM_COLOR: u8 = u8::MAX;

/// Gamma that leaves glyph coverage as Core Graphics drew it.
pub const GAMMA_NONE: f32 = 1.0;

/// Contrast that leaves glyph coverage as Core Graphics drew it.
pub const CONTRAST_NONE: f32 = 0.0;

static THICKEN: AtomicU8 = AtomicU8::new(THICKEN_FROM_COLOR);
static GAMMA: AtomicU32 = AtomicU32::new(GAMMA_NONE.to_bits());
static CONTRAST: AtomicU32 = AtomicU32::new(CONTRAST_NONE.to_bits());

/// Sets Core Graphics font smoothing for every glyph, whatever its color: 0 is
/// off, 1 to 4 thicken more at each step (higher values are clamped to 4), and
/// `THICKEN_FROM_COLOR` goes back to upstream's choice. A set level ignores the
/// app's `AppleFontSmoothing` preference. The level is part of each glyph's
/// cache key, so glyphs drawn before a change are drawn again.
pub fn set_thicken(level: u8) {
    let level = if level == THICKEN_FROM_COLOR {
        level
    } else {
        level.min(4)
    };
    THICKEN.store(level, Relaxed);
}

pub fn thicken() -> u8 {
    THICKEN.load(Relaxed)
}

/// Sets the gamma of the glyph shader's coverage correction (GPUI's Windows
/// and Linux renderers use 1.0 to 2.2). `GAMMA_NONE` turns it off.
pub fn set_gamma(gamma: f32) {
    GAMMA.store(gamma.to_bits(), Relaxed);
}

pub fn gamma() -> f32 {
    f32::from_bits(GAMMA.load(Relaxed))
}

/// Sets how much the glyph shader darkens coverage for light text on a dark
/// background (GPUI's Windows and Linux renderers use 0.0 to 1.0).
/// `CONTRAST_NONE` turns it off.
pub fn set_contrast(contrast: f32) {
    CONTRAST.store(contrast.to_bits(), Relaxed);
}

pub fn contrast() -> f32 {
    f32::from_bits(CONTRAST.load(Relaxed))
}
