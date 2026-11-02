//! View > Text style: the font and rows an editor draws with, by kind of file.
//!
//! Prose (Markdown and plain text) and code each remember a style (`[editor] prose_style` and
//! `code_style`). Mono is today's look: `[editor] font_family` at the weight in `[text]`. Serif and
//! Sans are the Mac's own fonts, nothing bundled, at regular weight: their family names start with a
//! dot, which gpui-pre-apple draws without thickening (`text_system.rs`, the system UI font's rule).

use gpui_kit::{FontWeight, SharedString};

use crate::settings::{Settings, Typeface};

/// New York, the system serif. `.New York` and `New York` don't resolve through CoreText's family
/// lookup (they fall back to Times New Roman and Helvetica); this alias lists New York's weights.
const SERIF_FAMILY: &str = ".AppleSystemUIFontSerif";
/// GPUI's name for the system UI font (SF Pro): `font_name_with_fallbacks` maps it to `.AppleSystemUIFont`.
const SANS_FAMILY: &str = ".SystemUIFont";
/// Rows of a proportional style, in font sizes; mono keeps the editor's own 1.5.
const READING_LINE_HEIGHT: f32 = 1.65;

impl Typeface {
    /// The family to set on the editor; `None` keeps GPUI Kit's editor font. `mono` is `EditorSettings::drawn_family`.
    pub fn family(self, mono: Option<SharedString>) -> Option<SharedString> {
        match self {
            Self::Mono => mono,
            Self::Serif => Some(SERIF_FAMILY.into()),
            Self::Sans => Some(SANS_FAMILY.into()),
        }
    }

    /// The weight to draw at: the setting's for mono, regular for the system fonts.
    pub fn weight(self, mono: f32) -> FontWeight {
        match self {
            Self::Mono => FontWeight(mono),
            Self::Serif | Self::Sans => FontWeight::NORMAL,
        }
    }

    /// Row height as a share of the font size; `None` keeps the editor's own.
    pub fn line_height(self) -> Option<f32> {
        match self {
            Self::Mono => None,
            Self::Serif | Self::Sans => Some(READING_LINE_HEIGHT),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Mono => "Mono",
            Self::Serif => "Serif",
            Self::Sans => "Sans",
        }
    }
}

/// Whether a file of `language` (`Document::language`) is prose: Markdown and plain text.
pub fn is_prose(language: &str) -> bool {
    crate::word_count::counts_words(language)
}

/// The style a file of the given kind draws in.
pub fn of(settings: &Settings, prose: bool) -> Typeface {
    if prose { settings.editor.prose_style } else { settings.editor.code_style }
}
