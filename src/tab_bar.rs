//! The tab bar: one row of tabs in the window's top band, right of the explorer column, drawn
//! for a `Pane`.
//!
//! A tab is as wide as what it shows (finch#x7jk): a slot at the left with the file's symbol (the close button under the pointer), the
//! title, and while it has one a mark after it (the unsaved dot, the lock, a warning or the pin), up to `TAB_MAX_WIDTH`; the bar scrolls past its room.
//! The active tab stands on a small plate with a lit top and a contact shadow; the rest are plain until the pointer lights them.
//! A press selects, a middle click closes, and dragging lifts the tab as a ghost that sits
//! under the pointer exactly where it was grabbed, its shadow easing up from flat; the tab's slot
//! follows the pointer through the bar. Let go in the bar, a copy of the ghost springs home into
//! the slot and crossfades with it. Every change to the pane goes through `Pane::later`, since a
//! click must not update the pane while it is drawing.
//!
//! The bar's motion lives in `Motion`, which the pane steps once per frame while anything moves
//! and not at all at rest. The tabs are laid out by it, not by flex, so a tab arriving or folding
//! away opens or closes its room continuously, a tab whose name or mark changes eases to its new width, a reorder glides every other tab to its new
//! place, and the active tab's mark is one shape that springs from tab to tab (the explorer
//! selection's spring). The dragged tab's slot never eases. A drag from elsewhere (another pane's
//! tab, or files) over the strip opens an empty gap where it would land, which eases open and
//! glides between tabs with the pointer; the tab dropped there takes the gap's place and width
//! instead of arriving again, and the gap folds if the drag leaves.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    f32::consts::TAU,
    path::Path,
    rc::Rc,
    time::Instant,
};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    drag::{DraggedFiles, Dropped},
    explorer::{icons, row::name_label},
    hover::{by_hover, icon_button, icon_button_weighted, FadeHover as _, Ink},
    motion::{ease_out, overshoot, Fade},
    pane::Pane,
    symbols,
    theme, tooltip,
    tokens::{
        ALPHA_HAIRLINE, ALPHA_HOVER, ALPHA_HOVER_LIGHT, ALPHA_PRIMARY, ALPHA_SECONDARY, ALPHA_SELECTED, ALPHA_SELECTED_LIGHT, ALPHA_SELECTED_UNFOCUSED, ALPHA_STATUS_DIM, ALPHA_TERTIARY,
        ALPHA_SELECTED_UNFOCUSED_LIGHT, ALPHA_SNIPPET_CARET, ALPHA_TAB_DRAGGED, ARRIVE, DOT, FADE_QUICK, LAYER_TOOLTIP, PEEK_EDGE, PEEK_GAP, PEEK_TEXT_MAX, PEEK_WIDTH, RADIUS_TOOLTIP, SAVE_BREATH_LOW, SAVE_BREATH_PERIOD, SAVE_BRIGHTEN, SAVE_SETTLE, SAVE_SLOW, SAVE_SWELL, TEXT_SUBHEADLINE, TEXT_SUBHEADLINE_LINE, TOOLTIP_PAD_X, TOOLTIP_PAD_Y, TOOLTIP_RISE,
        ICON_BUTTON, ICON_BUTTON_INSET, LAYER_TAB_LANDING, LIST_EDGE_FADE, MAX_STEP, RADIUS_TAB, REVEAL_INSET, SELECT_RESPONSE, SETTLE_RATE,
        TAB_ARRIVE_FADE_FROM, TAB_ICON, TAB_LINE_TITLE_FADE,
        TAB_CLOSE_HIT, TAB_CLOSE_SLOT, TAB_CLOSE_SYMBOL, TAB_DROP_ROOM, TAB_FLOOR_WIDTH, TAB_FOLDER_FADE, TAB_FOLDER_GAP, TAB_LIST_BADGE_INSET, TAB_LIST_GAP, TAB_LIST_SYMBOL, TAB_LOCK_DOT, TAB_LOCK_DOT_INSET, TAB_BAR_INSET, TAB_GAP, TAB_HEIGHT, TAB_MAX_WIDTH, TAB_PAD_LEAD, TAB_PAD_TRAIL,
        TAB_PLATE_RIM, TAB_PLATE_RIM_LIGHT, TAB_PLATE_ROOM, TAB_PLATE_SHADOW, TAB_PLATE_SHADOW_LIGHT, TAB_PLATE_UNFOCUSED, TAB_TITLE_DRIFT, TAB_TITLE_PAD, TAB_TRAIL_SLOT, TAB_WARNING_SYMBOL, TEXT_BODY,
    },
};

/// What the bar shows of one tab.
#[derive(Clone, PartialEq)]
pub struct TabLabel {
    /// The tab's document.
    pub id: EntityId,
    /// The file's name, or Untitled with the page's number.
    pub title: SharedString,
    /// The title is an Untitled page's first line (drawn quieter), not a file's name or "Untitled n".
    pub from_line: bool,
    /// The folder the file is in, drawn after the name a step quieter, when another tab in the pane has the same name.
    pub folder: Option<SharedString>,
    /// The file's full path with `~` for the home folder, for the tab's tip; none for an Untitled page.
    pub path: Option<SharedString>,
    pub dirty: bool,
    /// The file changed on disk under this tab's unsaved edits: the tab shows a warning sign in
    /// the dot's place.
    pub conflict: bool,
    /// The file is gone from disk under this tab's unsaved edits: shown the same, and the title
    /// is struck through.
    pub gone: bool,
    /// The file can't be written: the tab shows a lock in the dot's place.
    pub locked: bool,
    /// The pane's preview tab: its title is drawn in italics.
    pub preview: bool,
    /// The tab is pinned: a pin stands where the close button would.
    pub pinned: bool,
    /// When the save writing this tab's file began, once it has run longer than `SAVE_SLOW`: the dot breathes from there until it lands.
    pub saving: Option<Instant>,
}

impl TabLabel {
    /// The tab has a mark: unsaved edits, a locked file, or a warning.
    fn marked(&self) -> bool {
        self.dirty || self.locked || self.conflict || self.gone
    }

    /// The sign the mark shows: the warning, else the lock, else the dot.
    fn sign(&self) -> Sign {
        if self.conflict || self.gone {
            Sign::Warning
        } else if self.locked {
            Sign::Lock
        } else {
            Sign::Dot
        }
    }

    /// A locked file with unsaved edits shows a dot at its lock's corner too: the lock says the file can't be written, the dot says it has changes.
    fn lock_dot(&self) -> bool {
        self.locked && self.dirty && !(self.conflict || self.gone)
    }
}

/// Keeps `held`, each open Untitled page with its number, in line with `pages`, the Untitled pages open now in tab order: a closed page lets its number go, and a page not seen before takes the lowest number no open page holds. The first is 1, shown as plain "Untitled"; a number stays with its page for as long as it is open, as TextEdit's do. A frame where no page came or went looks each page up once and builds nothing.
pub fn number_untitled(held: &mut HashMap<EntityId, u32>, pages: impl Iterator<Item = EntityId> + Clone) {
    let (mut total, mut known) = (0, 0);
    for page in pages.clone() {
        total += 1;
        known += usize::from(held.contains_key(&page));
    }
    if known < held.len() {
        let open: HashSet<EntityId> = pages.clone().collect();
        held.retain(|id, _| open.contains(id));
    }
    if known < total {
        // The lowest number no page holds is at most one past the pages there will be, and only grows as numbers are taken.
        let mut taken = vec![false; held.len() + total - known + 2];
        for &number in held.values() {
            if let Some(slot) = taken.get_mut(number as usize) {
                *slot = true;
            }
        }
        let mut lowest = 1;
        for page in pages {
            if held.contains_key(&page) {
                continue;
            }
            while taken[lowest] {
                lowest += 1;
            }
            taken[lowest] = true;
            held.insert(page, lowest as u32);
        }
    }
}

/// The title of Untitled page number `number`: "Untitled", then "Untitled 2" and on.
pub fn untitled_title(number: u32) -> String {
    if number <= 1 { "Untitled".into() } else { format!("Untitled {number}") }
}

/// The ink of a title: a name taken from an Untitled page's first line is a step quieter than a file's.
fn line_ink(from_line: bool, text: Hsla) -> Hsla {
    if from_line { Hsla { a: text.a * TAB_LINE_TITLE_FADE, ..text } } else { text }
}

/// Whether `title` is already `untitled_title(number)`, so a frame that changed nothing builds no new string.
pub fn is_untitled_title(title: &str, number: u32) -> bool {
    match title.strip_prefix("Untitled") {
        Some("") => number <= 1,
        Some(rest) => rest.strip_prefix(' ').is_some_and(|digits| digits.parse::<u32>() == Ok(number) && number > 1),
        None => false,
    }
}

/// The words a tab's tip says, a line each: the file's full path (an Untitled page, its title), "Pinned" for a pinned tab, "Unsaved changes" for one with a dot, "Read-only" for a file that can't be written, and what happened to it on disk.
pub fn tab_tip(title: &str, path: Option<&str>, pinned: bool, dirty: bool, locked: bool, conflict: bool, gone: bool) -> SharedString {
    let mut lines = vec![path.unwrap_or(title)];
    if pinned {
        lines.push("Pinned");
    }
    if dirty {
        lines.push("Unsaved changes");
    }
    if locked {
        lines.push("Read-only");
    }
    if gone {
        lines.push("Deleted from disk");
    } else if conflict {
        lines.push("Changed on disk");
    }
    lines.join("\n").into()
}

/// `path` as the tab's tip shows it: with `~` for the home folder.
pub fn tip_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()).filter(|rest| !rest.as_os_str().is_empty()) {
        Some(rest) => format!("~/{}", rest.to_string_lossy()),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Whether `shown` is already `tip_path(path, home)`, so a frame that changed nothing builds no new string.
pub fn is_tip_path(shown: &str, path: &Path, home: Option<&Path>) -> bool {
    match home.and_then(|home| path.strip_prefix(home).ok()).filter(|rest| !rest.as_os_str().is_empty()) {
        Some(rest) => shown.strip_prefix("~/").is_some_and(|tail| *tail == *rest.to_string_lossy()),
        None => *shown == *path.to_string_lossy(),
    }
}

/// A tab being dragged: the payload GPUI carries, found again by the bar's drag-move. `pinned`
/// says where it lands in another pane: at the end of the pins. The ghost's constructor notes
/// where the pointer holds it, how wide it is and when it began to lift, so a drop in the bar can
/// glide a copy home from where the ghost was drawn.
pub struct TabDrag {
    pub id: EntityId,
    pub pinned: bool,
    grab: Cell<Point<Pixels>>,
    width: Cell<Pixels>,
    lifted_at: Cell<Option<Instant>>,
}

impl TabDrag {
    fn new(id: EntityId, pinned: bool) -> Self {
        Self { id, pinned, grab: Cell::new(Point::default()), width: Cell::new(TAB_DROP_ROOM), lifted_at: Cell::new(None) }
    }

    /// Where the ghost is drawn with the pointer at `pointer`, and how far it has lifted at `now`.
    pub fn ghost(&self, pointer: Point<Pixels>, now: Instant) -> (Bounds<Pixels>, f32) {
        let lift = self.lifted_at.get().map_or(0., |from| ease_out(now.saturating_duration_since(from).as_secs_f32() / ARRIVE));
        (Bounds::new(pointer - self.grab.get(), size(self.width.get(), TAB_HEIGHT)), lift)
    }
}

/// The lifted tab: it draws under the pointer while the drag lasts, its shadow easing up from flat
/// over `ARRIVE` from the moment it was picked up (`lifted_at`; `None` draws it already lifted).
struct Ghost {
    label: TabLabel,
    width: Pixels,
    lifted_at: Option<Instant>,
}

impl Render for Ghost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let t = self.lifted_at.map_or(1., |from| now.saturating_duration_since(from).as_secs_f32() / ARRIVE);
        if t < 1. {
            window.request_animation_frame();
        }
        card(&self.label, self.width, ease_out(t), cx)
    }
}

/// The lifted tab a held drag shows (the control socket's `drag-hold`), as a real drag would once
/// it has lifted.
pub fn ghost(label: TabLabel, width: Pixels, cx: &mut App) -> AnyView {
    cx.new(|_| Ghost { label, width, lifted_at: None }).into()
}

/// The lifted tab's card on the float plane (the tooltip's fill, rim, lit top and shadows, the held
/// pair), `lift` of the way up off the bar: its shadows ease from flat to the pair. It sets its own
/// font, since the ghost is drawn on a root of its own and the copy gliding home must match it.
/// It carries the tab's own sign, pin, preview italic and twin folder, drawn as the tab draws them, so nothing pops back on landing.
fn card(label: &TabLabel, width: Pixels, lift: f32, cx: &App) -> Div {
    let light = theme::is_light(cx);
    let theme = cx.theme();
    let (fill, border) = tooltip::float_fill(theme.background, theme.sidebar_foreground, light);
    let shadows = tooltip::shadows(light, true)
        .into_iter()
        .map(|shadow| BoxShadow { color: shadow.color.opacity(lift), offset: point(px(0.), shadow.offset.y * lift), blur_radius: shadow.blur_radius * lift, ..shadow })
        .collect();
    let text = theme::ink(ALPHA_PRIMARY, cx);
    // The tab as it is at rest: its file's symbol at the left, and after the title its sign, else its pin.
    let icon_ink = Hsla { a: text.a, ..theme::ink(1., cx) };
    let trail = if label.marked() {
        Some(sign_mark(label.sign(), theme.warning, text, f32::from(label.lock_dot())))
    } else if label.pinned {
        Some(div().text_color(theme::ink(ALPHA_TERTIARY, cx)).child(symbols::icon("pin.fill", TAB_CLOSE_SYMBOL)).into_any_element())
    } else {
        None
    };
    tooltip::float(f32::from(RADIUS_TAB), fill, border, light, true)
        .shadow(shadows)
        .w(width)
        .h(TAB_HEIGHT)
        .pl(TAB_PAD_LEAD)
        .pr(TAB_PAD_TRAIL)
        .flex()
        .items_center()
        .font_family(theme.font_family.clone())
        .font_weight(FontWeight(crate::settings::get(cx).text.weight))
        .text_size(TEXT_BODY)
        .text_color(text)
        .child(div().size(TAB_CLOSE_SLOT).flex_none().flex().items_center().justify_center().child(icons::file(if label.path.is_some() { &*label.title } else { "" }, TAB_ICON, icon_ink)))
        .child(title_text(&label.title, label.from_line, label.folder.as_ref(), label.preview, label.gone, line_ink(label.from_line, text)))
        .children(trail.map(|mark| div().w(TAB_TRAIL_SLOT).h(TAB_CLOSE_SLOT).flex_none().flex().items_center().justify_center().child(mark)))
}

/// A tab's title as the strip draws it: the name alone is one run; with a folder, the name keeps its width and the folder,
/// quieter, takes what is left and is cut first. A preview tab's is italic and a deleted file's is struck through.
/// A title taken from an Untitled page's first line (`from_line`) is words, not a file name: it has no extension to keep, so it is cut once at its end.
fn title_text(title: &SharedString, from_line: bool, folder: Option<&SharedString>, preview: bool, gone: bool, text: Hsla) -> Div {
    let styled = |label: Div| label.px(TAB_TITLE_PAD).when(preview, |label| label.italic()).when(gone, |label| label.line_through());
    match folder {
        None => styled(name_label(title, from_line, text, None)),
        Some(folder) => styled(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .overflow_hidden()
                .child(name_label(title, from_line, text, None).flex_none().max_w_full().pl(px(0.)))
                .child(name_label(folder, true, Hsla { a: text.a * TAB_FOLDER_FADE, ..text }, None).flex_initial().pl(TAB_FOLDER_GAP)),
        ),
    }
}

/// How wide tab `label` is with no mark after its title: the padding, the icon slot, the title (and its twin folder) as
/// `window` shapes it in `family` at `weight`, rounded up, and the title's pad each side.
fn natural_width(label: &TabLabel, family: &SharedString, weight: f32, window: &Window) -> f32 {
    let style = if label.preview { FontStyle::Italic } else { FontStyle::Normal };
    let measure = |text: &SharedString| {
        let font = Font { family: family.clone(), features: FontFeatures::default(), fallbacks: None, weight: FontWeight(weight), style };
        let run = TextRun { len: text.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
        f32::from(window.text_system().shape_line(text.clone(), TEXT_BODY, &[run], None).width)
    };
    let folder = label.folder.as_ref().map_or(0., |folder| f32::from(TAB_FOLDER_GAP) + measure(folder));
    let title = (measure(&label.title) + folder).ceil();
    f32::from(TAB_PAD_LEAD + TAB_CLOSE_SLOT + TAB_TITLE_PAD * 2. + TAB_PAD_TRAIL) + title
}

/// The width tab content wants: `natural` plus `trail` of the mark's slot, never past `TAB_MAX_WIDTH`.
fn wanted_width(natural: f32, trail: f32) -> f32 {
    (natural + trail * f32::from(TAB_TRAIL_SLOT)).min(f32::from(TAB_MAX_WIDTH))
}

/// A tab `wide` points wide as it draws in a strip with `room` for one tab: whole when it fits, else the room, but never
/// under `TAB_FLOOR_WIDTH` (a narrower tab keeps its own width), so one tab always shows whole instead of a slice of it.
fn fitted_width(wide: f32, room: f32) -> f32 {
    wide.min(room.max(f32::from(TAB_FLOOR_WIDTH)))
}

/// Records `place`, where a tab drawn as room alone sits, under `id` when rects are on.
fn rect_canvas(id: String, place: Bounds<Pixels>) -> Option<impl IntoElement> {
    crate::control::rects_enabled().then(|| canvas(move |_, window, _| crate::control::record(window, id.clone(), place), |_, (), _, _| {}).absolute().size_0())
}

/// Records the place of the element it sits in under `id` when rects are on.
fn rect_here(id: String) -> Option<impl IntoElement> {
    crate::control::rects_enabled().then(|| canvas(move |bounds, window, _| crate::control::record(window, id.clone(), bounds), |_, (), _, _| {}).absolute().top_0().left_0().size_full())
}

/// Steps one value `dt` seconds toward `to` on a critically damped spring, keeping its speed
/// when `to` moves; true while it still moves (inside `eps` and `eps_v` it lands on `to`).
fn follow(at: &mut f32, v: &mut f32, to: f32, omega: f32, dt: f32, eps: f32, eps_v: f32) -> bool {
    let x = *at - to;
    let c = *v + omega * x;
    let decay = (-omega * dt).exp();
    let (mut x2, mut v2) = ((x + c * dt) * decay, (*v - omega * c * dt) * decay);
    if x2 * x < 0. {
        (x2, v2) = (0., 0.);
    }
    let moving = x2.abs() >= eps || v2.abs() >= eps_v;
    if !moving {
        (x2, v2) = (0., 0.);
    }
    *at = to + x2;
    *v = v2;
    moving
}

/// How far a critically damped spring that settles in `length` seconds has come `t` seconds after
/// it began: 1 from `length` on, where it is within a thousandth and lands.
fn settle(t: f32, length: f32) -> f32 {
    if t >= length {
        return 1.;
    }
    let u = SETTLE_RATE / length * t.max(0.);
    1. - (1. + u) * (-u).exp()
}

/// One axis of a landing copy's place and speed: the critically damped spring that settles over
/// `ARRIVE`, from `from` with the speed the copy was let go at, toward `to`. With no speed it is
/// `from` plus `home` of the way, as it always was.
fn chip_axis(landing: &Landing, to: Pixels, from: Pixels, axis: usize) -> (f32, f32) {
    let (to, from) = (f32::from(to), f32::from(from));
    if landing.home >= 1. {
        return (to, 0.);
    }
    let w = SETTLE_RATE / ARRIVE;
    let (d0, t) = (from - to, landing.t);
    let c = landing.speed[axis] + w * d0;
    let decay = (-w * t).exp();
    (to + (d0 + c * t) * decay, (c - w * d0 - w * c * t) * decay)
}

pub(crate) fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (Rgba::from(a), Rgba::from(b));
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }.into()
}

/// One tab's motion. A tab that closed stays as a `leaving` slot until it has folded away.
struct Slot {
    label: TabLabel,
    leaving: bool,
    /// How much of its width the tab has: 0 gone, 1 whole.
    level: f32,
    level_v: f32,
    /// How much the tab shows as the active one, and where it is headed.
    lit: f32,
    lit_fade: Fade,
    lit_to: f32,
    /// How much the tab shows as the one being dragged (0 at rest in the bar, 1 lifted out).
    lifted: f32,
    /// The side the title drifts in from: 1 from the right, -1 from the left.
    dir: f32,
    /// Where the tab is drawn, from where layout puts it, and its speed.
    offset: f32,
    offset_v: f32,
    /// Where layout put it, in the strip's content.
    x: f32,
    placed: bool,
    /// The tab's width with no mark (`natural_width`) and whether it was measured for the label it shows: a new or retitled tab is measured
    /// before it is laid out (`Motion::measure`), and until then counts as `TAB_DROP_ROOM`.
    natural: f32,
    measured: bool,
    /// The width of the tab's content without its mark, easing to `natural` when its title changes, and its speed; `sized` once it has one.
    wide: f32,
    wide_v: f32,
    sized: bool,
    /// How much of the mark's slot after the title is open (0 shut, 1 open), fading when the tab gains or loses a mark or pin. It
    /// adds to `wide`, so the title keeps its room while it opens.
    trail_fade: Fade,
    trail: f32,
    /// The width the tab is drawn at this frame: `wide` and its open mark slot, fitted to the strip's room.
    w: f32,
    /// How much of the tab's mark (dot, lock or warning sign) shows, and which sign it last was, drawn while it fades away.
    mark: Fade,
    mark_level: f32,
    /// How much of the dot at a lock's corner shows.
    corner: Fade,
    corner_level: f32,
    /// How much the pin shows in the close button's place (0 the close button, 1 the pin).
    pin: Fade,
    pin_level: f32,
    sign: Sign,
    /// How much of each sign shows (by `Sign::ix`): the tab's sign fades to 1 and the others to 0, so signs it changed through fade
    /// out from where they are drawn.
    shares: [Fade; 3],
    share_levels: [f32; 3],
    /// The title a rename replaced, while it fades out, and how much of the new title shows.
    retitle: Option<Retitle>,
    title_level: f32,
    /// A save that landed or failed on this tab, settling its dot.
    settle: Option<Settle>,
    /// How much of its ink the dot shows while a slow save makes it breathe (1 at rest); held while the dot goes out, so the landing's fade starts from the level drawn.
    breath: f32,
}

/// Which sign a tab's mark slot shows.
#[derive(Clone, Copy, PartialEq)]
enum Sign {
    Warning,
    Lock,
    Dot,
}

impl Sign {
    /// Every sign, in the order they stack in the slot and index `Slot::shares`.
    const ALL: [Sign; 3] = [Sign::Warning, Sign::Lock, Sign::Dot];

    fn ix(self) -> usize {
        self as usize
    }
}

/// One sign drawn in the mark slot; the lock carries the dot at its corner, fading with `corner_level`.
fn sign_mark(sign: Sign, warning: Hsla, text: Hsla, corner_level: f32) -> AnyElement {
    match sign {
        Sign::Warning => div().text_color(warning).child(symbols::icon("exclamationmark.circle", TAB_WARNING_SYMBOL)).into_any_element(),
        Sign::Lock => div()
            .relative()
            .size_full()
            .child(div().size_full().flex().items_center().justify_center().child(symbols::icon("lock", TAB_CLOSE_SYMBOL)))
            // The dot at the lock's corner, fading with the edits it stands for.
            .child(div().absolute().top(TAB_LOCK_DOT_INSET).right(TAB_LOCK_DOT_INSET).size(TAB_LOCK_DOT).rounded_full().bg(text).opacity(corner_level))
            .into_any_element(),
        Sign::Dot => div().size(DOT).rounded_full().bg(text).into_any_element(),
    }
}

/// A title that a rename or a style change replaced, fading out; `since` is the first frame that drew the fade. `drifts` is
/// false for a style change with the same text, which crossfades in place.
struct Retitle {
    old: SharedString,
    old_folder: Option<SharedString>,
    old_preview: bool,
    old_gone: bool,
    old_from_line: bool,
    drifts: bool,
    since: Option<Instant>,
}

/// A save landing on a tab: `ok` it reached the disk, else it failed. It starts at `since` (the first frame that drew it) plus `delay`; `u` is how far through it is drawn.
struct Settle {
    ok: bool,
    delay: f32,
    since: Option<Instant>,
    u: f32,
}

/// How far a failed save's dot has swollen at `u` of its settle: it overshoots up over the first part, then eases back.
fn swell(u: f32) -> f32 {
    const UP: f32 = 0.4;
    if u < UP { overshoot(u / UP) } else { 1. - ease_out((u - UP) / (1. - UP)) }
}

/// How much of its ink a dot shows `t` seconds into a slow save's breath: full at the start, dimming to `SAVE_BREATH_LOW` and back over `SAVE_BREATH_PERIOD`
/// on a cosine, so it turns around softly at both ends. Before the start (`t` under 0) it is full.
pub fn breathe(t: f32) -> f32 {
    let phase = t.max(0.) / SAVE_BREATH_PERIOD * std::f32::consts::TAU;
    1. - (1. - SAVE_BREATH_LOW) * (0.5 - 0.5 * phase.cos())
}

impl Slot {
    fn new(label: TabLabel, level: f32) -> Self {
        let (mark, sign, mark_level) = (Fade::with(f32::from(label.marked()), FADE_QUICK), label.sign(), f32::from(label.marked()));
        let (corner, corner_level) = (Fade::with(f32::from(label.lock_dot()), FADE_QUICK), f32::from(label.lock_dot()));
        let shares = Sign::ALL.map(|own| Fade::with(f32::from(own == sign), FADE_QUICK));
        let share_levels = Sign::ALL.map(|own| f32::from(own == sign));
        let (pin, pin_level) = (Fade::with(f32::from(label.pinned), FADE_QUICK), f32::from(label.pinned));
        let trail = f32::from(label.marked() || label.pinned);
        Self {
            label,
            leaving: false,
            level,
            level_v: 0.,
            lit: 0.,
            lit_fade: Fade::at(0.),
            lit_to: 0.,
            lifted: 0.,
            dir: 0.,
            offset: 0.,
            offset_v: 0.,
            x: 0.,
            placed: false,
            natural: f32::from(TAB_DROP_ROOM),
            measured: false,
            wide: 0.,
            wide_v: 0.,
            sized: false,
            trail_fade: Fade::with(trail, FADE_QUICK),
            trail,
            w: 0.,
            mark,
            mark_level,
            corner,
            corner_level,
            pin,
            pin_level,
            sign,
            shares,
            share_levels,
            retitle: None,
            title_level: 1.,
            settle: None,
            breath: 1.,
        }
    }

    /// Whether the mark's slot after the title is open: the tab has a mark or a pin.
    fn trail_to(&self) -> f32 {
        f32::from(self.label.marked() || self.label.pinned)
    }

    /// The width the tab settles at in a strip with `room` for one tab.
    fn settled(&self, room: f32) -> f32 {
        fitted_width(wanted_width(self.natural, self.trail_to()), room)
    }
}

/// What a tab's icon slot drags: the tab whose file it is. Nothing in the window takes it; out of the window it becomes the system's file drag
/// of the file as it is on disk (unsaved edits don't go with it), the old title bar's proxy icon.
struct TabFile(EntityId);

/// Makes `slot`, the icon slot of tab `id`, a file drag: a Finder-style card under the pointer, and past the window's edge the system's
/// drag of the file (the explorer's rows drag the same way). `pane` finds the file's path as the drag starts.
fn file_drag<E: StatefulInteractiveElement>(slot: E, id: EntityId, pane: WeakEntity<Pane>) -> E {
    let source = pane.clone();
    slot.on_drag(TabFile(id), move |file, offset, _, cx| {
        let path = pane.read_with(cx, |pane, cx| pane.path_of(file.0, cx)).ok().flatten().unwrap_or_default();
        crate::explorer::row::file_drag(&path, offset, cx)
    })
    .external_drag_payload::<TabFile>(move |file, _, cx| {
        let path = source.read_with(cx, |pane, cx| pane.path_of(file.0, cx)).ok().flatten()?;
        Some(ExternalDragPayload::Files(FileDragPaths::new([(path, false)])))
    })
}

/// The card a rest on a tab opens: the tab's tip, then a few lines around its caret. `level` is how much of it shows; `x` and `y` are where
/// it hangs in the window (`x` glides to the tab the pointer is on).
struct Peek {
    id: EntityId,
    header: SharedString,
    lines: Vec<SharedString>,
    caret_row: usize,
    open: bool,
    fade: Fade,
    level: f32,
    x: f32,
    x_v: f32,
    y: f32,
    placed: bool,
    last: Option<Instant>,
}

/// The peek card drawn on the tooltip's float plane, `level` of the way in.
fn peek_card(peek: &Peek, light: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let ink = theme.sidebar_foreground;
    let (fill, border) = tooltip::float_fill(theme.background, ink, light);
    let editor = &crate::settings::get(cx).editor;
    let family = editor.drawn_family(cx).unwrap_or_else(|| theme.mono_font_family.clone());
    let size = editor.font_size.map(px).unwrap_or(theme.mono_font_size).min(px(PEEK_TEXT_MAX));
    let (tint, rule) = (Hsla { a: ALPHA_SNIPPET_CARET, ..ink }, Hsla { a: ALPHA_HAIRLINE, ..ink });
    let (words, text) = (theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_SECONDARY, cx));
    // The tip's first line is the path (or the Untitled title) in primary ink; the lines after it (Pinned, Unsaved changes, Read-only, what happened on disk) are flags, a step quieter.
    let (path, flags) = peek.header.split_once('\n').unwrap_or((&peek.header, ""));
    let body = tooltip::float(RADIUS_TOOLTIP, fill, border, light, false)
        .w(px(PEEK_WIDTH))
        .flex()
        .flex_col()
        .gap(px(TOOLTIP_PAD_Y))
        .px(px(TOOLTIP_PAD_X))
        .py(px(TOOLTIP_PAD_Y))
        .text_size(TEXT_SUBHEADLINE)
        .line_height(TEXT_SUBHEADLINE_LINE)
        .font_weight(FontWeight::NORMAL)
        .text_color(words)
        .opacity(peek.level)
        .top(px(TOOLTIP_RISE * (1. - peek.level)))
        .child(div().min_w_0().flex().flex_col().child(path.to_string()).when(!flags.is_empty(), |header| header.child(div().text_color(text).child(flags.to_string()))))
        .when(!peek.lines.is_empty(), |card| card.child(div().h(px(1.)).bg(rule)).child(crate::snippet::snippet(&peek.lines, peek.caret_row, family, size, text, tint)));
    deferred(anchored().position(point(px(peek.x), px(peek.y))).child(body)).with_priority(LAYER_TOOLTIP)
}

/// The active tab's mark: left and width, and their speeds.
#[derive(Default)]
struct Mark {
    live: bool,
    at: [f32; 2],
    v: [f32; 2],
}

/// The room a drag from elsewhere opens where it would land. It is only width: never a tab, so
/// it has no bounds, no number and nothing saved.
struct Gap {
    /// The live tab it sits before (after the last when past it).
    at: usize,
    level: f32,
    level_v: f32,
    /// The drag is over the strip; otherwise the gap folds away.
    open: bool,
    /// It was dropped on: the first tab that arrives takes its place and width.
    landing: bool,
    /// Where layout put it, in the strip's content.
    x: f32,
}

/// A tab dropped in the bar: a copy of its ghost springs from where it was let go into the tab's
/// slot over `ARRIVE` while its shadow eases out, and then crossfades with the slot.
struct Landing {
    id: EntityId,
    /// The ghost's place in the window as it was let go, and how far it had lifted.
    from: Bounds<Pixels>,
    lift: f32,
    at: Instant,
    /// How far home the copy has come, 0 to 1, as of this frame.
    home: f32,
    /// The tab isn't in the strip yet; the copy waits for its slot.
    waiting: bool,
    /// The copy's speed as it was let go, in points a second: a copy that interrupts another carries the pace
    /// the first was drawn at, so the turn is smooth.
    speed: [f32; 2],
    /// Seconds since the glide began, as of this frame.
    t: f32,
}

/// How long, in seconds, a copy waits for the tab it was let go with to reach the strip before it is dropped.
const LANDING_WAIT: f32 = 0.25;

/// How much of a window's width change a strip follows (1 for a pane the window's whole width, half for one of two side by side),
/// and the change the frames since the strip was last measured right carried it by (`since`, of which `delta` is this frame's),
/// shared with the strip's width canvas.
#[derive(Clone)]
struct Carry {
    share: Rc<Cell<f32>>,
    delta: Rc<Cell<f32>>,
    since: Rc<Cell<f32>>,
}

impl Default for Carry {
    fn default() -> Self {
        Self { share: Rc::new(Cell::new(1.)), delta: Rc::default(), since: Rc::default() }
    }
}

/// The width canvas's check of what the strip measured (`now`) against the width its layout used: a miss of half a point
/// sets `used` right, learns how much of the window's change the strip follows when this frame carried one, and notes a
/// strip that got narrower. Says whether one more frame is wanted to lay the tabs out again.
fn measured(used: &Cell<f32>, shrunk: &Cell<bool>, carry: &Carry, now: f32, flipped: bool) -> bool {
    let miss = now - used.get();
    if !(flipped || (now > 0. && miss.abs() > 0.5)) {
        // A strip the layout has right (to a twentieth of a point) has nothing building up.
        if miss.abs() < 0.05 {
            carry.since.set(0.);
        }
        return false;
    }
    if now > 0. {
        shrunk.set(shrunk.get() || miss < -0.5);
        let since = carry.since.get();
        // A button fading in or out moves the strip too: that miss says nothing of the window. A miss builds up over
        // every frame since the strip was last set right (misses under half a point never correct), so it is read against that change.
        if !flipped && carry.delta.get().abs() >= 0.5 && since.abs() >= 0.5 && miss.abs() > 0.5 {
            carry.share.set((carry.share.get() + miss / since).clamp(0., 1.));
        }
        used.set(now);
        carry.since.set(0.);
    }
    true
}

/// The bar's tabs and what moves them.
#[derive(Default)]
pub struct Motion {
    slots: Vec<Slot>,
    active: Option<EntityId>,
    mark: Mark,
    /// The mark is gliding to a tab it switched to; otherwise it sits on the active tab.
    glide: bool,
    /// How much of the mark is hidden: 1 right after the active tab left the pane, when the mark
    /// lands on the new tab and fades in, easing to 0.
    hidden: f32,
    hide: Fade,
    /// The tab order changed this frame.
    reordered: bool,
    seen: bool,
    /// The family and weight the tabs were measured in: a change measures every tab again.
    font: Option<(SharedString, f32)>,
    /// The strip's width the layout used, kept current by a canvas in the band.
    width: Rc<Cell<f32>>,
    /// The window's width when `width` was last set, so a resize frame can carry the strip's width to its own frame's.
    viewport: Cell<f32>,
    /// The width canvas found the strip narrower than the layout used, by half a point or more.
    shrunk: Rc<Cell<bool>>,
    /// How much of the window's change the strip follows, and what this frame carried; the width canvas learns the first from a miss.
    carry: Carry,
    /// The tabs' width in the strip as laid out this frame.
    content: f32,
    /// The tab list button showed in the frame last drawn, and where it sat then.
    list_shown: Cell<bool>,
    list_place: Rc<Cell<Bounds<Pixels>>>,
    /// The tab list's menu is open: its button holds its pressed look.
    list_held: Cell<bool>,
    /// How much of the tab list button shows (0 gone, 1 whole), its speed, and the room the strip gave it in the frame last drawn.
    /// The button's fade and the strip's room follow this level, which springs toward `list_shown`.
    list_level: Cell<f32>,
    /// The strip has been measured once; the first frame that knows its width shows the button as it is, without a fade.
    list_measured: Cell<bool>,
    list_level_v: f32,
    list_reserved: Cell<f32>,
    /// The badge on the tab list button: how much of the dot shows for a marked tab hidden past the strip's settled view, on the tab dot's own fade (made on first use).
    badge: Option<Fade>,
    badge_level: f32,
    /// The peek card a rest on a tab opens; `None` while none is up or fading.
    peek: Option<Peek>,
    /// The frame it last moved in; `None` at rest, so a move starts from the frame it begins in.
    last: Option<Instant>,
    /// The active tab is to be scrolled into view.
    reveal: bool,
    /// The gap a drag from elsewhere opened, while it shows.
    gap: Option<Gap>,
    /// The gap was opened by the control socket's `drag-hold`, which holds no real drag: the pane's own check for a drag that ended must leave it.
    gap_staged: bool,
    /// The dragged tab is well away from its bar, so its slot folds closed behind the card (and opens again when it comes back).
    folded: bool,
    /// The copy of a dropped tab, while it glides home and crossfades.
    landing: Option<Landing>,
}

impl Motion {
    /// Whether the tabs `same` finds in each slot, in order, are the ones now open with `active` showing, so a frame that only resizes skips building labels.
    pub fn unchanged(&self, active: Option<EntityId>, count: usize, mut same: impl FnMut(usize, &TabLabel) -> bool) -> bool {
        self.seen && self.slots.len() == count && active == self.active && self.slots.iter().enumerate().all(|(ix, slot)| !slot.leaving && same(ix, &slot.label))
    }

    /// Carries the strip's width to the window's width this frame: a resize moves the strip by what it moves the window
    /// times the share it was last seen to follow (a pane beside another follows half), so the layout uses this frame's
    /// width and the canvas after the strip only corrects a pane whose share changed.
    /// A strip that got narrower (by this carry or by that correction, half a point or more) asks to bring the active tab back into view.
    pub fn fit(&mut self, viewport: f32) {
        let (width, seen) = (self.width.get(), self.viewport.replace(viewport));
        let delta = if width > 0. && seen > 0. { viewport - seen } else { 0. };
        self.carry.delta.set(delta);
        self.carry.since.set(self.carry.since.get() + delta);
        if delta != 0. {
            let carried = delta * self.carry.share.get();
            self.width.set((width + carried).max(1.));
            self.reveal |= carried <= -0.5;
        }
        self.reveal |= self.shrunk.take();
    }

    /// Brings the slots in line with `labels` (`active` the showing tab). A tab not seen before
    /// arrives from nothing, one that is gone stays to fold away; the very first call shows
    /// everything in place.
    pub fn sync(&mut self, labels: Vec<TabLabel>, active: usize) {
        // The same tabs in the same state change nothing below, so a frame that only moves them skips the rebuild.
        if self.seen
            && self.slots.len() == labels.len()
            && labels.get(active).map(|label| label.id) == self.active
            && self.slots.iter().zip(&labels).all(|(slot, label)| !slot.leaving && slot.label == *label)
        {
            return;
        }
        let first = !self.seen;
        self.seen = true;
        let active_id = labels.get(active).map(|label| label.id);
        let old_ids: Vec<EntityId> = self.slots.iter().map(|slot| slot.label.id).collect();
        let new_ids: HashSet<EntityId> = labels.iter().map(|label| label.id).collect();
        let before: Vec<EntityId> = self.slots.iter().filter(|slot| !slot.leaving && new_ids.contains(&slot.label.id)).map(|slot| slot.label.id).collect();
        let kept: HashSet<EntityId> = before.iter().copied().collect();
        let after: Vec<EntityId> = labels.iter().map(|label| label.id).filter(|id| kept.contains(id)).collect();
        self.reordered |= before != after;

        let mut by_id: HashMap<EntityId, Slot> = std::mem::take(&mut self.slots).into_iter().map(|slot| (slot.label.id, slot)).collect();
        let mut out: Vec<Slot> = Vec::with_capacity(labels.len() + by_id.len());
        for label in labels {
            let mut slot = match by_id.remove(&label.id) {
                Some(slot) => slot,
                None => {
                    let mut slot = Slot::new(label.clone(), if first { 1. } else { 0. });
                    // The tab dropped in the gap takes its width and place instead of arriving again;
                    // landing elsewhere (at the end of the pins), it glides there from the gap.
                    if let Some(gap) = self.gap.take_if(|gap| gap.landing) {
                        (slot.level, slot.level_v, slot.x, slot.placed) = (gap.level, gap.level_v, gap.x, true);
                        // It takes the gap's width too, and eases from there to its own.
                        (slot.wide, slot.sized) = (f32::from(TAB_DROP_ROOM), true);
                        self.reordered = true;
                    }
                    slot
                }
            };
            // An existing tab whose title changed (a rename, a number, a folder joining it) crossfades it; a new tab shows its title whole.
            // The same text in another style (a preview tab kept, a file deleted under its edits) crossfades the same way, in place.
            let renamed = slot.label.title != label.title || slot.label.folder != label.folder;
            if renamed || slot.label.preview != label.preview || slot.label.gone != label.gone || slot.label.from_line != label.from_line {
                // Its width follows its new title, measured before the next layout.
                slot.measured = false;
                slot.retitle = Some(Retitle {
                    old: slot.label.title.clone(),
                    old_folder: slot.label.folder.clone(),
                    old_preview: slot.label.preview,
                    old_gone: slot.label.gone,
                    old_from_line: slot.label.from_line,
                    drifts: renamed,
                    since: None,
                });
            }
            slot.label = label;
            slot.leaving = false;
            out.push(slot);
        }
        // A closed tab keeps its place after the tab that was before it: each waits behind the last tab still open before it, in order, and one pass lays them in.
        let mut behind: HashMap<Option<EntityId>, Vec<Slot>> = HashMap::new();
        let mut open_before = None;
        for id in &old_ids {
            if new_ids.contains(id) {
                open_before = Some(*id);
                continue;
            }
            let Some(mut slot) = by_id.remove(id) else { continue };
            slot.leaving = true;
            if slot.level > 0. {
                behind.entry(open_before).or_default().push(slot);
            }
        }
        if !behind.is_empty() {
            let mut placed = Vec::with_capacity(out.len() + behind.values().map(Vec::len).sum::<usize>());
            placed.extend(behind.remove(&None).into_iter().flatten());
            for slot in out {
                let id = slot.label.id;
                placed.push(slot);
                placed.extend(behind.remove(&Some(id)).into_iter().flatten());
            }
            out = placed;
        }

        if active_id != self.active {
            let position = |id: Option<EntityId>| id.and_then(|id| out.iter().position(|slot| slot.label.id == id));
            let dir = match (position(self.active), position(active_id)) {
                (Some(from), Some(to)) => (to as f32 - from as f32).signum(),
                _ => 0.,
            };
            for slot in &mut out {
                let on = Some(slot.label.id) == active_id;
                slot.lit_to = f32::from(on);
                if on {
                    slot.dir = dir;
                }
                if first {
                    slot.lit = slot.lit_to;
                    slot.lit_fade = Fade::at(slot.lit_to);
                }
            }
            // A mark whose tab left the pane (closed, split off, dragged out) has nowhere to glide from.
            let left = self.active.is_some_and(|old| !out.iter().any(|slot| slot.label.id == old && !slot.leaving));
            self.glide = !first && !left;
            if left {
                self.hidden = 1.;
                self.hide = Fade::at(1.);
            }
            self.active = active_id;
        }
        self.slots = out;
    }

    /// Measures each tab not measured for the title it shows (all of them when the theme's family or the text weight
    /// changed), shaping its title as the tab draws it. Call it after `sync` and before `advance`, which lays them out.
    pub fn measure(&mut self, window: &Window, cx: &App) {
        let font = (cx.theme().font_family.clone(), crate::settings::get(cx).text.weight);
        if self.font.as_ref() != Some(&font) {
            self.slots.iter_mut().for_each(|slot| slot.measured = false);
            self.font = Some(font.clone());
        }
        for slot in self.slots.iter_mut().filter(|slot| !slot.measured) {
            (slot.natural, slot.measured) = (natural_width(&slot.label, &font.0, font.1, window), true);
        }
    }

    /// What tab `id` shows now, for the card a held drag lifts.
    pub fn label_of(&self, id: EntityId) -> Option<&TabLabel> {
        self.slots.iter().find(|slot| slot.label.id == id).map(|slot| &slot.label)
    }

    /// A save of tab `id` landed (`ok`) or failed: its dot settles (or swells) after `delay` seconds, and the next frame draws it.
    pub fn settle(&mut self, id: EntityId, ok: bool, delay: f32) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.label.id == id && !slot.leaving) {
            slot.settle = Some(Settle { ok, delay, since: None, u: 0. });
        }
    }

    /// Fills `into` with the slot each tab sits in, in one pass, for `label_at`.
    pub fn index_slots(&self, into: &mut HashMap<EntityId, usize>) {
        into.clear();
        into.extend(self.slots.iter().enumerate().map(|(ix, slot)| (slot.label.id, ix)));
    }

    /// What the tab in slot `ix` shows now.
    pub fn label_at(&self, ix: usize) -> &TabLabel {
        &self.slots[ix].label
    }

    /// Asks for the active tab to be scrolled into view with the next frame.
    pub fn reveal(&mut self) {
        self.reveal = true;
    }

    /// Where the tab list button sits in the window, `None` while the strip fits and it is hidden.
    pub fn list_button(&self) -> Option<Bounds<Pixels>> {
        (self.list_level.get() > 0.).then(|| self.list_place.get())
    }

    /// The tab list's menu opens (`true`) or closes: its button holds the pressed look meanwhile.
    pub fn hold_list(&self, held: bool) {
        self.list_held.set(held);
    }

    /// Whether any live tab with a dot (unsaved edits) or a warning (changed or gone on disk) has its mark outside the strip's view, drawn `at` points in, as the layout of this frame has them. The pane passes the view the strip settles in, so a glide's frames never change the answer.
    fn hidden_mark(&self, at: f32) -> bool {
        let view = self.width.get();
        let (gap, slot) = (f32::from(TAB_GAP), f32::from(TAB_TRAIL_SLOT));
        // The pinned tabs hold the strip's left (with the room before them), so they are never out of view and the rest scroll under them.
        let pinned: f32 = self.slots.iter().filter(|tab| !tab.leaving && tab.label.pinned).map(|tab| tab.level * (tab.w + gap)).sum();
        let held = if pinned > 0. { f32::from(TAB_PLATE_ROOM) + pinned } else { 0. };
        view > 0.
            && self.slots.iter().filter(|tab| !tab.leaving && !tab.label.pinned && (tab.label.dirty || tab.label.conflict || tab.label.gone)).any(|tab| {
                // The mark sits in the slot after the title, at the tab's right end.
                let right = tab.x + tab.level * (tab.w + gap) - gap - f32::from(TAB_PAD_TRAIL);
                right - slot < at + held || right > at + view
            })
    }

    /// Steps the list button's badge toward whether a marked tab is hidden past the view the strip settles in at `settled` points in, and says whether it still fades. Call it after `advance`, which lays the tabs out.
    pub fn advance_badge(&mut self, settled: f32, now: Instant) -> bool {
        let hidden = self.hidden_mark(settled);
        let fade = self.badge.get_or_insert_with(|| Fade::with(0., FADE_QUICK));
        fade.aim(f32::from(hidden), now);
        self.badge_level = fade.level(now);
        fade.is_moving(now)
    }

    /// Whether the peek card is up or still fading, so a pointer coming onto another tab hands it over at once instead of waiting again.
    pub fn peek_shows(&self) -> bool {
        self.peek.as_ref().is_some_and(|peek| peek.open || peek.level > 0.)
    }

    /// The tab the open peek card is over, for the pane to say where it sits.
    pub fn peek_id(&self) -> Option<EntityId> {
        self.peek.as_ref().filter(|peek| peek.open).map(|peek| peek.id)
    }

    /// Opens the peek card on tab `id`, with the tab's tip as `header` and its `lines` (the caret's on row `caret_row`, none when the
    /// text isn't built). A card already up keeps its place and glides to the new tab.
    pub fn peek_open(&mut self, id: EntityId, header: SharedString, lines: Vec<SharedString>, caret_row: usize) {
        match &mut self.peek {
            Some(peek) if peek.open || peek.level > 0. => {
                (peek.id, peek.header, peek.lines, peek.caret_row, peek.open) = (id, header, lines, caret_row, true);
            }
            _ => self.peek = Some(Peek { id, header, lines, caret_row, open: true, fade: Fade::with(0., FADE_QUICK), level: 0., x: 0., x_v: 0., y: 0., placed: false, last: None }),
        }
    }

    /// Puts the card away (it fades). True when it was open.
    pub fn peek_close(&mut self) -> bool {
        self.peek.as_mut().is_some_and(|peek| std::mem::replace(&mut peek.open, false))
    }

    /// Steps the peek card: its fade, and its glide under tab `over` (where the tab is drawn in the window) in a window `window_width` wide.
    /// Says whether it still moves; at rest, open or gone, it asks for no frame.
    pub fn advance_peek(&mut self, now: Instant, over: Option<Bounds<Pixels>>, window_width: Pixels) -> bool {
        let Some(peek) = &mut self.peek else { return false };
        let dt = peek.last.map_or(0., |last| now.saturating_duration_since(last).as_secs_f32().min(MAX_STEP));
        peek.fade.aim(f32::from(peek.open), now);
        peek.level = peek.fade.level(now);
        let mut moving = peek.fade.is_moving(now);
        if let Some(tab) = over.filter(|_| peek.open) {
            let centered = f32::from(tab.origin.x + tab.size.width / 2.) - PEEK_WIDTH / 2.;
            let to = centered.clamp(PEEK_EDGE, (f32::from(window_width) - PEEK_WIDTH - PEEK_EDGE).max(PEEK_EDGE));
            peek.y = f32::from(tab.origin.y + tab.size.height) + PEEK_GAP;
            if peek.placed {
                moving |= follow(&mut peek.x, &mut peek.x_v, to, TAU / SELECT_RESPONSE, dt, 0.25, 4.);
            } else {
                (peek.x, peek.x_v, peek.placed) = (to, 0., true);
            }
        }
        if !peek.open && peek.level <= 0. && !moving {
            self.peek = None;
            return false;
        }
        peek.last = moving.then_some(now);
        moving
    }

    /// Whether the dragged tab's slot is folded closed. True when that changed.
    pub fn fold_dragged(&mut self, folded: bool) -> bool {
        std::mem::replace(&mut self.folded, folded) != folded
    }

    /// Whether the dragged tab's slot is folded closed.
    pub fn is_folded(&self) -> bool {
        self.folded
    }

    /// Opens the gap before live tab `at`, from nothing or gliding there from where it is. True
    /// when anything changed.
    pub fn open_gap(&mut self, at: usize, staged: bool) -> bool {
        self.gap_staged = staged;
        match &mut self.gap {
            Some(gap) if gap.open && gap.at == at => false,
            Some(gap) => {
                // The tabs it passes glide aside instead of jumping.
                self.reordered |= gap.at != at;
                (gap.at, gap.open, gap.landing) = (at, true, false);
                true
            }
            None => {
                self.gap = Some(Gap { at, level: 0., level_v: 0., open: true, landing: false, x: 0. });
                true
            }
        }
    }

    /// Folds the gap away. True when it was open.
    pub fn close_gap(&mut self) -> bool {
        match &mut self.gap {
            Some(gap) if gap.open => {
                (gap.open, gap.landing) = (false, false);
                true
            }
            _ => false,
        }
    }

    /// Whether the gap shows for a held drag that GPUI has no drag for (`drag-hold`), which ends with `drag-release`.
    pub fn gap_staged(&self) -> bool {
        self.gap_staged
    }

    /// Something was dropped on the gap: the next tab to arrive takes it.
    pub fn land_gap(&mut self) {
        if let Some(gap) = &mut self.gap {
            gap.landing = gap.open;
        }
    }

    /// Takes a pending reveal: the scroll position (the strip drawn `at` pixels in) that brings the
    /// active tab fully into view with a `REVEAL_INSET` margin from the strip's edge, judged on the
    /// layout the tabs settle in. The pinned tabs at the left hold their place and cover the strip's
    /// first stretch, so the tab is brought clear of them; a pinned tab needs no scrolling. `None`
    /// when none is pending, the tab is fully in view already, or
    /// the strip is not measured yet (the reveal then waits for the next frame).
    pub fn take_reveal(&mut self, at: f32) -> Option<f32> {
        let view = self.width.get();
        if !self.reveal || view <= 0. {
            return None;
        }
        self.reveal = false;
        let (gap, edge) = (f32::from(TAB_GAP), f32::from(TAB_PLATE_ROOM));
        // Each live tab's room in the strip once it settles: its own width, fitted to the strip, and its gap.
        let units: Vec<(EntityId, f32)> = self.slots.iter().filter(|slot| !slot.leaving).map(|slot| (slot.label.id, slot.settled(view - gap - edge * 2.) + gap)).collect();
        let ix = units.iter().position(|(id, _)| Some(*id) == self.active)?;
        let pinned = self.slots.iter().filter(|slot| !slot.leaving && slot.label.pinned).count();
        if ix < pinned {
            return None;
        }
        // The tabs start `TAB_PLATE_ROOM` into the strip's content and end as far before its end.
        let left: f32 = edge + units[..ix].iter().map(|(_, unit)| unit).sum::<f32>();
        let right = left + units[ix].1 - gap;
        // Where a fade lies over the strip's end (more tabs past it), a tab under it is not in view; a strip with little room beside the tab has less of it.
        let held: f32 = if pinned > 0 { edge + units[..pinned].iter().map(|(_, unit)| unit).sum::<f32>() } else { 0. };
        let inset = f32::from(REVEAL_INSET).min((view - held - (units[ix].1 - gap)).max(0.) / 2.);
        let max = (edge * 2. + units.iter().map(|(_, unit)| unit).sum::<f32>() - view).max(0.);
        let (clear_left, clear_right) = (if at > 0.5 { inset } else { 0. }, if at < max - 0.5 { inset } else { 0. });
        if left >= at + held + clear_left && right <= at + view - clear_right {
            return None;
        }
        let to = if left < at + held + clear_left { left - held - inset } else { right + inset - view };
        let to = to.clamp(0., max);
        ((to - at).abs() > 0.5).then_some(to)
    }

    /// Starts tab `id`'s copy gliding home at `now` from `from`, where its ghost was let go `lift`
    /// of the way up. A `waiting` copy is of a tab not in the strip yet: it holds at `from` until
    /// the tab's slot exists (at most `LANDING_WAIT`) and glides from the frame that has it.
    pub fn land(&mut self, id: EntityId, from: Bounds<Pixels>, lift: f32, speed: [f32; 2], now: Instant, waiting: bool) {
        self.landing = Some(Landing { id, from, lift, at: now, home: 0., waiting, speed, t: 0. });
    }

    /// Where tab `id`'s copy is drawn now, how far it has lifted and how fast it moves, if it is in
    /// flight toward `to` (its slot's place): what a second carry of the same tab starts from.
    pub fn chip(&self, id: EntityId, to: Bounds<Pixels>) -> Option<(Bounds<Pixels>, f32, [f32; 2])> {
        let landing = self.landing.as_ref().filter(|landing| landing.id == id && !landing.waiting && landing.home < 1.)?;
        let ((x, vx), (y, vy)) = (chip_axis(landing, to.origin.x, landing.from.origin.x, 0), chip_axis(landing, to.origin.y, landing.from.origin.y, 1));
        let width = landing.from.size.width + (to.size.width - landing.from.size.width) * landing.home;
        let height = landing.from.size.height + (to.size.height - landing.from.size.height) * landing.home;
        Some((Bounds { origin: point(px(x), px(y)), size: size(width, height) }, landing.lift * (1. - landing.home), [vx, vy]))
    }

    /// Steps everything to `now`, lays the tabs out, and says whether anything still moves; the
    /// pane asks for another frame only then. The `dragging` tab's slot never eases, and a dropped
    /// tab's slot stays lifted until its copy is home.
    pub fn advance(&mut self, now: Instant, dragging: Option<EntityId>) -> bool {
        // The frame a move starts in draws its start state: no time has passed, so nothing steps.
        let dt = self.last.map_or(0., |last| now.duration_since(last).as_secs_f32().min(MAX_STEP));
        let (level_omega, glide_omega) = (SETTLE_RATE / ARRIVE, TAU / SELECT_RESPONSE);
        let mut moving = false;
        // A new drag takes over from a copy still on its way.
        if dragging.is_some() {
            self.landing = None;
        }
        if let Some(landing) = &mut self.landing {
            if landing.waiting {
                if let Some(slot) = self.slots.iter_mut().find(|slot| slot.label.id == landing.id && !slot.leaving) {
                    // The slot comes in under the card, lifted, so the bar never shows the tab flat before the copy glides.
                    (landing.waiting, landing.at, slot.lifted) = (false, now, 1.);
                } else if now.saturating_duration_since(landing.at).as_secs_f32() > LANDING_WAIT {
                    self.landing = None;
                }
            }
        }
        if let Some(landing) = &mut self.landing {
            landing.t = if landing.waiting { 0. } else { now.saturating_duration_since(landing.at).as_secs_f32() };
            landing.home = if landing.waiting { 0. } else { settle(landing.t, ARRIVE) };
        }
        let homing = self.landing.as_ref().filter(|landing| landing.home < 1.).map(|landing| landing.id);
        for slot in &mut self.slots {
            let to = if slot.leaving || (self.folded && dragging == Some(slot.label.id)) { 0. } else { 1. };
            moving |= follow(&mut slot.level, &mut slot.level_v, to, level_omega, dt, 0.004, 0.05);
            slot.level = slot.level.clamp(0., 1.);
            slot.lit_fade.aim(slot.lit_to, now);
            slot.lit = slot.lit_fade.level(now);
            moving |= slot.lit_fade.is_moving(now);
            let lifted_to = f32::from(dragging == Some(slot.label.id) || homing == Some(slot.label.id));
            slot.lifted += (lifted_to - slot.lifted) * (1. - (-dt * level_omega).exp());
            if (lifted_to - slot.lifted).abs() < 0.01 {
                slot.lifted = lifted_to;
            } else {
                moving = true;
            }
            // The mark fades with whether the tab has one; a tab first seen starts at its state.
            slot.mark.aim(f32::from(slot.label.marked()), now);
            slot.mark_level = slot.mark.level(now);
            moving |= slot.mark.is_moving(now);
            slot.corner.aim(f32::from(slot.label.lock_dot()), now);
            slot.corner_level = slot.corner.level(now);
            moving |= slot.corner.is_moving(now);
            slot.pin.aim(f32::from(slot.label.pinned), now);
            slot.pin_level = slot.pin.level(now);
            moving |= slot.pin.is_moving(now);
            // Each sign has its own fade toward 1 when it is the tab's sign and toward 0 when it isn't, so a sign that changes while the
            // mark shows crossfades over FADE_QUICK and a second change mid-fade turns every sign around from the level it is drawn at
            // (the one leaving keeps fading out, the new one fades in). One that changes while the mark is hidden just takes its place.
            if slot.label.marked() {
                let sign = slot.label.sign();
                if sign != slot.sign {
                    if slot.mark_level <= 0. {
                        slot.shares = Sign::ALL.map(|own| Fade::with(f32::from(own == sign), FADE_QUICK));
                    }
                    slot.sign = sign;
                }
            }
            for own in Sign::ALL {
                let share = &mut slot.shares[own.ix()];
                share.aim(f32::from(own == slot.sign), now);
                slot.share_levels[own.ix()] = share.level(now);
                moving |= share.is_moving(now);
            }
            // A renamed tab's title crossfades over FADE_QUICK from the first frame that draws it.
            if let Some(retitle) = &mut slot.retitle {
                let u = now.saturating_duration_since(*retitle.since.get_or_insert(now)).as_secs_f32() / FADE_QUICK;
                slot.title_level = ease_out(u);
                if u >= 1. {
                    slot.retitle = None;
                    slot.title_level = 1.;
                } else {
                    moving = true;
                }
            }
            // A save settling on the tab (or failing): from its first frame, over `SAVE_SETTLE` after its stagger. A save that lands saved
            // keeps the dot drawn while it shrinks, so the icon fades back in with it; edits made meanwhile cancel it.
            if let Some(settle) = &mut slot.settle {
                let since = *settle.since.get_or_insert(now);
                let t = now.saturating_duration_since(since).as_secs_f32() - settle.delay;
                if t >= SAVE_SETTLE || (settle.ok && slot.label.marked()) {
                    slot.settle = None;
                } else {
                    settle.u = (t / SAVE_SETTLE).max(0.);
                    if settle.ok {
                        slot.mark_level = slot.mark_level.max(1. - ease_out(settle.u));
                    }
                    moving = true;
                }
            }
            // A slow save breathes the dot, from full ink so it never pops; when it ends with the dot staying, the ink eases back to full from
            // where it is drawn, and when the dot goes out (the save landed) the level is held so the settle's fade starts from it.
            match slot.label.saving.filter(|_| slot.label.dirty) {
                Some(began) => {
                    slot.breath = breathe(now.saturating_duration_since(began).as_secs_f32() - SAVE_SLOW);
                    moving = true;
                }
                None if slot.label.marked() && slot.breath != 1. => {
                    slot.breath += (1. - slot.breath) * (1. - (-dt * level_omega).exp());
                    if (1. - slot.breath).abs() < 0.01 {
                        slot.breath = 1.;
                    } else {
                        moving = true;
                    }
                }
                None => {}
            }
            if !slot.label.marked() && slot.mark_level <= 0. {
                slot.breath = 1.;
            }
        }
        self.slots.retain(|slot| !(slot.leaving && slot.level <= 0.));
        if let Some(room) = &mut self.gap {
            moving |= follow(&mut room.level, &mut room.level_v, f32::from(room.open), level_omega, dt, 0.004, 0.05);
            room.level = room.level.clamp(0., 1.);
        }
        self.gap.take_if(|room| !room.open && room.level <= 0.);
        let (mut level, list_to) = (self.list_level.get(), f32::from(self.list_shown.get()));
        moving |= follow(&mut level, &mut self.list_level_v, list_to, level_omega, dt, 0.004, 0.05);
        self.list_level.set(level.clamp(0., 1.));
        self.hide.aim(0., now);
        self.hidden = self.hide.level(now);
        moving |= self.hide.is_moving(now);
        // The copy goes once its slot has crossfaded back, or with its tab.
        if let Some(landing) = &self.landing {
            match self.slots.iter().find(|slot| slot.label.id == landing.id && !slot.leaving) {
                Some(slot) if landing.home < 1. || slot.lifted > 0. => moving = true,
                None if landing.waiting => moving = true,
                _ => self.landing = None,
            }
        }

        // Layout: each tab takes its level of its own width, fitted to the strip's room, and its gap. The tabs start
        // `TAB_PLATE_ROOM` in and end as far before the content's end, so the scroll's clip never cuts the plate's rim and shadow.
        let (gap, edge) = (f32::from(TAB_GAP), f32::from(TAB_PLATE_ROOM));
        let width = (self.width.get() + self.list_reserved.get() - self.list_level.get() * f32::from(ICON_BUTTON + TAB_LIST_GAP)).max(0.);
        let room = if width > 0. { width - gap - edge * 2. } else { f32::MAX };
        let mut x = edge;
        let mut live = 0;
        for slot in &mut self.slots {
            // A tab's content eases to a new title's width, and its mark's slot opens or shuts beside it; the strip's room fits it
            // at once, so a resize never lags.
            slot.trail_fade.aim(slot.trail_to(), now);
            slot.trail = slot.trail_fade.level(now);
            moving |= slot.trail_fade.is_moving(now);
            if slot.sized {
                moving |= follow(&mut slot.wide, &mut slot.wide_v, slot.natural, level_omega, dt, 0.25, 4.);
            } else {
                (slot.wide, slot.sized) = (slot.natural, true);
            }
            slot.w = fitted_width(wanted_width(slot.wide, slot.trail), room);
            // The gap takes its level of the room it opens before the live tab it sits before.
            if let Some(room) = self.gap.as_mut().filter(|room| !slot.leaving && room.at == live) {
                room.x = x;
                x += room.level * (f32::from(TAB_DROP_ROOM) + gap);
            }
            live += usize::from(!slot.leaving);
            let new_x = x;
            x += slot.level * (slot.w + gap);
            if self.reordered && slot.placed {
                if dragging == Some(slot.label.id) {
                    (slot.offset, slot.offset_v) = (0., 0.);
                } else {
                    // Stay drawn where it was, then glide to the new place.
                    slot.offset += slot.x - new_x;
                }
            }
            slot.x = new_x;
            slot.placed = true;
            moving |= follow(&mut slot.offset, &mut slot.offset_v, 0., glide_omega, dt, 0.25, 4.);
        }
        if let Some(room) = self.gap.as_mut().filter(|room| room.at >= live) {
            room.x = x;
            x += room.level * (f32::from(TAB_DROP_ROOM) + gap);
        }
        self.content = x + edge;
        self.reordered = false;

        let target = self
            .slots
            .iter()
            .find(|slot| Some(slot.label.id) == self.active && !slot.leaving)
            .map(|slot| [slot.x + slot.offset, (slot.level * (slot.w + gap)).min(slot.w)]);
        match target {
            None => (self.mark.live, self.glide) = (false, false),
            Some(to) if !self.mark.live || !self.glide || dragging == self.active => {
                self.mark = Mark { live: true, at: to, v: [0.; 2] };
                self.glide = false;
            }
            Some(to) => {
                let mut mark_moving = false;
                for i in 0..2 {
                    mark_moving |= follow(&mut self.mark.at[i], &mut self.mark.v[i], to[i], glide_omega, dt, 0.25, 4.);
                }
                self.glide = mark_moving;
                moving |= mark_moving;
            }
        }
        self.last = moving.then_some(now);
        moving
    }
}

/// The bar's right padding while the window's preview button sits over it. `level` is how open the
/// preview column is drawn (its slide's own level): shut leaves room for the button, open leaves
/// the usual inset, and in between the room closes with the column.
pub fn button_room(level: f32) -> Pixels {
    let room = ICON_BUTTON + ICON_BUTTON_INSET * 2.;
    room + (TAB_BAR_INSET - room) * level.clamp(0., 1.)
}

/// The bar for `motion`, which the pane has synced and advanced for this frame. `key` is how lit the
/// active tab's mark is, 1 while the pane has the keyboard in the key window and 0 when it hasn't
/// (the pane eases it). `bounds` receives each live tab's place (where layout puts it, ignoring its
/// glide) for the drag, `left_inset` is the bar's left padding: more than usual keeps the first tab
/// off the traffic lights; `right` is the bar's right padding, which `button_room` sets so the
/// window's preview button has room at the right end; `band` is the bar's height, which the tab centers in.
pub fn render(
    motion: &Motion,
    key: f32,
    right: Pixels,
    left_inset: Pixels,
    band: Pixels,
    scroll: &ScrollHandle,
    bounds: &Rc<RefCell<Vec<Bounds<Pixels>>>>,
    cx: &mut Context<Pane>,
) -> impl IntoElement {
    let light = theme::is_light(cx);
    let (selected_alpha, unfocused_alpha, hover_alpha) =
        if light { (ALPHA_SELECTED_LIGHT, ALPHA_SELECTED_UNFOCUSED_LIGHT, ALPHA_HOVER_LIGHT) } else { (ALPHA_SELECTED, ALPHA_SELECTED_UNFOCUSED, ALPHA_HOVER) };
    let selected_alpha = unfocused_alpha + (selected_alpha - unfocused_alpha) * key;
    let theme = cx.theme();
    let foreground = theme.sidebar_foreground;
    let warning = theme.warning;
    let (muted, primary, pin_ink) = (theme::ink(ALPHA_SECONDARY, cx), theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_TERTIARY, cx));
    let live_count = motion.slots.iter().filter(|slot| !slot.leaving).count();
    bounds.borrow_mut().resize(live_count, Bounds::default());
    let gap = f32::from(TAB_GAP);
    let widest = f32::from(TAB_MAX_WIDTH) + gap;

    // The strip's visible span in its content, the widest tab wider each side so one scrolling into view is built.
    let (view_left, view_right) = {
        let (view, left) = (f32::from(scroll.bounds().size.width), -f32::from(scroll.offset().x));
        if view > 0. { (left - widest, left + view + widest) } else { (f32::MIN, f32::MAX) }
    };
    // A tab at rest out of sight draws as room alone; its place is still written for drags and reveals.
    let room = |slot: &Slot| {
        !slot.leaving
            && !slot.label.pinned
            && slot.level == 1.
            && slot.offset == 0.
            && slot.lifted == 0.
            && slot.lit_to == 0.
            && slot.breath == 1.
            && slot.lit == 0.
            && slot.mark_level == f32::from(slot.label.marked())
            && slot.share_levels[slot.sign.ix()] == 1.
            && slot.corner_level == f32::from(slot.label.lock_dot())
            && slot.pin_level == f32::from(slot.label.pinned)
            && slot.retitle.is_none()
            && slot.title_level == 1.
            && slot.settle.is_none()
            && slot.wide == slot.natural
            && slot.trail == slot.trail_to()
            && (slot.x + slot.w + gap < view_left || slot.x > view_right)
    };
    // Where the strip's content began when the tabs were last drawn, read off a tab that is still drawn: the places the
    // spacers write line up with the places the drawn tabs wrote.
    let content_left = {
        let places = bounds.borrow();
        motion.slots.iter().filter(|slot| !slot.leaving).enumerate().find(|(ix, slot)| !slot.label.pinned && !room(slot) && slot.offset == 0. && places[*ix].size.width > px(0.)).map(|(ix, slot)| places[ix].origin.x - px(slot.x))
    };
    let strip_top = scroll.bounds().origin.y + TAB_PLATE_ROOM;
    let mut live_ix = 0;
    let in_key = key;
    // The pinned tabs are drawn apart, in a layer held at the strip's left while the rest scroll under it; the flow keeps their room.
    let mut stuck: Vec<Div> = Vec::new();
    let mut tabs: Vec<_> = motion
        .slots
        .iter()
        .map(|slot| {
            let leaving = slot.leaving;
            let ix = live_ix;
            if !leaving {
                live_ix += 1;
            }
            if room(slot) {
                let place = content_left.map(|left| Bounds { origin: point(left + px(slot.x), strip_top), size: size(px(slot.w), TAB_HEIGHT) });
                if let Some((entry, place)) = bounds.borrow_mut().get_mut(ix).zip(place) {
                    *entry = place;
                }
                return div().flex_none().h(TAB_HEIGHT).w(px(slot.w + gap)).children(place.and_then(|place| rect_canvas(format!("tab:{}", ix + 1), place)));
            }
            let TabLabel { id, title, from_line, folder, path, conflict: _, gone, dirty: _, locked: _, preview, pinned, saving: _ } = slot.label.clone();
            let key = id.as_u64() as usize;
            let title: SharedString = title.into();
            let ghost_label = slot.label.clone();
            let record = bounds.clone();
            let lifted = bounds.clone();
            let offset = slot.offset;
            let drift = if slot.lit_to > 0.5 { slot.dir * TAB_TITLE_DRIFT * (1. - slot.lit) } else { 0. };
            // Out of key the selected tab's title and mark ink ease from primary toward secondary with the wash, which `in_key` eases.
            let text = mix(muted, primary, slot.lit * in_key);
            let is_active = slot.lit_to > 0.5;
            // An arriving tab opens its gap first and fades in over the last of it; a closing one
            // folds with its content whole and fades over the last of the fold.
            let fade = if leaving { (slot.level / (1. - TAB_ARRIVE_FADE_FROM)).min(1.) } else { ((slot.level - TAB_ARRIVE_FADE_FROM) / (1. - TAB_ARRIVE_FADE_FROM)).clamp(0., 1.) };
            let lift = slot.lifted;
            let on_close = cx.listener(move |this, _: &ClickEvent, window, cx| this.later(window, cx, move |pane, window, cx| pane.close_id(id, window, cx)));

            // The file's symbol stands in the slot at the left and gives way to the close button under the pointer; a pinned tab
            // keeps its symbol, since it has no close button. After the title, the mark's slot shows the warning, else the lock,
            // else the dot, else the pin. Pinning, unpinning and each mark crossfade over FADE_QUICK, each drawn at its level.
            let (mark_level, corner_level, share_levels, pin_level, breath) = (slot.mark_level, slot.corner_level * slot.breath, slot.share_levels, slot.pin_level, slot.breath);
            let settle = slot.settle.as_ref().map(|settle| (settle.ok, settle.u));
            let close_ink = Ink { rest: text, lit: primary, wash: foreground };
            // The file's symbol is the explorer's, tinted by the tab's ink: always from the primary colour (one bitmap per colour) with the tab's alpha. An Untitled page has no name, which the explorer draws as the plain text symbol.
            let icon_name = if path.is_some() { title.to_string() } else { String::new() };
            let icon_ink = Hsla { a: text.a, ..primary };
            // A saved file's tab hands its file out when its icon slot is dragged; an Untitled page or a deleted file has none to hand.
            let has_file = path.is_some() && !gone;
            let (close_pane, icon_pane) = (cx.weak_entity(), cx.weak_entity());
            let close = by_hover(("tab", key), move |over| {
                let closable = over.min(1.) * (1. - pin_level);
                // The hit is `TAB_CLOSE_HIT` square, laid out over the slot and out each side of it, so the title stays where it is.
                let out = (TAB_CLOSE_HIT - TAB_CLOSE_SLOT) / 2.;
                let button = div()
                    .absolute()
                    .top(-out)
                    .left(-out)
                    .size(TAB_CLOSE_HIT)
                    .opacity(closable)
                    .child(
                        icon_button(("tab-close", key), "xmark", TAB_CLOSE_HIT, TAB_CLOSE_SYMBOL, close_ink)
                            // A press here is the close button's, not the tab's.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .when(!leaving, |close| close.on_click(on_close))
                            // Pressed and dragged instead of clicked, the slot's file leaves as a file (`file_drag`).
                            .when(has_file, |close| file_drag(close, id, close_pane)),
                    )
                    .children((!leaving).then(|| rect_here(format!("tab-close:{}", ix + 1))).flatten());
                let icon = div()
                    .id(("tab-icon", key))
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(1. - closable)
                    .child(icons::file(&icon_name, TAB_ICON, icon_ink))
                    .when(has_file, |icon| file_drag(icon, id, icon_pane));
                div().relative().size(TAB_CLOSE_SLOT).flex_none().child(icon).when(pin_level < 1., |slot| slot.child(button))
            });
            // The mark's slot opens with the tab's width (`Slot::trail`); what it shows is drawn whole at its left and clipped while it opens.
            let trail = (slot.trail > 0.).then(|| {
                let marks = Sign::ALL.into_iter().filter(|sign| mark_level > 0. && share_levels[sign.ix()] > 0.).map(|sign| {
                    let mark = match (sign, settle) {
                        // A save shrinks the dot past nothing on the overshoot; a failed one swells it once, in the warning color.
                        (Sign::Dot, Some((ok, u))) => {
                            let (scale, ink) = if ok { ((1. - overshoot(u)).max(0.), text) } else { (1. + SAVE_SWELL * swell(u), mix(text, warning, swell(u).min(1.))) };
                            div().size(DOT * scale).rounded_full().bg(ink).into_any_element()
                        }
                        _ => sign_mark(sign, warning, text, corner_level),
                    };
                    let ink_level = if sign == Sign::Dot { breath } else { 1. };
                    div().absolute().inset_0().flex().items_center().justify_center().opacity(mark_level * ink_level * share_levels[sign.ix()]).child(mark)
                });
                let pin = (pin_level > 0.).then(|| {
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(pin_ink)
                        .opacity(pin_level * (1. - mark_level))
                        .child(symbols::icon("pin.fill", TAB_CLOSE_SYMBOL))
                });
                div()
                    .flex_none()
                    .w(TAB_TRAIL_SLOT * slot.trail)
                    .h(TAB_CLOSE_SLOT)
                    .overflow_hidden()
                    .child(div().relative().w(TAB_TRAIL_SLOT).h_full().children(marks).children(pin))
            });

            let tab = div()
                .id(("tab", key))
                // The pointer's fill, and the level the close button's swap reads. The tab in front has no wash of its own.
                .fade_hover(("tab", key), None, Hsla { a: if is_active { 0. } else { hover_alpha }, ..foreground })
                .relative()
                .flex_none()
                .w(px(slot.w))
                .h(TAB_HEIGHT)
                .pl(TAB_PAD_LEAD)
                .pr(TAB_PAD_TRAIL)
                .flex()
                .items_center()
                .rounded(RADIUS_TAB)
                // Over the band's window-move hitbox.
                .occlude()
                .text_size(TEXT_BODY)
                .text_color(text)
                .opacity(fade * (1. - lift * (1. - ALPHA_TAB_DRAGGED)))
                // A rest on a tab opens its peek card, which carries the tab's tip as its first lines (`Pane::peek_hover`). A child
                // watches the pointer, since the tab's own hover is the fade's.
                .child(div().id(("tab-peek", key)).absolute().inset_0().on_hover(cx.listener(move |this, over: &bool, window, cx| this.peek_hover(id, *over, window, cx))))
                .when(!leaving, |tab| {
                    tab.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            // A press puts the peek away until the pointer leaves the tab.
                            this.peek_press(id, cx);
                            // Cmd-click shows the file's path like Finder's title menu and leaves the tab as it is; a page with no file selects as usual.
                            if event.modifiers.platform && this.show_path_menu(id, window, cx) {
                                cx.stop_propagation();
                                return;
                            }
                            // A double-click on a preview tab keeps it.
                            let double = event.click_count >= 2;
                            this.later(window, cx, move |pane, window, cx| {
                                pane.activate_id(id, window, cx);
                                if double {
                                    pane.keep_id(id);
                                }
                            })
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| this.later(window, cx, move |pane, window, cx| pane.close_id(id, window, cx))),
                    )
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, window, cx| this.show_tab_menu(id, window, cx)))
                    .on_drag(TabDrag::new(id, pinned), move |drag, grab, window, cx| {
                        let width = lifted.borrow().get(ix).map_or(TAB_DROP_ROOM, |bounds| bounds.size.width);
                        let lifted_at = crate::motion::drawn_now(window.window_handle().window_id());
                        drag.grab.set(grab);
                        drag.width.set(width);
                        drag.lifted_at.set(Some(lifted_at));
                        cx.new(|_| Ghost { label: ghost_label.clone(), width, lifted_at: Some(lifted_at) })
                    })
                })
                // The slot at the left holds the file's symbol, which the close button takes over; the title follows, left-aligned, then the mark's slot.
                .child(close)
                .child({
                    // A renamed tab's old title fades out over the new one as it fades in, drifting the way a switch does.
                    let level = slot.title_level;
                    // A style flip with the same text crossfades in place; only a new text drifts.
                    let drifts = slot.retitle.as_ref().is_none_or(|retitle| retitle.drifts);
                    let shift = if drifts { TAB_TITLE_DRIFT } else { 0. };
                    let old = slot.retitle.as_ref().map(|retitle| (retitle.old.clone(), retitle.old_folder.clone(), retitle.old_preview, retitle.old_gone, retitle.old_from_line));
                    // An unselected tab's title lifts toward primary ink with the pointer, on the wash's own fade.
                    let lifts = 1. - slot.lit;
                    by_hover(("tab", key), move |over| {
                        let text = mix(text, primary, over.min(1.) * lifts);
                        // A save lands with one faint brighten of the title, up and back.
                        let text = match settle {
                            Some((true, u)) => mix(text, primary, SAVE_BRIGHTEN * 4. * u * (1. - u)),
                            _ => text,
                        };
                        let new = title_text(&title, from_line, folder.as_ref(), preview, gone, line_ink(from_line, text)).relative().left(px(drift + shift * (1. - level))).opacity(level);
                        let old = old.map(|(old, old_folder, old_preview, old_gone, old_from_line)| {
                            title_text(&old, old_from_line, old_folder.as_ref(), old_preview, old_gone, line_ink(old_from_line, text)).absolute().inset_0().left(px(drift - shift * level)).opacity(1. - level)
                        });
                        div().flex_1().min_w_0().relative().flex().child(new).children(old).children((!leaving).then(|| rect_here(format!("tab-title:{}", ix + 1))).flatten())
                    })
                })
                .children(trail)
                .when(!leaving, |tab| {
                    tab.child(
                        canvas(
                            move |bounds, window, _| {
                                if let Some(slot) = record.borrow_mut().get_mut(ix) {
                                    *slot = Bounds { origin: point(bounds.origin.x - px(offset), bounds.origin.y), size: bounds.size };
                                }
                                if crate::control::rects_enabled() {
                                    crate::control::record(window, format!("tab:{}", ix + 1), bounds);
                                }
                            },
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                });

            // The slot is as wide as the tab has arrived; the tab keeps its width and is clipped.
            let unit = slot.level * (slot.w + gap);
            let place = div().flex_none().h(TAB_HEIGHT).w(px(unit)).overflow_hidden().relative().left(px(offset)).child(tab);
            if pinned {
                stuck.push(place);
                div().flex_none().h(TAB_HEIGHT).w(px(unit))
            } else {
                place
            }
        })
        .collect();
    // The gap is empty room in the row, before the live tab it sits before.
    if let Some(room) = &motion.gap {
        let at = motion.slots.iter().enumerate().filter(|(_, slot)| !slot.leaving).nth(room.at).map_or(motion.slots.len(), |(ix, _)| ix);
        tabs.insert(at, div().flex_none().h(TAB_HEIGHT).w(px(room.level * (f32::from(TAB_DROP_ROOM) + gap))));
    }

    // The mark is the strip's first child, behind the tabs: a plate the active tab stands on, with a contact shadow and a rim,
    // and on a dark theme a lit top, the float plane's depth at tab size. On a light theme it is white on the grey band. Out
    // of key the shadow and lit top ease down to `TAB_PLATE_UNFOCUSED` with `key`, and on a dark theme the fill with them.
    let flat = TAB_PLATE_UNFOCUSED + (1. - TAB_PLATE_UNFOCUSED) * key;
    let show = 1. - motion.hidden;
    let mark_shape = || {
        let fill = if light { gpui_kit::white().opacity(show) } else { Hsla { a: selected_alpha * show, ..foreground } };
        let ([shadow, drop, blur], rim) = if light { (TAB_PLATE_SHADOW_LIGHT, TAB_PLATE_RIM_LIGHT) } else { (TAB_PLATE_SHADOW, TAB_PLATE_RIM) };
        let shadows = vec![
            BoxShadow { color: gpui_kit::black().opacity(shadow * flat * show), offset: point(px(0.), px(drop)), blur_radius: px(blur), spread_radius: px(0.), inset: false },
            BoxShadow { color: gpui_kit::black().opacity(rim * show), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(0.5), inset: false },
        ];
        div()
            .absolute()
            .top(TAB_PLATE_ROOM)
            .h(TAB_HEIGHT)
            .rounded(RADIUS_TAB)
            .bg(fill)
            .shadow(shadows)
            .left(px(motion.mark.at[0]))
            .w(px(if motion.mark.live { motion.mark.at[1] } else { 0. }))
            .children((!light && motion.mark.live).then(|| tooltip::top_highlight(f32::from(RADIUS_TAB)).opacity(flat * show)))
    };
    let mark = mark_shape();
    // The pinned tabs' layer sits `held` points in from the strip's content, which is how far the strip is scrolled, so they stay where they are
    // while the rest scroll under it. Once scrolled it is opaque, with its own copy of the mark (the strip's is under it).
    let held = (-f32::from(scroll.offset().x)).max(0.);
    let pinned_width: f32 = if stuck.is_empty() { 0. } else { f32::from(TAB_PLATE_ROOM) + motion.slots.iter().filter(|slot| slot.label.pinned).map(|slot| slot.level * (slot.w + gap)).sum::<f32>() };
    let band_ground = crate::tokens::tab_band_ground(light);
    let pinned_layer = (!stuck.is_empty()).then(|| {
        div()
            .absolute()
            .top_0()
            .left(px(held))
            .w(px(pinned_width))
            .h(TAB_HEIGHT + TAB_PLATE_ROOM * 2.)
            .py(TAB_PLATE_ROOM)
            .pl(TAB_PLATE_ROOM)
            .flex()
            .overflow_hidden()
            .when(held > 0., |layer| layer.bg(band_ground).child(mark_shape()))
            .children(stuck)
    });

    // A dropped tab's copy springs from where it was let go into its slot (where the slot was laid
    // out last frame), its shadow easing out, then crossfades with the slot as that comes back.
    let landing = motion.landing.as_ref().and_then(|landing| {
        let (ix, slot) = motion.slots.iter().filter(|slot| !slot.leaving).enumerate().find(|(_, slot)| slot.label.id == landing.id)?;
        // A tab that just arrived has no place from last frame: the copy stays where it was let go.
        let to = bounds.borrow().get(ix).copied().unwrap_or(landing.from);
        let home = landing.home;
        let toward = |from: Pixels, to: Pixels| from + (to - from) * home;
        let origin = point(px(chip_axis(landing, to.origin.x, landing.from.origin.x, 0).0), px(chip_axis(landing, to.origin.y, landing.from.origin.y, 1).0));
        let width = toward(landing.from.size.width, to.size.width);
        let copy = card(&slot.label, width, landing.lift * (1. - home), cx).opacity(if home < 1. { 1. } else { slot.lifted }).children(rect_here("tab-landing".into()));
        Some(deferred(anchored().position(origin).child(copy)).with_priority(LAYER_TAB_LANDING))
    });

    // The tab list button shows while the strip's content is wider than its view, both as this frame lays them out. The strip
    // is already as much narrower as the room the button took in the frame last drawn, so the content overflows by that much
    // more before it is needed. The room is the button's level of its width, so the tabs ease narrower as it fades in.
    let view = motion.width.get();
    let overflow = view > 0. && motion.content - view > motion.list_reserved.get() + 0.5;
    // The first frame that knows the strip's width shows the button as it is; every later change fades.
    if view > 0. && !motion.list_measured.replace(true) && overflow {
        motion.list_level.set(1.);
    }
    let level = motion.list_level.get();
    let reserved = level * f32::from(ICON_BUTTON + TAB_LIST_GAP);
    motion.list_reserved.set(reserved);
    // The level springs toward the answer from the next frame; a changed answer asks for it.
    let flipped = overflow != motion.list_shown.replace(overflow);

    // The strip's width as laid out, for the next frame's resize: a canvas after the strip, so the strip has
    // measured itself. This frame's layout used the width carried from last frame's; a miss asks for one frame to re-lay the tabs.
    let used = motion.width.clone();
    let shrunk = motion.shrunk.clone();
    let carry = motion.carry.clone();
    let scrolled = scroll.clone();
    let width = canvas(
        move |_, window, _| {
            if measured(&used, &shrunk, &carry, f32::from(scrolled.bounds().size.width), flipped) {
                window.request_animation_frame();
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_full();

    // The strip fades into the band where more tabs lie past its end, instead of clipping them hard.
    let (fade_scroll, ground) = (scroll.clone(), band_ground);
    let one_tab = motion.slots.iter().filter(|slot| !slot.leaving).map(|slot| slot.w).fold(0., f32::max);
    let edges = canvas(
        move |_, _, _| (),
        move |_, (), window, _| {
            let (view, at, max) = (fade_scroll.bounds(), -f32::from(fade_scroll.offset().x), f32::from(fade_scroll.max_offset().x));
            // The pinned tabs hold the strip's left: its left fade starts at their right edge.
            let beyond = Bounds { origin: point(view.origin.x + px(pinned_width), view.origin.y), size: size((view.size.width - px(pinned_width)).max(px(0.)), view.size.height) };
            // A strip with little room beside one tab (a pane at its least) weakens the fades with the room, so the tab stays whole.
            let room = ((f32::from(beyond.size.width) - one_tab) / (f32::from(LIST_EDGE_FADE) * 2.)).clamp(0., 1.);
            paint_fade(beyond, (at / f32::from(LIST_EDGE_FADE)).clamp(0., 1.) * room, ((max - at) / f32::from(LIST_EDGE_FADE)).clamp(0., 1.) * room, ground, window);
        },
    )
    .absolute()
    .size_full();

    let list_button = (overflow || level > 0.).then(|| {
        let badge = motion.badge_level;
        let place = motion.list_place.clone();
        // Out of key its rest ink eases down toward ALPHA_STATUS_DIM with `key`, as the window's own buttons do.
        let ink = Ink { rest: theme::ink(ALPHA_STATUS_DIM + (ALPHA_SECONDARY - ALPHA_STATUS_DIM) * key, cx), lit: primary, wash: foreground };
        // The button fades in as its room opens: the box is its level of the button's width, clipping the button where it is still opening.
        div().flex_none().w(px(reserved)).overflow_hidden().opacity(level).child(
            div().flex_none().pl(TAB_LIST_GAP).child(
                icon_button_weighted("tab-list", "chevron.down", ICON_BUTTON, TAB_LIST_SYMBOL, symbols::SEMIBOLD, ink)
                    // Over the band's window-move hitbox.
                    .occlude()
                    .tooltip(tooltip::tip("Show all tabs"))
                    // Its menu is open: the pressed look stays until the menu closes, then fades back.
                    .held(motion.list_held.get())
                    .tooltip_show_delay(tooltip::DELAY)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.later(window, cx, |pane, window, cx| {
                            pane.show_tab_list(window, cx);
                        })
                    }))
                    .child(canvas(move |bounds, _, _| place.set(bounds), |_, (), _, _| {}).absolute().size_full())
                    // A tab hidden past the strip has a dot (or a warning): its dot at the button's top right, like a badge.
                    .children((badge > 0.).then(|| div().absolute().top(TAB_LIST_BADGE_INSET).right(TAB_LIST_BADGE_INSET).size(DOT).rounded_full().bg(primary).opacity(badge)))
                    .children(rect_here("tab-list".into())),
            ),
        )
    });

    // The peek card a rest on a tab opens, on the tooltip's layer; it glides to the tab the pointer moves to.
    let peek = motion.peek.as_ref().filter(|peek| peek.level > 0.).map(|peek| peek_card(peek, light, cx));

    // Right of the explorer, clear of the window's preview button where it sits over the bar.
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(band)
        .flex()
        .items_center()
        // The strip starts `TAB_PLATE_ROOM` early, since its tabs start that far into it.
        .pl((left_inset - TAB_PLATE_ROOM).max(px(0.)))
        // Room for the window's preview button at the right edge only where it sits over this bar.
        .pr(right)
        // A light theme's band is a step under the page, so the active tab's white plate reads on it.
        .when(light, |bar| bar.bg(band_ground))
        .children(crate::control::fresh("tab-list"))
        .child(
            div()
                .id("tab-strip")
                .relative()
                .flex()
                .flex_1()
                .min_w_0()
                .overflow_x_scroll()
                .track_scroll(scroll)
                // Room for the plate's shadow and rim inside the scroll's clip.
                .py(TAB_PLATE_ROOM)
                // The hand wins: a wheel or trackpad scroll stops a glide at once, and the badge on the tab list button follows the view it lands in.
                .on_scroll(cx.listener(|this, _, _, cx| {
                    this.hand_scrolled();
                    cx.notify();
                }))
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<TabDrag>, window, cx| {
                    let drag = event.drag(cx);
                    let (id, pinned, card) = (drag.id, drag.pinned, drag.ghost(Point::default(), Instant::now()).0);
                    this.drag_over(id, pinned, event.bounds, event.event.position, cx);
                    this.fold_over(id, card, event.bounds, event.event.position, window.viewport_size(), cx);
                }))
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<DraggedFiles>, _, cx| this.gap_over(false, event.bounds, event.event.position, cx)))
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| this.gap_over(false, event.bounds, event.event.position, cx)))
                .on_drop(cx.listener(|this, drag: &TabDrag, window, cx| {
                    let pointer = window.mouse_position();
                    let now = crate::motion::drawn_now(window.window_handle().window_id());
                    let (from, lift) = drag.ghost(pointer, now);
                    this.land(drag.id, from, lift, now, cx);
                    this.drop_on_bar(Dropped::Tab(drag.id), pointer.x, cx)
                }))
                .on_drop(cx.listener(|this, files: &DraggedFiles, window, cx| this.drop_on_bar(Dropped::Files(files.0.to_vec()), window.mouse_position().x, cx)))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    this.drop_on_bar(Dropped::Files(paths.paths().to_vec()), window.mouse_position().x, cx)
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
                .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
                .children(crate::control::fresh("tab:"))
                .children(crate::control::fresh("tab-close:"))
                .children(crate::control::fresh("tab-title:"))
                .child(mark)
                // The room at each end the layout keeps clear for the plate's rim and shadow.
                .child(div().flex_none().w(TAB_PLATE_ROOM))
                .children(tabs)
                .child(div().flex_none().w(TAB_PLATE_ROOM))
                .children(pinned_layer),
        )
        // The tab list button, at the strip's right end while the strip overflows.
        .children(list_button)
        .child(width)
        .child(edges)
        // The landing copy's rect goes with the copy: forgotten each build, recorded again while it draws.
        .children(crate::control::fresh("tab-landing"))
        .children(landing)
        .children(peek)
}

/// The strip's two ends fading into `ground` over `LIST_EDGE_FADE`, each by its strength (1 while
/// a full fade of tabs lies past that end, less as the last of them scrolls into view).
fn paint_fade(bounds: Bounds<Pixels>, left: f32, right: f32, ground: Hsla, window: &mut Window) {
    crate::scrollbar::paint_ramp(bounds, true, true, 0., left, ground, window);
    crate::scrollbar::paint_ramp(bounds, true, false, 0., right, ground, window);
}

#[cfg(test)]
mod tests {
    use gpui_kit::EntityId;

    use std::{collections::HashMap, path::Path};

    use super::{is_tip_path, is_untitled_title, number_untitled, tab_tip, tip_path, untitled_title};

    fn page(n: u64) -> EntityId {
        EntityId::from(n)
    }

    fn numbers(held: &HashMap<EntityId, u32>, pages: &[u64]) -> Vec<u32> {
        pages.iter().map(|n| held[&page(*n)]).collect()
    }

    fn label(dirty: bool, conflict: bool) -> super::TabLabel {
        super::TabLabel { id: page(1), title: "a.txt".into(), from_line: false, folder: None, path: None, dirty, conflict, gone: false, locked: false, preview: false, pinned: false, saving: None }
    }

    #[test]
    fn a_sign_swap_crossfades_and_turns_around_when_it_flips_back() {
        use super::Sign;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut motion = super::Motion::default();
        motion.sync(vec![label(true, false)], 0);
        assert!(!motion.advance(at(0), None));
        // The file changes: the dot fades under the new sign.
        motion.sync(vec![label(true, true)], 0);
        assert!(motion.advance(at(0), None));
        let [warning, _, dot] = motion.slots[0].share_levels;
        assert!(warning < 1e-3 && dot > 1. - 1e-3);
        assert!(motion.advance(at(60), None));
        let drawn = motion.slots[0].share_levels[Sign::Warning.ix()];
        assert!(drawn > 0. && drawn < 1.);
        // Flipping back turns around from where it is drawn: the dot keeps the level it had.
        motion.sync(vec![label(true, false)], 0);
        assert!(motion.advance(at(60), None));
        assert!((motion.slots[0].share_levels[Sign::Dot.ix()] - (1. - drawn)).abs() < 1e-6);
        assert!(motion.advance(at(120), None));
        // It lands and the frames stop.
        assert!(!motion.advance(at(400), None));
        assert_eq!(motion.slots[0].share_levels, [0., 0., 1.]);
    }

    #[test]
    fn a_style_flip_with_the_same_text_starts_a_retitle_carrying_the_old_style() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let tab = |preview, gone| super::TabLabel { preview, gone, ..label(false, false) };
        let mut motion = super::Motion::default();
        motion.sync(vec![tab(true, false)], 0);
        assert!(!motion.advance(at(0), None));
        // Keeping the preview tab: the title stays, the italic crossfades out in place.
        motion.sync(vec![tab(false, false)], 0);
        let retitle = motion.slots[0].retitle.as_ref().expect("a retitle starts");
        assert!(retitle.old_preview && !retitle.old_gone && !retitle.drifts);
        assert_eq!(retitle.old, "a.txt");
        assert!(motion.advance(at(0), None));
        assert!(motion.advance(at(60), None));
        let mid = motion.slots[0].title_level;
        assert!(mid > 0. && mid < 1.);
        let mut ms = 60;
        while motion.advance(at(ms), None) {
            ms += 16;
            assert!(ms < 1000, "never lands");
        }
        assert!(motion.slots[0].retitle.is_none() && motion.slots[0].title_level == 1.);
        // The file is deleted under its edits: the strikethrough fades in, from the roman style.
        motion.sync(vec![tab(false, true)], 0);
        let retitle = motion.slots[0].retitle.as_ref().expect("a retitle starts");
        assert!(!retitle.old_preview && !retitle.old_gone && !retitle.drifts);
        // A rename still drifts.
        motion.sync(vec![super::TabLabel { title: "b.txt".into(), ..tab(false, true) }], 0);
        assert!(motion.slots[0].retitle.as_ref().is_some_and(|retitle| retitle.drifts && retitle.old_gone));
    }

    #[test]
    fn pinning_crossfades_the_pin_and_turns_around_from_the_drawn_level() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let pinned = |pinned| super::TabLabel { pinned, ..label(false, false) };
        let mut motion = super::Motion::default();
        motion.sync(vec![pinned(false)], 0);
        assert!(!motion.advance(at(0), None));
        // Pinning: the pin starts from nothing and is part way after 60 ms.
        motion.sync(vec![pinned(true)], 0);
        assert!(motion.advance(at(0), None));
        assert!(motion.slots[0].pin_level < 1e-3);
        assert!(motion.advance(at(60), None));
        let drawn = motion.slots[0].pin_level;
        assert!(drawn > 0. && drawn < 1.);
        // Unpinning turns around from where it is drawn.
        motion.sync(vec![pinned(false)], 0);
        assert!(motion.advance(at(60), None));
        assert!((motion.slots[0].pin_level - drawn).abs() < 1e-6);
        assert!(motion.advance(at(120), None));
        assert!(motion.slots[0].pin_level < drawn);
        // It lands on the close button and the frames stop.
        assert!(!motion.advance(at(400), None));
        assert_eq!(motion.slots[0].pin_level, 0.);
    }

    #[test]
    fn the_tab_list_level_climbs_turns_around_and_lands() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut motion = super::Motion::default();
        motion.sync(labels(2), 0);
        assert!(!motion.advance(at(0), None));
        // The tabs overflow: the level climbs from nothing by steps well under a whole button.
        motion.list_shown.set(true);
        assert!(motion.advance(at(0), None));
        assert_eq!(motion.list_level.get(), 0.);
        let mut last = 0.;
        for ms in (16..=64).step_by(16) {
            assert!(motion.advance(at(ms), None));
            let level = motion.list_level.get();
            assert!(level > last && level - last < 0.5, "{last} then {level}");
            last = level;
        }
        // They stop overflowing mid-climb: it turns around from where it is drawn and keeps its speed.
        motion.list_shown.set(false);
        assert!(motion.advance(at(64), None));
        assert_eq!(motion.list_level.get(), last);
        assert!(motion.advance(at(80), None));
        assert!(motion.list_level.get() < last + 0.2);
        let mut ms = 80;
        while motion.advance(at(ms), None) {
            ms += 16;
            assert!(ms < 1000, "never lands");
        }
        assert_eq!(motion.list_level.get(), 0.);
    }

    #[test]
    fn the_list_badge_follows_the_settled_view_and_fades_on_the_tab_dots_fade() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut motion = super::Motion::default();
        let mut tabs = labels(30);
        tabs[24].dirty = true;
        motion.sync(tabs, 0);
        motion.width.set(560.);
        motion.advance(at(0), None);
        // A dirty tab far past the strip's end: the badge fades in from nothing, from its first frame.
        assert!(motion.advance_badge(0., at(0)));
        assert!(motion.badge_level < 1e-3);
        assert!(motion.advance_badge(0., at(60)));
        let drawn = motion.badge_level;
        assert!(drawn > 0. && drawn < 1.);
        assert!(!motion.advance_badge(0., at(1000)));
        assert_eq!(motion.badge_level, 1.);
        // The strip settles on that tab: the badge turns around from where it is drawn.
        let end = motion.slots[24].x - 100.;
        assert!(motion.advance_badge(end, at(1000)));
        assert_eq!(motion.badge_level, 1.);
        assert!(motion.advance_badge(end, at(1060)));
        assert!(motion.badge_level < 1. && motion.badge_level > 0.);
        assert!(!motion.advance_badge(end, at(2000)));
        assert_eq!(motion.badge_level, 0.);
    }

    #[test]
    fn the_first_measured_frame_shows_the_tab_list_button_without_a_fade() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut motion = super::Motion::default();
        motion.sync(labels(2), 0);
        // What the bar's first frame with a width does when the tabs overflow.
        motion.list_shown.set(true);
        assert!(!motion.list_measured.replace(true));
        motion.list_level.set(1.);
        assert!(!motion.advance(t0 + Duration::from_millis(16), None));
        assert_eq!(motion.list_level.get(), 1.);
    }

    fn labels(count: u64) -> Vec<super::TabLabel> {
        (1..=count).map(|n| super::TabLabel { id: page(n), title: format!("{n}.txt").into(), ..label(false, false) }).collect()
    }

    /// Gives every tab `natural` points of content, as `Motion::measure` would with a window to shape titles in.
    fn fit_tabs(motion: &mut super::Motion, natural: f32) {
        for slot in &mut motion.slots {
            (slot.natural, slot.measured) = (natural, true);
        }
    }

    #[test]
    fn a_shrinking_window_lays_the_tabs_in_this_frames_width() {
        use std::time::Instant;
        let t0 = Instant::now();
        let mut motion = super::Motion::default();
        motion.sync(labels(6), 0);
        // The strip was measured 800 wide in a 1000 wide window.
        motion.fit(1000.);
        motion.width.set(800.);
        motion.advance(t0, None);
        let mut last = motion.content;
        for window in (700..1000).rev().step_by(50) {
            motion.fit(window as f32);
            motion.advance(t0, None);
            // The layout narrows in the same frame the window does, so the content never overflows what it was.
            assert_eq!(motion.width.get(), window as f32 - 200.);
            assert!(motion.content <= last + 1e-3, "{} then {}", last, motion.content);
            last = motion.content;
        }
    }

    #[test]
    fn a_strip_that_gets_narrower_brings_the_active_tab_back_into_view() {
        let mut motion = super::Motion::default();
        motion.sync(labels(20), 13);
        fit_tabs(&mut motion, 146.);
        // 20 tabs of 146 (150 with the gap) in a strip 1200 wide, drawn 1100 in: the 14th tab (1953 to 2099, after the 3 point room at the
        // strip's start) lies clear of both fades.
        motion.fit(1400.);
        motion.width.set(1200.);
        motion.fit(1400.);
        assert_eq!(motion.take_reveal(1100.), None);
        // A window 300 narrower takes the tab out from under the strip's right edge; the strip asks for it back.
        motion.fit(1100.);
        assert_eq!(motion.width.get(), 900.);
        assert_eq!(motion.take_reveal(1100.), Some(2099. + 24. - 900.));
        // The same window again asks for nothing, and a window that grows leaves the strip where it is.
        motion.fit(1100.);
        assert_eq!(motion.take_reveal(1223.), None);
        motion.fit(1400.);
        assert_eq!(motion.take_reveal(1223.), None);
        // A strip the width canvas found narrower than the layout used asks too.
        motion.shrunk.set(true);
        motion.fit(1400.);
        assert!(motion.reveal);
    }

    #[test]
    fn a_tab_fits_its_content_and_a_narrow_strip_shrinks_it_to_the_room_and_reveals_it_whole() {
        // A tab is as wide as its content, the mark's slot added while it has one, up to the most a tab is.
        assert_eq!(super::wanted_width(150., 0.), 150.);
        assert_eq!(super::wanted_width(150., 1.), 150. + f32::from(crate::tokens::TAB_TRAIL_SLOT));
        assert_eq!(super::wanted_width(400., 1.), f32::from(crate::tokens::TAB_MAX_WIDTH));
        // Room for it: its own width. Less room: the room, but never under the floor, and a tab narrower than the floor keeps its own.
        assert_eq!(super::fitted_width(150., 1200.), 150.);
        assert_eq!(super::fitted_width(150., 130.), 130.);
        assert_eq!(super::fitted_width(150., 40.), f32::from(crate::tokens::TAB_FLOOR_WIDTH));
        assert_eq!(super::fitted_width(90., 40.), 90.);
        // A strip 120 wide with three unmeasured tabs (120 each): each is fitted to 116 (120 with its gap), and the third comes into view with its edges 2 points clear.
        let mut motion = super::Motion::default();
        motion.sync(labels(3), 2);
        motion.fit(300.);
        motion.width.set(120.);
        motion.fit(300.);
        motion.reveal();
        assert_eq!(motion.take_reveal(0.), Some(241.));
    }

    #[test]
    fn a_reveal_brings_a_tab_clear_of_the_pinned_ones_and_leaves_a_pinned_tab_alone() {
        let mut motion = super::Motion::default();
        let mut tabs = labels(20);
        tabs[0].pinned = true;
        tabs[1].pinned = true;
        // Tab 3 (index 2) is the first that scrolls: drawn 300 in it lies under the two pinned tabs (300 held), so the strip comes back to the start.
        motion.sync(tabs.clone(), 2);
        motion.fit(1400.);
        motion.width.set(1200.);
        motion.fit(1400.);
        motion.reveal();
        assert_eq!(motion.take_reveal(300.), Some(0.));
        // A pinned tab is in view wherever the strip is scrolled.
        motion.sync(tabs, 1);
        motion.reveal();
        assert_eq!(motion.take_reveal(300.), None);
    }

    #[test]
    fn a_pane_beside_another_learns_its_share_of_a_resize_in_one_correction() {
        // Two panes side by side: the strip is half the window less 200 points of chrome, so it follows half of a resize.
        let corrections = |learn: bool, step: usize| {
            let mut motion = super::Motion::default();
            motion.sync(labels(6), 0);
            motion.fit(1000.);
            motion.width.set(400.);
            let mut count = 0;
            for window in (500..=990).rev().step_by(step) {
                motion.fit(window as f32);
                if !learn {
                    motion.carry.delta.set(0.);
                }
                let strip = (window as f32 - 200.) / 2.;
                if super::measured(&motion.width, &motion.shrunk, &motion.carry, strip, false) {
                    count += 1;
                }
                // Misses under half a point are left, so the strip may be that far off while it builds up.
                assert!((motion.width.get() - strip).abs() <= 0.5, "{} against {}", motion.width.get(), strip);
            }
            count
        };
        // Without the share every frame of a 10 point drag lays the tabs out twice; with it only the first does.
        assert_eq!(corrections(false, 10), 50);
        assert_eq!(corrections(true, 10), 1);
        // A slow drag, a point a frame, builds its miss up over two frames: the share is learned from both, not from the last.
        assert_eq!(corrections(false, 1), 246);
        assert_eq!(corrections(true, 1), 1);
    }

    #[test]
    fn a_frame_with_the_same_tabs_is_known_unchanged() {
        use std::time::Instant;
        let mut motion = super::Motion::default();
        let tabs = labels(50);
        assert!(!motion.unchanged(Some(page(1)), 50, |_, _| true));
        motion.sync(tabs.clone(), 0);
        let mut looked = 0;
        assert!(motion.unchanged(Some(page(1)), 50, |ix, label| {
            looked += 1;
            *label == tabs[ix]
        }));
        assert_eq!(looked, 50);
        assert!(!motion.unchanged(Some(page(2)), 50, |_, _| true));
        assert!(!motion.unchanged(Some(page(1)), 49, |_, _| true));
        assert!(!motion.unchanged(Some(page(1)), 50, |ix, _| ix != 7));
        motion.advance(Instant::now(), None);
    }

    #[test]
    fn a_second_swap_mid_fade_keeps_every_sign_where_it_is_drawn() {
        use super::Sign;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut motion = super::Motion::default();
        motion.sync(vec![label(true, false)], 0);
        assert!(!motion.advance(at(0), None));
        // Dot to lock, then the lock to the warning 100 ms in, while the dot is still going out.
        motion.sync(vec![super::TabLabel { locked: true, ..label(true, false) }], 0);
        assert!(motion.advance(at(0), None));
        assert!(motion.advance(at(100), None));
        let before = motion.slots[0].share_levels;
        assert!(before[Sign::Dot.ix()] > 0. && before[Sign::Lock.ix()] < 1.);
        motion.sync(vec![super::TabLabel { locked: true, ..label(true, true) }], 0);
        assert!(motion.advance(at(100), None));
        // The frame of the second change draws what the last frame drew, so nothing jumps.
        let now = motion.slots[0].share_levels;
        assert!(now[Sign::Warning.ix()] < 1e-3 && (now[Sign::Lock.ix()] - before[Sign::Lock.ix()]).abs() < 1e-6 && (now[Sign::Dot.ix()] - before[Sign::Dot.ix()]).abs() < 1e-6);
        assert!(motion.advance(at(160), None));
        let mid = motion.slots[0].share_levels;
        assert!(mid[Sign::Warning.ix()] > 0. && mid[Sign::Lock.ix()] < before[Sign::Lock.ix()] && mid[Sign::Dot.ix()] < before[Sign::Dot.ix()]);
        // It lands on the warning alone and the frames stop.
        assert!(!motion.advance(at(600), None));
        assert_eq!(motion.slots[0].share_levels, [1., 0., 0.]);
    }

    #[test]
    fn a_slow_save_breathes_the_dot_then_the_landing_fades_it_from_where_it_is_drawn_and_frames_stop() {
        use super::{breathe, SAVE_BREATH_LOW, SAVE_BREATH_PERIOD, SAVE_SLOW};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // The curve: full at the start, the low at half a period, full again after one, and never past either.
        assert_eq!(breathe(-1.), 1.);
        assert!((breathe(0.) - 1.).abs() < 1e-6);
        assert!((breathe(SAVE_BREATH_PERIOD / 2.) - SAVE_BREATH_LOW).abs() < 1e-6);
        assert!((breathe(SAVE_BREATH_PERIOD) - 1.).abs() < 1e-6);
        assert!((0..=240).all(|i| (SAVE_BREATH_LOW - 1e-6..=1. + 1e-6).contains(&breathe(i as f32 * 0.01))));
        // A fast save never sets `saving`: a dirty tab at rest asks for no frames and the dot stays full.
        let mut motion = super::Motion::default();
        motion.sync(vec![label(true, false)], 0);
        assert!(!motion.advance(at(0), None));
        assert!(!motion.advance(at(300), None));
        assert_eq!(motion.slots[0].breath, 1.);
        // The save is still writing once it turns slow: the dot starts full and breathes, asking for a frame each time.
        let slow = |saving| super::TabLabel { saving, ..label(true, false) };
        motion.sync(vec![slow(Some(at(0)))], 0);
        let began = (SAVE_SLOW * 1000.) as u64;
        let mut series = Vec::new();
        for ms in (began..=began + 1800).step_by(150) {
            assert!(motion.advance(at(ms), None));
            series.push(motion.slots[0].breath);
        }
        assert_eq!(series[0], 1.);
        assert!(series.iter().any(|&alpha| alpha < 0.6) && series.iter().all(|&alpha| alpha >= SAVE_BREATH_LOW - 1e-6));
        // It swings down and back up: not monotone.
        assert!(series.windows(2).any(|pair| pair[1] < pair[0]) && series.windows(2).any(|pair| pair[1] > pair[0]));
        // It lands mid-breath: the save's own settle takes over from the alpha drawn then.
        let land = began + 1800;
        assert!(motion.advance(at(land), None));
        let drawn = motion.slots[0].breath;
        assert!(drawn < 0.5);
        motion.sync(vec![label(false, false)], 0);
        motion.settle(page(1), true, 0.);
        assert!(motion.advance(at(land), None));
        assert_eq!(motion.slots[0].breath, drawn);
        assert!((motion.slots[0].mark_level * motion.slots[0].breath - drawn).abs() < 1e-3);
        assert!(motion.advance(at(land + 90), None));
        assert!(motion.slots[0].mark_level * motion.slots[0].breath < drawn);
        // Frames stop once the settle lands, with the dot full for the next edit.
        assert!(!motion.advance(at(land + 600), None));
        assert_eq!(motion.slots[0].breath, 1.);
        assert_eq!(motion.slots[0].mark_level, 0.);
        // A save that fails keeps the dot: its ink eases back to full from where it is drawn, then the frames stop.
        let mut motion = super::Motion::default();
        motion.sync(vec![slow(Some(at(0)))], 0);
        assert!(motion.advance(at(began + 600), None));
        assert!((motion.slots[0].breath - SAVE_BREATH_LOW).abs() < 1e-6);
        motion.sync(vec![label(true, false)], 0);
        assert!(motion.advance(at(began + 650), None));
        let back = motion.slots[0].breath;
        assert!(back > SAVE_BREATH_LOW && back < 1.);
        let mut ms = began + 650;
        while motion.advance(at(ms), None) {
            ms += 16;
            assert!(ms < began + 5000);
        }
        assert_eq!(motion.slots[0].breath, 1.);
    }

    #[test]
    fn a_copy_waits_for_the_tab_it_was_let_go_with_then_glides_and_stops() {
        use super::gpui::{point, px, size, Bounds};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut motion = super::Motion::default();
        motion.sync(vec![label(false, false)], 0);
        motion.advance(at(0), None);
        let from = Bounds::new(point(px(300.), px(10.)), size(px(120.), px(28.)));
        // Let go with a tab from another pane: nothing homes until its slot is here.
        let other = page(2);
        motion.land(other, from, 1., [0.; 2], at(0), true);
        assert!(motion.advance(at(16), None));
        assert!(motion.landing.as_ref().is_some_and(|landing| landing.waiting && landing.home == 0.));
        // The slot arrives: the glide starts from that frame and the slot sits lifted under the copy.
        let mut arrived = label(false, false);
        arrived.id = other;
        motion.sync(vec![label(false, false), arrived], 0);
        assert!(motion.advance(at(32), None));
        let landing = motion.landing.as_ref().unwrap();
        assert!(!landing.waiting && landing.home == 0.);
        assert_eq!(motion.slots[1].lifted, 1.);
        assert!(motion.advance(at(80), None));
        assert!(motion.landing.as_ref().unwrap().home > 0.);
        // It lands, crossfades and the frames stop.
        let mut ms = 80;
        while motion.advance(at(ms), None) && ms < 5000 {
            ms += 16;
        }
        assert!(motion.landing.is_none() && ms < 2000);
        // A tab that never arrives is dropped after the bound.
        motion.land(page(3), from, 1., [0.; 2], at(ms), true);
        assert!(motion.advance(at(ms + 100), None));
        assert!(!motion.advance(at(ms + 400), None) && motion.landing.is_none());
    }

    #[test]
    fn a_second_carry_starts_where_the_first_copy_is_drawn_at_its_pace() {
        use super::gpui::{point, px, size, Bounds};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut motion = super::Motion::default();
        let id = label(false, false).id;
        motion.sync(vec![label(false, false)], 0);
        motion.advance(at(0), None);
        let from = Bounds::new(point(px(300.), px(10.)), size(px(120.), px(28.)));
        let slot = Bounds::new(point(px(20.), px(10.)), size(px(120.), px(28.)));
        motion.land(id, from, 0., [0.; 2], at(0), false);
        motion.advance(at(16), None);
        motion.advance(at(80), None);
        // Mid-glide the copy is drawn between the two and is moving toward the slot.
        let (drawn, _, speed) = motion.chip(id, slot).unwrap();
        assert!(drawn.origin.x < from.origin.x && drawn.origin.x > slot.origin.x && speed[0] < 0.);
        // The same copy at the same instant, started afresh from there with that speed, is drawn in the same place.
        let mut next = super::Motion::default();
        next.sync(vec![label(false, false)], 0);
        next.advance(at(0), None);
        next.land(id, drawn, 0., speed, at(80), false);
        next.advance(at(80), None);
        let (again, _, pace) = next.chip(id, slot).unwrap();
        assert!(f32::from((again.origin.x - drawn.origin.x).abs()) < 0.01 && (pace[0] - speed[0]).abs() < 0.5);
    }

    #[test]
    fn untitled_pages_are_numbered_in_the_order_they_were_made() {
        let mut held = HashMap::new();
        number_untitled(&mut held, [page(1)].into_iter());
        assert_eq!(numbers(&held, &[1]), [1]);
        number_untitled(&mut held, [page(1), page(2), page(3)].into_iter());
        assert_eq!(numbers(&held, &[1, 2, 3]), [1, 2, 3]);
    }

    #[test]
    fn a_number_stays_with_its_page_and_a_closed_one_is_taken_again() {
        let mut held = HashMap::new();
        number_untitled(&mut held, [page(1), page(2), page(3)].into_iter());
        // The first page closes: the others keep their numbers, even the one that is now alone first.
        number_untitled(&mut held, [page(2), page(3)].into_iter());
        assert_eq!(held.len(), 2);
        assert_eq!(numbers(&held, &[2, 3]), [2, 3]);
        // A new page takes the lowest number nobody holds.
        number_untitled(&mut held, [page(2), page(3), page(4)].into_iter());
        assert_eq!(numbers(&held, &[2, 3, 4]), [2, 3, 1]);
        // A page reordered in the bar keeps its number.
        number_untitled(&mut held, [page(4), page(3), page(2)].into_iter());
        assert_eq!(numbers(&held, &[2, 3, 4]), [2, 3, 1]);
    }

    #[test]
    fn the_title_says_the_number_and_is_recognised_without_building_it() {
        assert_eq!(untitled_title(1), "Untitled");
        assert_eq!(untitled_title(2), "Untitled 2");
        assert!(is_untitled_title("Untitled", 1));
        assert!(is_untitled_title("Untitled 12", 12));
        assert!(!is_untitled_title("Untitled 2", 3));
        assert!(!is_untitled_title("Untitled", 2));
        assert!(!is_untitled_title("Untitled 1", 1));
        assert!(!is_untitled_title("notes.txt", 1));
    }

    #[test]
    fn a_tip_names_the_path_and_what_the_tab_hides() {
        let home = Some(Path::new("/Users/h"));
        let path = tip_path(Path::new("/Users/h/code/mod.rs"), home);
        assert_eq!(path, "~/code/mod.rs");
        assert_eq!(tab_tip("mod.rs", Some(&path), false, false, false, false, false), "~/code/mod.rs");
        assert_eq!(tab_tip("mod.rs", Some(&path), false, false, true, false, false), "~/code/mod.rs\nRead-only");
        assert_eq!(tab_tip("mod.rs", Some(&path), false, false, false, true, false), "~/code/mod.rs\nChanged on disk");
        assert_eq!(tab_tip("mod.rs", Some(&path), false, false, true, false, true), "~/code/mod.rs\nRead-only\nDeleted from disk");
        // A dirty tab and a pinned one say so, before what the disk did.
        assert_eq!(tab_tip("mod.rs", Some(&path), false, true, false, false, false), "~/code/mod.rs\nUnsaved changes");
        assert_eq!(tab_tip("mod.rs", Some(&path), true, false, false, false, false), "~/code/mod.rs\nPinned");
        assert_eq!(tab_tip("mod.rs", Some(&path), true, true, true, true, false), "~/code/mod.rs\nPinned\nUnsaved changes\nRead-only\nChanged on disk");
        // An Untitled page has no path: its title stands in.
        assert_eq!(tab_tip("Untitled 2", None, false, false, false, false, false), "Untitled 2");
    }

    #[test]
    fn a_path_outside_home_is_whole_and_a_shown_one_is_recognised() {
        let home = Some(Path::new("/Users/h"));
        assert_eq!(tip_path(Path::new("/tmp/a.txt"), home), "/tmp/a.txt");
        assert_eq!(tip_path(Path::new("/Users/h"), home), "/Users/h");
        assert_eq!(tip_path(Path::new("/Users/hh/a.txt"), home), "/Users/hh/a.txt");
        assert!(is_tip_path("~/code/mod.rs", Path::new("/Users/h/code/mod.rs"), home));
        assert!(!is_tip_path("/Users/h/code/mod.rs", Path::new("/Users/h/code/mod.rs"), home));
        assert!(is_tip_path("/tmp/a.txt", Path::new("/tmp/a.txt"), home));
        assert!(!is_tip_path("/tmp/b.txt", Path::new("/tmp/a.txt"), None));
    }
}
