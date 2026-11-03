//! The look's shared numbers: every size, alpha and motion length the views use.
//! Views take their values from here instead of writing literals; a value that
//! is settings-driven (panel widths, the editor font size) or computed at paint
//! time keeps its `px(` in the view with a trailing `// design: <why>` comment.
//!
//! `scripts/tokens.sh` prints this file as the table in `docs/design/system.md`,
//! so keep one `///` line directly above each `pub const` and the value on the
//! same line as the const. Light-theme values sit next to their dark twin as
//! `<NAME>_LIGHT`. Text colors come from the theme (the sidebar foreground, the
//! muted foreground) at the alphas below; the code palettes (and the Settings window's pane tile colors) are the only places a color is
//! written out, the palettes as `PALETTE_<SCHEME>_<DARK|LIGHT>` tables that `theme::set` lays over the highlight theme.
//!
//! New tokens go under the section they belong to; a new surface adds a section.
//! Nothing here draws.

#![allow(dead_code)]

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use gpui_kit::{FontFeatures, Hsla, Pixels, hsla, px};

// ---- Type: sizes in points ----

/// Tree row names: body, 13 pt (the sidebar's base size, as in Harrier's `TEXT_BODY`).
pub const TEXT_BODY: Pixels = px(13.);
/// Line box of a tree row name.
pub const TEXT_BODY_LINE: Pixels = px(16.);
/// Explorer header (the folder's name) and tree placeholders ("Loading", "Empty"): subheadline, 11 pt (was 11.5, off the scale).
pub const TEXT_SUBHEADLINE: Pixels = px(11.);
/// Line box of a tree placeholder.
pub const TEXT_SUBHEADLINE_LINE: Pixels = px(14.);

// ---- Spacing, sizes and radii ----

/// Explorer row pitch: a file tree is denser than Harrier's 30 pt tab rows.
pub const ROW_HEIGHT: Pixels = px(22.);
/// Extra indent per tree depth level: one icon's width, so a child's chevron sits under its parent's icon.
pub const ROW_INDENT: Pixels = px(16.);
/// Row content leading: the list's 6 pt inset plus this lands on the header's 14.
pub const ROW_LEADING: Pixels = px(8.);
/// Row content trailing inset.
pub const ROW_TRAILING: Pixels = px(8.);
/// Gap between a row's icon and its name.
pub const ROW_GAP: Pixels = px(6.);
/// Corner radius of a tree row's wash.
pub const RADIUS_ROW: Pixels = px(8.);
/// How far a row's wash (selection, hover, drop) sits in from the row's top and bottom, so two washes on neighbouring rows leave a 2 pt gap.
pub const ROW_WASH_INSET_Y: Pixels = px(1.);
/// Corner radius of the name field open in a tree row (rename, new item).
pub const RADIUS_FIELD: Pixels = px(5.);
/// Corner radius of a card that floats off a row: the drag card. The refused-name card hangs under the name field and takes `RADIUS_FIELD` so the two read as one piece.
pub const RADIUS_FLOAT_CARD: f32 = 8.;
/// How far the name field sits inside its row's selection wash, on every side it touches.
pub const FIELD_INSET: Pixels = px(1.);
/// The focus ring of that name field: a band inside the field's edge, since the row leaves 1 pt around it and the text starts 3 pt in.
pub const ROW_FIELD_RING: Pixels = px(2.);
/// The disclosure chevron: 9 pt, one size app-wide.
pub const CHEVRON: Pixels = px(9.);
/// The row's leading slot the chevron sits centered in, the folder's hit area: 16 pt, the indent.
pub const CHEVRON_SLOT: Pixels = px(16.);
/// A row's file or folder icon (Finder's), 16 pt.
pub const ICON: Pixels = px(16.);
/// An indent guide's width under an open folder.
pub const GUIDE_WIDTH: Pixels = px(1.);
/// The unsaved-changes dot after a file name.
pub const DOT: Pixels = px(6.);
/// The copy badge on a file drag card while Option is held, Finder's green plus: its diameter.
pub const DRAG_BADGE: Pixels = px(14.);
/// How far the copy badge hangs past the drag card's top right corner, on both sides.
pub const DRAG_BADGE_OVERHANG: Pixels = px(5.);
/// The copy badge's plus: arm length and stroke width.
pub const DRAG_BADGE_PLUS: [f32; 2] = [7., 1.5];
/// Horizontal inset of the tree list inside the explorer.
pub const TREE_INSET: Pixels = px(6.);
/// Explorer header's side inset.
pub const HEADER_INSET: Pixels = px(14.);
/// Explorer header height below the top band.
pub const HEADER_HEIGHT: Pixels = px(28.);
/// Top band along the window's top that the traffic lights sit in: the explorer header clears it, the editor's top inset is this tall, and dragging it moves the window.
pub const TOP_BAND: Pixels = px(52.);
/// The compact band over a pane whose top isn't the window's top (the lower pane of a split down): the 28 pt tab with 6 pt above and below. The tab bar, the editor's top inset, the focused pane's edge and the drop zones use it in place of `TOP_BAND`.
pub const PANE_BAND: Pixels = px(40.);
/// The editor's top inset in Minimal mode, with the tab bar gone: a little room over the first line, reached by the same ease as the bar leaving.
pub const MINIMAL_TOP: Pixels = px(28.);
/// The strip along the window's top edge that brings the tabs and window buttons back in Minimal mode while the pointer is in it.
pub const MINIMAL_EDGE: Pixels = px(8.);
/// Two Escapes this close together (in seconds) leave Minimal mode.
pub const MINIMAL_ESCAPE: f32 = 0.5;
/// A side panel can't be dragged narrower than this.
pub const PANEL_WIDTH_MIN: Pixels = px(120.);
/// A side panel can't be dragged wider than this.
pub const PANEL_WIDTH_MAX: Pixels = px(640.);
/// The editor keeps at least this much of the window's width when the explorer and the preview are open: the preview gives way first, then the explorer, and the window can't be made narrower than `PANEL_WIDTH_MIN` plus this.
pub const EDITOR_MIN: Pixels = px(360.);
/// The window can't be made shorter than this (the height floor a saved size already had).
pub const WINDOW_MIN_HEIGHT: Pixels = px(200.);
/// The preview panel's inset on its left, right and bottom (the top is the top band).
pub const PREVIEW_INSET: Pixels = px(12.);
/// The preview's prose sits at most this far inside the panel's own inset on its left and right, as the editor's text sits inside its pane (about 24 pt); the panel frame, scrollbar and top band keep `PREVIEW_INSET`. It reaches this at the default panel width (420 pt) and narrows with the panel to `PREVIEW_READING_INSET_MIN`.
pub const PREVIEW_READING_INSET: Pixels = px(24.);
/// The least the preview's prose sits inside the panel's inset, at the narrowest panel (`PANEL_WIDTH_MIN`), where the column would otherwise be about 48 pt.
pub const PREVIEW_READING_INSET_MIN: Pixels = px(8.);
/// The least the prose sits from the panel's right: the scrollbar's track, so the track never covers a line's end or takes its clicks.
pub const PREVIEW_READING_INSET_RIGHT_MIN: Pixels = SCROLL_TRACK;
/// The reading inset as a share of the preview's width inside the panel's inset (24 of the default 396 pt), held between the two insets above.
pub const PREVIEW_READING_SHARE: f32 = 0.0606;
/// A top band icon button's square hit area, centered on the traffic lights' line.
pub const ICON_BUTTON: Pixels = px(22.);
/// A top band icon button's symbol: window chrome, 14 pt.
pub const ICON_BUTTON_SYMBOL: f32 = 14.;
/// A top band icon button's distance from the window's right edge.
pub const ICON_BUTTON_INSET: Pixels = px(12.);
/// The explorer button's distance from the window's left edge: right of the traffic lights, so it ends at 112; in full screen, where there are no lights, it sits `ICON_BUTTON_INSET` from the edge.
pub const EXPLORER_BUTTON_LEFT: Pixels = px(90.);
/// An explorer header icon button's symbol, 13 pt (the header's buttons sit closer together than the top band's).
pub const HEADER_BUTTON_SYMBOL: f32 = 13.;
/// Corner radius of an explorer header button's and root name's wash.
pub const RADIUS_BUTTON: Pixels = px(6.);
/// The small chevron after the root's name in the explorer header, in points.
pub const HEADER_CHEVRON: f32 = 8.;
/// Gap between the explorer header's pieces; also the root name's wash padding on each side.
pub const HEADER_GAP: Pixels = px(6.);
/// Gap between the root's name and its chevron.
pub const HEADER_CHEVRON_GAP: Pixels = px(3.);

// A hairline (the tab bar's rule, a divider) is one device pixel, `1 / scale factor` points,
// in the theme's foreground at `ALPHA_HAIRLINE`; the window's scale factor gives the points.

// ---- Tabs: the bar in the top band, right of the explorer ----

/// A tab's height; the tabs sit centered in the top band, on the traffic lights' line.
pub const TAB_HEIGHT: Pixels = px(28.);
/// The room a drag from elsewhere opens where it would land, and a lifted tab's width before its place is known.
pub const TAB_DROP_ROOM: Pixels = px(120.);
/// A tab in a strip narrower than the tab shrinks to the room but never under this: the file symbol and a cut title that still reads ("n…-3.md").
pub const TAB_FLOOR_WIDTH: Pixels = px(116.);
/// A tab is as wide as its icon, name and mark, and never grows past this width (finch#x7jk).
pub const TAB_MAX_WIDTH: Pixels = px(200.);
/// How far (vertically) a dragged tab is held from its bar before its slot folds closed behind the card.
pub const TAB_FOLD_AWAY: Pixels = px(60.);
/// How near the bar the dragged tab comes back before its folded slot opens again.
pub const TAB_FOLD_BACK: Pixels = px(45.);
/// A tab's padding before its icon slot: with the icon's own inset the icon sits 8 pt in (finch#x7jk).
pub const TAB_PAD_LEAD: Pixels = px(7.);
/// A tab's padding after its title (or its mark): with the title's own pad, 11 pt after the name (finch#x7jk).
pub const TAB_PAD_TRAIL: Pixels = px(5.);
/// The gap between two tabs. No line sits in it: the tabs are words with a plate under the active one (finch#x7jk).
pub const TAB_GAP: Pixels = px(4.);
/// Space each side of a tab's title, between it and the icon before it and the mark after it.
pub const TAB_TITLE_PAD: Pixels = px(6.);
/// The bar's inset on its left, and the gap before the preview button on its right.
pub const TAB_BAR_INSET: Pixels = px(8.);
/// A tab's corner radius.
pub const RADIUS_TAB: Pixels = px(7.);
/// The square slot at a tab's left that holds its file icon and, under the pointer, the close button.
pub const TAB_CLOSE_SLOT: Pixels = px(16.);
/// The close button's hit and wash: a square a little larger than its slot, so it is easy to land on; it reaches 2 pt out each side of the slot and moves nothing.
pub const TAB_CLOSE_HIT: Pixels = px(20.);
/// A tab's close symbol, in points.
pub const TAB_CLOSE_SYMBOL: f32 = 9.;
/// The slot after a tab's name for its unsaved dot, lock, warning or pin; the tab grows by it while one shows (finch#x7jk).
pub const TAB_TRAIL_SLOT: Pixels = px(10.);
/// A tab's file icon, in points: the explorer's tinted symbol, a little smaller than its rows', centered in the close slot.
pub const TAB_ICON: f32 = 13.;
/// The dot at the corner of a locked tab's lock that says the file has unsaved edits: its size.
pub const TAB_LOCK_DOT: Pixels = px(5.);
/// How far that dot sits in from the top and right of the tab's mark slot, over the lock's corner.
pub const TAB_LOCK_DOT_INSET: Pixels = px(0.);
/// The warning sign on a tab whose file changed on disk, in points.
pub const TAB_WARNING_SYMBOL: f32 = 11.;
/// The active tab's plate on a dark theme: its contact shadow (black alpha, y, blur), as the segmented plate's (finch#x7jk).
pub const TAB_PLATE_SHADOW: [f32; 3] = [0.35, 1., 1.5];
/// The plate's contact shadow on a light theme, a half-point shadow like a push button's.
pub const TAB_PLATE_SHADOW_LIGHT: [f32; 3] = [0.2, 0.5, 1.5];
/// The plate's rim: black at this alpha, half a point wide, on a dark theme.
pub const TAB_PLATE_RIM: f32 = 0.3;
/// The plate's rim on a light theme.
pub const TAB_PLATE_RIM_LIGHT: f32 = 0.1;
/// How much of its shadow and lit top the plate keeps when the pane doesn't hold the keys: it flattens, as the segmented plate does out of key.
pub const TAB_PLATE_UNFOCUSED: f32 = 0.5;
/// The tab bar's band on a light theme: a step under the white page, so the active tab's white plate reads (finch#x7jk).
pub const TAB_BAND_LIGHT: u32 = 0xF6F6F6;
/// Room around the tab strip's tabs for the plate's shadow and rim, which the strip's scroll clip would cut: padding above and below,
/// and layout room before the first tab and after the last (finch#zrt1). The strip starts this much early, so the tabs stay put.
pub const TAB_PLATE_ROOM: Pixels = px(3.);
/// The tab bar's left clearance for the traffic lights and the explorer button while the explorer is hidden: the button ends at 112, and 8 pt more is the bar's usual inset.
pub const TAB_LIGHTS_CLEAR: Pixels = px(120.);
/// The space between the strip and the tab list button at its right end.
pub const TAB_LIST_GAP: Pixels = px(4.);
/// The tab list button's chevron, in points.
pub const TAB_LIST_SYMBOL: f32 = 10.;
/// The tab being dragged, left in the bar, at this alpha.
pub const ALPHA_TAB_DRAGGED: f32 = 0.35;
/// The folder after a tab's name (when two tabs share the name), as a share of the name's ink.
pub const TAB_FOLDER_FADE: f32 = 0.7;
/// The title an Untitled page takes from its first line, as a share of a file name's ink.
pub const TAB_LINE_TITLE_FADE: f32 = 0.8;
/// The space between a tab's name and the folder after it.
pub const TAB_FOLDER_GAP: Pixels = px(5.);

// ---- Preview prose: the markdown preview's type, as in Harrier's prose rules ----

/// Preview footnote text: 13 pt, a step under the 15 pt body so the notes read as notes.
pub const PREVIEW_FOOTNOTE: f32 = 13.;
/// Preview body text: 15 pt (title3), at `PREVIEW_LINE` leading.
pub const PREVIEW_BODY: f32 = 15.;
/// Preview body leading, a multiple of the body size.
pub const PREVIEW_LINE: f32 = 1.6;
/// The reading column of prose, in characters (Markdown and text files; the preview's column too).
pub const READING_MEASURE_CHARS: f32 = 70.;
/// The reading column of code, in characters, in a pane wider than 1,400 pt (`READING_WIDE_PANE` in gpui-base).
pub const READING_MEASURE_CODE_CHARS: f32 = 100.;
/// One character of the preview's body text, as a share of its size (the width of "0" in the system face).
pub const PREVIEW_BODY_ADVANCE_EM: f32 = 0.495;
/// The preview's text column holds to this width, `READING_MEASURE_CHARS` characters of body text, centered when the panel is wider.
pub const PREVIEW_MEASURE: Pixels = px(READING_MEASURE_CHARS * PREVIEW_BODY * PREVIEW_BODY_ADVANCE_EM);
/// Preview heading sizes for levels 1 to 4 (large title, title1, title2, title3); deeper levels take the last.
pub const PREVIEW_HEADINGS: [f32; 4] = [26., 22., 17., 15.];
/// Preview code (blocks and spans) as a share of the body size: 0.867 em, 13 pt at 15.
pub const PREVIEW_CODE_EM: f32 = 0.867;
/// Preview code block leading, a multiple of the code size.
pub const PREVIEW_CODE_LINE: f32 = 1.55;
/// An inline code span's weight in the preview. The code font is thickened by the renderer and the prose's system font is not, so a span at the editor's weight 500 draws heavier than the words around it; this lighter weight brings it level (finch#9kdz).
pub const PREVIEW_INLINE_CODE_WEIGHT: f32 = 400.;
/// A code block's fill: the foreground at this alpha (the card fill).
pub const ALPHA_CARD: f32 = 0.04;
/// A code block's and table's fill on a light theme: the foreground at this alpha, a faint grey on the white page.
pub const ALPHA_CARD_LIGHT: f32 = 0.045;
/// An inline code span's fill: the foreground at this alpha (the surface fill).
pub const ALPHA_SURFACE: f32 = 0.08;
/// Corner radius of a code block (the pane card).
pub const RADIUS_CARD: Pixels = px(6.);
/// Corner radius of an inline code span (the cell tint).
pub const RADIUS_TINT: Pixels = px(3.);
/// A table's hairline and a rule: the foreground at this alpha (border, separator).
pub const ALPHA_HAIRLINE: f32 = 0.12;
/// A quote's bar: the foreground at this alpha.
pub const ALPHA_QUOTE_BAR: f32 = 0.25;
/// A quote's bar width.
pub const QUOTE_BAR: Pixels = px(2.);
/// A callout's fill on a dark theme: the kind's colour at this alpha, a tint the ink still reads on.
pub const ALPHA_CALLOUT_DARK: f32 = 0.09;
/// A callout's fill on a light theme: the kind's colour at this alpha, a little firmer since a tint on white is paler.
pub const ALPHA_CALLOUT_LIGHT: f32 = 0.13;
/// A callout's left bar width, a quote's bar width.
pub const CALLOUT_BAR: Pixels = px(2.);
/// A callout's left bar: the kind's colour at this alpha, firmer than the fill so the bar reads as the kind's mark.
pub const ALPHA_CALLOUT_BAR: f32 = 0.85;
/// A callout's corner radius: a code block's, so cards in the preview round alike.
pub const RADIUS_CALLOUT: Pixels = RADIUS_CARD;
/// A link's underline thickness: a hairline, so a run of links reads as text first.
pub const LINK_UNDERLINE: Pixels = px(1.);
/// A link's underline: the link colour at this alpha, fainter than the text so links read as text first and as links second.
pub const ALPHA_LINK_UNDERLINE: f32 = 0.4;
/// System blue on a dark theme (`systemBlue`), for links.
pub const LINK_BLUE: u32 = 0x0A84FF;
/// A link's blue on a light theme: deeper than `systemBlue` (0x007AFF, 3.6:1 on the preview ground) so link text reaches 5.8:1 and the Note title 4.7:1 on its tinted fill; the Note callout shares it.
pub const LINK_BLUE_LIGHT: u32 = 0x005CC0;
/// System blue on a light theme (`systemBlue`), for the preview's selection, which stays the Mac's selection blue while links go deeper.
pub const SELECTION_BLUE_LIGHT: u32 = 0x007AFF;
/// Preview callout colours on a dark theme: Note, Tip, Important, Warning, Caution and a sixth kind (blue, green, purple, orange, red, teal); fill, bar and title.
pub const CALLOUT_DARK: [u32; 6] = [LINK_BLUE, 0x30D158, 0xBF5AF2, 0xFF9F0A, 0xFF453A, 0x40C8E0];
/// Preview callout colours on a light theme, deeper so the title reads on its pale fill; Note shares the link's deeper blue, and Tip's green (0x248A3D is 3.4:1 there) is deepened to 4.7:1 on its tinted fill, like the Note.
pub const CALLOUT_LIGHT: [u32; 6] = [LINK_BLUE_LIGHT, 0x186E2F, 0x8944AB, 0xB25000, 0xD70015, 0x00829B];
/// Selected text in the preview: the system blue at this alpha on a dark theme.
pub const ALPHA_TEXT_SELECTION: f32 = 0.4;
/// Selected text in the preview on a light theme.
pub const ALPHA_TEXT_SELECTION_LIGHT: f32 = 0.3;
/// The `<mark>` highlight on a dark theme: system yellow, which the pale light-theme yellow would make unreadable under light ink; laid at `ALPHA_MARK_DARK`.
pub const MARK_AMBER_DARK: u32 = 0xFFD60A;
/// How much of `MARK_AMBER_DARK` a `<mark>` takes over the dark ground, a tint that keeps light ink at 4.5:1 or better (5.0:1 for 0xD4D4D4 ink).
pub const ALPHA_MARK_DARK: f32 = 0.3;
/// The inset of a card (a code block, a failed diagram or formula, a missing picture's plate), on every side; the vendored text view's `CARD_INSET_PX` is the same 14 pt.
pub const CARD_INSET: Pixels = px(14.);

// ---- Fills and color: the foreground over the sidebar, as washes ----

/// Selected tree row while the tree has the keyboard and the window is key.
pub const ALPHA_SELECTED: f32 = 0.10;
/// Selected tree row while the tree isn't focused or the window isn't key (was 0.08 with a focused 0.14).
pub const ALPHA_SELECTED_UNFOCUSED: f32 = 0.07;
/// Row under the pointer.
pub const ALPHA_HOVER: f32 = 0.04;
/// A hidden (dot) file's icon, and its name in the dimmer text color (`ALPHA_DIM`).
pub const ALPHA_HIDDEN_ICON: f32 = 0.5;
/// The dimmer text color (`tertiaryLabelColor`): the muted foreground at this alpha, under `ALPHA_SECONDARY` so a hidden name reads dimmer than a normal one.
pub const ALPHA_DIM: f32 = 0.4;
/// A git status colour in the dark theme: the theme's red, magenta, yellow or green at this alpha, so the
/// saturated colors sit calmer on the `#1E1E1E` ground (all stay above 4.5:1 there; red is the closest, 4.7:1).
pub const ALPHA_GIT_DARK: f32 = 0.85;
/// A git status colour in the light theme: the theme's color mixed this far toward black. The kit's light
/// green and yellow are 3.3:1 and 2.9:1 on white; at this depth all four pass 4.5:1 (yellow is the
/// closest, about 5.5:1, so the colours stay as colorful as the kit allows).
pub const GIT_DEEPEN_LIGHT: f32 = 0.30;
/// How far a tree icon moves toward its git status colour (finch#nb5m): Finder's icon keeps its shape and
/// shading and wears the colour, where a flat fill would make every icon one silhouette.
pub const ICON_GIT_TINT: f32 = 0.6;
/// An indent guide at rest: the foreground at this alpha.
pub const ALPHA_GUIDE: f32 = 0.07;
/// The indent guide of the group holding the selection.
pub const ALPHA_GUIDE_LIT: f32 = 0.18;
/// An explorer header button under the pointer: the foreground at this alpha, its glyph at full ink.
pub const ALPHA_BUTTON_HOVER: f32 = 0.06;
/// An icon button (the tab close, the preview button, an explorer header button) and a header's root name while pressed: the same wash that fades up from `ALPHA_BUTTON_HOVER`.
pub const ALPHA_BUTTON_PRESSED: f32 = 0.14;
/// An explorer header button that can't run (Enclosing folder at the filesystem root): its glyph at this alpha.
pub const ALPHA_BUTTON_OFF: f32 = 0.4;

// ---- Dark and light themes: a light theme's washes are 1.4 times the dark ones, as in Harrier ----

/// `ALPHA_SELECTED` on a light theme.
pub const ALPHA_SELECTED_LIGHT: f32 = 0.14;
/// `ALPHA_SELECTED_UNFOCUSED` on a light theme.
pub const ALPHA_SELECTED_UNFOCUSED_LIGHT: f32 = 0.098;
/// `ALPHA_HOVER` on a light theme.
pub const ALPHA_HOVER_LIGHT: f32 = 0.056;
/// The fill of the name field open in a tree row: white at this alpha, a light wash over the selection on a dark theme.
pub const ALPHA_FIELD: f32 = 0.12;
/// `ALPHA_FIELD` on a light theme: opaque white.
pub const ALPHA_FIELD_LIGHT: f32 = 1.;

// ---- Theme: ink, ground and the switch fade ----

/// Secondary text and glyphs (`secondaryLabelColor`): the foreground at this alpha before the theme's gamma.
pub const ALPHA_SECONDARY: f32 = 0.55;
/// Tertiary text and glyphs (`tertiaryLabelColor`): the foreground at this alpha before the theme's gamma.
pub const ALPHA_TERTIARY: f32 = 0.25;
/// An alpha under this is a fill or a hairline and keeps the alpha as given; at or over it, `ink` applies the theme's gamma.
pub const INK_FLOOR: f32 = 0.2;
/// The exponent of a dark theme's ink curve, `1 - (1 - a)^1.25`: 0.25 gives 0.3, 0.55 gives 0.63, 0.85 gives 0.91 (AppKit's label greys).
pub const INK_GAMMA_DARK: f32 = 1.25;
/// The exponent of a light theme's ink curve, `1 - (1 - a)^1.85`, divided by the same curve at `ALPHA_PRIMARY` so primary is exactly `TEXT_LIGHT` (#454545) on white: tertiary 1.6:1, dim 2.9:1, secondary 5.3:1, primary 9.6:1.
pub const INK_GAMMA_LIGHT: f32 = 1.85;
/// How far toward black the vendored editor's own light ink moves its foreground (vendor/gpui-base system_look.rs mirrors it); the app's chrome ink no longer uses it, since a light theme's ink is `TEXT_LIGHT` itself.
pub const INK_TOWARD_BLACK: f32 = 0.55;
/// A theme switch, the Mac's or the setting's, fades every window over this many seconds.
pub const THEME_FADE: f32 = 0.3;

/// The ink for text and glyphs: `foreground` at `alpha` through the theme's gamma. An alpha under `INK_FLOOR` (a fill or a hairline) comes back as given.
pub fn ink(light: bool, foreground: Hsla, alpha: f32) -> Hsla {
    if alpha < INK_FLOOR {
        return Hsla { a: alpha, ..foreground };
    }
    if light {
        Hsla { h: 0., s: 0., l: (TEXT_LIGHT >> 16 & 0xFF) as f32 / 255., a: ink_alpha(true, alpha) }
    } else {
        Hsla { a: ink_alpha(false, alpha), ..foreground }
    }
}

/// `alpha` through the theme's ink curve (`INK_GAMMA_LIGHT` or `INK_GAMMA_DARK`), for text drawn outside the theme's colors, such as the exported page. The light curve is scaled so `ALPHA_PRIMARY` gives 1 (the full `TEXT_LIGHT`) and the lower alphas keep their steps below it.
pub fn ink_alpha(light: bool, alpha: f32) -> f32 {
    let curve = |a: f32, gamma: f32| 1. - (1. - a).powf(gamma);
    if light { (curve(alpha, INK_GAMMA_LIGHT) / curve(ALPHA_PRIMARY, INK_GAMMA_LIGHT)).min(1.) } else { curve(alpha, INK_GAMMA_DARK) }
}

/// The window's chrome ground, one per theme, AppKit's own text backgrounds (`textBackgroundColor`): white in the light, the
/// standard dark grey `#1E1E1E` in the dark, both neutral. Was a warm beige paper and a near-black; the Mac's
/// traditional look was wanted instead (finch#367y).
pub fn chrome_ground(light: bool) -> Hsla {
    if light { hsla(0., 0., 1., 1.) } else { hsla(0., 0., 30. / 255., 1.) }
}

/// The tab bar's band: the chrome ground on a dark theme, where the active tab's plate lifts by its fill and lit top, and
/// `TAB_BAND_LIGHT` on a light one, where the plate is white (finch#x7jk).
pub fn tab_band_ground(light: bool) -> Hsla {
    if light { gpui_kit::rgb(TAB_BAND_LIGHT).into() } else { chrome_ground(false) }
}

/// The text you read on a light theme (the editor's plain code and the preview's body): a neutral dark grey, about 9.6:1 on
/// white. It is also a light theme's primary ink (tab titles, tree names, status text, tooltips), which was near pure black
/// and read harsh (gpui-editor#3zj1); finch#e28a settled on about 9.5:1, kept here without the old warm tint (finch#367y),
/// and finch#sk5m made the chrome match it.
pub const TEXT_LIGHT: u32 = 0x454545;

// ---- Code colors: the editor's syntax palettes, one per scheme and appearance, laid over the highlight theme by `theme::set` ----

// Plain RRGGBB values, each scheme's own published hex values, taken from the theme's own files (the source is on each
// table's line). Xcode (the default) is Xcode's Default (Dark) and Default (Light) themes. A role the theme gives no
// color of its own takes the color its rules draw that code in, often its foreground. Plain code, fields' ink when `colored_roles` is off, variables and operators take
// the app's text ink at `ALPHA_PRIMARY`, and punctuation the secondary ink (`ALPHA_SECONDARY`), both through `ink`; no
// code role is bold. The ground under the code is the app's own (`chrome_ground`), not the scheme's.

/// One scheme's colors in one appearance, in the order of `Palette::of`: keyword, string, number, type, declared type
/// (interfaces, traits), function, macro, attribute, comment, link, then the colored roles: field, parameter, constant,
/// enum variant, `self`, built-in type, method, label or lifetime, string escape, Markdown list marker, Markdown quote mark.
/// With `colored_roles` off the roles after `link` fall back to the ones the grammar had before (constant to number,
/// variant and built-in type to type, `self` to keyword, method to function, escape to string).
#[derive(Clone, Copy)]
pub struct Palette {
    pub keyword: u32,
    pub string: u32,
    pub number: u32,
    pub type_: u32,
    pub declared_type: u32,
    pub function: u32,
    pub macro_: u32,
    pub attribute: u32,
    pub comment: u32,
    pub link: u32,
    pub field: u32,
    pub parameter: u32,
    pub constant: u32,
    pub variant: u32,
    pub self_: u32,
    pub builtin_type: u32,
    pub method: u32,
    pub label: u32,
    pub escape: u32,
    pub list_marker: u32,
    pub quote: u32,
}

impl Palette {
    const fn of(c: [u32; 21]) -> Self {
        Self {
            keyword: c[0],
            string: c[1],
            number: c[2],
            type_: c[3],
            declared_type: c[4],
            function: c[5],
            macro_: c[6],
            attribute: c[7],
            comment: c[8],
            link: c[9],
            field: c[10],
            parameter: c[11],
            constant: c[12],
            variant: c[13],
            self_: c[14],
            builtin_type: c[15],
            method: c[16],
            label: c[17],
            escape: c[18],
            list_marker: c[19],
            quote: c[20],
        }
    }
}

/// Xcode Default (Dark): the default scheme on a dark theme.
pub const PALETTE_XCODE_DARK: Palette = Palette::of([0xFC5FA3, 0xFC6A5D, 0xD0BF69, 0xD0A8FF, 0x5DD8FF, 0x67B7A4, 0xFD8F3F, 0xBF8555, 0x6C7986, 0x5482FF, 0x4EB0CC, 0x9AD6C4, 0x9686F5, 0x5DD8FF, 0xFF7AB2, 0xB281EB, 0x7FCDBA, 0xE0A06A, 0xFF8170, 0xD0BF69, 0x78C2B3]);
/// Xcode Default (Light): the default scheme on a light theme.
pub const PALETTE_XCODE_LIGHT: Palette = Palette::of([0x9B2393, 0xC41A16, 0x1C00CF, 0x3900A0, 0x0B4F79, 0x326D74, 0x643820, 0x815F03, 0x5D6C79, 0x0E0EFF, 0x0F68A0, 0x1F7A5F, 0x6C36A9, 0x0F7A8A, 0xA0285F, 0x5A2CA0, 0x2E7D6A, 0x8A5A00, 0x9E2A1D, 0x2E6DA4, 0x52796F]);
/// Atom One Dark.
pub const PALETTE_ONE_DARK: Palette = Palette::of([0xC678DD, 0x98C379, 0xD19A66, 0xE5C07B, 0x56B6C2, 0x61AFEF, 0x61AFEF, 0xD19A66, 0x5C6370, 0x61AFEF, 0xE06C75, 0xBE5046, 0xD19A66, 0x56B6C2, 0xE06C75, 0x56B6C2, 0x528BFF, 0xC678DD, 0x56B6C2, 0xE5C07B, 0x98C379]);
/// Atom One Light.
pub const PALETTE_ONE_LIGHT: Palette = Palette::of([0xA626A4, 0x50A14F, 0x986801, 0xC18401, 0x0184BC, 0x4078F2, 0x4078F2, 0x986801, 0xA0A1A7, 0x4078F2, 0xE45649, 0xCA1243, 0x986801, 0x0184BC, 0xE45649, 0x0184BC, 0x526FFF, 0xA626A4, 0x0184BC, 0xC18401, 0x50A14F]);
/// Catppuccin Mocha.
pub const PALETTE_CATPPUCCIN_DARK: Palette = Palette::of([0xCBA6F7, 0xA6E3A1, 0xFAB387, 0xF9E2AF, 0x89DCEB, 0x89B4FA, 0xF5C2E7, 0xF2CDCD, 0x6C7086, 0x74C7EC, 0xB4BEFE, 0xEBA0AC, 0xFAB387, 0x94E2D5, 0xF38BA8, 0x74C7EC, 0xF5E0DC, 0xF5C2E7, 0x94E2D5, 0xF9E2AF, 0x9399B2]);
/// Catppuccin Latte.
pub const PALETTE_CATPPUCCIN_LIGHT: Palette = Palette::of([0x8839EF, 0x40A02B, 0xFE640B, 0xDF8E1D, 0x04A5E5, 0x1E66F5, 0xEA76CB, 0xDD7878, 0x7C7F93, 0x209FB5, 0x7287FD, 0xE64553, 0xFE640B, 0x179299, 0xD20F39, 0x209FB5, 0xDC8A78, 0xEA76CB, 0x179299, 0xDF8E1D, 0x7C7F93]);
/// Tokyo Night (Night).
pub const PALETTE_TOKYO_NIGHT_DARK: Palette = Palette::of([0xBB9AF7, 0x9ECE6A, 0xFF9E64, 0x2AC3DE, 0x0DB9D7, 0x7AA2F7, 0x7DCFFF, 0xE0AF68, 0x565F89, 0x7DCFFF, 0x73DACA, 0xE0AF68, 0xFF9E64, 0x41A6B5, 0xF7768E, 0x0DB9D7, 0x89DDFF, 0xBB9AF7, 0x89DDFF, 0xFF9E64, 0x73DACA]);
/// Tokyo Night Day.
pub const PALETTE_TOKYO_NIGHT_LIGHT: Palette = Palette::of([0x9854F1, 0x587539, 0xB15C00, 0x007197, 0x188092, 0x2E7DE9, 0x188092, 0x8C6C3E, 0x848CB5, 0x2E7DE9, 0x118C74, 0x8C6C3E, 0xB15C00, 0x007197, 0xF52A65, 0x188092, 0x7847BD, 0x9854F1, 0x118C74, 0x8C6C3E, 0x587539]);
/// Dracula.
pub const PALETTE_DRACULA_DARK: Palette = Palette::of([0xFF79C6, 0xF1FA8C, 0xBD93F9, 0x8BE9FD, 0x8BE9FD, 0x50FA7B, 0xFFB86C, 0x50FA7B, 0x6272A4, 0x8BE9FD, 0x8BE9FD, 0xFFB86C, 0xBD93F9, 0xFF5555, 0xBD93F9, 0x8BE9FD, 0x50FA7B, 0xFF79C6, 0xFF79C6, 0xFFB86C, 0x8BE9FD]);
/// Alucard (Dracula's light theme).
pub const PALETTE_DRACULA_LIGHT: Palette = Palette::of([0xA3144D, 0x846E15, 0x644AC9, 0x036A96, 0x036A96, 0x14710A, 0xA34D14, 0x14710A, 0x6C664B, 0x036A96, 0x036A96, 0xA34D14, 0x644AC9, 0xCB3A2A, 0x644AC9, 0x036A96, 0x14710A, 0xA3144D, 0xA3144D, 0xA34D14, 0x036A96]);
/// GitHub Dark Default. Source: https://open-vsx.org/api/GitHub/github-vscode-theme/6.3.5/file/GitHub.github-vscode-theme-6.3.5.vsix (extension/themes/dark-default.json, built from primer/github-vscode-theme)
pub const PALETTE_GITHUB_DARK: Palette = Palette::of([0xFF7B72, 0xA5D6FF, 0x79C0FF, 0xFFA657, 0xFFA657, 0xD2A8FF, 0xD2A8FF, 0xD2A8FF, 0x8B949E, 0xA5D6FF, 0xE6EDF3, 0xFFA657, 0x79C0FF, 0x79C0FF, 0x79C0FF, 0x79C0FF, 0xD2A8FF, 0xFF7B72, 0xFF7B72, 0xFFA657, 0x7EE787]);
/// GitHub Light Default. Source: https://open-vsx.org/api/GitHub/github-vscode-theme/6.3.5/file/GitHub.github-vscode-theme-6.3.5.vsix (extension/themes/light-default.json, built from primer/github-vscode-theme)
pub const PALETTE_GITHUB_LIGHT: Palette = Palette::of([0xCF222E, 0x0A3069, 0x0550AE, 0x953800, 0x953800, 0x8250DF, 0x8250DF, 0x8250DF, 0x6E7781, 0x0A3069, 0x1F2328, 0x953800, 0x0550AE, 0x0550AE, 0x0550AE, 0x0550AE, 0x8250DF, 0xCF222E, 0xCF222E, 0x953800, 0x116329]);
/// VS Code Dark Modern. Source: https://raw.githubusercontent.com/microsoft/vscode/main/extensions/theme-defaults/themes/dark_modern.json (includes dark_plus.json, dark_vs.json)
pub const PALETTE_VSCODE_DARK: Palette = Palette::of([0x569CD6, 0xCE9178, 0xB5CEA8, 0x4EC9B0, 0x4EC9B0, 0xDCDCAA, 0xDCDCAA, 0xDCDCAA, 0x6A9955, 0x4DAAFC, 0x9CDCFE, 0x9CDCFE, 0x4FC1FF, 0x4FC1FF, 0x569CD6, 0x4EC9B0, 0xDCDCAA, 0x569CD6, 0xD7BA7D, 0x6796E6, 0x6A9955]);
/// VS Code Light Modern. Source: https://raw.githubusercontent.com/microsoft/vscode/main/extensions/theme-defaults/themes/light_modern.json (includes light_plus.json, light_vs.json)
pub const PALETTE_VSCODE_LIGHT: Palette = Palette::of([0x0000FF, 0xA31515, 0x098658, 0x267F99, 0x267F99, 0x795E26, 0x795E26, 0x795E26, 0x008000, 0x005FB8, 0x001080, 0x001080, 0x0070C1, 0x0070C1, 0x0000FF, 0x267F99, 0x795E26, 0x0000FF, 0xEE0000, 0x0451A5, 0x0451A5]);
/// JetBrains Darcula. Source: https://raw.githubusercontent.com/JetBrains/intellij-community/master/platform/platform-resources/src/DefaultColorSchemesManager.xml (Darcula) with https://raw.githubusercontent.com/intellij-rust/intellij-rust/master/src/main/resources/org/rust/ide/colors/RustDarcula.xml
pub const PALETTE_JETBRAINS_DARK: Palette = Palette::of([0xCC7832, 0x6A8759, 0x6897BB, 0xA9B7C6, 0xA9B7C6, 0xFFC66D, 0x4EADE5, 0xBBB529, 0x808080, 0x287BDE, 0x9876AA, 0xA9B7C6, 0x9876AA, 0x9876AA, 0xCC7832, 0xCC7832, 0xFFC66D, 0x20999D, 0xCC7832, 0xCC7832, 0x6A8759]);
/// IntelliJ Light. Source: https://raw.githubusercontent.com/JetBrains/intellij-community/master/platform/platform-resources/src/themes/Light.xml (parent Default in DefaultColorSchemesManager.xml) with https://raw.githubusercontent.com/intellij-rust/intellij-rust/master/src/main/resources/org/rust/ide/colors/RustDefault.xml
pub const PALETTE_JETBRAINS_LIGHT: Palette = Palette::of([0x0033B3, 0x067D17, 0x1750EB, 0x000000, 0x000000, 0x00627A, 0xDD6718, 0x9E880D, 0x8C8C8C, 0x006DCC, 0x871094, 0x000000, 0x871094, 0x871094, 0x0033B3, 0x0033B3, 0x080808, 0x20999D, 0x0037A6, 0x0033B3, 0x067D17]);
/// Solarized Dark. Source: https://raw.githubusercontent.com/microsoft/vscode/main/extensions/theme-solarized-dark/themes/solarized-dark-color-theme.json
pub const PALETTE_SOLARIZED_DARK: Palette = Palette::of([0x859900, 0x2AA198, 0xD33682, 0xCB4B16, 0xCB4B16, 0x268BD2, 0x268BD2, 0x268BD2, 0x586E75, 0x839496, 0x268BD2, 0x839496, 0xCB4B16, 0x268BD2, 0x268BD2, 0x859900, 0x268BD2, 0x93A1A1, 0xCB4B16, 0xB58900, 0x859900]);
/// Solarized Light. Source: https://raw.githubusercontent.com/microsoft/vscode/main/extensions/theme-solarized-light/themes/solarized-light-color-theme.json
pub const PALETTE_SOLARIZED_LIGHT: Palette = Palette::of([0x859900, 0x2AA198, 0xD33682, 0xCB4B16, 0xCB4B16, 0x268BD2, 0x268BD2, 0x268BD2, 0x93A1A1, 0x657B83, 0x268BD2, 0x657B83, 0xCB4B16, 0x268BD2, 0x268BD2, 0x859900, 0x268BD2, 0x586E75, 0xCB4B16, 0xB58900, 0x859900]);
/// Gruvbox Dark Medium. Source: https://open-vsx.org/api/jdinhlife/gruvbox/1.29.1/file/jdinhlife.gruvbox-1.29.1.vsix (extension/themes/gruvbox-dark-medium.json, built from jdinhify/vscode-theme-gruvbox)
pub const PALETTE_GRUVBOX_DARK: Palette = Palette::of([0xFB4934, 0xB8BB26, 0xD3869B, 0xFABD2F, 0xFABD2F, 0x8EC07C, 0xFABD2F, 0xFABD2F, 0x928374, 0xD3869B, 0x83A598, 0x83A598, 0xD3869B, 0x83A598, 0xFE8019, 0xFABD2F, 0x8EC07C, 0xFE8019, 0xD3869B, 0x83A598, 0x98971A]);
/// Gruvbox Light Medium. Source: https://open-vsx.org/api/jdinhlife/gruvbox/1.29.1/file/jdinhlife.gruvbox-1.29.1.vsix (extension/themes/gruvbox-light-medium.json, built from jdinhify/vscode-theme-gruvbox)
pub const PALETTE_GRUVBOX_LIGHT: Palette = Palette::of([0x9D0006, 0x79740E, 0x8F3F71, 0xB57614, 0xB57614, 0x427B58, 0xB57614, 0xB57614, 0x928374, 0x8F3F71, 0x076678, 0x076678, 0x8F3F71, 0x076678, 0xAF3A03, 0xB57614, 0x427B58, 0xAF3A03, 0x8F3F71, 0x076678, 0x98971A]);
/// Rosé Pine (main). Source: https://raw.githubusercontent.com/rose-pine/vscode/main/themes/rose-pine-color-theme.json
pub const PALETTE_ROSE_PINE_DARK: Palette = Palette::of([0x31748F, 0xF6C177, 0xEBBCBA, 0x9CCFD8, 0x9CCFD8, 0xEBBCBA, 0xEBBCBA, 0xEBBCBA, 0x6E6A86, 0xC4A7E7, 0xE0DEF4, 0xC4A7E7, 0x31748F, 0xE0DEF4, 0x31748F, 0x9CCFD8, 0xEBBCBA, 0x31748F, 0x31748F, 0x908CAA, 0x908CAA]);
/// Rosé Pine Dawn. Source: https://raw.githubusercontent.com/rose-pine/vscode/main/themes/rose-pine-dawn-color-theme.json
pub const PALETTE_ROSE_PINE_LIGHT: Palette = Palette::of([0x286983, 0xEA9D34, 0xD7827E, 0x56949F, 0x56949F, 0xD7827E, 0xD7827E, 0xD7827E, 0x9893A5, 0x907AA9, 0x575279, 0x907AA9, 0x286983, 0x575279, 0x286983, 0x56949F, 0xD7827E, 0x286983, 0x286983, 0x797593, 0x797593]);
/// Ayu Dark. Source: https://raw.githubusercontent.com/ayu-theme/vscode-ayu/master/ayu-dark.json
pub const PALETTE_AYU_DARK: Palette = Palette::of([0xFF8F40, 0xAAD94C, 0xD2A6FF, 0x59C2FF, 0x39BAE6, 0xFFB454, 0xE6C08A, 0xE6C08A, 0x5A6673, 0x39BAE6, 0xF07178, 0xD2A6FF, 0x95E6CB, 0x95E6CB, 0x39BAE6, 0x39BAE6, 0xFFB454, 0xFF8F40, 0x95E6CB, 0xFFB454, 0x95E6CB]);
/// Ayu Light. Source: https://raw.githubusercontent.com/ayu-theme/vscode-ayu/master/ayu-light.json
pub const PALETTE_AYU_LIGHT: Palette = Palette::of([0xFA8532, 0x86B300, 0xA37ACC, 0x22A4E6, 0x55B4D4, 0xEBA400, 0xE59645, 0xE59645, 0xADAEB1, 0x55B4D4, 0xF07171, 0xA37ACC, 0x4CBF99, 0x4CBF99, 0x55B4D4, 0x55B4D4, 0xEBA400, 0xFA8532, 0x4CBF99, 0xEBA400, 0x4CBF99]);
/// Everforest Dark Medium. Source: https://raw.githubusercontent.com/sainnhe/everforest-vscode/master/themes/everforest-dark.json
pub const PALETTE_EVERFOREST_DARK: Palette = Palette::of([0xE67E80, 0xDBBC7F, 0xD699B6, 0x7FBBB3, 0x7FBBB3, 0xA7C080, 0x83C092, 0xD699B6, 0x859289, 0xA7C080, 0xD3C6AA, 0xD3C6AA, 0xD699B6, 0xD3C6AA, 0xD699B6, 0x7FBBB3, 0xA7C080, 0xE69875, 0xA7C080, 0xE67E80, 0x859289]);
/// Everforest Light Medium. Source: https://raw.githubusercontent.com/sainnhe/everforest-vscode/master/themes/everforest-light.json
pub const PALETTE_EVERFOREST_LIGHT: Palette = Palette::of([0xF85552, 0xDFA000, 0xDF69BA, 0x3A94C5, 0x3A94C5, 0x8DA101, 0x35A77C, 0xDF69BA, 0x939F91, 0x8DA101, 0x5C6A72, 0x5C6A72, 0xDF69BA, 0x5C6A72, 0xDF69BA, 0x3A94C5, 0x8DA101, 0xF57D26, 0x8DA101, 0xF85552, 0x939F91]);
/// Kanagawa Wave. Source: https://raw.githubusercontent.com/rebelot/kanagawa.nvim/master/lua/kanagawa/themes.lua
pub const PALETTE_KANAGAWA_DARK: Palette = Palette::of([0x957FB8, 0x98BB6C, 0xD27E99, 0x7AA89F, 0x7AA89F, 0x7E9CD8, 0xE46876, 0xFFA066, 0x727169, 0x7FB4CA, 0xE6C384, 0xB8B4D0, 0xFFA066, 0xFFA066, 0xE46876, 0x7FB4CA, 0x7E9CD8, 0xC0A36E, 0xC0A36E, 0x7FB4CA, 0xB8B4D0]);
/// Kanagawa Lotus. Source: https://raw.githubusercontent.com/rebelot/kanagawa.nvim/master/lua/kanagawa/themes.lua
pub const PALETTE_KANAGAWA_LIGHT: Palette = Palette::of([0x624C83, 0x6F894E, 0xB35B79, 0x597B75, 0x597B75, 0x4D699B, 0xC84053, 0xCC6D00, 0x8A8980, 0x6693BF, 0x77713F, 0x5D57A3, 0xCC6D00, 0xCC6D00, 0xC84053, 0x6693BF, 0x4D699B, 0x836F4A, 0x836F4A, 0x6693BF, 0x5D57A3]);
/// Night Owl. Source: https://raw.githubusercontent.com/sdras/night-owl-vscode-theme/main/themes/Night%20Owl-color-theme.json
pub const PALETTE_NIGHT_OWL_DARK: Palette = Palette::of([0xC792EA, 0xECC48D, 0xF78C6C, 0xFFCB8B, 0xFFCB8B, 0x82AAFF, 0x82AAFF, 0xC5E478, 0x637777, 0xFF869A, 0xBAEBE2, 0xD7DBE0, 0x82AAFF, 0xC5E478, 0x7FDBCA, 0xC5E478, 0x82AAFF, 0xC792EA, 0xF78C6C, 0xD9F5DD, 0x697098]);
/// Light Owl. Source: https://raw.githubusercontent.com/sdras/night-owl-vscode-theme/main/themes/Night%20Owl-Light-color-theme.json
pub const PALETTE_NIGHT_OWL_LIGHT: Palette = Palette::of([0x994CC3, 0xC96765, 0xAA0982, 0x111111, 0x111111, 0x4876D6, 0x4876D6, 0x4876D6, 0x939DBB, 0xFF869A, 0x0C969B, 0x403F53, 0x4876D6, 0x4876D6, 0x0C969B, 0x4876D6, 0x4876D6, 0x994CC3, 0xAA0982, 0x111111, 0x697098]);
/// Nightfox. Source: https://raw.githubusercontent.com/EdenEast/nightfox.nvim/main/lua/nightfox/palette/nightfox.lua (roles per lua/nightfox/group/modules/treesitter.lua, syntax.lua, lsp_semantic_tokens.lua)
pub const PALETTE_NIGHTFOX_DARK: Palette = Palette::of([0x9D79D6, 0x81B29A, 0xF4A261, 0xDBC074, 0xD16983, 0x86ABDC, 0xC94F6D, 0xF6B079, 0x738091, 0xF6B079, 0x719CD6, 0x7AD5D6, 0xF6B079, 0xF6B079, 0xC94F6D, 0x7AD5D6, 0x86ABDC, 0xBAA1E2, 0xE0C989, 0x7AD5D6, 0xAEAFB0]);
/// Dayfox. Source: https://raw.githubusercontent.com/EdenEast/nightfox.nvim/main/lua/nightfox/palette/dayfox.lua (roles per lua/nightfox/group/modules/treesitter.lua, syntax.lua, lsp_semantic_tokens.lua)
pub const PALETTE_NIGHTFOX_LIGHT: Palette = Palette::of([0x6E33CE, 0x396847, 0x955F61, 0xAC5402, 0x8C1D28, 0x223D90, 0xA5222F, 0x7F5152, 0x837A72, 0x7F5152, 0x2848A9, 0x22676D, 0x7F5152, 0x7F5152, 0xA5222F, 0x22676D, 0x223D90, 0x5E2BAF, 0x924702, 0x22676D, 0x643F61]);
/// Monokai Pro (Filter: default). Source: https://marketplace.visualstudio.com/_apis/public/gallery/publishers/monokai/vsextensions/theme-monokai-pro-vscode/latest/vspackage (official extension, extension/themes/Monokai Pro.json)
pub const PALETTE_MONOKAI_PRO_DARK: Palette = Palette::of([0xFF6188, 0xFFD866, 0xAB9DF2, 0x78DCE8, 0x78DCE8, 0xA9DC76, 0xA9DC76, 0xC1C0C0, 0x727072, 0xA9DC76, 0xFCFCFA, 0xFC9867, 0xAB9DF2, 0xAB9DF2, 0xC1C0C0, 0x78DCE8, 0xA9DC76, 0xFF6188, 0xAB9DF2, 0x939293, 0x939293]);
/// Monokai Pro Light. Source: https://marketplace.visualstudio.com/_apis/public/gallery/publishers/monokai/vsextensions/theme-monokai-pro-vscode/latest/vspackage (official extension, extension/themes/Monokai Pro Light.json)
pub const PALETTE_MONOKAI_PRO_LIGHT: Palette = Palette::of([0xE14775, 0xCC7A0A, 0x7058BE, 0x1C8CA8, 0x1C8CA8, 0x269D69, 0x269D69, 0x706B6E, 0xA59FA0, 0x269D69, 0x29242A, 0xE16032, 0x7058BE, 0x7058BE, 0x706B6E, 0x1C8CA8, 0x269D69, 0xE14775, 0x7058BE, 0x918C8E, 0x918C8E]);
/// Material Darker. Source: https://marketplace.visualstudio.com/_apis/public/gallery/publishers/equinusocio/vsextensions/vsc-material-theme/latest/vspackage (official extension, extension/build/themes/Material-Theme-Darker.json)
pub const PALETTE_MATERIAL_DARK: Palette = Palette::of([0x89DDFF, 0xC3E88D, 0xF78C6C, 0xFFCB6B, 0xFFCB6B, 0x82AAFF, 0x82AAFF, 0x82AAFF, 0x545454, 0xF07178, 0xEEFFFF, 0xEEFFFF, 0xFF9CAC, 0xEEFFFF, 0x89DDFF, 0xFFCB6B, 0x82AAFF, 0xC792EA, 0xEEFFFF, 0x89DDFF, 0xFF9CAC]);
/// Material Lighter. Source: https://marketplace.visualstudio.com/_apis/public/gallery/publishers/equinusocio/vsextensions/vsc-material-theme/latest/vspackage (official extension, extension/build/themes/Material-Theme-Lighter.json)
pub const PALETTE_MATERIAL_LIGHT: Palette = Palette::of([0x39ADB5, 0x91B859, 0xF76D47, 0xE2931D, 0xE2931D, 0x6182B8, 0x6182B8, 0x6182B8, 0x90A4AE, 0xE53935, 0x90A4AE, 0x90A4AE, 0xFF5370, 0x90A4AE, 0x39ADB5, 0xE2931D, 0x6182B8, 0x9C3EDA, 0x90A4AE, 0x39ADB5, 0xFF5370]);
/// Tomorrow Night. Source: https://raw.githubusercontent.com/chriskempson/tomorrow-theme/master/textmate/Tomorrow-Night.tmTheme
pub const PALETTE_TOMORROW_DARK: Palette = Palette::of([0xB294BB, 0xB5BD68, 0xDE935F, 0xF0C674, 0xF0C674, 0x81A2BE, 0x81A2BE, 0x81A2BE, 0x969896, 0xC5C8C6, 0xCC6666, 0xDE935F, 0xDE935F, 0xCC6666, 0xCC6666, 0xF0C674, 0x81A2BE, 0xB294BB, 0xDE935F, 0xC5C8C6, 0xC5C8C6]);
/// Tomorrow. Source: https://raw.githubusercontent.com/chriskempson/tomorrow-theme/master/textmate/Tomorrow.tmTheme
pub const PALETTE_TOMORROW_LIGHT: Palette = Palette::of([0x8959A8, 0x718C00, 0xF5871F, 0xC99E00, 0xC99E00, 0x4271AE, 0x4271AE, 0x4271AE, 0x8E908C, 0x4D4D4C, 0xC82829, 0xF5871F, 0xF5871F, 0xC82829, 0xC82829, 0xC99E00, 0x4271AE, 0x8959A8, 0xF5871F, 0x4D4D4C, 0x4D4D4C]);
/// Vitesse Dark. Source: https://raw.githubusercontent.com/antfu/vscode-theme-vitesse/main/themes/vitesse-dark.json
pub const PALETTE_VITESSE_DARK: Palette = Palette::of([0x4D9375, 0xC98A7D, 0x4C9A91, 0x5DA994, 0x5D99A9, 0x80A665, 0x80A665, 0x80A665, 0x687668, 0x858480, 0xB8A965, 0xBD976A, 0xC99076, 0xBD976A, 0xC99076, 0x5DA994, 0x80A665, 0xCB7676, 0xC99076, 0xD4976C, 0x5D99A9]);
/// Vitesse Light. Source: https://raw.githubusercontent.com/antfu/vscode-theme-vitesse/main/themes/vitesse-light.json
pub const PALETTE_VITESSE_LIGHT: Palette = Palette::of([0x1E754F, 0xB56959, 0x2F798A, 0x2E8F82, 0x2E808F, 0x59873A, 0x59873A, 0x59873A, 0xA0ADA0, 0x8F908C, 0x998418, 0xB07D48, 0xA65E2B, 0xB07D48, 0xA65E2B, 0x2E8F82, 0x59873A, 0xAB5959, 0xA65E2B, 0xA65E2B, 0x2E808F]);
/// Flexoki Dark. Source: https://raw.githubusercontent.com/kepano/flexoki/main/vscode/Flexoki-Dark-color-theme.json
pub const PALETTE_FLEXOKI_DARK: Palette = Palette::of([0x879A39, 0x3AA99F, 0x8B7EC8, 0xD0A215, 0xD0A215, 0xDA702C, 0xCE5D97, 0xD0A215, 0x878580, 0x4385BE, 0x4385BE, 0xCECDC3, 0xD0A215, 0xCECDC3, 0xCE5D97, 0xD0A215, 0x879A39, 0x4385BE, 0xCECDC3, 0xD0A215, 0xCECDC3]);
/// Flexoki Light. Source: https://raw.githubusercontent.com/kepano/flexoki/main/vscode/Flexoki-Light-color-theme.json
pub const PALETTE_FLEXOKI_LIGHT: Palette = Palette::of([0x66800B, 0x24837B, 0x5E409D, 0xAD8301, 0xAD8301, 0xBC5215, 0xA02F6F, 0xAD8301, 0x6F6E69, 0x205EA6, 0x205EA6, 0x100F0F, 0xAD8301, 0x100F0F, 0xA02F6F, 0xAD8301, 0x66800B, 0x205EA6, 0x100F0F, 0xAD8301, 0x100F0F]);
/// Modus Vivendi. Source: https://raw.githubusercontent.com/protesilaos/modus-themes/main/modus-themes.el
pub const PALETTE_MODUS_DARK: Palette = Palette::of([0xB6A0FF, 0x79A8FF, 0xFFFFFF, 0x6AE4B9, 0x6AE4B9, 0xFEACD0, 0xFF7F86, 0xFF7F86, 0x989898, 0x79A8FF, 0x00D3D0, 0x00D3D0, 0x00BCFF, 0x6AE4B9, 0xB6A0FF, 0x6AE4B9, 0xD09DC0, 0xFFFFFF, 0xFEACD0, 0x989898, 0x9AC8E0]);
/// Modus Operandi. Source: https://raw.githubusercontent.com/protesilaos/modus-themes/main/modus-themes.el
pub const PALETTE_MODUS_LIGHT: Palette = Palette::of([0x531AB6, 0x3548CF, 0x000000, 0x005F5F, 0x005F5F, 0x721045, 0xA0132F, 0xA0132F, 0x595959, 0x3548CF, 0x005E8B, 0x005E8B, 0x0000B0, 0x005F5F, 0x531AB6, 0x005F5F, 0x7B435C, 0x000000, 0x721045, 0x595959, 0x2A5045]);
/// Bluloco Dark. Source: https://raw.githubusercontent.com/uloco/theme-bluloco-dark/main/themes/bluloco-dark-color-theme.json
pub const PALETTE_BLULOCO_DARK: Palette = Palette::of([0x10B1FE, 0xF9C859, 0xFF78F8, 0xFF6480, 0xFF6480, 0x3FC56B, 0x3FC56B, 0x50ACAE, 0x636D83, 0x10B1FE, 0xCE9887, 0x8BCDEF, 0x9F7EFE, 0x9F7EFE, 0x10B1FE, 0x10B1FE, 0x3FC56B, 0x10B1FE, 0xFF936A, 0xCE9887, 0x9F7EFE]);
/// Bluloco Light. Source: https://raw.githubusercontent.com/uloco/theme-bluloco-light/main/themes/bluloco-light-color-theme.json
pub const PALETTE_BLULOCO_LIGHT: Palette = Palette::of([0x0098DD, 0xC5A332, 0xCE33C0, 0xD52753, 0xD52753, 0x23974A, 0x23974A, 0x047485, 0xA0A1A7, 0x0098DD, 0xA05A48, 0x40B8C5, 0x823FF1, 0x823FF1, 0x0098DD, 0x0098DD, 0x23974A, 0x0098DD, 0xDF631C, 0xA05A48, 0x823FF1]);
/// Horizon Dark. Source: https://raw.githubusercontent.com/jolaleye/horizon-theme-vscode/master/themes/horizon.json
pub const PALETTE_HORIZON_DARK: Palette = Palette::of([0xB877DB, 0xFAB795, 0xF09483, 0xFAC29A, 0xFAC29A, 0x25B0BC, 0x25B0BC, 0x25B0BC, 0x4C4D53, 0xFAB795, 0xE95678, 0xE95678, 0xF09483, 0xE95678, 0xFAC29A, 0xFAC29A, 0x25B0BC, 0xB877DB, 0x25B0BC, 0xF09483, 0xFAB795]);
/// Horizon Bright. Source: https://raw.githubusercontent.com/jolaleye/horizon-theme-vscode/master/themes/horizon-bright.json
pub const PALETTE_HORIZON_LIGHT: Palette = Palette::of([0x8A31B9, 0xF6661E, 0xDC3318, 0xF77D26, 0xF77D26, 0x1D8991, 0x1D8991, 0x1D8991, 0x989190, 0xF6661E, 0xDA103F, 0xDA103F, 0xDC3318, 0xDA103F, 0xF77D26, 0xF77D26, 0x1D8991, 0x8A31B9, 0x1D8991, 0xDC3318, 0xF6661E]);
/// Oxocarbon Dark. Source: https://raw.githubusercontent.com/nyoom-engineering/oxocarbon.nvim/main/lua/oxocarbon/init.lua
pub const PALETTE_OXOCARBON_DARK: Palette = Palette::of([0x78A9FF, 0xBE95FF, 0x82CFFF, 0x78A9FF, 0x78A9FF, 0xFF7EB6, 0x08BDBA, 0x82CFFF, 0x525252, 0xBE95FF, 0xEE5396, 0xD0D0D0, 0xBE95FF, 0xBE95FF, 0xD0D0D0, 0x78A9FF, 0x08BDBA, 0x82CFFF, 0x82CFFF, 0x3DDBD9, 0x3DDBD9]);
/// Oxocarbon Light. Source: https://raw.githubusercontent.com/nyoom-engineering/oxocarbon.nvim/main/lua/oxocarbon/init.lua. The comment is the light palette's grey `base05`: the theme draws comments in near-black `#161616`, darker than its code.
pub const PALETTE_OXOCARBON_LIGHT: Palette = Palette::of([0xEE5396, 0xBE95FF, 0xFFAB91, 0xEE5396, 0xEE5396, 0x673AB7, 0x08BDBA, 0xFFAB91, 0x90A4AE, 0xBE95FF, 0xFF6F00, 0x37474F, 0xBE95FF, 0xBE95FF, 0x37474F, 0xEE5396, 0x08BDBA, 0xFFAB91, 0xFFAB91, 0xFF7EB6, 0xFF7EB6]);

/// The palette of `scheme` in the dark or the light appearance.
pub fn palette(scheme: crate::settings::Scheme, light: bool) -> &'static Palette {
    use crate::settings::Scheme;
    match (scheme, light) {
        (Scheme::Xcode, false) => &PALETTE_XCODE_DARK,
        (Scheme::Xcode, true) => &PALETTE_XCODE_LIGHT,
        (Scheme::Ayu, false) => &PALETTE_AYU_DARK,
        (Scheme::Ayu, true) => &PALETTE_AYU_LIGHT,
        (Scheme::Bluloco, false) => &PALETTE_BLULOCO_DARK,
        (Scheme::Bluloco, true) => &PALETTE_BLULOCO_LIGHT,
        (Scheme::Catppuccin, false) => &PALETTE_CATPPUCCIN_DARK,
        (Scheme::Catppuccin, true) => &PALETTE_CATPPUCCIN_LIGHT,
        (Scheme::Dracula, false) => &PALETTE_DRACULA_DARK,
        (Scheme::Dracula, true) => &PALETTE_DRACULA_LIGHT,
        (Scheme::Everforest, false) => &PALETTE_EVERFOREST_DARK,
        (Scheme::Everforest, true) => &PALETTE_EVERFOREST_LIGHT,
        (Scheme::Flexoki, false) => &PALETTE_FLEXOKI_DARK,
        (Scheme::Flexoki, true) => &PALETTE_FLEXOKI_LIGHT,
        (Scheme::GitHub, false) => &PALETTE_GITHUB_DARK,
        (Scheme::GitHub, true) => &PALETTE_GITHUB_LIGHT,
        (Scheme::Gruvbox, false) => &PALETTE_GRUVBOX_DARK,
        (Scheme::Gruvbox, true) => &PALETTE_GRUVBOX_LIGHT,
        (Scheme::Horizon, false) => &PALETTE_HORIZON_DARK,
        (Scheme::Horizon, true) => &PALETTE_HORIZON_LIGHT,
        (Scheme::JetBrains, false) => &PALETTE_JETBRAINS_DARK,
        (Scheme::JetBrains, true) => &PALETTE_JETBRAINS_LIGHT,
        (Scheme::Kanagawa, false) => &PALETTE_KANAGAWA_DARK,
        (Scheme::Kanagawa, true) => &PALETTE_KANAGAWA_LIGHT,
        (Scheme::Material, false) => &PALETTE_MATERIAL_DARK,
        (Scheme::Material, true) => &PALETTE_MATERIAL_LIGHT,
        (Scheme::Modus, false) => &PALETTE_MODUS_DARK,
        (Scheme::Modus, true) => &PALETTE_MODUS_LIGHT,
        (Scheme::MonokaiPro, false) => &PALETTE_MONOKAI_PRO_DARK,
        (Scheme::MonokaiPro, true) => &PALETTE_MONOKAI_PRO_LIGHT,
        (Scheme::NightOwl, false) => &PALETTE_NIGHT_OWL_DARK,
        (Scheme::NightOwl, true) => &PALETTE_NIGHT_OWL_LIGHT,
        (Scheme::Nightfox, false) => &PALETTE_NIGHTFOX_DARK,
        (Scheme::Nightfox, true) => &PALETTE_NIGHTFOX_LIGHT,
        (Scheme::One, false) => &PALETTE_ONE_DARK,
        (Scheme::One, true) => &PALETTE_ONE_LIGHT,
        (Scheme::Oxocarbon, false) => &PALETTE_OXOCARBON_DARK,
        (Scheme::Oxocarbon, true) => &PALETTE_OXOCARBON_LIGHT,
        (Scheme::RosePine, false) => &PALETTE_ROSE_PINE_DARK,
        (Scheme::RosePine, true) => &PALETTE_ROSE_PINE_LIGHT,
        (Scheme::Solarized, false) => &PALETTE_SOLARIZED_DARK,
        (Scheme::Solarized, true) => &PALETTE_SOLARIZED_LIGHT,
        (Scheme::Tomorrow, false) => &PALETTE_TOMORROW_DARK,
        (Scheme::Tomorrow, true) => &PALETTE_TOMORROW_LIGHT,
        (Scheme::TokyoNight, false) => &PALETTE_TOKYO_NIGHT_DARK,
        (Scheme::TokyoNight, true) => &PALETTE_TOKYO_NIGHT_LIGHT,
        (Scheme::Vitesse, false) => &PALETTE_VITESSE_DARK,
        (Scheme::Vitesse, true) => &PALETTE_VITESSE_LIGHT,
        (Scheme::VsCode, false) => &PALETTE_VSCODE_DARK,
        (Scheme::VsCode, true) => &PALETTE_VSCODE_LIGHT,
    }
}

/// How much a vivid scheme's saturation grows: the color's own saturation times this, plus `VIVID_SATURATION_ADD`, up to 1. Greys (under 0.12) only get the contrast.
pub const VIVID_SATURATION_GAIN: f32 = 1.18;
/// The flat saturation a vivid scheme adds on top of the gain.
pub const VIVID_SATURATION_ADD: f32 = 0.05;
/// How much lighter a vivid color gets on a dark theme (the brighter hue the lift is for).
pub const VIVID_LIGHTNESS_DARK: f32 = 0.04;
/// The least contrast a vivid code color keeps against the ground (WCAG ratio): the lift never leaves text harder to read than this.
pub const VIVID_CONTRAST: f32 = 4.5;

/// How many hues semantic colors choose from.
pub const SEMANTIC_HUES: usize = 12;
/// Steps between the next two semantic hues in the list, in twelfths of the wheel; coprime with `SEMANTIC_HUES`, so every hue is used and neighbors are far apart.
pub const SEMANTIC_HUE_STRIDE: usize = 5;
/// Where the semantic hues start on the wheel, as a share of a turn (a warm red).
pub const SEMANTIC_HUE_START: f32 = 0.01;
/// What vivid adds to the semantic hues' saturation.
pub const SEMANTIC_VIVID_SATURATION: f32 = 0.15;

/// WCAG relative luminance of `c`.
fn luminance(c: Hsla) -> f32 {
    let c = c.to_rgb();
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio of two colors, 1 to 21.
pub fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `color` made more vivid for the vivid setting: saturation lifted (and, on a dark theme, a little lighter), then
/// moved in lightness, lighter on a dark ground and darker on a light one, until it reads at `VIVID_CONTRAST` against
/// `ground`. A grey keeps its saturation (the comment role skips vivid altogether, being quiet by design). Run once when the theme is set, never per frame.
pub fn vivid(color: Hsla, light: bool, ground: Hsla) -> Hsla {
    if color.s < 0.12 {
        return readable(color, light, ground);
    }
    let mut out = Hsla { s: (color.s * VIVID_SATURATION_GAIN + VIVID_SATURATION_ADD).min(1.), ..color };
    if !light {
        out.l = (out.l + VIVID_LIGHTNESS_DARK).min(0.9);
    }
    readable(out, light, ground)
}

/// `color` moved in lightness, lighter on a dark ground and darker on a light one, until it reads at `VIVID_CONTRAST`
/// against `ground` (or the lightness runs out).
pub fn readable(mut color: Hsla, light: bool, ground: Hsla) -> Hsla {
    let step = if light { -0.01 } else { 0.01 };
    while contrast(color, ground) < VIVID_CONTRAST && (0.1..0.95).contains(&(color.l + step)) {
        color.l += step;
    }
    color
}

// ---- Parts of speech: View > Highlight tints a class of words, one soft hue each (nouns, verbs, adjectives, adverbs, conjunctions) ----

/// The tints on a dark theme, in the order nouns, verbs, adjectives, adverbs, conjunctions: pastel blue, green, amber, rose and violet, soft against the `#1E1E1E` ground (all over 7:1).
pub const POS_DARK: [u32; 5] = [0x7FB2F0, 0x7FD59A, 0xE6B86A, 0xE58FB5, 0xB59CF0];
/// The tints on a light theme, in the same order: the same hues deepened to read on white (all over 5:1).
pub const POS_LIGHT: [u32; 5] = [0x2F6DB5, 0x2E7D46, 0x946000, 0xB03A72, 0x6B4CB8];

// ---- Tooltips: the system tip's look, as in Harrier ----

/// Corner radius of a tooltip.
pub const RADIUS_TOOLTIP: f32 = 5.;
/// A tooltip's side padding, and the gap between its words and its key.
pub const TOOLTIP_PAD_X: f32 = 8.;
/// A tooltip's top and bottom padding.
pub const TOOLTIP_PAD_Y: f32 = 4.;
/// A tooltip wraps at this width.
pub const TOOLTIP_MAX_W: f32 = 280.;
/// How far right of the pointer's tip a tooltip starts (the arrow's width).
pub const TOOLTIP_OFFSET_X: f32 = 4.;
/// How far below the pointer's tip a tooltip starts (the arrow's height).
pub const TOOLTIP_OFFSET_Y: f32 = 18.;
/// How far above the pointer's tip a flipped tooltip ends.
pub const TOOLTIP_GAP_ABOVE: f32 = 6.;
/// How far a tooltip settles toward the pointer as it fades in.
pub const TOOLTIP_RISE: f32 = 2.;
/// A press this near where a tooltip's control was pressed keeps its tip away.
pub const TOOLTIP_PRESS_REACH: f32 = 24.;
/// Draw order of a tooltip and its fading ghost: the topmost deferred layer.
pub const LAYER_TOOLTIP: usize = 1000;
/// Draw order of a dropped tab's copy gliding home: over the window's content, under tooltips.
pub const LAYER_TAB_LANDING: usize = 2;
/// How long the pointer rests before a tooltip shows (AppKit's initial delay).
pub const TOOLTIP_DELAY: Duration = Duration::from_millis(500);
/// A tooltip asked for within this long of the last one hiding shows at once (AppKit's warm tooltips).
pub const TOOLTIP_WARM: Duration = Duration::from_millis(600);
/// How long Finch must have been in the background before coming back washes the caret's line.
pub const WELCOME_BACK_AFTER: Duration = Duration::from_secs(180);
/// How long that welcome-back wash fades, in seconds: slower than a paste's.
pub const WELCOME_BACK_WASH: f32 = 0.5;
/// Every float's ambient shadow on a dark theme (alpha, offset down, blur): the soft pool under a tooltip, the find bar and a card, under `SHADOW_FLOAT_CONTACT`.
pub const SHADOW_FLOAT_AMBIENT: [f32; 3] = [0.22, 3., 8.];
/// Every float's ambient shadow on a light theme (alpha, offset down, blur): the only thing that lifts a white float off the white page, so deeper than the dark one is loud but not so wide it blooms into a grey cloud.
pub const SHADOW_FLOAT_AMBIENT_LIGHT: [f32; 3] = [0.18, 4., 12.];
/// Every float's contact shadow on a dark theme (alpha, offset down, blur): ui-standards' float pair.
pub const SHADOW_FLOAT_CONTACT: [f32; 3] = [0.2, 1., 2.];
/// Every float's contact shadow on a light theme: the dark one's offset, firmer, so the edge reads on white.
pub const SHADOW_FLOAT_CONTACT_LIGHT: [f32; 3] = [0.2, 1., 2.5];
/// A held float's lift shadow on a dark theme (alpha, offset down, blur), in place of the ambient: the dragged tab's and the drag card's, one plane above the rest because the pointer carries them.
pub const SHADOW_FLOAT_LIFT: [f32; 3] = [0.3, 10., 24.];
/// A held float's lift shadow on a light theme: deeper and wider than `SHADOW_FLOAT_AMBIENT_LIGHT`, so a card in the hand sits a plane above a resting float on white.
pub const SHADOW_FLOAT_LIFT_LIGHT: [f32; 3] = [0.28, 10., 24.];
/// A tooltip's words: the sidebar foreground at this alpha (`labelColor`).
pub const ALPHA_PRIMARY: f32 = 0.85;
/// A tooltip's rim: the foreground at this alpha.
pub const ALPHA_BORDER: f32 = 0.12;
/// A tooltip's lit top edge on a dark theme: white at this alpha (a light theme has none: white on white).
pub const ALPHA_HIGHLIGHT: f32 = 0.14;
/// A tooltip's rim on a light theme: the foreground at this alpha, firmer than the dark rim since white floats stand on a white page.
pub const ALPHA_BORDER_LIGHT: f32 = 0.2;
/// A tooltip's fill on a dark theme: the ground this far of the way toward the foreground.
pub const PANEL_LIFT: f32 = 0.13;
/// A tooltip's fill on a light theme: the ground this far of the way toward white (all of it: a float is white, lifted by its rim and shadows).
pub const PANEL_LIFT_LIGHT: f32 = 1.;
/// A floating card (a refused name's reason) fading and settling in, like a tooltip's arrival.
pub const OVERLAY_ENTER: f32 = 0.17;
/// A floating card fading out.
pub const OVERLAY_EXIT: f32 = 0.1;
/// The warning symbol leading a refused name's reason: caption size, 11 pt.
pub const ERROR_SYMBOL: f32 = 11.;

// ---- Motion: lengths in seconds, responses of the selection spring ----

/// Response of the selection wash's spring when it moves to another row.
pub const SELECT_RESPONSE: f32 = 0.35;
/// Response of the hover wash's spring when it follows the pointer.
pub const HOVER_SNAP_RESPONSE: f32 = 0.11;
/// Hover wash fading in.
pub const HOVER_IN: f32 = 0.12;
/// Hover wash fading out.
pub const HOVER_OUT: f32 = 0.2;
/// The selection wash easing between its focused and unfocused alpha.
pub const FADE_QUICK: f32 = 0.15;
/// What one frame spends wrapping a hidden tab's text after its pane's width changed, so the frame that shows it finds the rows ready.
pub const PRELAYOUT_SLICE: Duration = Duration::from_millis(2);
/// Frames a pane's width holds still before hidden tabs are laid out for it, so a divider drag doesn't pay for it.
pub const PRELAYOUT_SETTLE_FRAMES: u32 = 6;
/// Longest step one frame advances a wash, so a late frame doesn't jump it.
pub const MAX_STEP: f32 = 1. / 30.;
/// A folder's rows folding open or shut, and the chevron turning with them: a critically damped spring that settles in this long (Harrier's `SETTLE`).
pub const SETTLE: f32 = 0.35;
/// A folding row's fade starts this share of the fold in, plus `FOLD_STAGGER_STEP` for each row down.
pub const FOLD_FADE_FROM: f32 = 0.15;
/// How much of the fold a folding row's fade takes.
pub const FOLD_FADE_SPAN: f32 = 0.5;
/// Each row down the fold starts its fade this share later (15 ms of the 0.35 s), up to `FOLD_STAGGER_ROWS` rows.
pub const FOLD_STAGGER_STEP: f32 = 0.043;
/// The rows after which a fold's fades stop starting later.
pub const FOLD_STAGGER_ROWS: usize = 6;
/// A folder read that takes longer than this shows its "Loading" row; a faster one never flashes it.
pub const LOADING_DELAY: Duration = Duration::from_millis(200);
/// How long files dragged over a closed folder wait before it opens.
pub const DROP_OPEN_DELAY: Duration = Duration::from_millis(700);
/// How long a tree change (a dot gained or lost, a file arrived) may wait for a first frame before a row scrolled into view shows it at rest instead of animating a change it was not there for.
pub const UNSEEN_GRACE: Duration = Duration::from_secs(1);
/// The wash on a row whose file arrived from outside the app, fading out on the shared ease-out.
pub const ARRIVED_WASH: f32 = 0.6;
/// Most rows that wear the arrival wash at once, so a big batch of new files shows a few, not a flicker down the tree.
pub const ARRIVED_MAX: usize = 4;
/// The last stem characters a cut file name keeps before its extension, so "…" never sits against the dot ("an-unusual…me.md").
pub const TAIL_KEPT: usize = 2;
/// The folder (or the whole tree) that takes a drop, and the zone of a pane that takes a dragged tab or file: the accent color at this alpha behind it.
pub const ALPHA_DROP: f32 = 0.22;
/// Most rows a tree draws past its bottom edge while a fold slides them in, in points (about 54 rows).
pub const FOLD_OVERDRAW: f32 = 1200.;
/// A tooltip fading out where it stood, and a tooltip leaving with the pointer.
pub const TOOLTIP_FADE_OUT: f32 = 0.12;

// ---- Explorer filter field and the empty state ----

/// The filter capsule's height at the foot of the explorer.
pub const FILTER_HEIGHT: Pixels = px(24.);
/// Space around the filter capsule: the focus ring needs room outside it.
pub const FILTER_INSET: Pixels = px(10.);
/// Side padding inside the filter capsule, before the symbol and after the field.
pub const FILTER_PAD_X: Pixels = px(8.);
/// Gap between the filter symbol and the field, and between the empty state's lines.
pub const FILTER_GAP: Pixels = px(6.);
/// Gap between a text field's leading symbol and its text (the find bar's, Go to line's and the
/// filter's): a Mac search field leaves about 5 pt from its magnifier to the first letter, and
/// this plus the symbol's own side bearing comes to that.
pub const FIELD_SYMBOL_GAP: Pixels = px(4.);
/// The filter symbol in the capsule: bar control, 12 pt.
pub const FILTER_SYMBOL: f32 = 12.;
/// How far a field's pressable magnifier's wash reaches past the symbol on each side (the layout keeps the symbol's own box: the wash is padded in and pulled out by this much).
pub const FIELD_LENS_PAD: Pixels = px(3.);
/// The filter capsule's fill at rest: the foreground at this alpha.
pub const ALPHA_FILTER: f32 = 0.08;
/// The filter capsule's fill while the field has the keyboard.
pub const ALPHA_FILTER_FOCUSED: f32 = 0.12;
/// `ALPHA_FILTER` on a light theme.
pub const ALPHA_FILTER_LIGHT: f32 = 0.112;
/// `ALPHA_FILTER_FOCUSED` on a light theme.
pub const ALPHA_FILTER_FOCUSED_LIGHT: f32 = 0.168;
/// The focus ring's band width, drawn outside the capsule.
pub const FILTER_RING: Pixels = px(3.);
/// How much farther out the focus ring starts before it closes in.
pub const FILTER_RING_FROM: Pixels = px(6.);
/// The focus ring's alpha when fully shown.
pub const ALPHA_RING: f32 = 0.5;
/// A tree name while a filter shows: the name at this alpha, so the bold matching letters stand out at full ink.
pub const ALPHA_NAME_DIM: f32 = 0.85;
/// The empty state sits this far above the true center of its pane.
pub const EMPTY_LIFT: Pixels = px(16.);
/// The empty state's symbol: 26 pt.
pub const EMPTY_SYMBOL: f32 = 26.;
/// The empty state's detail wraps at this width.
pub const EMPTY_DETAIL_WIDTH: Pixels = px(280.);
/// Extra space between the empty state's line and its buttons, on top of the lines' own gap.
pub const EMPTY_ACTION_GAP: Pixels = px(6.);
/// The empty state's detail line: 12 pt.
pub const TEXT_DETAIL: Pixels = px(12.);
/// Line box of the empty state's detail.
pub const TEXT_DETAIL_LINE: Pixels = px(16.);

// ---- Status line: the strip at the foot of a pane ----

/// The status line's height: one row of caption text under the editor, with a hairline on top.
pub const STATUS_HEIGHT: Pixels = px(22.);
/// The status line's text: caption size, 11 pt.
pub const TEXT_CAPTION: Pixels = px(11.);
/// The status line's inset from the pane's left and right edges.
pub const STATUS_INSET: Pixels = px(12.);
/// Space between the status line's fields; a clickable field adds `STATUS_FIELD_PAD` inside its wash.
pub const STATUS_GAP: Pixels = px(4.);
/// A clickable status field's padding inside its hover wash, left and right.
pub const STATUS_FIELD_PAD: Pixels = px(6.);
/// Corner radius of a clickable status field's hover wash.
pub const RADIUS_STATUS_FIELD: Pixels = px(4.);
/// A status field's height, its hover wash included: 20 pt, the least an icon button is.
pub const STATUS_FIELD_HEIGHT: Pixels = px(20.);
/// The symbol before the config trail in the status line (the caret's place in a JSON, TOML or YAML file): 10 pt, a touch under the caption's 11 pt text so it reads as a mark, not a word.
pub const STATUS_SYMBOL: f32 = 10.;
/// The least width the position keeps (it truncates with "…" past it) before the file's fields start to drop out of a narrow pane.
pub const STATUS_POSITION_MIN: Pixels = px(72.);
/// The status line's text alpha in a pane without the keyboard and in a window that isn't key (lit: `ALPHA_SECONDARY`): chrome loses emphasis like the line numbers beside it, still readable.
pub const ALPHA_STATUS_DIM: f32 = 0.35;

// ---- Tab motion ----

/// A tab arriving or folding away: the length its width settles in, as a critically damped spring.
pub const ARRIVE: f32 = 0.22;
/// The spring rate (1 - (1 + u) e^-u within a thousandth) a settle length is divided into.
pub const SETTLE_RATE: f32 = 9.23;
/// How far, in points, a tab's title drifts in from the side the switch came from.
pub const TAB_TITLE_DRIFT: f32 = 3.;
/// The width fraction an arriving tab shows before its title and close button fade in.
pub const TAB_ARRIVE_FADE_FROM: f32 = 0.4;
/// The overshoot curve, a cubic bezier's two control points (x1, y1, x2, y2): it runs past 1 (about a tenth) and settles back. `motion::overshoot`.
pub const OVERSHOOT: [f32; 4] = [0.34, 1.56, 0.64, 1.];
/// A save landing on its tab: the length the dot shrinks past nothing (or swells, for a failed save) as the file icon fades back in.
pub const SAVE_SETTLE: f32 = 0.18;
/// Save all: how long each saved tab waits after the one before it, left to right, in seconds.
pub const SAVE_STAGGER: f32 = 0.03;
/// The most tabs a save all ripple staggers; the rest land with the last.
pub const SAVE_STAGGER_MAX: u32 = 8;
/// How far the title of a saved tab brightens toward primary ink at the peak of the settle.
pub const SAVE_BRIGHTEN: f32 = 0.35;
/// How much bigger, as a share of its size, the dot swells at the peak of a failed save.
pub const SAVE_SWELL: f32 = 0.7;
/// A save still writing this long after it began (seconds) makes its tab's dot breathe; a quicker one shows nothing, so a normal save never flickers.
pub const SAVE_SLOW: f32 = 0.25;
/// The dimmest a breathing dot gets, as a share of its full ink: soft enough to read as waiting, not as a fault.
pub const SAVE_BREATH_LOW: f32 = 0.45;
/// One breath of the dot, full to dim and back, in seconds: slower than a pulse so a long save looks calm.
pub const SAVE_BREATH_PERIOD: f32 = 1.2;
/// The peek card a rest on a tab opens: its width, and its least distance from the window's edges.
pub const PEEK_WIDTH: f32 = 320.;
pub const PEEK_EDGE: f32 = 8.;
/// The gap between a tab's bottom edge and its peek card.
pub const PEEK_GAP: f32 = 4.;
/// How many lines of the tab's text the peek shows, around its caret.
pub const PEEK_LINES: usize = 5;
/// How many characters of a line the peek reads, so one huge line never copies into it.
pub const PEEK_COLUMNS: usize = 120;
/// The largest the peek's text goes, in points: the editor's own size, held down so five lines stay small.
pub const PEEK_TEXT_MAX: f32 = 12.;
/// A snippet's line height, as a multiple of its text size, and the side padding of its lines in points.
pub const SNIPPET_LINE: f32 = 1.5;
pub const SNIPPET_PAD: f32 = 4.;
/// The wash on the caret's line in a snippet: the ink at this alpha.
pub const ALPHA_SNIPPET_CARET: f32 = 0.12;
/// Open quickly's peek card: how many lines of the highlighted file it shows, how many bytes it reads of the file to find them,
/// its widest, the narrowest it is still drawn (a window with less room beside the list hides it), and its gap to the list and to the window's edge.
pub const OPEN_PEEK_LINES: usize = 12;
pub const OPEN_PEEK_BYTES: usize = 4096;
pub const OPEN_PEEK_WIDTH: f32 = 360.;
pub const OPEN_PEEK_MIN_WIDTH: f32 = 220.;
pub const OPEN_PEEK_GAP: f32 = 8.;
/// How many files' first lines Open quickly keeps, so arrowing back to one shows it at once.
pub const OPEN_PEEK_CACHED: usize = 8;

/// Tabular figures (`tnum`) for every changing number, one shared `Arc`, in the sans face.
pub fn tabular() -> FontFeatures {
    static TABULAR: LazyLock<FontFeatures> =
        LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".to_string(), 1)])));
    TABULAR.clone()
}

// ---- Panes: the split editor ----

/// A pane opening or closing: the length its share of the split settles in, as a critically damped spring (`motion::Glide`).
pub const SPLIT: f32 = 0.3;
/// A jump to a tab or a file in the tree: the length the scroll offset glides in to bring it into view (`motion::ScrollGlide`).
pub const SCROLL_GLIDE: f32 = 0.22;
/// A jump to a row the tree has to scroll to: how far from the edge it comes in from the row lands.
pub const REVEAL_INSET: Pixels = px(24.);
/// A jump to a row already in view: the selection's wash brightens by this fraction of itself at the peak of the flash, over `SCROLL_GLIDE`.
pub const SELECTED_FLASH: f32 = 1.0;
/// An unfocused pane (with more than one in the window): the ground drawn over it at this alpha.
pub const PANE_WASH: f32 = 0.3;
/// The focused pane's lit edge under its tab bar: this thick.
pub const PANE_EDGE: Pixels = px(1.);
/// The focused pane's edge on a dark theme: the foreground at this alpha.
pub const PANE_EDGE_ALPHA: f32 = 0.85;
/// The focused pane's edge on a light theme: softer, where dark ink on white reads stronger.
pub const PANE_EDGE_ALPHA_LIGHT: f32 = 0.45;
/// The width of the strip around a divider that takes the pointer; the line itself is one device pixel.
pub const DIVIDER_GRAB: Pixels = px(8.);
/// Of a side-by-side divider's `DIVIDER_GRAB`, this much reaches left of the line and the rest right of it, so the strip stays off the left pane's scrollbar (`SCROLL_INSET` plus `SCROLL_KNOB` is 9 in from the edge, and the strip stops 2 short of the knob).
pub const DIVIDER_GRAB_LEAD: Pixels = px(2.);
/// A divider under the pointer or held, any divider: its hairline eases to the foreground at this alpha (through the theme's ink) over `HOVER_IN`, and back over `HOVER_OUT`.
pub const ALPHA_DIVIDER_LIT: f32 = 0.35;
/// The least a dragged divider leaves either pane, in points: the bar's inset, one `TAB_FLOOR_WIDTH` tab, the list button and the preview button's room still show a whole tab (finch#jmzs).
pub const PANE_MIN: f32 = 200.;
/// A divider dragged past `PANE_MIN` stretches: the line moves this share of the pointer's overshoot (UIScrollView's 0.55).
pub const STRETCH_PULL: f32 = 0.55;
/// The most a divider stretches past `PANE_MIN`, in points.
pub const STRETCH_MAX: f32 = 36.;
/// The spring rate for a damping of 0.75 that settles in the span it is divided into: the envelope e^(-ζωt) is under a thousandth there (ln 1000).
pub const SOFT_SETTLE_RATE: f32 = 6.91;
/// A divider's double-click evens the split out on a spring of this damping (settle `SETTLE`): about 3% past even.
pub const EQUALIZE_DAMPING: f32 = 0.75;
/// The pointer's last this many moves give the speed a released stretch springs back with.
pub const TRAIL_SAMPLES: usize = 16;
/// The speed is read over the last this many seconds of the pointer's movement.
pub const TRAIL_SPAN: f32 = 0.1;
/// While a tab or files are dragged over a pane: each edge band, which opens a new pane there, is this share of the pane's width (left, right) or of its height under the bar (top, bottom).
pub const PANE_DROP_EDGE: f32 = 0.25;
/// The drop preview's corner radius.
pub const PANE_DROP_RADIUS: Pixels = px(8.);
/// The drop preview sits this far inside the zone it marks, in points.
pub const PANE_DROP_INSET: f32 = 4.;
/// A pane shorter than this shows its empty state as the title and the buttons only: the symbol and the line would clip.
pub const EMPTY_COMPACT_HEIGHT: Pixels = px(170.);

// ---- Column slides: the explorer and the preview showing or hiding ----

/// A side panel sliding open or shut: the length its drawn width settles in, as a critically damped spring.
pub const COLUMN_SLIDE: f32 = 0.4;
/// The share of an opening slide a panel's content waits hidden before it eases in; a closing slide fades it over its last share this size.
pub const COLUMN_FADE_LEAD: f32 = 0.3;

/// How close to its end, in points, a sliding column lands instead of crawling the last of the spring.
pub const COLUMN_LAND: f32 = 0.5;

// ---- Scrollbars and list edges ----

/// A scrollbar knob's alpha over its ground at rest.
pub const KNOB_ALPHA: f32 = 0.4;
/// A scrollbar knob's alpha while the pointer is over its track.
pub const KNOB_ALPHA_HOVER: f32 = 0.55;
/// The track's wash under the knob at full hover.
pub const TRACK_ALPHA: f32 = 0.05;
/// The hairline on the track's inner edge at full hover.
pub const HAIRLINE_ALPHA: f32 = 0.08;

/// A scrollbar's knob across, at rest.
pub const SCROLL_KNOB: Pixels = px(7.);
/// A scrollbar's knob across while the pointer is over its track.
pub const SCROLL_KNOB_HOVER: Pixels = px(11.);
/// The faint track drawn under the knob while the pointer is over it.
pub const SCROLL_TRACK: Pixels = px(15.);
/// How much of the last page stays in view when a click in a scrollbar's track pages the view, as AppKit leaves a line or two.
pub const SCROLL_PAGE_OVERLAP: Pixels = px(40.);
/// The shortest a knob gets along its track.
pub const SCROLL_KNOB_MIN: Pixels = px(24.);
/// The knob's inset from the view's edge and from the track's ends.
pub const SCROLL_INSET: Pixels = px(2.);
/// A scrollbar holds this long after the last hand scroll before it fades.
pub const SCROLL_IDLE: f32 = 0.5;
/// A scrollbar fading out, on smoothstep.
pub const SCROLL_FADE_OUT: f32 = 0.32;
/// A find tick is `SCROLL_KNOB` across (the knob's width at rest), right-aligned at `SCROLL_INSET`,
/// so it sits under the knob and never past it. A match's tick is at least this tall, so a single
/// line stays visible in a long file; ticks join only where they touch within a device pixel.
pub const MARK_MATCH_MIN: Pixels = px(3.);
/// The caret line's tick on the track, tall.
pub const MARK_CARET: Pixels = px(2.);
/// A tick's corner radius, at most half its height.
pub const MARK_RADIUS: Pixels = px(1.5);
/// The caret line's tick: the caret color at this alpha.
pub const MARK_CARET_ALPHA: f32 = 0.9;
/// The empty Untitled page's "Start typing…" hint: the editor's muted ink at this alpha, so it reads as a prompt and never as text.
pub const HINT_ALPHA: f32 = 0.6;
/// The tree and the preview fade this far at an edge with more to scroll past it (the bottom, and the top once scrolled).
pub const LIST_EDGE_FADE: Pixels = px(24.);

// ---- Find bar: the capsule floating over the editor's text ----

/// How far (points) the find bar rises as it arrives, over `OVERLAY_ENTER`; it leaves over `OVERLAY_EXIT`.
pub const OVERLAY_ENTER_RISE: f32 = 8.;
/// A refusal's shake: how far (points) the refusing field or tab bar swings at most (the first swing; it decays to 0).
pub const SHAKE_PEAK: f32 = 6.;
/// A refusal's shake: how long it runs, in seconds.
pub const SHAKE_SECS: f32 = 0.35;
/// A refusal's shake: how many full swings it makes.
pub const SHAKE_SWINGS: f32 = 3.;
/// The find bar's height with one row; a replace row adds the same again.
pub const FIND_HEIGHT: Pixels = px(32.);
/// The find bar's and Go to line's corner radius, in points: half the drawn height, `FIND_HEIGHT` plus the rim's 1 pt on each side, so the ends are true half circles.
pub const RADIUS_FLOAT_CAPSULE: f32 = 17.;
/// The find bar's distance under the tab band, in the editor's top right.
pub const FIND_INSET_TOP: Pixels = px(8.);
/// The find bar's distance from the editor's right edge: clear of the scrollbar's match ticks.
pub const FIND_INSET_RIGHT: Pixels = px(20.);
/// Space between the bar's rim and the field capsule, and between its controls.
pub const FIND_PAD: Pixels = px(4.);
/// The field capsule's height inside the bar.
pub const FIND_FIELD_HEIGHT: Pixels = px(24.);
/// The field capsule's width.
pub const FIND_FIELD_WIDTH: Pixels = px(176.);
/// The bar's controls: symbol size, in points.
pub const FIND_SYMBOL: f32 = 13.;
/// The Go to line float's width: its field capsule (`FIND_FIELD_WIDTH` and a little more, for "120:8" and the placeholder) between the rim's `FIND_PAD`s.
pub const GO_TO_LINE_WIDTH: Pixels = px(200.);
/// The Open Quickly float's width: room for a long file name and some of its folder on each row.
pub const OPEN_QUICKLY_WIDTH: Pixels = px(440.);
/// How many files Open Quickly lists at most: a longer list means typing more of the name.
pub const OPEN_QUICKLY_ROWS: usize = 10;
/// How long the closed-bar Find next count float stays after its last step before it fades out, in seconds.
pub const FIND_FLOAT_HOLD: f32 = 1.;
/// The tab list button's badge dot (`DOT` wide, shown while a hidden tab has unsaved edits or a warning): how far it sits in from the button's top and right.
pub const TAB_LIST_BADGE_INSET: Pixels = px(2.);
/// The explorer's one leading edge, from the column's side: the root header's name, the first chevrons' slot and the filter capsule's left edge all start here (the rows' own 6 pt list inset plus `ROW_LEADING`), and the capsule's right edge and the header buttons end the same distance in.
pub const SIDEBAR_EDGE: Pixels = px(14.);
/// The chevron's ink starts about this far inside its 9 pt box (a right-pointing one 2.5, a turned-down one 1.5), so the chevron is drawn this far left of the slot's edge and its ink lands on `SIDEBAR_EDGE`, give or take half a point.
pub const CHEVRON_BEARING: Pixels = px(2.);
/// The find bar's option toggles (Match case, Whole word, Regular expression) at rest: the ink at this alpha behind the button, so each reads as a button before it is touched.
pub const ALPHA_OPTION_REST: f32 = 0.07;
/// The same toggles when on: the accent color at this alpha behind the button, clear of the rest wash and the press wash.
pub const ALPHA_OPTION_ON: f32 = 0.32;
/// The preview's link pill (where the link under the pointer goes): how far it sits in from the preview's left and bottom edges.
pub const LINK_PILL_INSET: f32 = 8.;
/// The longest the link pill's words run, in characters; a longer link loses its middle.
pub const LINK_PILL_MAX_CHARS: usize = 44;
/// The preview's Copied badge: how far it sits in from the preview's right edge. The link pill's inset plus the scrollbar's track (`SCROLL_TRACK`, 15 pt), so the badge clears the track instead of sitting on it.
pub const COPIED_BADGE_INSET_RIGHT: Pixels = px(23.);
/// The preview code block's copy button symbol (and its check after a click), in points.
pub const COPY_SYMBOL: f32 = 12.;
/// How long a copy or paste may run before its source row's name starts to breathe (finch#jhd9); a quicker copy shows nothing.
pub const COPY_QUIET: Duration = Duration::from_millis(250);
/// The Compare view's header (file name, change count, buttons): its height.
pub const COMPARE_HEADER_HEIGHT: Pixels = px(40.);
/// The Compare view's inset: the header's sides, and the text inside each column.
pub const COMPARE_INSET: Pixels = px(12.);
/// The least width the Compare header keeps for the file name (its start and its extension stay readable); the count and the buttons give way first.
pub const COMPARE_NAME_MIN: Pixels = px(120.);
/// A changed line's wash in the Compare view: the theme's green (yours) or red (disk) at this alpha.
pub const ALPHA_COMPARE_WASH: f32 = 0.18;
/// A change whose side is not the picked one, in the Compare view: its text and wash at this alpha.
pub const ALPHA_COMPARE_UNPICKED: f32 = 0.4;
/// The blank filler rows of the shorter side of a change: the foreground at this alpha.
pub const ALPHA_COMPARE_FILLER: f32 = 0.05;
/// The Compare view's per-change chip (Yours, Disk): its height.
pub const COMPARE_CHIP_HEIGHT: Pixels = px(16.);
/// The chip's corner radius.
pub const RADIUS_COMPARE_CHIP: Pixels = px(4.);
/// The picked side's chip: the foreground at this alpha behind it.
pub const ALPHA_COMPARE_CHIP_ON: f32 = 0.16;
/// The least right padding of a change's first row, so its text stops short of the Yours or Disk chip; a wider chip (an old version's time) asks for more.
pub const COMPARE_CHIP_ROOM: Pixels = px(64.);
/// The gap between a change's first-row text and the chip beside it, added to the chip's own width when the room is measured.
pub const COMPARE_CHIP_GAP: Pixels = px(2.);
/// How many rows of context the Compare view leaves above the first change when it opens scrolled to it.
pub const COMPARE_CONTEXT_ROWS: usize = 3;
/// The line numbers beside each side of the Compare view: the sidebar foreground at this alpha, as the editor gutter's other lines.
pub const ALPHA_COMPARE_NUMBER: f32 = 0.4;
/// The space between a Compare line number and its text.
pub const COMPARE_NUMBER_GAP: Pixels = px(12.);

// ---- Settings window: a SwiftUI Settings form (sidebar of panes, grouped rows), as Harrier's Settings is ----

/// The Settings window's width: fixed, the sidebar and a 440 pt page.
pub const SETTINGS_WINDOW_WIDTH: f32 = 630.;
/// The Settings window's height when it opens; it resizes taller or shorter, never wider.
pub const SETTINGS_WINDOW_HEIGHT: f32 = 620.;
/// The shortest the Settings window gets.
pub const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 420.;
/// The sidebar's width: the system sidebar material behind the pane list and the traffic lights.
pub const SETTINGS_SIDEBAR_WIDTH: f32 = 190.;
/// The pane list's inset from the sidebar's sides.
pub const SETTINGS_SIDEBAR_INSET: f32 = 10.;
/// Where the first pane row starts: under the traffic lights.
pub const SETTINGS_SIDEBAR_TOP: f32 = 46.;
/// A pane row's height; the selection pill is the row less `ROW_WASH_INSET_Y` above and below.
pub const SETTINGS_SIDEBAR_ROW: f32 = 32.;
/// A pane row's inset left of its tile, and the gap between tile and name.
pub const SETTINGS_SIDEBAR_ROW_PAD: f32 = 8.;
/// A pane's colored tile behind its symbol, as System Settings sets each pane's icon: its side.
pub const SETTINGS_TILE: f32 = 20.;
/// The tile's corner radius.
pub const SETTINGS_TILE_RADIUS: f32 = 5.;
/// The symbol on the tile, in points.
pub const SETTINGS_TILE_SYMBOL: f32 = 12.;
/// How far the tile's top is lighter than its bottom: the white mixed into its top edge.
pub const SETTINGS_TILE_SHEEN: f32 = 0.16;
/// The white ring a pane's tile wears on the selected row, so the tile stays a tile on the accent (or grey) pill: its alpha.
pub const SETTINGS_TILE_RING_ALPHA: f32 = 0.9;
/// The General pane's tile: System Settings' grey.
pub const SETTINGS_TILE_GRAY: u32 = 0x8E8E93;
/// The Editor pane's tile: system blue.
pub const SETTINGS_TILE_BLUE: u32 = 0x007AFF;
/// The Writing pane's tile: system orange.
pub const SETTINGS_TILE_ORANGE: u32 = 0xFF9500;
/// The Code Colors pane's tile: system indigo.
pub const SETTINGS_TILE_INDIGO: u32 = 0x5856D6;
/// The Text pane's tile: system pink.
pub const SETTINGS_TILE_PINK: u32 = 0xFF2D55;
/// The pane title in the top band, level with the traffic lights: semibold, in points.
pub const SETTINGS_TITLE_SIZE: f32 = 15.;
/// The page's side and foot inset around its form.
pub const SETTINGS_FORM_INSET: f32 = 20.;
/// The page's space between the top band and the first section.
pub const SETTINGS_FORM_TOP: f32 = 8.;
/// The step from one section's last line to the next section's header.
pub const SETTINGS_SECTION_GAP: f32 = 26.;
/// A section header's height (13 pt semibold) and its gap to the box under it.
pub const SETTINGS_HEADER_HEIGHT: f32 = 22.;
/// The gap between a header and its box, and between a box and its footnote.
pub const SETTINGS_HEADER_GAP: f32 = 8.;
/// A grouped box's corner radius.
pub const SETTINGS_GROUP_RADIUS: f32 = 12.;
/// A grouped box's inset on its rows' sides; its 1 pt edge sits outside this.
pub const SETTINGS_GROUP_INSET: f32 = 10.;
/// A row's least height (label and control, no note).
pub const SETTINGS_ROW_HEIGHT: f32 = 40.;
/// A row's space above and below its label and note.
pub const SETTINGS_ROW_PAD: f32 = 8.;
/// The gap between a row's label and its control, and between a label and the note under it.
pub const SETTINGS_ROW_GAP: f32 = 12.;
/// A note under a row's label (11 pt): its line height.
pub const SETTINGS_NOTE_LINE: f32 = 14.;
/// How far a footnote under a box and a note under a row stop short of the control column on the right.
pub const SETTINGS_NOTE_TRAIL: f32 = 48.;
/// The rule between two rows, and between a stepper's halves: its height.
pub const SETTINGS_RULE_HEIGHT: f32 = 1.;
/// A switch's track, as an NSSwitch: width and height.
pub const SETTINGS_SWITCH_TRACK: (f32, f32) = (38., 22.);
/// A switch's knob: width and height at rest.
pub const SETTINGS_SWITCH_KNOB: (f32, f32) = (18., 18.);
/// How far the knob sits in from the track's ends.
pub const SETTINGS_SWITCH_INSET: f32 = 2.;
/// How much wider the knob gets at the middle of a slide and while the row is held.
pub const SETTINGS_KNOB_STRETCH: f32 = 4.;
/// How far the knob's soft shadow sits below it.
pub const SETTINGS_KNOB_SHADOW_DROP: f32 = 0.5;
/// The knob's and the slider thumb's lightness: a light capsule in both appearances.
pub const SETTINGS_KNOB_LIGHTNESS: f32 = 0.97;
/// The knob's shadow: black at this alpha.
pub const SETTINGS_KNOB_SHADOW_ALPHA: f32 = 0.22;
/// A knob, a thumb or a segmented pill's slide settles on a critically damped spring in this many seconds.
pub const SETTINGS_SPRING: f32 = 0.35;
/// A push button, a field, a stepper and a segmented control: their height, as AppKit's regular ones.
pub const SETTINGS_CONTROL_HEIGHT: f32 = 22.;
/// A push button's and a segment's padding on its sides.
pub const SETTINGS_BUTTON_PAD: f32 = 10.;
/// A segmented control's space around its pill, and between segments.
pub const SETTINGS_SEGMENT_PAD: f32 = 2.;
/// A pop-up's round chip of chevrons: its diameter.
pub const SETTINGS_POPUP_CHIP: f32 = 20.;
/// The gap between a pop-up's title and its chip.
pub const SETTINGS_POPUP_GAP: f32 = 8.;
/// The chevrons in a pop-up's chip and a stepper's halves, in points.
pub const SETTINGS_CHEVRON: f32 = 8.;
/// A stepper's arrow half: width.
pub const SETTINGS_STEPPER_WIDTH: f32 = 20.;
/// A text field's width beside its label.
pub const SETTINGS_FIELD_WIDTH: f32 = 220.;
/// A hex field's width.
pub const SETTINGS_HEX_WIDTH: f32 = 92.;
/// A text field's padding on its sides, inside its bezel.
pub const SETTINGS_FIELD_PAD: f32 = 6.;
/// A color override's swatch: its side and corner radius.
pub const SETTINGS_SWATCH: f32 = 18.;
/// The swatch's corner radius.
pub const SETTINGS_SWATCH_RADIUS: f32 = 5.;
/// A slider's height: the track the thumb travels in.
pub const SETTINGS_SLIDER_HEIGHT: f32 = 22.;
/// A slider's rail thickness.
pub const SETTINGS_SLIDER_RAIL: f32 = 4.;
/// A slider's thumb: width and height.
pub const SETTINGS_SLIDER_KNOB: (f32, f32) = (20., 16.);
/// A slider row's label column and its value readout column.
pub const SETTINGS_SLIDER_LABEL: f32 = 110.;
/// A slider's value readout: its width.
pub const SETTINGS_SLIDER_READOUT: f32 = 48.;
/// The code preview card's text size and line height, in points.
pub const SETTINGS_PREVIEW_SIZE: f32 = 12.;
/// The preview's line height.
pub const SETTINGS_PREVIEW_LINE: f32 = 18.;
/// The preview card's padding.
pub const SETTINGS_PREVIEW_PAD: f32 = 12.;
/// A grouped box's fill in the dark theme: white at this alpha over the ground.
pub const SETTINGS_GROUP_ALPHA: f32 = 0.04;
/// The Settings page's ground in the dark theme, Harrier's: a step lighter than the editor's ground, so the form reads as a window of its own.
pub const SETTINGS_GROUND_DARK: u32 = 0x1f1f21;
/// The Settings page's ground in the light theme: AppKit's light window grey, so the white grouped boxes stand off it as in System Settings.
pub const SETTINGS_GROUND_LIGHT: u32 = 0xF0F0F0;
/// The tint over the sidebar column, so it stays its own surface where the material draws flat (a window that isn't key): white at this alpha in the dark theme, black in the light.
pub const SETTINGS_SIDEBAR_FILL_ALPHA: f32 = 0.05;
pub const SETTINGS_SIDEBAR_FILL_ALPHA_LIGHT: f32 = 0.04;
/// A grouped box's fill in the light theme: white at this alpha, a card on the window grey.
pub const SETTINGS_GROUP_ALPHA_LIGHT: f32 = 1.;
/// The hairline around a box in the dark theme: white at this alpha.
pub const SETTINGS_EDGE_ALPHA: f32 = 0.07;
/// The hairline around a box in the light theme: black at this alpha.
pub const SETTINGS_EDGE_ALPHA_LIGHT: f32 = 0.08;
/// The rule between two rows: the wash (white in the dark theme, black in the light) at this alpha.
pub const SETTINGS_RULE_ALPHA: f32 = 0.09;
/// A button's, a stepper's, a pop-up chip's and a segmented control's fill: the wash at this alpha.
pub const SETTINGS_FILL_ALPHA: f32 = 0.10;
/// `SETTINGS_FILL_ALPHA` in the light theme.
pub const SETTINGS_FILL_ALPHA_LIGHT: f32 = 0.06;
/// The same fill under the pointer.
pub const SETTINGS_FILL_HOVER_ALPHA: f32 = 0.16;
/// `SETTINGS_FILL_HOVER_ALPHA` in the light theme.
pub const SETTINGS_FILL_HOVER_ALPHA_LIGHT: f32 = 0.10;
/// The same fill while pressed.
pub const SETTINGS_FILL_PRESS_ALPHA: f32 = 0.22;
/// `SETTINGS_FILL_PRESS_ALPHA` in the light theme.
pub const SETTINGS_FILL_PRESS_ALPHA_LIGHT: f32 = 0.15;
/// A switch track and a slider rail while off: the wash at this alpha.
pub const SETTINGS_TRACK_ALPHA: f32 = 0.16;
/// `SETTINGS_TRACK_ALPHA` in the light theme: firmer than the dark one's step, so an off switch's track stands off a white box (finch#efkp).
pub const SETTINGS_TRACK_ALPHA_LIGHT: f32 = 0.2;
/// A text field's fill in the dark theme: black at this alpha, sunk into the box.
pub const SETTINGS_FIELD_ALPHA: f32 = 0.22;
/// A text field's fill in the light theme: black at this alpha.
pub const SETTINGS_FIELD_ALPHA_LIGHT: f32 = 0.04;
/// A text field's one edge: the wash at this alpha.
pub const SETTINGS_FIELD_EDGE_ALPHA: f32 = 0.12;
/// A text field whose text isn't valid yet (a bad color, a shortcut): the danger color at this alpha over its fill.
pub const SETTINGS_FIELD_BAD_ALPHA: f32 = 0.18;
/// A pane row of the sidebar under the pointer: the wash at this alpha.
pub const SETTINGS_SIDEBAR_HOVER_ALPHA: f32 = 0.06;
/// The selected pane row while the window isn't key: the wash at this alpha, grey instead of the accent.
pub const SETTINGS_SIDEBAR_IDLE_ALPHA: f32 = 0.14;
/// Most matched entries the explorer filter builds rows for, in index order; past it a note row says how many show. One letter in a 100k-file folder would otherwise build a node and a row for every match.
pub const FILTER_MATCH_CAP: usize = 1500;
/// Most files and folders the file index (Open Quickly, the explorer filter, `[[` note links) holds; the walk stops there and Open Quickly says the list is cut. A window with no folder is rooted at the home folder, and ~/Library alone holds over a million entries on this Mac, each about 150 bytes while the index is built. 200,000 was picked as about 30 MB at that size, twice the 100,000-file folder the filter was measured on.
pub const INDEX_ENTRY_CAP: usize = 200_000;

// ---- Explorer memory ----

/// How many child rows the shut folders that stay warm keep between them, so a quick reopen still slides with its rows: twelve 5,000-file folders held 60,000 items (about 8 MB dirty heap and 120,000 live allocations) before this; the folder shut longest goes first once the kept rows pass it.
pub const WARM_SHUT_ROWS: usize = 3000;
/// How many steps the explorer's Cmd-Z list keeps: the oldest is let go of past it, and a new root empties the list.
pub const UNDO_STEPS: usize = 100;

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers a source line writes before its comment: runs of digits and dots.
    fn literals(code: &str) -> Vec<f32> {
        let mut found = Vec::new();
        let mut run = String::new();
        for ch in code.chars().chain(std::iter::once(' ')) {
            if ch.is_ascii_digit() || ch == '.' {
                run.push(ch);
                continue;
            }
            if run.chars().any(|c| c.is_ascii_digit()) {
                found.extend(run.parse::<f32>());
            }
            run.clear();
        }
        found
    }

    /// The editor's own copies of the ink curve and selection alphas (vendor/gpui-base, system_look.rs and
    /// theme_tokens.rs, which can't reach this crate) each carry a `// mirrors tokens::NAME` comment: the line's
    /// number must be that token. The unfocused fill moved from system_look.rs to theme_tokens.rs in c41c9aeb.
    #[test]
    fn system_look_copies_match_the_tokens() {
        let sources = [
            include_str!("../vendor/gpui-base/src/input/base/system_look.rs"),
            include_str!("../vendor/gpui-base/src/theme_tokens.rs"),
        ];
        let mut checked = 0;
        for line in sources.iter().flat_map(|source| source.lines()) {
            let Some((code, name)) = line.split_once("// mirrors tokens::") else { continue };
            let token = match name.trim() {
                "INK_GAMMA_DARK" => INK_GAMMA_DARK,
                "INK_GAMMA_LIGHT" => INK_GAMMA_LIGHT,
                "INK_TOWARD_BLACK" => INK_TOWARD_BLACK,
                "ALPHA_SELECTED_UNFOCUSED" => ALPHA_SELECTED_UNFOCUSED,
                "ALPHA_SELECTED_UNFOCUSED_LIGHT" => ALPHA_SELECTED_UNFOCUSED_LIGHT,
                "ALPHA_TEXT_SELECTION" => ALPHA_TEXT_SELECTION,
                "ALPHA_TEXT_SELECTION_LIGHT" => ALPHA_TEXT_SELECTION_LIGHT,
                other => panic!("the vendored copies mirror an unknown token `{other}`"),
            };
            assert!(literals(code).iter().any(|v| (v - token).abs() < 1e-6), "`{}` doesn't carry tokens::{}", code.trim(), name.trim());
            checked += 1;
        }
        assert_eq!(checked, 7, "a vendored copy lost a mirrors comment");
    }

    fn all_palettes() -> Vec<(crate::settings::Scheme, bool, &'static Palette)> {
        crate::settings::Scheme::ALL.iter().flat_map(|scheme| [false, true].map(|light| (*scheme, light, palette(*scheme, light)))).collect()
    }

    #[test]
    fn vivid_colors_read_on_the_ground_and_never_lose_their_hue() {
        for (scheme, light, p) in all_palettes() {
            let ground = chrome_ground(light);
            for hex in [p.keyword, p.string, p.number, p.type_, p.declared_type, p.function, p.macro_, p.attribute, p.link, p.field, p.parameter, p.constant, p.variant, p.self_, p.builtin_type, p.method, p.label, p.escape, p.list_marker, p.quote] {
                let base: Hsla = gpui_kit::rgb(hex).into();
                let lifted = vivid(base, light, ground);
                assert!(contrast(lifted, ground) >= VIVID_CONTRAST - 0.05 || (lifted.l - if light { 0.1 } else { 0.95 }).abs() < 0.02, "{scheme:?} light={light} {hex:06X} reads at {}", contrast(lifted, ground));
                assert!((lifted.h - base.h).abs() < 1e-4, "{scheme:?} {hex:06X} changed hue");
                assert!(lifted.s >= base.s - 1e-4, "{scheme:?} {hex:06X} lost saturation");
            }
        }
    }

    #[test]
    fn xcode_keeps_the_hues_it_always_had() {
        assert_eq!((PALETTE_XCODE_DARK.keyword, PALETTE_XCODE_DARK.string, PALETTE_XCODE_DARK.number, PALETTE_XCODE_DARK.comment), (0xFC5FA3, 0xFC6A5D, 0xD0BF69, 0x6C7986));
        assert_eq!((PALETTE_XCODE_LIGHT.keyword, PALETTE_XCODE_LIGHT.string, PALETTE_XCODE_LIGHT.number, PALETTE_XCODE_LIGHT.link), (0x9B2393, 0xC41A16, 0x1C00CF, 0x0E0EFF));
    }

    #[test]
    fn the_copied_badge_clears_the_scrollbar_track() {
        assert_eq!(COPIED_BADGE_INSET_RIGHT, px(LINK_PILL_INSET) + SCROLL_TRACK);
    }

    #[test]
    fn marked_text_reads_on_the_dark_ground() {
        let ground = chrome_ground(false);
        let fill: Hsla = gpui_kit::rgb(MARK_AMBER_DARK).into();
        let under = ground.blend(fill.opacity(ALPHA_MARK_DARK));
        // The brightest ink a dark theme could set, and the preview's body ink at its dimmest plausible.
        for ink in [gpui_kit::rgb(0xFFFFFF), gpui_kit::rgb(0xD4D4D4)] {
            assert!(contrast(ink.into(), under) >= 4.5, "ink {ink:?} reads at {}", contrast(ink.into(), under));
        }
    }
}
