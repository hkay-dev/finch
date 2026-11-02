//! Color swatches: a small round chip beside each color written in code (`#FF9F0A`, `#fff`,
//! `#RRGGBBAA`, `0x423E39`, `rgb(...)`, `rgba(...)`, `hsl(...)`, `hsla(...)`), and a click on it
//! opens the Mac color panel on that color. Picking rewrites the text in the format it was
//! written in, and the whole pick session is one Undo step.
//!
//! The chip takes no width: the text, the caret math and the selection are untouched. A sibling
//! view (`Chips`, drawn by the pane over the editor, as the caret layer is) paints the chips from
//! where the editor says each token starts (`range_to_bounds`), so the editor's own runs are never
//! split or recolored, and the LSP document-colors path is not used. The chip sits in the space
//! before the token when a space or tab is there, and under the token's first letter otherwise.
//!
//! It is a layer on the shared mark driver (`marks.rs`): the scan reads only `view.bytes` (the
//! screen and a screen each way), runs only when the text revision or that window changes (a
//! scroll step inside the window, a caret move and a selection change cost one compare), and keeps
//! at most `MAX` swatches. Prose (Markdown and plain text) is never scanned, where `#123` is an
//! issue number and not a color. The list is shared with the view through an entity that is
//! notified only when the list differs.
//!
//! The pick: a click on a chip reads the token's text, remembers its format and range, and shows
//! `NSColorPanel` with a target of ours (written against the Objective-C runtime like
//! `native_menu.rs`; AppKit is already loaded). Each pick first steps the history back to where
//! the session began (`history_jump`) and then replaces the token once (`replace_range`), so the
//! session is one step however long the drag. Any other edit ends the session (the text revision
//! no longer matches) and the panel's further picks are ignored until a chip is clicked again.

use std::{
    cell::RefCell,
    ops::Range,
    rc::{Rc, Weak},
};

use gpui_kit::component::input::{EditorState, Rope};
use gpui_kit::{component::ActiveTheme as _, *};
use objc2::{
    ClassType, class, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, NSObject},
    sel,
};

use crate::{
    marks::{self, MarkLayer, View},
    motion,
    tokens::{ALPHA_PRIMARY, FADE_QUICK},
};

/// The most swatches a window holds.
const MAX: usize = 500;

/// The biggest window, in bytes, that is scanned (a minified one-line file is left alone).
const MAX_SCAN: usize = 512 * 1024;

/// The longest `rgb(...)` or `hsl(...)` argument list looked at, in bytes.
const MAX_ARGS: usize = 80;

// ---- Colors and the formats they are written in ----

/// An sRGB color with a 0..=1 alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: f32,
}

impl Color {
    fn alpha_byte(self) -> u8 {
        (self.a.clamp(0., 1.) * 255.).round() as u8
    }

    /// The same 8-bit color: what a pick that changed nothing visible compares by.
    fn same(self, other: Color) -> bool {
        (self.r, self.g, self.b, self.alpha_byte()) == (other.r, other.g, other.b, other.alpha_byte())
    }

    fn hsla(self) -> Hsla {
        Rgba { r: self.r as f32 / 255., g: self.g as f32 / 255., b: self.b as f32 / 255., a: self.a }.into()
    }
}

/// How a color was written, so a pick is written the same way.
#[derive(Clone, Debug, PartialEq)]
enum Format {
    /// `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` (`prefix` `#`), or `0xrrggbb` (`0x`, `0X`).
    Hex { prefix: &'static str, digits: usize, upper: bool },
    /// `rgb(...)`, `rgba(...)`, `hsl(...)`, `hsla(...)` in the comma or the space syntax.
    Func {
        /// As written (`rgb`, `RGBA`).
        name: String,
        hsl: bool,
        /// Between the first three arguments, and before the alpha.
        sep: &'static str,
        alpha_sep: &'static str,
        /// `rgb` channels written as percentages.
        percent: bool,
        /// The hue was written with `deg`.
        deg: bool,
        /// An alpha was written, and whether as a percentage.
        alpha: Option<bool>,
    },
}

impl Format {
    fn has_alpha(&self) -> bool {
        match self {
            Format::Hex { digits, prefix, .. } => *prefix == "#" && matches!(digits, 4 | 8),
            Format::Func { .. } => true,
        }
    }

    /// `color` written in this format.
    fn write(&self, color: Color) -> String {
        match self {
            Format::Hex { prefix, digits, upper } => {
                let a = color.alpha_byte();
                let want_alpha = *prefix == "#" && (matches!(digits, 4 | 8) || a != 255);
                let all = [color.r, color.g, color.b];
                let short = *prefix == "#" && *digits <= 4 && all.iter().all(|c| c % 17 == 0) && (!want_alpha || a % 17 == 0);
                let mut out = String::from(*prefix);
                let channels: Vec<u8> = all.into_iter().chain(want_alpha.then_some(a)).collect();
                for channel in channels {
                    let text = match (short, upper) {
                        (true, true) => format!("{:X}", channel / 17),
                        (true, false) => format!("{:x}", channel / 17),
                        (false, true) => format!("{channel:02X}"),
                        (false, false) => format!("{channel:02x}"),
                    };
                    out.push_str(&text);
                }
                out
            }
            Format::Func { name, hsl, sep, alpha_sep, percent, deg, alpha } => {
                let (x, y, z) = if *hsl {
                    let (h, s, l) = rgb_to_hsl(color.r, color.g, color.b);
                    (format!("{}{}", h.round() as i32 % 360, if *deg { "deg" } else { "" }), format!("{}%", (s * 100.).round()), format!("{}%", (l * 100.).round()))
                } else if *percent {
                    let pct = |c: u8| format!("{}%", (c as f32 * 100. / 255.).round());
                    (pct(color.r), pct(color.g), pct(color.b))
                } else {
                    (color.r.to_string(), color.g.to_string(), color.b.to_string())
                };
                let mut out = format!("{name}({x}{sep}{y}{sep}{z}");
                if alpha.is_some() || color.alpha_byte() != 255 {
                    if *alpha == Some(true) {
                        out.push_str(&format!("{alpha_sep}{}%", (color.a * 100.).round()));
                    } else {
                        let number = format!("{:.2}", color.a);
                        out.push_str(&format!("{alpha_sep}{}", number.trim_end_matches('0').trim_end_matches('.')));
                    }
                }
                out.push(')');
                out
            }
        }
    }
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255., g as f32 / 255., b as f32 / 255.);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.;
    let d = max - min;
    if d == 0. {
        return (0., 0., l);
    }
    let s = d / (1. - (2. * l - 1.).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.)
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    (h * 60., s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1. - (2. * l - 1.).abs()) * s;
    let h = h.rem_euclid(360.) / 60.;
    let x = c * (1. - (h % 2. - 1.).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    let m = l - c / 2.;
    let byte = |v: f32| ((v + m).clamp(0., 1.) * 255.).round() as u8;
    (byte(r), byte(g), byte(b))
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The color that starts at byte `i` of `text`, with its end and format; `lang` decides about
/// the short all-digit hex forms (`#123` is a color in CSS and an issue number in a comment).
fn color_at(text: &str, i: usize, lang: &str) -> Option<(usize, Color, Format)> {
    let b = text.as_bytes();
    let before = i.checked_sub(1).map(|at| b[at]);
    match b[i] {
        b'#' => {
            if before.is_some_and(|c| is_word(c) || c == b'&') {
                return None;
            }
            hex_at(b, i, 1, lang)
        }
        b'0' if matches!(b.get(i + 1), Some(b'x' | b'X')) => {
            if before.is_some_and(|c| is_word(c) || c == b'.') {
                return None;
            }
            hex_at(b, i, 2, lang)
        }
        b'r' | b'R' | b'h' | b'H' => {
            if before.is_some_and(|c| is_word(c) || c == b'-' || c == b'.') {
                return None;
            }
            func_at(text, i)
        }
        _ => None,
    }
}

fn hex_at(b: &[u8], i: usize, prefix_len: usize, lang: &str) -> Option<(usize, Color, Format)> {
    let start = i + prefix_len;
    let digits = b[start..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
    let end = start + digits;
    if b.get(end).is_some_and(|c| is_word(*c) || *c == b'-') {
        return None;
    }
    let digits_ok = if prefix_len == 1 { matches!(digits, 3 | 4 | 6 | 8) } else { digits == 6 };
    if !digits_ok {
        return None;
    }
    let body = &b[start..end];
    if prefix_len == 1 && digits <= 4 && body.iter().all(u8::is_ascii_digit) {
        let css = matches!(lang, "css" | "html" | "svelte" | "astro" | "ejs" | "erb");
        if !css && body.iter().any(|c| *c != body[0]) {
            return None;
        }
    }
    let value = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
    let channel = |at: usize| if digits <= 4 { value(body[at]) * 17 } else { value(body[at * 2]) * 16 + value(body[at * 2 + 1]) };
    let a = if matches!(digits, 4 | 8) { channel(3) as f32 / 255. } else { 1. };
    let prefix = match b[i + prefix_len - 1] {
        b'#' => "#",
        b'X' => "0X",
        _ => "0x",
    };
    let upper = !body.iter().any(u8::is_ascii_lowercase);
    Some((end, Color { r: channel(0), g: channel(1), b: channel(2), a }, Format::Hex { prefix, digits, upper }))
}

fn func_at(text: &str, i: usize) -> Option<(usize, Color, Format)> {
    let rest = &text[i..];
    let lower = |n: usize| rest.get(..n).map(str::to_ascii_lowercase);
    let (hsl, name_len) = match (lower(4).as_deref(), lower(3).as_deref()) {
        (Some("rgba"), _) => (false, 4),
        (Some("hsla"), _) => (true, 4),
        (_, Some("rgb")) => (false, 3),
        (_, Some("hsl")) => (true, 3),
        _ => return None,
    };
    let bytes = rest.as_bytes();
    if bytes.get(name_len) != Some(&b'(') {
        return None;
    }
    let args = &bytes[name_len + 1..];
    let close = args.iter().take(MAX_ARGS).position(|c| matches!(c, b')' | b'\n'))?;
    if args[close] != b')' {
        return None;
    }
    let inner = &rest[name_len + 1..name_len + 1 + close];
    let parts: Vec<&str> = inner.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|part| !part.is_empty()).collect();
    if !matches!(parts.len(), 3 | 4) {
        return None;
    }
    let sep = if inner.contains(',') {
        if inner.contains(", ") { ", " } else { "," }
    } else {
        " "
    };
    let alpha_sep = if inner.contains('/') {
        if inner.contains(" / ") { " / " } else { "/" }
    } else {
        sep
    };
    let number = |part: &str| -> Option<(f32, bool)> {
        let (digits, percent) = part.strip_suffix('%').map_or((part, false), |digits| (digits, true));
        let value: f32 = digits.parse().ok()?;
        value.is_finite().then_some((value, percent))
    };
    let alpha = match parts.get(3) {
        Some(part) => {
            let (value, percent) = number(part)?;
            Some((if percent { value / 100. } else { value }, percent))
        }
        None => None,
    };
    let (rgb, percent, deg) = if hsl {
        let hue = parts[0].strip_suffix("deg");
        let (h, _) = number(hue.unwrap_or(parts[0]))?;
        let (s, s_pct) = number(parts[1])?;
        let (l, l_pct) = number(parts[2])?;
        if !s_pct || !l_pct || parts[0].ends_with('%') {
            return None;
        }
        (hsl_to_rgb(h, (s / 100.).clamp(0., 1.), (l / 100.).clamp(0., 1.)), false, hue.is_some())
    } else {
        let channels = [number(parts[0])?, number(parts[1])?, number(parts[2])?];
        let percent = channels[0].1;
        if channels.iter().any(|(_, pct)| *pct != percent) {
            return None;
        }
        let byte = |(value, pct): (f32, bool)| (if pct { value * 255. / 100. } else { value }).round().clamp(0., 255.) as u8;
        ((byte(channels[0]), byte(channels[1]), byte(channels[2])), percent, false)
    };
    let color = Color { r: rgb.0, g: rgb.1, b: rgb.2, a: alpha.map_or(1., |(a, _)| a).clamp(0., 1.) };
    let format = Format::Func { name: rest[..name_len].to_string(), hsl, sep, alpha_sep, percent, deg, alpha: alpha.map(|(_, percent)| percent) };
    Some((i + name_len + 1 + close + 1, color, format))
}

// ---- The scan ----

/// One color in the window.
#[derive(Clone, Debug, PartialEq)]
struct Swatch {
    /// The token's bytes in the text.
    range: Range<usize>,
    /// Its buffer row, so painting touches only the chips on screen.
    row: usize,
    color: Color,
    /// A space or tab stands before the token: the chip takes its place, else it goes under.
    before: bool,
    /// The edit that led to this scan made this chip: it fades in once. Never set by the scan alone.
    fresh: bool,
}

/// The colors in `text`, which starts at byte `base` of the document and row `first_row`.
fn scan(text: &str, base: usize, first_row: usize, lang: &str) -> Vec<Swatch> {
    let b = text.as_bytes();
    let (mut out, mut i, mut row, mut counted) = (Vec::new(), 0, first_row, 0);
    while i < b.len() && out.len() < MAX {
        if matches!(b[i], b'#' | b'0' | b'r' | b'R' | b'h' | b'H')
            && let Some((end, color, _)) = color_at(text, i, lang)
        {
            row += b[counted..i].iter().filter(|c| **c == b'\n').count();
            counted = i;
            let before = i > 0 && matches!(b[i - 1], b' ' | b'\t');
            out.push(Swatch { range: base + i..base + end, row, color, before, fresh: false });
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Whether `chunks` (one text, cut anywhere) hold the start of a color token: `#` before a hex digit, `0x`, `rgb` or `hsl` in any case. A cheap look that allocates nothing, so a window without colors is never copied; a start split between two chunks is still seen, as the last two bytes carry over.
fn has_start<'a>(chunks: impl Iterator<Item = &'a str>) -> bool {
    let (mut two, mut one) = (0u8, 0u8);
    for chunk in chunks {
        for &byte in chunk.as_bytes() {
            let lower = byte.to_ascii_lowercase();
            if (one == b'#' && byte.is_ascii_hexdigit()) || (one == b'0' && lower == b'x') || matches!((two, one, lower), (b'r', b'g', b'b') | (b'h', b's', b'l')) {
                return true;
            }
            (two, one) = (one, lower);
        }
    }
    false
}

/// The colors in the window `bytes` of `text`, which starts at row `first_row`. The window is copied out only when it holds the start of some color.
fn scan_window(text: &Rope, bytes: Range<usize>, first_row: usize, lang: &str) -> Vec<Swatch> {
    let window = text.slice(bytes.clone());
    if !has_start(window.chunks()) {
        return Vec::new();
    }
    scan(&window.to_string(), bytes.start, first_row, lang)
}

/// Sets `fresh` on the chips of `new` that the edit just made: not in `old` (the list before the scan), told from the ones the edit only moved or recolored. The chips before the first difference are the same; the chips after the last keep their color and length and all shift by one amount; of what lies between, a chip whose token starts where an old one started was typed on, not made. Only chips on rows `old_rows` had can be new, so rows that scrolled into the window with the edit pop as before. Returns where that shift begins (the old start of the first chip of the tail) and its size, when there is one, so the fades keyed by start can follow their chips.
fn mark_fresh(old: &[Swatch], new: &mut [Swatch], old_rows: &Range<usize>) -> Option<(usize, isize)> {
    let head = old.iter().zip(new.iter()).take_while(|(a, b)| a.range == b.range && a.color == b.color).count();
    let shift = |a: &Swatch, b: &Swatch| (a.color == b.color && a.range.len() == b.range.len()).then(|| b.range.start as isize - a.range.start as isize);
    let (tail, by) = match (old.last(), new.last()) {
        (Some(a), Some(b)) => shift(a, b).map_or((0, 0), |by| (old.iter().rev().zip(new.iter().rev()).take_while(|(a, b)| shift(a, b) == Some(by)).count(), by)),
        _ => (0, 0),
    };
    let (old_end, new_end) = (old.len().saturating_sub(tail).max(head), new.len().saturating_sub(tail).max(head));
    let (before, after) = (&old[head..old_end], &mut new[head..new_end]);
    for swatch in after {
        swatch.fresh = old_rows.contains(&swatch.row) && !before.iter().any(|a| a.range.start == swatch.range.start);
    }
    // The chips after the edit moved by `by`, so what is keyed by their old starts must follow.
    (by != 0 && tail > 0).then(|| (old[old.len() - tail].range.start, by))
}

// ---- The layer ----

/// The swatches of the screen: the layer writes them, the chips view reads them, and a hidden tab empties them.
type Shared = Rc<RefCell<Rc<Vec<Swatch>>>>;

/// What edits moved since the view last armed a list: from this start on, chips are this many bytes further.
type Shifts = Rc<RefCell<Vec<(usize, isize)>>>;

/// What the chips view draws, shared with the layer; notified only when the list differs.
#[derive(Default)]
struct List {
    items: Shared,
    shifts: Shifts,
}

/// The swatch layer of one editor.
pub struct Layer {
    list: Entity<List>,
    /// The list's swatches, the same cell the view reads.
    items: Shared,
    /// The text revision and rows of the last scan.
    seen: Option<(u64, Range<usize>)>,
    /// The same moves cell the view reads: what each edit shifted, until the view paints the list it made.
    shifts: Shifts,
}

impl Layer {
    pub fn new(_: &Entity<EditorState>, cx: &mut App) -> Self {
        let list = cx.new(|_| List::default());
        let items = list.read(cx).items.clone();
        let shifts = list.read(cx).shifts.clone();
        Self { list, items, shifts, seen: None }
    }

    fn set(&self, items: Vec<Swatch>, cx: &mut App) {
        if **self.items.borrow() != items {
            *self.items.borrow_mut() = Rc::new(items);
            self.list.update(cx, |_, cx| cx.notify());
        }
    }
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        true
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        if view.prose() || view.bytes.len() > MAX_SCAN {
            self.seen = None;
            self.shifts.borrow_mut().clear();
            self.set(Vec::new(), cx);
            return;
        }
        let seen = (view.key.rev, view.key.rows.clone());
        if self.seen.as_ref() == Some(&seen) {
            return;
        }
        // Only a scan that follows an edit of text already scanned can find a chip the edit made: an open, a scroll, a tab coming back (`release` forgot the scan) and a rescan of other rows find only what was always there.
        let edited = self.seen.replace(seen).filter(|(rev, _)| *rev != view.key.rev);
        let mut found = scan_window(view.text, view.bytes.clone(), view.key.rows.start, view.lang);
        if let Some((_, rows)) = edited {
            let moved = mark_fresh(&self.items.borrow(), &mut found, &rows);
            self.shifts.borrow_mut().extend(moved);
        }
        self.set(found, cx);
    }

    /// The chips of a hidden tab are not drawn; the first update after the tab shows rescans and sets them again.
    fn release(&mut self) {
        self.seen = None;
        self.shifts.borrow_mut().clear();
        *self.items.borrow_mut() = Rc::default();
    }

    fn held(&self) -> usize {
        self.items.borrow().len()
    }
}

// ---- The chips ----

/// The view over the editor that paints the chips and takes the click on one.
pub struct Chips {
    list: Entity<List>,
    editor: Entity<EditorState>,
    pointer: Rc<RefCell<Pointer>>,
    _watch: [Subscription; 2],
}

/// The chips view of `editor`, or `None` when it has no swatch layer. The pane draws it over the editor.
pub fn chips(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) -> Option<Entity<Chips>> {
    let list = marks::with_layer(editor, cx, |layer: &mut Layer| (layer.list.clone(), false))?;
    Some(window.use_keyed_state(("swatches", editor.entity_id()), cx, |_, cx| {
        let watch = [
            cx.observe(&list, |_, _, cx| cx.notify()),
            // The editor scrolled or changed: the chips move with it, but a file with none draws nothing.
            cx.observe(editor, |this: &mut Chips, _, cx| {
                if !this.list.read(cx).items.borrow().is_empty() {
                    cx.notify();
                }
            }),
        ];
        Chips { list, editor: editor.clone(), pointer: Rc::default(), _watch: watch }
    }))
}

/// A chip with where it sits: its bounds, the token's bytes and its color.
type Placed = (Bounds<Pixels>, Range<usize>, Hsla);

/// The chips of `items` that are on screen and not under the caret's cover, and the text area they are clipped to. `None` before the editor has painted.
fn place(state: &EditorState, items: &[Swatch]) -> Option<(Vec<Placed>, Bounds<Pixels>)> {
    let (rows, mask) = (state.visible_row_range()?, state.text_bounds()?);
    let cover = state.caret_cover();
    let mut placed = Vec::new();
    for swatch in items.iter().filter(|swatch| rows.contains(&swatch.row)) {
        let Some(at) = state.range_to_bounds(&(swatch.range.start..swatch.range.start)) else { continue };
        let line = at.size.height;
        let bounds = if swatch.before {
            let side = (line * 0.38).max(px(6.));
            Bounds::new(point(at.origin.x - side - px(1.), at.origin.y + (line - side) / 2.), size(side, side))
        } else {
            let side = (line * 0.26).max(px(4.));
            Bounds::new(point(at.origin.x, at.origin.y + line - side - px(0.5)), size(side, side))
        };
        if cover.is_some_and(|cover| cover.intersects(&bounds)) {
            continue;
        }
        placed.push((bounds, swatch.range.clone(), swatch.color.hsla()));
    }
    Some((placed, mask))
}

/// How far the fill of a pressed chip draws in from its rim.
const PRESS_INSET: Pixels = px(0.75);

/// What the chips keep between frames: each lit chip's hover fade (by where its token starts), the chip held down, the chip the pointer was last seen over, and the fade-in of each chip an edit just made (by start), armed once per list.
#[derive(Default)]
struct Pointer {
    fades: Vec<(usize, motion::Fade)>,
    arrivals: Vec<(usize, motion::Fade)>,
    armed: Option<Weak<Vec<Swatch>>>,
    down: Option<usize>,
    over: Option<usize>,
}

impl Pointer {
    /// The edits' moves of the chips after them: each key that sat at or past `from` is now `by` further on, so a running fade stays with its chip. Keys are few, and `shifts` is one entry per edit since the last paint.
    fn shift(&mut self, shifts: &[(usize, isize)]) {
        let keys = self.fades.iter_mut().chain(self.arrivals.iter_mut()).map(|(start, _)| start).chain(self.down.iter_mut()).chain(self.over.iter_mut());
        for start in keys {
            for &(from, by) in shifts {
                if *start >= from {
                    *start = start.saturating_add_signed(by);
                }
            }
        }
    }
}

impl Render for Chips {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.list.read(cx).items.borrow().clone();
        if items.is_empty() {
            return div().into_any_element();
        }
        let editor = self.editor.clone();
        let pointer = self.pointer.clone();
        let shifts = self.list.read(cx).shifts.clone();
        let ink = cx.theme().foreground;
        let (prepaint_editor, prepaint_items) = (editor.clone(), items.clone());
        // The editor records where its lines are while it paints, and this view is painted after it, so the chips are placed in paint, from the frame being drawn and not the one before. The hitboxes, which only prepaint can insert, sit where the chips were last frame: a scroll frame's hand lags by one frame.
        canvas(
            move |_, window, cx| {
                let state = prepaint_editor.read(cx);
                if !state.is_editable() {
                    return Vec::new();
                }
                let Some((placed, mask)) = place(state, &prepaint_items) else { return Vec::new() };
                placed
                    .into_iter()
                    .filter_map(|(bounds, range, _)| {
                        let area = bounds.dilate(px(2.)).intersect(&mask);
                        (area.size.width > px(0.) && area.size.height > px(0.)).then(|| (window.insert_hitbox(area, HitboxBehavior::Normal), range.start))
                    })
                    .collect::<Vec<(Hitbox, usize)>>()
            },
            move |_, hitboxes, window, cx| {
                let Some((placed, mask)) = place(editor.read(cx), &items) else { return };
                if placed.is_empty() {
                    return;
                }
                let view = window.current_view();
                let now = motion::frame_now(window.window_handle().window_id());
                let hovered = hitboxes.iter().find(|(hitbox, _)| hitbox.is_hovered(window)).map(|(_, start)| *start);
                for (hitbox, _) in &hitboxes {
                    window.set_cursor_style(CursorStyle::PointingHand, hitbox);
                }
                let (levels, arrivals, pressed, moving) = {
                    let mut pointer = pointer.borrow_mut();
                    pointer.over = hovered;
                    let mut moving = false;
                    // A list the edit's scan just made: its new chips start fading in at this frame, the first that paints them. A chip under the caret's cover fades unseen, so uncovering it later pops nothing.
                    if !pointer.armed.as_ref().is_some_and(|armed| Weak::ptr_eq(armed, &Rc::downgrade(&items))) {
                        pointer.armed = Some(Rc::downgrade(&items));
                        let moved = std::mem::take(&mut *shifts.borrow_mut());
                        pointer.shift(&moved);
                        for swatch in items.iter().filter(|swatch| swatch.fresh) {
                            if !pointer.arrivals.iter().any(|(start, _)| *start == swatch.range.start) {
                                let mut fade = motion::Fade::with(0., FADE_QUICK);
                                fade.aim(1., now);
                                pointer.arrivals.push((swatch.range.start, fade));
                            }
                        }
                    }
                    let mut levels = Vec::with_capacity(placed.len());
                    let mut arrivals = Vec::with_capacity(placed.len());
                    for (_, range, _) in &placed {
                        let at = pointer.fades.iter().position(|(start, _)| *start == range.start).unwrap_or_else(|| {
                            pointer.fades.push((range.start, motion::Fade::at(0.)));
                            pointer.fades.len() - 1
                        });
                        let fade = &mut pointer.fades[at].1;
                        fade.aim(f32::from(hovered == Some(range.start)), now);
                        moving |= fade.is_moving(now);
                        levels.push(fade.level(now));
                        let arrive = pointer.arrivals.iter().find(|(start, _)| *start == range.start).map_or(1., |(_, fade)| {
                            moving |= fade.is_moving(now);
                            fade.level(now)
                        });
                        arrivals.push(arrive);
                    }
                    pointer.fades.retain(|(_, fade)| fade.is_moving(now) || fade.level(now) > 0.);
                    pointer.arrivals.retain(|(_, fade)| fade.is_moving(now));
                    (levels, arrivals, pointer.down.filter(|down| hovered == Some(*down)), moving)
                };
                if moving {
                    window.request_animation_frame();
                }
                window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                    for (((bounds, range, color), level), arrive) in placed.iter().zip(&levels).zip(&arrivals) {
                        let radius = bounds.size.width / 2.;
                        // Under the color, a neutral ground shows a see-through color as such.
                        window.paint_quad(quad(*bounds, radius, ink.opacity(0.2 * arrive), px(0.), ink, BorderStyle::default()));
                        let rim = ink.opacity((0.4 + (ALPHA_PRIMARY - 0.4) * level) * arrive);
                        let color = color.opacity(*arrive);
                        if pressed == Some(range.start) {
                            let fill = bounds.dilate(-PRESS_INSET);
                            window.paint_quad(quad(*bounds, radius, gpui::transparent_black(), px(1.), rim, BorderStyle::default()));
                            window.paint_quad(quad(fill, fill.size.width / 2., color, px(0.), ink, BorderStyle::default()));
                        } else {
                            window.paint_quad(quad(*bounds, radius, color, px(1.), rim, BorderStyle::default()));
                        }
                    }
                });
                let hits: Vec<(Bounds<Pixels>, Range<usize>)> = placed.into_iter().map(|(bounds, range, _)| (bounds.dilate(px(2.)), range)).collect();
                let editor = editor.clone();
                let down = pointer.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !mask.contains(&event.position) {
                        return;
                    }
                    if let Some((_, range)) = hits.iter().find(|(bounds, _)| bounds.contains(&event.position)) {
                        cx.stop_propagation();
                        if editor.read(cx).is_editable() {
                            down.borrow_mut().down = Some(range.start);
                            cx.notify(view);
                        }
                        open(&editor, range.clone(), window, cx);
                    }
                });
                let up = pointer.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && up.borrow_mut().down.take().is_some() {
                        cx.notify(view);
                    }
                });
                // The pointer moving on or off a chip asks for the frame its fade starts in.
                let moved = pointer.clone();
                window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let now_over = hitboxes.iter().find(|(hitbox, _)| hitbox.is_hovered(window)).map(|(_, start)| *start);
                    if std::mem::replace(&mut moved.borrow_mut().over, now_over) != now_over {
                        cx.notify(view);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}

// ---- The pick ----

/// The pick in progress: the token it rewrites and where the history stood before it began.
struct Session {
    editor: WeakEntity<EditorState>,
    window: AnyWindowHandle,
    /// The token's bytes when the session began; every pick rewrites these.
    range: Range<usize>,
    format: Format,
    /// The caret when the session began.
    caret: usize,
    /// History steps applied before the first pick, whether a pick has happened, and the text revision
    /// the session expects: at open, then after each pick. Any other revision ends the session.
    steps: usize,
    picked: bool,
    rev: u64,
    /// The color the text shows now.
    shown: Color,
    app: AsyncApp,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    /// The panel's target, made on the first click and kept for the life of the app.
    static TARGET: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "FinchColorSwatchTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(colorChanged:))]
        fn color_changed(&self, panel: &AnyObject) {
            let Some(color) = panel_color(panel) else { return };
            let Some(app) = SESSION.with_borrow(|session| session.as_ref().map(|session| session.app.clone())) else { return };
            // AppKit can send this while the app is borrowed (`setColor:` does, inside a click), so the pick waits for a turn of its own.
            app.spawn(async move |cx| cx.update(|cx| pick(color, cx))).detach();
        }
    }
);

/// The panel's color in sRGB.
fn panel_color(panel: &AnyObject) -> Option<Color> {
    // SAFETY: plain AppKit calls on the main thread, where AppKit runs the panel's action.
    unsafe {
        let color: Retained<AnyObject> = msg_send![panel, color];
        let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let srgb: Option<Retained<AnyObject>> = msg_send![&*color, colorUsingColorSpace: space];
        let srgb = srgb?;
        let (mut r, mut g, mut b, mut a) = (0f64, 0f64, 0f64, 0f64);
        let _: () = msg_send![&*srgb, getRed: &mut r, green: &mut g, blue: &mut b, alpha: &mut a];
        let byte = |v: f64| (v.clamp(0., 1.) * 255.).round() as u8;
        Some(Color { r: byte(r), g: byte(g), b: byte(b), a: a.clamp(0., 1.) as f32 })
    }
}

/// A chip was clicked: opens the color panel on the token's color.
fn open(editor: &Entity<EditorState>, range: Range<usize>, window: &Window, cx: &mut App) {
    let (token, editable, caret) = {
        let state = editor.read(cx);
        (state.text().slice(range.clone()).to_string(), state.is_editable(), state.cursor())
    };
    let Some((end, color, format)) = color_at(&token, 0, "css") else { return };
    if !editable || end != token.len() {
        return;
    }
    let has_alpha = format.has_alpha();
    let app = cx.to_async();
    let rev = editor.read(cx).text_revision();
    SESSION.set(Some(Session { editor: editor.downgrade(), window: window.window_handle(), range, format, caret, steps: 0, picked: false, rev, shown: color, app }));
    // SAFETY: plain AppKit calls on the main thread; the target is kept alive by `TARGET`.
    unsafe {
        let target = TARGET.with_borrow_mut(|target| target.get_or_insert_with(|| msg_send![Target::class(), new]).clone());
        let panel: *mut AnyObject = msg_send![class!(NSColorPanel), sharedColorPanel];
        let ns: Retained<AnyObject> = msg_send![class!(NSColor), colorWithSRGBRed: color.r as f64 / 255., green: color.g as f64 / 255., blue: color.b as f64 / 255., alpha: color.a as f64];
        // `setColor:` runs the panel's action at once, so it goes before the target is set.
        let _: () = msg_send![panel, setColor: &*ns];
        let _: () = msg_send![panel, setShowsAlpha: Bool::new(has_alpha)];
        let _: () = msg_send![panel, setTarget: &*target];
        let _: () = msg_send![panel, setAction: sel!(colorChanged:)];
        let _: () = msg_send![panel, setContinuous: Bool::YES];
        let _: () = msg_send![panel, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
    }
}

/// The panel picked `color`: the token becomes it, in the format it had, as the one step of the session.
fn pick(color: Color, cx: &mut App) {
    let Some(mut session) = SESSION.take() else { return };
    if session.shown.same(color) {
        SESSION.set(Some(session));
        return;
    }
    let Some(editor) = session.editor.upgrade() else { return };
    let kept = cx
        .update_window(session.window, |_, window, cx| editor.update(cx, |state, cx| rewrite(&mut session, color, state, window, cx)))
        .unwrap_or(false);
    if kept {
        SESSION.set(Some(session));
    }
}

/// Puts `color` into the token. False when the session is over: the text was edited since the last pick.
fn rewrite(session: &mut Session, color: Color, state: &mut EditorState, window: &mut Window, cx: &mut Context<EditorState>) -> bool {
    if !state.is_editable() || state.text_revision() != session.rev {
        return false;
    }
    if session.picked {
        // Back to the text and the caret as the session found them: this pick replaces the last one.
        state.history_jump(session.steps, window, cx);
    } else {
        session.steps = state.history_counts().0;
        session.caret = state.cursor();
    }
    let text = session.format.write(color);
    let range = session.range.clone();
    let caret = if session.caret <= range.start {
        session.caret
    } else if session.caret >= range.end {
        session.caret + text.len() - range.len()
    } else {
        range.start + text.len()
    };
    state.replace_range(range, &text, caret, window, cx);
    session.picked = true;
    session.rev = state.text_revision();
    session.shown = color;
    true
}

#[cfg(test)]
mod tests {
    // Named, not `super::*`: the glob would bring gpui's `test` attribute over the standard one.
    use super::{Color, FADE_QUICK, Format, Pointer, color_at, mark_fresh, motion, scan, scan_window};
    use gpui_kit::component::input::Rope;
    use std::time::Instant;

    /// A window with no color is looked at in place: the scan of a 200-row Rust window with no colors
    /// allocates nothing (before: a copy of the whole window per revision), and a window with colors,
    /// however the rope cut its chunks around a token, finds the same chips as scanning its text.
    #[test]
    fn a_window_without_colors_is_not_copied_and_one_with_colors_finds_the_same_chips() {
        let line = "    let rate = compute(rows, 0.5) + other_value.rem_euclid(3); // hash and rest of nothing\n";
        let rope = Rope::from(line.repeat(200).as_str());
        let (found, allocated) = crate::test_alloc::allocations(|| scan_window(&rope, 0..rope.len(), 0, "rust"));
        assert!(found.is_empty());
        // The chunk iterator keeps one small stack of its own; what matters is that it does not grow with the window.
        let big = Rope::from(line.repeat(2000).as_str());
        let (_, allocated_big) = crate::test_alloc::allocations(|| scan_window(&big, 0..big.len(), 0, "rust"));
        assert!(allocated <= 1 && allocated_big == allocated, "no copy of the window ({allocated} allocations, {allocated_big} for ten times the text)");
        // A `#` that is a comment or an attribute is no start: no copy for 200 rows of them either.
        for comments in ["# comment about the next step\n", "#[derive(Debug)]\n", "#!/bin/sh\n"] {
            let rope = Rope::from(comments.repeat(200).as_str());
            let (found, allocated) = crate::test_alloc::allocations(|| scan_window(&rope, 0..rope.len(), 0, "rust"));
            assert!(found.is_empty() && allocated <= 1, "{comments:?}: {allocated} allocations");
        }

        let filler = "let word = rows.rem(1);\n".repeat(300);
        for token in ["#ff9f0a", "0x423E39", "rgb(1, 2, 3)", "RGBA(1 2 3 / 50%)", "hsl(120, 50%, 50%)"] {
            for at in (0..filler.len()).step_by(24) {
                for pad in 0..24 {
                    let text = format!("{}{}{token} {}", &filler[..at], " ".repeat(pad), &filler[at..]);
                    let rope = Rope::from(text.as_str());
                    let expect = scan(&text, 0, 0, "rust");
                    assert_eq!(expect.len(), 1, "{token} at {at}+{pad}");
                    assert_eq!(scan_window(&rope, 0..rope.len(), 0, "rust"), expect, "{token} at {at}+{pad}");
                }
            }
        }
    }

    fn parse(token: &str) -> Option<(Color, Format)> {
        color_at(token, 0, "css").filter(|(end, ..)| *end == token.len()).map(|(_, color, format)| (color, format))
    }

    fn round(token: &str) -> String {
        let (color, format) = parse(token).unwrap_or_else(|| panic!("{token} parses"));
        format.write(color)
    }

    #[test]
    fn hex_forms_parse_and_write_back_the_same() {
        for token in ["#FF9F0A", "#ff9f0a", "#fff", "#FFF", "#f80c", "#FF9F0A80", "#ff9f0a80", "0x423E39", "0X423e39"] {
            assert_eq!(round(token), token);
        }
        let (color, _) = parse("#FF9F0A").unwrap();
        assert_eq!((color.r, color.g, color.b, color.a), (255, 159, 10, 1.));
        let (color, _) = parse("#fff8").unwrap();
        assert_eq!(color.alpha_byte(), 0x88);
    }

    #[test]
    fn functions_parse_and_write_back_the_same() {
        for token in ["rgb(255, 159, 10)", "rgb(255,159,10)", "rgb(255 159 10)", "rgba(255, 159, 10, 0.5)", "rgb(255 159 10 / 0.5)", "rgb(255 159 10 / 50%)", "rgb(100%, 50%, 0%)", "hsl(210, 50%, 40%)", "hsla(210deg 50% 40% / 0.25)"] {
            assert_eq!(round(token), token);
        }
    }

    #[test]
    fn a_pick_keeps_the_format_and_widens_it_only_when_it_must() {
        let pick = Color { r: 0x12, g: 0x34, b: 0x56, a: 1. };
        assert_eq!(parse("#fff").unwrap().1.write(pick), "#123456");
        assert_eq!(parse("#FFF").unwrap().1.write(Color { r: 0x11, g: 0x22, b: 0x33, a: 1. }), "#123");
        assert_eq!(parse("0xFFFFFF").unwrap().1.write(Color { a: 0.5, ..pick }), "0x123456");
        assert_eq!(parse("#ffffff").unwrap().1.write(Color { a: 0.5, ..pick }), "#12345680");
        assert_eq!(parse("rgb(1, 2, 3)").unwrap().1.write(Color { a: 0.5, ..pick }), "rgb(18, 52, 86, 0.5)");
        assert_eq!(parse("hsl(0, 0%, 0%)").unwrap().1.write(Color { r: 255, g: 0, b: 0, a: 1. }), "hsl(0, 100%, 50%)");
    }

    #[test]
    fn scan_finds_colors_and_leaves_names_and_numbers_alone() {
        let css = "a { color: #FF9F0A; b: rgba(0,0,0,.5); }\n#header { c: 0x423E39 }\n/* issue #1234 and #abcd-ef */\nfade#fff x#123456 &#123; to-rgb(1,2,3) rgb(var(--x))\n.d { e: #000; f: hsl(10 20% 30%); }";
        let found = scan(css, 0, 0, "css");
        let tokens: Vec<&str> = found.iter().map(|swatch| &css[swatch.range.clone()]).collect();
        assert_eq!(tokens, ["#FF9F0A", "rgba(0,0,0,.5)", "0x423E39", "#1234", "#000", "hsl(10 20% 30%)"]);
        assert_eq!(found.iter().map(|swatch| swatch.row).collect::<Vec<_>>(), [0, 0, 1, 2, 4, 4]);
        assert!(found[0].before);
        // Not CSS: `#1234` is an issue number, `#000` a repeated digit that stays a color.
        let rust = "// see #1234 and #000 and #abc";
        let tokens: Vec<&str> = scan(rust, 0, 0, "rust").iter().map(|swatch| &rust[swatch.range.clone()]).collect();
        assert_eq!(tokens, ["#000", "#abc"]);
    }

    #[test]
    fn scan_offsets_follow_the_window() {
        let found = scan("x\n  #fff", 100, 7, "css");
        assert_eq!((found[0].range.clone(), found[0].row, found[0].before), (104..108, 8, true));
    }

    #[test]
    fn an_edit_fades_in_only_the_chips_it_made() {
        let fresh = |old: &str, new: &str| {
            let before = scan(old, 0, 0, "css");
            let mut after = scan(new, 0, 0, "css");
            mark_fresh(&before, &mut after, &(0..100));
            after.iter().map(|swatch| swatch.fresh).collect::<Vec<_>>()
        };
        // Typed: the first chip, then it grows and recolors in place.
        assert_eq!(fresh("a { b: }", "a { b: #ff9 }"), [true]);
        assert_eq!(fresh("a { b: #ff9 }", "a { b: #ff9f }"), [false]);
        assert_eq!(fresh("a { b: #ff9f }", "a { b: #ff9f0a }"), [false]);
        // Text typed before the chips only moves them.
        assert_eq!(fresh("a #fff b #000", "aa #fff b #000"), [false, false]);
        // Pasted between two: only the pasted one is new.
        assert_eq!(fresh("#fff #000", "#fff #123456 #000"), [false, true, false]);
        // Rows the old list never covered pop.
        let mut after = scan("#fff", 0, 200, "css");
        mark_fresh(&[], &mut after, &(0..100));
        assert!(!after[0].fresh);
    }

    #[test]
    fn a_running_fade_follows_its_chip_when_an_edit_before_it_shifts_it() {
        let now = Instant::now();
        let mut arrival = motion::Fade::with(0., FADE_QUICK);
        arrival.aim(1., now);
        let mut hover = motion::Fade::with(0., FADE_QUICK);
        hover.aim(1., now);
        let mut pointer = Pointer::default();
        pointer.arrivals.push((10, arrival));
        pointer.fades.push((10, hover));
        // A chip at 10 with a fade running; three bytes typed at 0.
        let old = scan("123456789 #fff", 0, 0, "css");
        let mut new = scan("abc123456789 #fff", 0, 0, "css");
        assert_eq!((old[0].range.start, new[0].range.start), (10, 13));
        let shift = mark_fresh(&old, &mut new, &(0..100));
        assert!(!new[0].fresh);
        pointer.shift(&shift.into_iter().collect::<Vec<_>>());
        assert_eq!((pointer.arrivals[0].0, pointer.fades[0].0), (13, 13));
        assert!(pointer.arrivals[0].1.is_moving(now));
        assert!(pointer.arrivals[0].1.level(now) < 1.);
    }
}
