//! The folder tree's rows and the light behind them.
//!
//! Rows carry no selected or hover fill of their own. One selection shape and
//! one hover shape per window are painted behind the list by `ShapesLayer`, a
//! view of its own that the workspace hosts under the explorer column. It places
//! them from the row index, the scroll offset and the folds' placement, and each
//! follows on a spring, from where it is drawn. A hover notifies that view alone.
//! Rects are kept relative to the list plus its scroll offset, so scrolling
//! carries a shape instead of gliding it. A shape asks for a frame only while it
//! moves or fades, so a tree at rest draws nothing (Harrier's `motion/glide.rs`
//! is the model).

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    f32::consts::TAU,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    rc::Rc,
    time::Instant,
};

use gpui_kit::base::{TreeEntry, TreeState, actions::Confirm};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::motion::{Fade, Fold};
use crate::tokens::{FOLD_FADE_FROM, FOLD_FADE_SPAN, FOLD_OVERDRAW, FOLD_STAGGER_ROWS, FOLD_STAGGER_STEP};

use super::{Explorer, ExplorerEvent, PLACEHOLDER_EMPTY, PLACEHOLDER_LOADING, filter, git::Status, icons};
use crate::drag::DraggedFiles;
use crate::tokens::{ALPHA_DROP, ALPHA_FIELD, ALPHA_FIELD_LIGHT, ALPHA_NAME_DIM, FIELD_INSET, RADIUS_FIELD};
use crate::symbols;
use crate::tokens::{ERROR_SYMBOL, OVERLAY_ENTER, OVERLAY_EXIT, RADIUS_FLOAT_CARD, TOOLTIP_MAX_W, TOOLTIP_RISE};

use crate::control;
use crate::tokens::{
    ALPHA_GUIDE, ALPHA_GUIDE_LIT, ALPHA_HIDDEN_ICON, ALPHA_HOVER, ALPHA_HOVER_LIGHT, ARRIVED_MAX, ARRIVED_WASH,
    ALPHA_SELECTED, ALPHA_SELECTED_LIGHT, ALPHA_SELECTED_UNFOCUSED, ALPHA_SELECTED_UNFOCUSED_LIGHT,
    CHEVRON, CHEVRON_BEARING, CHEVRON_SLOT, DOT, DRAG_BADGE, DRAG_BADGE_OVERHANG, DRAG_BADGE_PLUS, FADE_QUICK, GUIDE_WIDTH, HOVER_IN, HOVER_SNAP_RESPONSE, TAIL_KEPT, UNSEEN_GRACE,
    ICON, MAX_STEP, RADIUS_ROW, ROW_GAP, ROW_HEIGHT, ROW_INDENT, ROW_LEADING, ROW_TRAILING, ROW_WASH_INSET_Y,
    SCROLL_GLIDE, SELECTED_FLASH, SELECT_RESPONSE, TREE_INSET, TEXT_BODY, TEXT_BODY_LINE, TEXT_SUBHEADLINE, TEXT_SUBHEADLINE_LINE,
};

/// The tree's colors.
#[derive(Clone, Copy, Default)]
pub struct Ink {
    /// The raw foreground, for washes (fills under 0.2 alpha); text uses `primary`.
    pub text: Hsla,
    /// Names at `ALPHA_PRIMARY` through the theme's ink.
    pub primary: Hsla,
    /// Secondary text and glyphs.
    pub muted: Hsla,
    /// Hidden names, the dimmer text.
    pub dim: Hsla,
    pub light: bool,
    /// The system accent color, for the drop wash on a folder row.
    pub accent: Hsla,
    /// An icon's tint for each git status.
    pub git: crate::theme::GitInks,
}

/// The name field open in a row (a rename or a new item) and, when the name was refused, why.
#[derive(Clone)]
pub struct Edit {
    pub path: PathBuf,
    pub input: Entity<InputState>,
    pub error: Option<SharedString>,
    /// The ink of the error.
    pub danger: Hsla,
}

type Rect = [f32; 4];

/// One shape's motion: a critically damped spring per edge that keeps its speed
/// when the target changes.
#[derive(Default)]
struct Follower {
    live: bool,
    at: Rect,
    v: Rect,
    to: Rect,
}

impl Follower {
    /// Aims at `to`; the first aim shows in place.
    fn aim(&mut self, to: Rect) {
        if !self.live {
            self.live = true;
            self.at = to;
            self.v = [0.; 4];
        }
        self.to = to;
    }

    /// Lands on the target at once: rows that fold carry their shapes, which must not trail them.
    fn snap(&mut self) {
        self.at = self.to;
        self.v = [0.; 4];
    }

    /// Steps `dt` seconds and says whether it still moves. A `chase` stiffens by how
    /// many half rows it trails and never trails more than a row (the hover shape).
    fn step(&mut self, dt: f32, response: f32, chase: bool) -> bool {
        let row = f32::from(ROW_HEIGHT);
        let behind = (0..4)
            .map(|i| (self.at[i] - self.to[i]).abs())
            .fold(0., f32::max);
        let mut omega = TAU / response;
        if chase {
            omega *= (behind / (row / 2.)).max(1.);
        }
        let decay = (-omega * dt).exp();
        let mut moving = false;
        for i in 0..4 {
            let (x, v) = (self.at[i] - self.to[i], self.v[i]);
            let c = v + omega * x;
            let (mut x2, mut v2) = ((x + c * dt) * decay, (v - omega * c * dt) * decay);
            if x2 * x < 0. {
                (x2, v2) = (0., 0.);
            }
            if chase {
                x2 = x2.clamp(-row, row);
            }
            if x2.abs() < 0.25 && v2.abs() < 4. {
                (x2, v2) = (0., 0.);
            } else {
                moving = true;
            }
            self.at[i] = self.to[i] + x2;
            self.v[i] = v2;
        }
        moving
    }
}

/// One name's ink level on its way: from `from` to `to` over `FADE_QUICK`, on the shared fade. A
/// brightening that waits for the selection to land is `pending` until the shapes know how far the
/// selection travels, then it starts `delay` later than that moment.
struct InkLevel {
    fade: Fade,
    to: f32,
    pending: bool,
}

impl InkLevel {
    /// A level that starts at `now`.
    fn started(from: f32, to: f32, now: Instant) -> Self {
        let mut fade = Fade::with(from, FADE_QUICK);
        fade.aim(to, now);
        Self { fade, to, pending: false }
    }

    /// A level held at `from` until `start` says when it heads for `to`.
    fn waiting(from: f32, to: f32) -> Self {
        Self { fade: Fade::with(from, FADE_QUICK), to, pending: true }
    }

    /// Lets a waiting level go, starting at `at`.
    fn start(&mut self, at: Instant) {
        self.pending = false;
        self.fade.aim(self.to, at);
    }

    fn at(&self, now: Instant) -> f32 {
        self.fade.level(now)
    }

    fn done(&self, now: Instant) -> bool {
        !self.pending && !self.fade.is_moving(now)
    }
}

/// How long after a jump of `rows` rows the selection spring (rate `2 pi / SELECT_RESPONSE`) is
/// within one row of its target: `t` in `(1 + wt) e^-wt = 1 / rows`. A move of one row or less waits for nothing.
fn arrival_delay(rows: f32) -> f32 {
    if rows <= 1. {
        return 0.;
    }
    let (mut low, mut high) = (0f32, 30f32);
    for _ in 0..40 {
        let mid = (low + high) / 2.;
        if (1. + mid) * (-mid).exp() > 1. / rows {
            low = mid;
        } else {
            high = mid;
        }
    }
    low * SELECT_RESPONSE / TAU
}

/// Starts the pending brightenings of the window's names, `rows` rows from the selection's old
/// place, `delay` after `now`.
fn start_pending(window: WindowId, rows: f32, now: Instant) {
    let delay = std::time::Duration::from_secs_f32(arrival_delay(rows));
    INKS.with_borrow_mut(|all| {
        if let Some(inks) = all.get_mut(&window) {
            for level in inks.levels.values_mut().filter(|level| level.pending) {
                level.start(now + delay);
            }
        }
    });
}

/// A window's names' ink: the row last seen selected (`None` before the first look, which shows
/// in place) and the levels still moving. A name with no level is at rest: primary when selected.
#[derive(Default)]
struct Inks {
    selected: Option<Option<SharedString>>,
    levels: HashMap<SharedString, InkLevel>,
}

#[derive(Default)]
struct Shapes {
    /// The row under the pointer, by its id: an expand or a reload moves rows, not the pointer.
    hovered: Option<SharedString>,
    selected: Follower,
    hover: Follower,
    hover_level: Fade,
    /// How lit the selection is: 0 with the tree not focused, 1 with it focused.
    lit: Option<Fade>,
    /// The frame it last moved in; `None` at rest, so a move starts from the frame it begins in.
    last: Option<Instant>,
    /// A frame has been prepainted in this window.
    drawn: bool,
    /// The shape layer's view: a hover change redraws it alone.
    view: Option<EntityId>,
    /// When the selection's flash began, for a jump to a row already in view.
    flash: Option<Instant>,
}

/// One row's piece of an indent guide: the line's x and the row's top and height in the window,
/// and the open folder (by path hash) the line belongs to.
struct Guide {
    x: f32,
    y: f32,
    height: f32,
    group: u64,
}

thread_local! {
    static SHAPES: RefCell<HashMap<WindowId, Shapes>> = RefCell::new(HashMap::new());
    static INKS: RefCell<HashMap<WindowId, Inks>> = RefCell::new(HashMap::new());
}

/// Tells the names which row is selected, at `now`, and says whether a name's ink still moves, so
/// the caller asks for the next frame. The old row's name dims and the new one's brightens.
pub fn set_selected(window: WindowId, selected: Option<SharedString>, now: Instant) -> bool {
    INKS.with_borrow_mut(|all| {
        let inks = all.entry(window).or_default();
        // A brightening the shapes never placed (the row is out of view) starts at once.
        for level in inks.levels.values_mut().filter(|level| level.pending) {
            level.start(now);
        }
        if inks.selected.as_ref() != Some(&selected) {
            if let Some(old) = inks.selected.take() {
                let level = |inks: &Inks, id: &SharedString, at_rest: f32| inks.levels.get(id).map_or(at_rest, |level| level.at(now));
                if let Some(old) = old {
                    let from = level(inks, &old, 1.);
                    inks.levels.insert(old, InkLevel::started(from, 0., now));
                }
                if let Some(new) = &selected {
                    let from = level(inks, new, 0.);
                    inks.levels.insert(new.clone(), InkLevel::waiting(from, 1.));
                }
            }
            inks.selected = Some(selected);
        }
        inks.levels.retain(|_, level| !level.done(now));
        !inks.levels.is_empty()
    })
}

/// How far the name of row `id` has brightened toward the primary ink, 0 to 1.
pub fn ink_level(window: WindowId, id: &SharedString, selected: bool, now: Instant) -> f32 {
    INKS.with_borrow(|all| {
        all.get(&window)
            .and_then(|inks| inks.levels.get(id))
            .map_or(if selected { 1. } else { 0. }, |level| level.at(now))
    })
}

thread_local! {
    /// Each window's folders wearing the drop wash (or letting go of it), by path.
    static WASH: RefCell<HashMap<WindowId, HashMap<PathBuf, Fold>>> = RefCell::new(HashMap::new());
}

/// How much of the drop wash the folder at `path` shows this frame, 0 to 1, on the `SETTLE`
/// spring: it comes on when `washed` turns true and lets go when it turns false, turning around
/// from where it is drawn. Asks for frames only while the spring moves.
pub(super) fn wash_level(window: &mut Window, path: &PathBuf, washed: bool) -> f32 {
    let id = window.window_handle().window_id();
    let now = crate::motion::drawn_now(id);
    let (level, moving) = WASH.with_borrow_mut(|all| {
        let Some(folds) = (if washed { Some(all.entry(id).or_default()) } else { all.get_mut(&id) }) else {
            return (0., false);
        };
        match folds.get_mut(path) {
            None if washed => {
                folds.insert(path.clone(), Fold::opening(now, false));
            }
            None => return (0., false),
            Some(fold) if fold.is_open() != washed => fold.reverse(now),
            Some(_) => {}
        }
        let fold = &folds[path];
        let (level, moving) = (fold.level(now), fold.is_moving(now));
        if !washed && !moving {
            folds.remove(path);
        }
        (level, moving)
    });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// A row's unsaved dot: how much of it shows, and until when a change that no frame has drawn still fades.
struct Mark {
    ease: Fade,
    /// A change nobody saw (the row is scrolled out) snaps to its target when it is first drawn after this.
    stale: Option<Instant>,
}

thread_local! {
    /// Each window's unsaved dots that are drawn or still fading, by path: a marked row keeps its entry at rest,
    /// so the row that loses its mark knows to fade the dot out instead of dropping it.
    static MARKS: RefCell<HashMap<(WindowId, PathBuf), Mark>> = RefCell::new(HashMap::new());
}

/// The unsaved set changed from `old` to `new`: a path that gained a mark gets a dot at 0 to fade in, and
/// every dot is told how long its change may wait for a first frame.
pub fn marks_changed(window: WindowId, old: &HashSet<PathBuf>, new: &HashSet<PathBuf>) {
    let stale = Instant::now() + UNSEEN_GRACE;
    MARKS.with_borrow_mut(|all| {
        for path in new.difference(old) {
            all.entry((window, path.clone())).or_insert(Mark { ease: Fade::with(0., FADE_QUICK), stale: None });
        }
        for ((id, path), mark) in all.iter_mut() {
            if *id == window {
                mark.stale = if new.contains(path) == old.contains(path) { mark.stale } else { Some(stale) };
            }
        }
    });
}

/// Whether some dot is still fading out for the row at `path`, so the row keeps its dot to draw it.
fn mark_pending(path: &PathBuf) -> bool {
    MARKS.with_borrow(|all| all.keys().any(|(_, held)| held == path))
}

/// How much of the unsaved dot of the row at `path` shows this frame, 0 to 1: it fades in and out over
/// `FADE_QUICK`, turning around from where it is drawn. Asks for frames only while it fades.
fn mark_level(window: &mut Window, path: &PathBuf, marked: bool) -> f32 {
    let id = window.window_handle().window_id();
    let now = crate::motion::drawn_now(id);
    let target = f32::from(marked);
    let (level, moving) = MARKS.with_borrow_mut(|all| {
        let key = (id, path.clone());
        let mark = all.entry(key.clone()).or_insert(Mark { ease: Fade::with(target, FADE_QUICK), stale: None });
        if let Some(stale) = mark.stale {
            if mark.ease.is_moving(now) {
                mark.stale = None;
            } else if Instant::now() > stale {
                *mark = Mark { ease: Fade::with(target, FADE_QUICK), stale: None };
            }
        }
        mark.ease.aim(target, now);
        let (level, moving) = (mark.ease.level(now), mark.ease.is_moving(now));
        if !marked && !moving {
            all.remove(&key);
        }
        (level, moving)
    });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// A row whose file arrived: when it was noted (real time) and the first frame that drew its wash.
struct Arrival {
    noted: Instant,
    since: Option<Instant>,
}

impl Arrival {
    /// The wash is over, or was never drawn and has waited too long to mean anything.
    fn spent(&self, now: Instant) -> bool {
        match self.since {
            Some(since) => self.level(since, now).is_none(),
            None => now.saturating_duration_since(self.noted) > UNSEEN_GRACE,
        }
    }

    /// The wash `now`, drawn since `since`: up over `HOVER_IN`, then down over `ARRIVED_WASH`, both on
    /// the shared fade; `None` once it is gone.
    fn level(&self, since: Instant, now: Instant) -> Option<f32> {
        let mut rise = Fade::with(0., HOVER_IN);
        rise.aim(1., since);
        if rise.is_moving(now) {
            return Some(rise.level(now));
        }
        let mut fall = Fade::with(1., ARRIVED_WASH);
        fall.aim(0., since + std::time::Duration::from_secs_f32(HOVER_IN));
        fall.is_moving(now).then(|| fall.level(now))
    }
}

thread_local! {
    /// Each window's rows wearing the arrival wash, by path: at most `ARRIVED_MAX`, each from its first drawn frame.
    static ARRIVALS: RefCell<HashMap<(WindowId, PathBuf), Arrival>> = RefCell::new(HashMap::new());
}

/// Files that arrived from outside the app: the rows that show them get one wash, up to `ARRIVED_MAX` at a
/// time in the window. The caller syncs the tree after, which draws the rows.
pub fn arrived(window: WindowId, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let now = Instant::now();
    ARRIVALS.with_borrow_mut(|all| {
        all.retain(|_, arrival| !arrival.spent(now));
        let mut live = all.keys().filter(|(id, _)| *id == window).count();
        for path in paths {
            if live >= ARRIVED_MAX {
                break;
            }
            if all.insert((window, path), Arrival { noted: now, since: None }).is_none() {
                live += 1;
            }
        }
    });
}

/// Whether the row at `path` wears (or is about to wear) an arrival wash in some window.
fn arriving(path: &PathBuf) -> bool {
    ARRIVALS.with_borrow(|all| all.keys().any(|(_, held)| held == path))
}

/// How much of the arrival wash the row at `path` shows this frame: 0 up to 1 over `HOVER_IN`, then down
/// to 0 over `ARRIVED_WASH`, from the first frame that draws it. Asks for frames only while it moves.
fn arrival_level(window: &mut Window, path: &PathBuf) -> f32 {
    let id = window.window_handle().window_id();
    let now = crate::motion::drawn_now(id);
    let (level, moving) = ARRIVALS.with_borrow_mut(|all| {
        let key = (id, path.clone());
        let Some(arrival) = all.get_mut(&key) else {
            return (0., false);
        };
        if arrival.since.is_none() && arrival.spent(Instant::now()) {
            all.remove(&key);
            return (0., false);
        }
        let since = *arrival.since.get_or_insert(now);
        let Some(level) = arrival.level(since, now) else {
            all.remove(&key);
            return (0., false);
        };
        (level, true)
    });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// A name's ink while its file is copied: the row's rest ink, the lit ink, and the level the row
/// was drawn at (0 at rest, 1 lit, as when selected) where the breath starts and settles back to.
struct Breath {
    path: PathBuf,
    rest: Hsla,
    lit: Hsla,
    level: f32,
}

/// The ink a name rests at: the muted ink, or the dimmer one for a hidden file. Git's status and the folder
/// accent never reach a name (finch#nb5m); the status is on the icon.
fn rest_ink(ink: &Ink, hidden: bool) -> Hsla {
    if hidden { ink.dim } else { ink.muted }
}

/// The colour an icon wears for git `status`.
fn git_tint(ink: &Ink, status: Status) -> Hsla {
    match status {
        Status::Conflicted => ink.git.conflicted,
        Status::Deleted => ink.git.deleted,
        Status::Modified => ink.git.modified,
        Status::Added => ink.git.added,
    }
}

/// A row's icon wearing git's colour for `status` (Finder's own with none).
fn icon_for(label: &str, is_folder: bool, ink: &Ink, status: Option<Status>) -> AnyElement {
    let size = f32::from(ICON);
    match (is_folder, status.map(|status| git_tint(ink, status))) {
        (true, None) => icons::folder(size),
        (true, Some(tint)) => icons::folder_tinted(size, tint),
        (false, None) => icons::file(label, size, ink.muted),
        (false, Some(tint)) => icons::file_tinted(label, size, tint),
    }
}

/// The icon of the row at `path`, easing to its tint when git's answer for it just changed.
fn tree_icon(label: &SharedString, is_folder: bool, ink: Ink, status: Option<Status>, path: &PathBuf) -> AnyElement {
    if !shift_pending(path) {
        return icon_for(label, is_folder, &ink, status);
    }
    let to_alpha = if !is_folder && icons::is_symbol(label) { status.map_or(ink.muted, |status| git_tint(&ink, status)).a } else { 1. };
    let label = label.clone();
    IconFade { path: path.clone(), to: status, to_alpha, make: Box::new(move |status| icon_for(&label, is_folder, &ink, status)) }.into_any_element()
}

/// An icon whose git status changed: its tint, as the shares of the statuses it eases from, and the status
/// it eases to over `FADE_QUICK`.
struct Shift {
    /// The tints the icon was drawn in, each with its share of the colour (the shares add to 1): one status
    /// when the tree last drew it at rest, more when a newer result turned a running fade around.
    from: Vec<(Option<Status>, f32)>,
    to: Option<Status>,
    ease: Fade,
    /// The first frame that draws the fade starts it; until then it waits (and snaps after `stale`).
    started: bool,
    stale: Instant,
    /// How far the fade had come when last drawn, where a newer result turns it around from.
    drawn: Option<f32>,
}

thread_local! {
    /// Each window's icons still easing into a new git tint, by path: an entry leaves when its fade lands,
    /// when nobody draws it for `UNSEEN_GRACE`, and with its window.
    static SHIFTS: RefCell<HashMap<(WindowId, PathBuf), Shift>> = RefCell::new(HashMap::new());
}

/// Git's answer changed for `changes` (path, status the tree drew, status now): each icon eases to its new
/// tint from the first frame that draws it, an interrupted one from the blend it reached.
pub fn git_changed(window: WindowId, changes: Vec<(PathBuf, Option<Status>, Option<Status>)>) {
    let now = Instant::now();
    SHIFTS.with_borrow_mut(|all| {
        all.retain(|(id, _), shift| *id != window || shift.started || now < shift.stale);
        for (path, old, new) in changes {
            let fresh = Shift { from: vec![(old, 1.)], to: new, ease: Fade::with(0., FADE_QUICK), started: false, stale: now + UNSEEN_GRACE, drawn: None };
            match all.entry((window, path)) {
                std::collections::hash_map::Entry::Occupied(mut held) => {
                    let held = held.get_mut();
                    let from = match held.drawn {
                        Some(level) => blend(&held.from, held.to, level),
                        None => held.from.clone(),
                    };
                    *held = Shift { from, ..fresh };
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(fresh);
                }
            }
        }
    });
}

/// `from` after a fade towards `to` has come `level` of the way: each share scaled down, `to` taking the rest.
fn blend(from: &[(Option<Status>, f32)], to: Option<Status>, level: f32) -> Vec<(Option<Status>, f32)> {
    let mut shares: Vec<(Option<Status>, f32)> = Vec::with_capacity(from.len() + 1);
    for (status, share) in from.iter().map(|&(status, share)| (status, share * (1. - level))).chain([(to, level)]) {
        match shares.iter_mut().find(|(held, _)| *held == status) {
            Some((_, held)) => *held += share,
            None => shares.push((status, share)),
        }
    }
    shares.retain(|(_, share)| *share > 0.);
    shares
}

/// Whether some window is easing the icon at `path`, so its row builds the fade into its icon.
fn shift_pending(path: &PathBuf) -> bool {
    SHIFTS.with_borrow(|all| all.keys().any(|(_, held)| held == path))
}

/// A tree icon easing into a new git tint while it is painted: the tints it comes from, each at the opacity
/// that blends them in their shares, with the new one laid over at the fade's level. Built at layout, from
/// the frame's time, and asks for frames only while it eases.
struct IconFade {
    path: PathBuf,
    to: Option<Status>,
    /// The new icon's own alpha: 1 for a Finder icon, the symbol's ink alpha for a symbol.
    to_alpha: f32,
    make: Box<dyn Fn(Option<Status>) -> AnyElement>,
}

impl IconFade {
    /// The shares the icon comes from and how far the fade has come this frame; `None` when nothing eases it.
    fn level(&self, window: &mut Window) -> Option<(Vec<(Option<Status>, f32)>, f32)> {
        let id = window.window_handle().window_id();
        let now = crate::motion::drawn_now(id);
        let key = (id, self.path.clone());
        let eased = SHIFTS.with_borrow_mut(|all| {
            let shift = all.get_mut(&key)?;
            if !shift.started {
                if Instant::now() > shift.stale {
                    all.remove(&key);
                    return None;
                }
                shift.ease.aim(1., now);
                shift.started = true;
            }
            let level = shift.ease.level(now);
            shift.drawn = Some(level);
            let from = shift.from.clone();
            if !shift.ease.is_moving(now) {
                all.remove(&key);
                return None;
            }
            Some((from, level))
        })?;
        window.request_animation_frame();
        Some(eased)
    }
}

impl Element for IconFade {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let layer = |opacity: f32, icon: AnyElement| div().absolute().top_0().left_0().opacity(opacity).child(icon);
        let mut stack = div().relative().size(ICON).flex_none();
        match self.level(window) {
            None => stack = stack.child((self.make)(self.to)),
            Some((from, level)) => {
                // Each lower layer is laid at its share of what is under it, so the blend of the old tints is exact.
                // The old layers sit in one group scaled so the pair stays as opaque as the new icon alone is: a
                // symbol is see-through, and opaque old layers would end the fade over-solid, then snap.
                let mut under = 0.;
                let mut old = div().absolute().top_0().left_0().size(ICON).opacity((1. - level) / (1. - self.to_alpha * level).max(1e-3));
                for (status, share) in from {
                    under += share;
                    old = old.child(layer(share / under, (self.make)(status)));
                }
                stack = stack.child(old).child(layer(level, (self.make)(self.to)));
            }
        }
        let mut icon = stack.into_any_element();
        let layout = icon.request_layout(window, cx);
        (layout, icon)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, icon: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) {
        icon.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, icon: &mut Self::RequestLayoutState, _: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        icon.paint(window, cx);
    }
}

impl IntoElement for IconFade {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// A file being copied: how many copies of it run, when the last one landed, and its name's ink.
struct Copying {
    running: usize,
    /// The copy has run past `COPY_QUIET`: its row's name breathes.
    due: bool,
    landed: Option<Instant>,
    ease: Fade,
    /// Where the breath last aimed (`true` up to the lit ink); `None` before the first frame.
    aimed_up: Option<bool>,
    settling: bool,
}

thread_local! {
    /// Each window's source rows of copies in flight, by path.
    static COPYING: RefCell<HashMap<(WindowId, PathBuf), Copying>> = RefCell::new(HashMap::new());
}

/// Copies of `paths` start: their rows breathe once a copy has outlasted `COPY_QUIET`.
pub fn copy_begins(window: WindowId, paths: &[PathBuf]) {
    let now = Instant::now();
    COPYING.with_borrow_mut(|all| {
        all.retain(|_, held| held.running > 0 || held.landed.is_some_and(|at| now.saturating_duration_since(at) <= UNSEEN_GRACE));
        for path in paths {
            let held = all.entry((window, path.clone())).or_insert(Copying { running: 0, due: false, landed: None, ease: Fade::at(0.), aimed_up: None, settling: false });
            held.running += 1;
            held.landed = None;
            held.settling = false;
        }
    });
}

/// `COPY_QUIET` passed: those of `paths` still copying start to breathe. True when one does, so the
/// caller redraws the rows once; the frames after come from the names' own paint.
pub fn copy_runs_long(window: WindowId, paths: &[PathBuf]) -> bool {
    COPYING.with_borrow_mut(|all| {
        let mut any = false;
        for path in paths {
            if let Some(held) = all.get_mut(&(window, path.clone())).filter(|held| held.running > 0) {
                held.due = true;
                any = true;
            }
        }
        any
    })
}

/// Copies of `paths` landed (or failed): a copy that never breathed leaves no trace, one that did
/// settles to rest over a fade, and the caller redraws the rows.
pub fn copy_lands(window: WindowId, paths: &[PathBuf]) {
    let now = Instant::now();
    COPYING.with_borrow_mut(|all| {
        for path in paths {
            let key = (window, path.clone());
            let Some(held) = all.get_mut(&key) else { continue };
            held.running = held.running.saturating_sub(1);
            if held.running > 0 {
                continue;
            }
            if held.due {
                held.landed = Some(now);
            } else {
                all.remove(&key);
            }
        }
    });
}

/// Whether the row at `path` breathes (or is settling back) in some window.
fn copying(path: &PathBuf) -> bool {
    let now = Instant::now();
    COPYING.with_borrow(|all| {
        all.iter().any(|((_, held), copy)| held == path && copy.due && copy.landed.is_none_or(|at| now.saturating_duration_since(at) <= UNSEEN_GRACE))
    })
}

/// How lit the name at `path` is this frame, 0 at rest to 1 lit: easing up over `HOVER_IN` and down over
/// `HOVER_OUT` while the copy runs, then to where the row rests over one fade once it lands. Asks for
/// frames only while the copy runs or the name settles.
fn copy_level(window: &mut Window, path: &PathBuf, rests_at: f32) -> f32 {
    let id = window.window_handle().window_id();
    let now = crate::motion::drawn_now(id);
    let (level, moving) = COPYING.with_borrow_mut(|all| {
        let key = (id, path.clone());
        let Some(held) = all.get_mut(&key) else { return (rests_at, false) };
        if held.aimed_up.is_none() {
            held.ease = Fade::at(rests_at);
        }
        if held.running > 0 {
            if !held.ease.is_moving(now) {
                let up = held.aimed_up.map_or(rests_at < 0.5, |prev| !prev);
                held.ease.aim(if up { 1. } else { 0. }, now);
                held.aimed_up = Some(up);
            }
            return (held.ease.level(now), true);
        }
        if !held.settling {
            held.ease.aim(rests_at, now);
            held.settling = true;
        }
        let (level, moving) = (held.ease.level(now), held.ease.is_moving(now));
        if !moving {
            all.remove(&key);
        }
        (level, moving)
    });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// Drops a closed window's shape state.
pub fn forget(window: WindowId) {
    SHAPES.with_borrow_mut(|all| {
        all.remove(&window);
    });
    WASH.with_borrow_mut(|all| {
        all.remove(&window);
    });
    INKS.with_borrow_mut(|all| {
        all.remove(&window);
    });
    crate::control::forget(window);
    COPYING.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
    MARKS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
    SHIFTS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
    ARRIVALS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
    super::filter::forget(window);
    crate::motion::forget(window);
}

/// A window's explorer took a new root: the washes and arrivals of the old root's rows go, the rest stays.
pub fn reset_root(window: WindowId) {
    WASH.with_borrow_mut(|all| {
        all.remove(&window);
    });
    ARRIVALS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
    SHIFTS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
}

/// The shapes behind the rows, a view of their own. The workspace hosts it just under the
/// explorer column, so a hover change or a glide frame redraws it alone: the explorer and its
/// rows are not its ancestors. It finds the list from the list's scroll handle.
pub struct ShapesLayer {
    tree: Entity<TreeState>,
    scroll: UniformListScrollHandle,
    folds: Rc<RefCell<Folds>>,
    pub ink: Ink,
    /// The list is on screen (the explorer shows rows, not the filter's "no matches").
    pub shown: bool,
    /// The picked rows, by id: each wears the selection's wash where it is, with no glide.
    pub picked: Rc<HashSet<SharedString>>,
}

impl ShapesLayer {
    pub fn new(tree: Entity<TreeState>, scroll: UniformListScrollHandle, folds: Rc<RefCell<Folds>>) -> Self {
        Self { tree, scroll, folds, ink: Ink::default(), shown: true, picked: Rc::default() }
    }
}

impl Render for ShapesLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_id = window.window_handle().window_id();
        let view = cx.entity_id();
        SHAPES.with_borrow_mut(|all| all.entry(window_id).or_default().view = Some(view));
        let (tree, folds, scroll, ink, shown, picked) = (self.tree.clone(), self.folds.clone(), self.scroll.clone(), self.ink, self.shown, self.picked.clone());
        canvas(
            move |_, window, cx| {
                let id = window.window_handle().window_id();
                // The rendered frame's dispatch tree is empty until one frame has been drawn.
                let (drawn, hovered) = SHAPES.with_borrow_mut(|all| {
                    let shapes = all.entry(id).or_default();
                    (std::mem::replace(&mut shapes.drawn, true), shapes.hovered.clone())
                });
                // Only a focused tree in the key window answers its keys, an editor in focus does not.
                let focused = drawn
                    && control::window_key(window)
                    && window.is_action_available(&Confirm { secondary: false }, cx);
                // The frame's time was ticked by the workspace's render: the rows and shapes read it.
                (focused, hovered, crate::motion::drawn_now(id))
            },
            move |_, (focused, hovered, now), window, cx| {
                // The list's rect: the tree container's, from the list the explorer laid out this
                // frame (it reaches past the container's bottom by the fold's overdraw). Read at
                // paint, after every prepaint, so it is this frame's.
                let (list, fold_moving) = {
                    let folds = folds.borrow();
                    let bounds = scroll.0.borrow().base_handle.bounds();
                    let inset = TREE_INSET;
                    let list = Bounds::new(
                        point(bounds.origin.x - inset, bounds.origin.y),
                        size(bounds.size.width + inset * 2., px((f32::from(bounds.size.height) - folds.overdraw(now)).max(0.))),
                    );
                    (list, folds.moving(now))
                };
                if !shown || list.size.width <= px(0.) {
                    return;
                }
                let fade = {
                    let entries = tree.read(cx).entries();
                    // Once a frame here, so nothing to remember.
                    super::sticky::EdgeFade::read(&Default::default(), &scroll, folds.borrow().overdraw(now), &|ix| entries[ix].depth(), entries.len())
                };
                let targets = targets(list, &scroll, tree.read(cx), &folds.borrow(), hovered.as_ref(), &picked, now);
                paint(list, focused, now, targets, fold_moving, &scroll, ink, fade, window);
            },
        )
        .absolute()
        .inset_0()
    }
}

/// Puts the hover shape on the row with `id`, as if the pointer sat there
/// (the control socket's `explorer hover`). The caller notifies the explorer.
pub fn set_hovered(window: &Window, id: SharedString) {
    let window = window.window_handle().window_id();
    SHAPES.with_borrow_mut(|all| all.entry(window).or_default().hovered = Some(id));
}

/// Makes the selection shape appear on its row where it is, with no glide from the old one.
pub fn snap_selection(window: &Window) {
    let window = window.window_handle().window_id();
    SHAPES.with_borrow_mut(|all| all.entry(window).or_default().selected = Follower::default());
}

/// Starts the selection's flash at `now`: its wash brightens and settles back over `SCROLL_GLIDE`.
/// The caller notifies the shapes layer.
pub fn flash(window: WindowId, now: Instant) {
    SHAPES.with_borrow_mut(|all| all.entry(window).or_default().flash = Some(now));
}

/// Where the rows' shapes want to be this frame, from row index, scroll offset and fold placement.
#[derive(Default)]
struct Targets {
    /// The selected row's rect (relative to the list plus its scroll offset) and, for a row sliding
    /// out from under its folder, the part of it that shows (in the window).
    selected: Option<(Rect, Option<Bounds<Pixels>>)>,
    /// The other picked rows' rects and what shows of each, as `selected` has them.
    picked: Vec<(Rect, Option<Bounds<Pixels>>)>,
    /// The hovered row's rect and whether it is selected or picked.
    hover: Option<(Rect, bool)>,
    guides: Vec<Guide>,
    /// The group (an open folder's path hash) holding the selected row.
    selected_group: Option<u64>,
    /// Some row drawn this frame is placed by a fold, so the shapes ride it instead of trailing it.
    folding: bool,
}

/// Places the rows from the list's top, the scroll offset and the folds, the way the list draws
/// them (row pitch `ROW_HEIGHT`, `Folds::place` for rows a fold moves).
fn targets(
    list: Bounds<Pixels>,
    scroll: &UniformListScrollHandle,
    tree: &TreeState,
    folds: &Folds,
    hovered: Option<&SharedString>,
    picked: &HashSet<SharedString>,
    now: Instant,
) -> Targets {
    let row = f32::from(ROW_HEIGHT);
    let inset = f32::from(TREE_INSET);
    let offset = scroll.0.borrow().base_handle.offset();
    let (left, top) = (f32::from(list.origin.x) + inset, f32::from(list.origin.y + offset.y));
    let width = f32::from(list.size.width) - 2. * inset;
    let scrolled = (-f32::from(offset.y)).max(0.);
    // Rows a fold moves are drawn up to as many rows away as the folds hold.
    let reach: usize = folds.runs.iter().map(|run| run.n).sum();
    let first = (scrolled / row) as usize;
    let last = ((scrolled + f32::from(list.size.height)) / row).ceil() as usize + reach + 1;
    let selected_ix = tree.selected_index();
    let hovered_ix = hovered.and_then(|id| tree.index_of(id));
    let mut out = Targets::default();
    for ix in first..=last {
        let Some(entry) = tree.entry(ix) else { break };
        let id = &entry.item().id;
        if id.ends_with(PLACEHOLDER_LOADING) || id.ends_with(PLACEHOLDER_EMPTY) {
            continue;
        }
        let place = folds.place(ix, now);
        let (shift, clip) = place.map_or((0., None), |place| (place.shift, place.clip));
        let slot = ix as f32 * row;
        // What shows of the row, in points from the list's top.
        let (show_top, show_bottom) = match clip {
            Some(clip) => (slot + clip, slot + row - shift),
            None => (slot - shift, slot - shift + row),
        };
        if show_bottom - show_top < 0.5 {
            continue;
        }
        out.folding |= place.is_some();
        let wash_inset = f32::from(ROW_WASH_INSET_Y);
        let rect = [inset, slot - shift + wash_inset, width, row - 2. * wash_inset];
        let selected = selected_ix == Some(ix);
        let is_picked = !selected && picked.contains(id);
        if selected || is_picked {
            let mask = clip.map(|_| {
                Bounds::new(
                    point(px(left), px(top + show_top)), // design: runtime geometry from the fold
                    size(px(width), px(show_bottom - show_top)), // design: runtime geometry from the fold
                )
            });
            if selected {
                out.selected = Some((rect, mask));
            } else {
                out.picked.push((rect, mask));
            }
        }
        if hovered_ix == Some(ix) {
            out.hover = Some((rect, selected || is_picked));
        }
        // One guide line per open folder above this row, under that folder's icon, grouped by the
        // folder's path.
        let depth = entry.depth();
        if depth == 0 {
            continue;
        }
        let path = PathBuf::from(id.to_string());
        for level in 0..depth {
            let Some(folder) = path.ancestors().nth(depth - level) else { continue };
            let mut hasher = DefaultHasher::new();
            folder.hash(&mut hasher);
            let group = hasher.finish();
            let center = ROW_LEADING + ROW_INDENT * level as f32 + CHEVRON_SLOT + ICON / 2.;
            out.guides.push(Guide { x: left + f32::from(center - GUIDE_WIDTH / 2.), y: top + show_top, height: show_bottom - show_top, group });
            if selected && level == depth - 1 {
                out.selected_group = Some(group);
            }
        }
    }
    out
}

fn paint(
    list: Bounds<Pixels>,
    focused: bool,
    now: Instant,
    targets: Targets,
    fold_moving: bool,
    scroll: &UniformListScrollHandle,
    ink: Ink,
    fade: super::sticky::EdgeFade,
    window: &mut Window,
) {
    let id = window.window_handle().window_id();
    let offset = scroll.0.borrow().base_handle.offset();
    let (ox, oy) = (
        f32::from(list.origin.x + offset.x),
        f32::from(list.origin.y + offset.y),
    );
    let light = ink.light;
    let (selected, selected_unfocused, hover) = if light {
        (ALPHA_SELECTED_LIGHT, ALPHA_SELECTED_UNFOCUSED_LIGHT, ALPHA_HOVER_LIGHT)
    } else {
        (ALPHA_SELECTED, ALPHA_SELECTED_UNFOCUSED, ALPHA_HOVER)
    };

    let (mut quads, mut lines, moving, jump) = SHAPES.with_borrow_mut(|all| {
        let shapes = all.entry(id).or_default();
        let dt = shapes.last.map_or(0., |last| {
            now.saturating_duration_since(last)
                .as_secs_f32()
                .min(MAX_STEP)
        });
        let mut moving = false;
        let mut jump = None;

        let mut selected_mask = None;
        match targets.selected {
            Some((rect, mask)) => {
                shapes.selected.aim(rect);
                if targets.folding {
                    shapes.selected.snap();
                }
                selected_mask = mask;
                jump = Some((shapes.selected.at[1] - shapes.selected.to[1]).abs() / f32::from(ROW_HEIGHT));
                moving |= shapes.selected.step(dt, SELECT_RESPONSE, false);
            }
            None => shapes.selected = Follower::default(),
        }
        moving |= fold_moving;

        let lit_to = if focused { 1. } else { 0. };
        let lit_fade = shapes.lit.get_or_insert(Fade::at(lit_to));
        lit_fade.aim(lit_to, now);
        moving |= lit_fade.is_moving(now);
        let lit = lit_fade.level(now);

        let probe = targets.hover;
        let shows = probe.is_some_and(|(_, selected)| !selected);
        if let Some((rect, _)) = probe {
            shapes.hover.aim(rect);
            if targets.folding {
                shapes.hover.snap();
            }
        }
        shapes.hover_level.aim(f32::from(shows), now);
        moving |= shapes.hover_level.is_moving(now);
        let hover_level = shapes.hover_level.level(now);
        if shapes.hover.live {
            moving |= shapes.hover.step(dt, HOVER_SNAP_RESPONSE, true);
            // Faded out and no row hovered: forget, so the next row fades in in place.
            if probe.is_none() && hover_level == 0. {
                shapes.hover = Follower::default();
            }
        }
        // A jump to a row already in view: the wash swells and settles back over `SCROLL_GLIDE`.
        let flash = match shapes.flash {
            Some(since) => {
                let t = now.saturating_duration_since(since).as_secs_f32() / SCROLL_GLIDE;
                if t >= 1. {
                    shapes.flash = None;
                    0.
                } else {
                    moving = true;
                    (std::f32::consts::PI * t).sin()
                }
            }
            None => 0.,
        };
        shapes.last = moving.then_some(now);

        let at = |r: Rect| {
            Bounds::new(
                point(px(r[0] + ox), px(r[1] + oy)), // design: runtime geometry from computed rects
                size(px(r[2]), px(r[3])), // design: runtime geometry from computed rects
            )
        };
        let list_top = f32::from(list.origin.y);
        // A wash fades whole, by its centre, as its row's ink does.
        let seen = |bounds: Bounds<Pixels>| fade.at(f32::from(bounds.origin.y + bounds.size.height / 2.) - list_top);
        let mut quads = Vec::with_capacity(2);
        if shapes.hover.live && hover_level > 0. {
            let bounds = at(shapes.hover.at);
            quads.push((bounds, ink.text.opacity(hover * hover_level * seen(bounds)), None));
        }
        let cover = shapes.selected.live.then(|| at(shapes.selected.at));
        let rest = selected_unfocused + (selected - selected_unfocused) * lit;
        if shapes.selected.live {
            let alpha = rest * (1. + SELECTED_FLASH * flash);
            let bounds = at(shapes.selected.at);
            quads.push((bounds, ink.text.opacity(alpha * seen(bounds)), selected_mask));
        }
        // The other picked rows wear the same wash, in place.
        for &(rect, mask) in &targets.picked {
            let bounds = at(rect);
            quads.push((bounds, ink.text.opacity(rest * seen(bounds)), mask));
        }
        let covers = || cover.into_iter().chain(targets.picked.iter().map(|&(rect, _)| at(rect)));

        // Each row's pieces of a line join into one run per group (a piece that touches or overlaps the run
        // joins it: a fold that opens thousands of rows stacks their pieces over one another, and each
        // would become its own set of quads for good), then the selection's and the picked rows' shapes are
        // cut out of it, so the line parts around the glide and closes behind it.
        let mut runs: Vec<(f32, f32, f32, u64)> = Vec::new();
        for guide in &targets.guides {
            let end = guide.y + guide.height;
            match runs
                .iter_mut()
                .rev()
                .find(|run| run.0 == guide.x && run.3 == guide.group && guide.y <= run.2 + 0.5 && end >= run.1 - 0.5)
            {
                Some(run) => {
                    run.1 = run.1.min(guide.y);
                    run.2 = run.2.max(end);
                }
                None => runs.push((guide.x, guide.y, end, guide.group)),
            }
        }
        let mut lines = Vec::with_capacity(runs.len() + 1);
        let mut pieces: Vec<(f32, f32)> = Vec::new();
        let mut faded: Vec<(f32, f32, f32, f32)> = Vec::new();
        for (x, top, bottom, group) in runs {
            let alpha = if targets.selected_group == Some(group) {
                ALPHA_GUIDE_LIT
            } else {
                ALPHA_GUIDE
            };
            pieces.clear();
            pieces.push((top, bottom));
            for cover in covers().filter(|cover| x >= f32::from(cover.origin.x) && x < f32::from(cover.right())) {
                let (cut_top, cut_bottom) = (f32::from(cover.origin.y), f32::from(cover.bottom()));
                for n in 0..pieces.len() {
                    let (from, to) = pieces[n];
                    pieces[n] = (from, to.min(cut_top));
                    pieces.push((from.max(cut_bottom), to));
                }
                pieces.retain(|(from, to)| to > from);
            }
            for &(from, to) in &pieces {
                // The guide fades along its length where the list's edge fade or the pinned names reach it.
                faded.clear();
                fade.pieces(from - list_top, to - list_top, &mut faded);
                for &(from, to, a, b) in &faded {
                    let bounds = Bounds::new(
                        point(px(x), px(from + list_top)), // design: runtime geometry from measured rows
                        size(GUIDE_WIDTH, px(to - from)), // design: runtime geometry from measured rows
                    );
                    let fill: gpui::Background = if a >= 1. && b >= 1. {
                        ink.text.opacity(alpha).into()
                    } else {
                        gpui::linear_gradient(180., gpui::linear_color_stop(ink.text.opacity(alpha * a), 0.), gpui::linear_color_stop(ink.text.opacity(alpha * b), 1.))
                    };
                    lines.push((bounds, fill));
                }
            }
        }
        (quads, lines, moving, jump)
    });
    if let Some(rows) = jump {
        start_pending(id, rows, now);
    }

    window.with_content_mask(Some(ContentMask { bounds: list }), |window| {
        for (bounds, fill) in lines.drain(..) {
            window.paint_quad(gpui::fill(bounds, fill));
        }
        for (bounds, fill, show) in quads.drain(..) {
            let quad = gpui::fill(bounds, fill).corner_radii(Corners::all(RADIUS_ROW));
            match show {
                // A row sliding out from under its folder keeps its wash inside what shows of it.
                Some(show) => window.with_content_mask(Some(ContentMask { bounds: show.intersect(&list) }), |window| {
                    window.paint_quad(quad);
                }),
                None => window.paint_quad(quad),
            }
        }
    });
    if moving {
        window.request_animation_frame();
    }
}

/// One fold and where its rows sit in the tree's list: `n` rows follow row `ix`, and a folder's own
/// row turns its chevron with the fold. A `deep` fold also shuts the folders inside when it lands
/// (Collapse all). A `lone` run is a single row that arrived from outside: `id` is that row's path,
/// `ix` the row above it and `n` is 1, so the rows below slide down and the row opens out of the
/// gap; it is no folder's fold, so nothing that looks a folder's fold up by `id` sees it.
pub struct Run {
    pub id: SharedString,
    pub fold: Fold,
    pub ix: usize,
    pub n: usize,
    pub deep: bool,
    pub lone: bool,
}

/// The folds under way in a tree, and the rows that left it and are closing their gaps. Rows read it as they are
/// built, so it is shared with the explorer.
#[derive(Default)]
pub struct Folds {
    pub runs: Vec<Run>,
    pub ghosts: Vec<Ghost>,
}

/// A row that left the tree (its file was removed from outside or trashed), closing its gap on the fold spring: the
/// rows below stay where they were drawn and slide up as it closes, while the row itself, which the tree no longer
/// holds, is drawn from these fields. `gap` is the list index of the first row below it, kept right by `anchor`
/// (that row's path) when later syncs move the rows.
pub struct Ghost {
    pub fold: Fold,
    pub gap: usize,
    pub anchor: Option<SharedString>,
    pub label: SharedString,
    pub is_folder: bool,
    /// Drawn dimmed, as a dot file or an ignored one is.
    pub dim: bool,
    pub depth: usize,
    pub status: Option<Status>,
}

/// One ghost as the row it hangs from draws it: `top` points below that row's drawn top, `height` points of gap left.
#[derive(Clone)]
pub struct Leaving {
    top: f32,
    height: f32,
    opacity: f32,
    label: SharedString,
    is_folder: bool,
    dim: bool,
    depth: usize,
    status: Option<Status>,
}

/// Where a folding row is drawn, relative to its slot in the list.
#[derive(Clone, Copy)]
pub struct Place {
    /// How far up the row is drawn, in points: the rows below a fold slide down with it.
    shift: f32,
    /// Where a row that comes out from under its folder starts to show, in points from its slot's
    /// top; `None` for a row that is not cut.
    clip: Option<f32>,
    opacity: f32,
}

/// What a row needs to know of the folds when it is built.
#[derive(Clone, Default)]
pub struct FoldView {
    pub place: Option<Place>,
    /// The turn of this row's own chevron while its fold runs, 0 to 1.
    pub turn: Option<f32>,
    /// The ghosts that hang from this row, drawn closing in the gap below it.
    pub leaving: Vec<Leaving>,
}

impl FoldView {
    /// How far up the row is drawn from its slot, in points, for the edge fade to read it where it is.
    pub fn shift(&self) -> f32 {
        self.place.map_or(0., |place| place.shift)
    }
}

impl Folds {
    /// No fold and no ghost is under way.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.ghosts.is_empty()
    }

    /// What the folds do to the row at `ix` and to the chevron of the folder with `id`, at `now`.
    pub fn view(&self, ix: usize, id: &SharedString, now: Instant) -> FoldView {
        let leaving = self.leaving_at(ix, now);
        let place = self.place(ix, now).or_else(|| (!leaving.is_empty()).then_some(Place { shift: 0., clip: None, opacity: 1. }));
        FoldView { place, turn: self.runs.iter().find(|run| !run.lone && run.id == *id).map(|run| run.fold.level(now)), leaving }
    }

    /// The ghosts that hang from row `ix`: the row above their gap, or the first row for a gap at the very top.
    /// A ghost sits below the ghosts of its gap that came before it.
    fn leaving_at(&self, ix: usize, now: Instant) -> Vec<Leaving> {
        let row = f32::from(ROW_HEIGHT);
        let mut out = Vec::new();
        for (n, ghost) in self.ghosts.iter().enumerate() {
            let hangs = if ghost.gap == 0 { ix == 0 } else { ghost.gap == ix + 1 };
            if !hangs {
                continue;
            }
            let heights = |from: usize, to: usize| -> f32 {
                self.ghosts[from..to].iter().filter(|other| other.gap == ghost.gap).map(|other| other.fold.level(now) * row).sum()
            };
            let level = ghost.fold.level(now);
            // Below its row, after the ghosts before it; at the very top, above the first row, which is held down by all of them.
            let top = if ghost.gap == 0 { -heights(n, self.ghosts.len()) } else { row + heights(0, n) };
            let height = level * row;
            if height < 0.5 {
                continue;
            }
            let opacity = ((level - FOLD_FADE_FROM) / FOLD_FADE_SPAN).clamp(0., 1.);
            out.push(Leaving { top, height, opacity, label: ghost.label.clone(), is_folder: ghost.is_folder, dim: ghost.dim, depth: ghost.depth, status: ghost.status });
        }
        out
    }

    /// How far up the rows after row `ix` are drawn: every fold above it holds back the rows it opens, and every
    /// ghost above it holds the rows below where they were (negative: drawn down).
    fn shift_above(&self, ix: usize, now: Instant) -> f32 {
        let row = f32::from(ROW_HEIGHT);
        let folded: f32 = self
            .runs
            .iter()
            .filter(|run| run.n > 0 && run.ix < ix)
            .map(|run| (1. - run.fold.level(now)) * run.n as f32 * row)
            .sum();
        let held: f32 = self.ghosts.iter().filter(|ghost| ghost.gap <= ix).map(|ghost| ghost.fold.level(now) * row).sum();
        folded - held
    }

    fn place(&self, ix: usize, now: Instant) -> Option<Place> {
        let row = f32::from(ROW_HEIGHT);
        let shift = self.shift_above(ix, now);
        let mut clip: Option<f32> = None;
        let mut opacity = 1.;
        for run in self.runs.iter().filter(|run| run.n > 0 && run.ix < ix && ix <= run.ix + run.n) {
            let k = ix - run.ix - 1;
            // The bottom edge of the folder's row as drawn, in this row's slot terms.
            let line = -(k as f32) * row - self.shift_above(run.ix, now);
            clip = Some(clip.map_or(line, |at| at.max(line)));
            let start = FOLD_FADE_FROM + FOLD_STAGGER_STEP * k.min(FOLD_STAGGER_ROWS) as f32;
            opacity *= ((run.fold.level(now) - start) / FOLD_FADE_SPAN).clamp(0., 1.);
        }
        (shift != 0. || clip.is_some()).then_some(Place { shift, clip, opacity })
    }

    /// How far past its bottom edge the list draws rows: the rows a fold slides in from beyond the
    /// edge are laid out below it, clipped, so the first frame of a fold shows what was there.
    pub fn overdraw(&self, now: Instant) -> f32 {
        let row = f32::from(ROW_HEIGHT);
        let total: f32 = self.runs.iter().filter(|run| run.n > 0).map(|run| (1. - run.fold.level(now)) * run.n as f32 * row).sum();
        total.min(FOLD_OVERDRAW)
    }

    /// A fold or a ghost runs and has not landed.
    pub fn moving(&self, now: Instant) -> bool {
        self.runs.iter().any(|run| run.fold.is_moving(now)) || self.ghosts.iter().any(|ghost| ghost.fold.is_moving(now))
    }
}

/// `element` drawn where its fold puts it: shifted up, and cut where it comes out from under its folder. The
/// `ghosts` of rows that left hang from it, in the gap below.
fn placed(element: AnyElement, place: Place, ghosts: Vec<AnyElement>) -> AnyElement {
    let row = f32::from(ROW_HEIGHT);
    let slot = div().relative().w_full().h(ROW_HEIGHT).children(ghosts);
    let inner = |element: AnyElement, top: f32| {
        div()
            .absolute()
            .top(px(top)) // design: runtime geometry from the fold
            .left_0()
            .w_full()
            .h(ROW_HEIGHT)
            .when(place.opacity < 1., |inner| inner.opacity(place.opacity))
            .occlude()
            .child(element)
    };
    match place.clip {
        None => slot.child(inner(element, -place.shift)).into_any_element(),
        Some(clip) => {
            // The window of the slot's row that shows: from the folder's edge down to the row's bottom.
            let shown = row - place.shift - clip;
            if shown < 0.5 {
                return slot.into_any_element();
            }
            slot.child(
                div()
                    .absolute()
                    .top(px(clip)) // design: runtime geometry from the fold
                    .left_0()
                    .w_full()
                    .h(px(shown)) // design: runtime geometry from the fold
                    .overflow_hidden()
                    .child(inner(element, -place.shift - clip)),
            )
            .into_any_element()
        }
    }
}

/// The rows Command- and Shift-click picked, by id, and their paths in row order: a picked name
/// keeps the selected ink, and a drag from any of them carries them all.
#[derive(Clone, Default)]
pub struct PickedRows {
    pub ids: Rc<HashSet<SharedString>>,
    pub paths: Rc<[PathBuf]>,
}

/// One row of the tree: a folder chevron, an icon, the name and the unsaved dot,
/// or a placeholder line for a folder that is loading or empty. `fold` is where the tree's folds
/// put it, if they do, and `level` how far its name has brightened toward the primary ink. A row in `ignored`
/// (shown only because Show ignored files is on) draws dimmed, as a dot file does.
pub fn row(
    ix: usize,
    entry: &TreeEntry,
    marked: &HashSet<PathBuf>,
    ignored: &HashSet<PathBuf>,
    ink: Ink,
    editor: &WeakEntity<Explorer>,
    hits: Option<&filter::Hits>,
    edit: Option<&Edit>,
    drop_target: Option<&PathBuf>,
    mut fold: FoldView,
    level: f32,
    git: &super::git::Statuses,
    picked: &PickedRows,
) -> AnyElement {
    let place = fold.place;
    let leaving = std::mem::take(&mut fold.leaving);
    let element = plain(ix, entry, marked, ignored, ink, editor, hits, edit, drop_target, fold, level, git, picked);
    match place {
        Some(place) => {
            let ghosts = leaving.into_iter().map(|leaving| ghost_row(&leaving, place.shift, ink)).collect();
            placed(element, place, ghosts)
        }
        None => element,
    }
}

/// A row that left the tree, drawn as the gap under its neighbour closes: the row keeps its bottom at the gap's
/// bottom and slides up under the row above, cut at the gap's top, fading. Plain fills, no hover and no click.
fn ghost_row(leaving: &Leaving, shift: f32, ink: Ink) -> AnyElement {
    let row = f32::from(ROW_HEIGHT);
    let hidden = leaving.dim;
    let rest = rest_ink(&ink, hidden);
    let icon = icon_for(&leaving.label, leaving.is_folder, &ink, leaving.status);
    let content = div()
        .absolute()
        .top(px(leaving.height - row)) // design: runtime geometry from the fold
        .left_0()
        .w_full()
        .h(ROW_HEIGHT)
        .pl(ROW_LEADING + ROW_INDENT * leaving.depth as f32)
        .pr(ROW_TRAILING)
        .flex()
        .items_center()
        .text_size(TEXT_BODY)
        .line_height(TEXT_BODY_LINE)
        .child(div().w(CHEVRON_SLOT).h_full().flex_none())
        .child(div().flex_none().when(hidden, |icon| icon.opacity(ALPHA_HIDDEN_ICON)).child(icon))
        .child(name_label(&leaving.label, leaving.is_folder, rest, None));
    div()
        .absolute()
        .top(px(leaving.top - shift)) // design: runtime geometry from the fold
        .left_0()
        .w_full()
        .h(px(leaving.height)) // design: runtime geometry from the fold
        .overflow_hidden()
        .opacity(leaving.opacity)
        .child(content)
        .into_any_element()
}

/// A row as it stands in its slot: see `row`.
fn plain(
    ix: usize,
    entry: &TreeEntry,
    marked: &HashSet<PathBuf>,
    ignored: &HashSet<PathBuf>,
    ink: Ink,
    editor: &WeakEntity<Explorer>,
    hits: Option<&filter::Hits>,
    edit: Option<&Edit>,
    drop_target: Option<&PathBuf>,
    fold: FoldView,
    level: f32,
    git: &super::git::Statuses,
    picked: &PickedRows,
) -> AnyElement {
    let id = &entry.item().id;
    let indent = ROW_LEADING + ROW_INDENT * entry.depth() as f32;
    if id.ends_with(PLACEHOLDER_LOADING) || id.ends_with(PLACEHOLDER_EMPTY) {
        return div()
            .h(ROW_HEIGHT)
            .pl(indent + CHEVRON_SLOT + ICON + ROW_GAP)
            .flex()
            .items_center()
            .text_size(TEXT_SUBHEADLINE)
            .line_height(TEXT_SUBHEADLINE_LINE)
            .text_color(ink.muted)
            .font_features(crate::tokens::tabular())
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(entry.item().label.clone()))
            .into_any_element();
    }

    let is_folder = entry.is_folder();
    // How far the chevron has turned, 0 pointing right to 1 pointing down: with its folder's fold, else at rest.
    let turn = fold.turn.unwrap_or(if entry.is_expanded() { 1. } else { 0. });
    let path = PathBuf::from(id.to_string());
    let is_marked = marked.contains(&path);
    let label = entry.item().label.clone();
    let hidden = label.starts_with('.') || ignored.contains(&path);
    // Git's status shows on the icon, never on the name: a name rests in the plain ink (the hidden dimming kept).
    let status = git.get(&path);
    let icon = tree_icon(&label, is_folder, ink, status, &path);
    let rest = rest_ink(&ink, hidden);
    // A name brightens to the primary ink as the selection glides in, and dims back as it leaves;
    // a picked row's name stays bright.
    let is_picked = picked.ids.contains(id);
    let level = if is_picked { 1. } else { level };
    let name_ink = if level >= 1. { ink.primary } else { crate::tab_bar::mix(rest, ink.primary, level) };
    // While a filter shows, every name is one step dimmer and the matched letters draw bold at full ink.
    let marks = hits.map(|hits| (hits.get(id).map_or(&[][..], Vec::as_slice), ink.primary));
    let name_ink = if marks.is_some() { name_ink.opacity(ALPHA_NAME_DIM) } else { name_ink };
    let editing_here = edit.filter(|edit| edit.path == path);
    let name = match editing_here {
        // The field fills the row less its inset, its fill reaching a half gap left of the text, and
        // the text starts exactly where the plain label's does.
        Some(edit) => {
            let (pad, fill) = (ROW_GAP / 2., if ink.light { ALPHA_FIELD_LIGHT } else { ALPHA_FIELD });
            let input = edit.input.clone();
            // A refused name rings its field in the card's danger colour, from the frame the card starts in.
            let danger = edit.error.is_some().then_some(edit.danger);
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h(ROW_HEIGHT - FIELD_INSET * 2.)
                .ml(pad)
                .px(pad)
                .flex()
                .items_center()
                .rounded(RADIUS_FIELD)
                .bg(gpui::white().opacity(fill))
                // The focus ring is drawn inside the field's edge (the row leaves 1 pt around it), and only while the window is key.
                .child(
                    canvas(
                        move |bounds, window, cx| {
                            let focused = control::window_key(window) && input.focus_handle(cx).contains_focused(window, cx);
                            (bounds, filter::field_focus(input.entity_id(), focused, window), danger.unwrap_or_else(|| cx.theme().ring))
                        },
                        |_, (bounds, level, ring), window, _| filter::paint_inset_ring(window, bounds, RADIUS_FIELD, level, ring),
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(
                    Input::new(&edit.input)
                        .xsmall()
                        .appearance(false)
                        .bordered(false)
                        .focus_bordered(false)
                        .px_0()
                        .text_size(TEXT_BODY)
                        .line_height(TEXT_BODY_LINE)
                        .w_full(),
                )
                .into_any_element()
        }
        None => {
            let breath = (marks.is_none() && copying(&path)).then(|| Breath { path: path.clone(), rest, lit: ink.primary, level });
            name_label_breathing(&label, is_folder, name_ink, marks, breath).into_any_element()
        }
    };
    // A name that was refused: the reason, on a floating card under the field.
    let note = editing_here.map(|edit| RefusedCard {
        error: edit.error.clone(),
        danger: edit.danger,
        ink: ink.text,
        light: ink.light,
        // The card starts where the field's fill does: the icon's gap less the field's left margin.
        left: indent + CHEVRON_SLOT + ICON + ROW_GAP / 2.,
    });
    let editor = editor.clone();
    // Dropped files go into this folder, or into the folder of this file.
    let drop_dir = if is_folder { path.clone() } else { path.parent().map_or_else(PathBuf::new, Into::into) };
    let rect_id = control::rects_enabled().then(|| format!("row:{}", path.display()));
    let tip_id = id.clone();
    let tip_ignored = ignored.contains(&path);
    let key = id.clone();

    let accent = ink.accent;
    let washed = drop_target == Some(&path) && is_folder;
    let wash_path = is_folder.then(|| path.clone());
    // A file that arrived from outside washes in the selection's ink, at its unfocused alpha.
    let arrive = arriving(&path).then(|| path.clone());
    let (wash_ink, wash_alpha) = (ink.text, if ink.light { ALPHA_SELECTED_UNFOCUSED_LIGHT } else { ALPHA_SELECTED_UNFOCUSED });
    div()
        .id(("explorer-row", ix))
        .relative()
        .w_full()
        .h(ROW_HEIGHT)
        .pl(indent)
        .pr(ROW_TRAILING)
        // The row whose name field is open shows no path tooltip: the field is what you are using.
        .when(editing_here.is_none(), |row| row.tooltip(move |window, cx| {
            // Built when the tip shows, not per frame.
            crate::tooltip::tip(row_tip(&tip_id, crate::pane::home_folder(), tip_ignored))(window, cx)
        }).tooltip_show_delay(crate::tooltip::DELAY))
        .flex()
        .items_center()
        .rounded(RADIUS_ROW)
        .text_size(TEXT_BODY)
        .line_height(TEXT_BODY_LINE)
        .text_color(ink.primary)
        .on_hover(move |hovered, window, cx| {
            let id = window.window_handle().window_id();
            let changed = SHAPES.with_borrow_mut(|all| {
                let shapes = all.entry(id).or_default();
                let view = shapes.view;
                let next = if *hovered {
                    Some(key.clone())
                } else if shapes.hovered.as_ref() == Some(&key) {
                    None
                } else {
                    return None;
                };
                let changed = shapes.hovered != next;
                shapes.hovered = next;
                changed.then_some(view).flatten()
            });
            // Only the shape layer redraws: the rows do not change with the pointer.
            if let Some(view) = changed {
                cx.notify(view);
            }
        })
        .when(rect_id.is_some() || wash_path.is_some() || arrive.is_some(), |row| {
            row.child(
                canvas(
                    move |bounds, window, _| {
                        if let Some(rect_id) = rect_id {
                            control::record(window, rect_id, bounds);
                        }
                        (
                            wash_path.as_ref().map_or(0., |path| wash_level(window, path, washed)),
                            arrive.as_ref().map_or(0., |path| arrival_level(window, path)),
                        )
                    },
                    move |bounds, (drop, arrival), window, _| {
                        let wash = Bounds::new(
                            point(bounds.origin.x, bounds.origin.y + ROW_WASH_INSET_Y),
                            size(bounds.size.width, bounds.size.height - ROW_WASH_INSET_Y * 2.),
                        );
                        if drop > 0. {
                            window.paint_quad(gpui::fill(wash, accent.opacity(ALPHA_DROP * drop)).corner_radii(Corners::all(RADIUS_ROW)));
                        }
                        if arrival > 0. {
                            window.paint_quad(gpui::fill(wash, wash_ink.opacity(wash_alpha * arrival)).corner_radii(Corners::all(RADIUS_ROW)));
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
        })
        .child(
            // The leading slot is the folder's hit area; a child's slot sits under its parent's icon.
            // The chevron rests on the slot's leading edge, the explorer's one left edge (`SIDEBAR_EDGE`).
            div()
                .w(CHEVRON_SLOT)
                .h_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_start()
                .when(is_folder, |slot| {
                    slot.child(div().ml(-CHEVRON_BEARING).when(hidden, |chevron| chevron.opacity(ALPHA_HIDDEN_ICON)).child(icons::chevron(turn).size(CHEVRON).text_color(ink.muted)))
                }),
        )
        .child(
            div()
                .flex_none()
                .when(hidden, |icon| icon.opacity(ALPHA_HIDDEN_ICON))
                .child(icon),
        )
        .child(name)
        .children(note)
        .when(editing_here.is_none(), |row| {
            // A picked row's drag carries every picked row, under a card that counts them.
            let dragged: Rc<[PathBuf]> = if is_picked { picked.paths.clone() } else { Rc::from([path.clone()]) };
            let drag_label: SharedString = match dragged.len() {
                1 => label.clone(),
                n => format!("{n} items").into(),
            };
            let (drag_editor, drag_paths) = (editor.clone(), dragged.clone());
            row.on_drag(DraggedFiles(dragged), move |_, offset, _, cx| {
                drag_editor.update(cx, |this, cx| this.set_drop_target(None, cx)).ok();
                let (card, border) = crate::tooltip::float_fill(cx.theme().background, ink.text, ink.light);
                let green = cx.theme().green;
                cx.new(|_| DragPreview::new(drag_label.clone(), is_folder, offset, ink.primary, card, border, ink.light, green))
            })
            // Out of the window the drag becomes the system's file drag.
            .external_drag_payload::<DraggedFiles>(move |_, _, _| {
                Some(ExternalDragPayload::Files(FileDragPaths::new(drag_paths.iter().map(|path| (path.clone(), path.is_dir())))))
            })
        })
        // Files dragged over a row go into its folder (a file's own folder): a later row's claim
        // replaces the tree's, so the one under the pointer wins.
        .on_drag_move::<DraggedFiles>({
            let (editor, dir) = (editor.clone(), drop_dir.clone());
            move |event, _, cx| {
                if event.bounds.contains(&event.event.position) {
                    editor.update(cx, |this, cx| this.set_drop_target(Some(dir.clone()), cx)).ok();
                }
            }
        })
        .on_drag_move::<ExternalPaths>({
            let (editor, dir) = (editor.clone(), drop_dir.clone());
            move |event, _, cx| {
                if event.bounds.contains(&event.event.position) {
                    editor.update(cx, |this, cx| this.set_drop_target(Some(dir.clone()), cx)).ok();
                }
            }
        })
        .on_drag_move::<crate::text_drag::DraggedText>({
            let (editor, dir) = (editor.clone(), drop_dir.clone());
            move |event, _, cx| {
                if event.bounds.contains(&event.event.position) {
                    editor.update(cx, |this, cx| this.set_drop_target(Some(dir.clone()), cx)).ok();
                }
            }
        })
        .on_drop::<crate::text_drag::DraggedText>({
            let (editor, dir) = (editor.clone(), drop_dir.clone());
            move |dropped, window, cx| {
                let dropped = dropped.clone();
                editor.update(cx, |this, cx| this.drop_text(dir.clone(), dropped, window, cx)).ok();
            }
        })
        .on_drop::<DraggedFiles>({
            let (editor, dir) = (editor.clone(), drop_dir.clone());
            move |dragged, window, cx| {
                let (paths, copy) = (dragged.0.to_vec(), window.modifiers().alt);
                editor.update(cx, |this, cx| this.drop_onto(paths, dir.clone(), copy, window, cx)).ok();
            }
        })
        .on_drop::<ExternalPaths>({
            let (editor, dir) = (editor.clone(), drop_dir.clone());
            move |dropped, window, cx| {
                let paths = dropped.paths().to_vec();
                editor.update(cx, |this, cx| this.drop_onto(paths, dir.clone(), true, window, cx)).ok();
            }
        })
        .on_mouse_down(MouseButton::Right, {
            let (editor, path) = (editor.clone(), path.clone());
            move |_, window, cx| {
                // This row's menu, not the empty space's.
                cx.stop_propagation();
                editor.update(cx, |this, cx| this.context_menu(Some(path.clone()), window, cx)).ok();
            }
        })
        // The unsaved dot fades in and out; a row that lost its mark keeps the dot until the fade lands.
        .when(is_marked || mark_pending(&path), |row| {
            let (dot_path, color) = (path.clone(), ink.muted);
            row.child(
                canvas(
                    move |_, window, _| mark_level(window, &dot_path, is_marked),
                    move |bounds, level, window, _| {
                        if level > 0. {
                            window.paint_quad(gpui::fill(bounds, color.opacity(level)).corner_radii(Corners::all(bounds.size.width / 2.)));
                        }
                    },
                )
                .size(DOT)
                .flex_none(),
            )
        })
        .when(editing_here.is_none(), |row| {
            row.on_click(move |event, _, cx| {
                let modifiers = event.modifiers();
                // Command- and Shift-click pick rows, as in Finder: nothing opens, and the tree's
                // own click (select, open or shut a folder) doesn't run.
                if modifiers.platform || modifiers.shift {
                    cx.stop_propagation();
                    editor.update(cx, |this, cx| this.pick(&path, modifiers.shift, cx)).ok();
                    return;
                }
                // Option-click on a folder opens or shuts it and every folder inside, as in Finder.
                if modifiers.alt && is_folder {
                    cx.stop_propagation();
                    editor.update(cx, |this, cx| this.toggle_subtree(&path, cx)).ok();
                    return;
                }
                // Option-click opens the file beside the one showing; a double-click keeps the tab a
                // single click opened as a preview.
                let path = path.clone();
                let event = if event.modifiers().alt {
                    ExplorerEvent::OpenToSide(path)
                } else if event.click_count() >= 2 {
                    ExplorerEvent::Open(path)
                } else {
                    ExplorerEvent::Preview(path)
                };
                editor
                    .update(cx, |this, cx| {
                        this.unpick(cx);
                        if !is_folder {
                            // The click is the user's own pick: a Save As follow still waiting for its row is let go.
                            this.follow_pending = None;
                            cx.emit(event);
                        }
                    })
                    .ok();
            })
        })
        .into_any_element()
}

/// A floating card on the shared float plane, with the given corner `radius`. `held` is a card the
/// pointer carries (the drag card), which stands one plane above and keeps the deeper lift shadow.
fn float_card(radius: f32, fill: Hsla, border: Hsla, light: bool, held: bool) -> Div {
    crate::tooltip::float(radius, fill, border, light, held)
}

/// What follows the pointer while a row is dragged: the row's icon and name on a small card.
pub(crate) struct DragPreview {
    label: SharedString,
    is_folder: bool,
    /// Where the pointer took hold of the row, so the card sits just below and right of it.
    offset: Point<Pixels>,
    text: Hsla,
    card: Hsla,
    border: Hsla,
    light: bool,
    /// The copy badge's green.
    green: Hsla,
    /// How much of the copy badge shows: up while Option is held, down when it is let go.
    copy: Fade,
}

impl DragPreview {
    fn new(label: SharedString, is_folder: bool, offset: Point<Pixels>, text: Hsla, card: Hsla, border: Hsla, light: bool, green: Hsla) -> Self {
        Self { label, is_folder, offset, text, card, border, light, green, copy: Fade::default() }
    }
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The card's name is at the primary ink, so its symbol follows that.
        let icon = if self.is_folder { icons::folder(f32::from(ICON)) } else { icons::file(&self.label, f32::from(ICON), self.text) };
        // Option copies on drop, so the badge follows it; it fades on the shared fade, turning around from where it is drawn.
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        self.copy.aim(f32::from(window.modifiers().alt), now);
        let level = self.copy.level(now);
        if self.copy.is_moving(now) {
            window.request_animation_frame();
        }
        let green = self.green;
        div().pl(self.offset.x + ROW_GAP).pt(self.offset.y + ROW_GAP).child(
            float_card(RADIUS_FLOAT_CARD, self.card, self.border, self.light, true)
                .h(ROW_HEIGHT)
                .max_w(px(TOOLTIP_MAX_W))
                .px(ROW_GAP)
                .flex()
                .items_center()
                .text_color(self.text)
                .text_size(TEXT_BODY)
                .line_height(TEXT_BODY_LINE)
                .child(icon)
                // A long name is cut in the middle, keeping its extension, as in the tree; the label's own left pad is the gap after the icon.
                .child(name_label(&self.label, self.is_folder, self.text, None))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, (), window, _| {
                            // A held Option nobody moves the mouse under is a change no event redraws for: ask for the frame.
                            window.on_modifiers_changed(|_, window, _| window.refresh());
                            if level > 0. {
                                window.paint_quad(gpui::fill(bounds, green.opacity(level)).corner_radii(Corners::all(bounds.size.width / 2.)));
                                let [arm, stroke] = DRAG_BADGE_PLUS;
                                let (arm, stroke) = (px(arm), px(stroke));
                                let centre = bounds.center();
                                let ink = gpui_kit::white().opacity(level);
                                window.paint_quad(gpui::fill(Bounds::centered_at(centre, size(arm, stroke)), ink));
                                window.paint_quad(gpui::fill(Bounds::centered_at(centre, size(stroke, arm)), ink));
                            }
                        },
                    )
                    .absolute()
                    .top(-DRAG_BADGE_OVERHANG)
                    .right(-DRAG_BADGE_OVERHANG)
                    .size(DRAG_BADGE),
                ),
        )
    }
}

/// The card a file's drag shows under the pointer (`offset` is where it took hold), for a drag that starts outside the tree, like a tab's icon.
pub(crate) fn file_drag(path: &std::path::Path, offset: Point<Pixels>, cx: &mut App) -> Entity<DragPreview> {
    let label = SharedString::from(path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |name| name.to_string_lossy().into_owned()));
    let light = crate::theme::is_light(cx);
    let text = crate::theme::ink(crate::tokens::ALPHA_PRIMARY, cx);
    let (card, border) = crate::tooltip::float_fill(cx.theme().background, cx.theme().sidebar_foreground, light);
    let green = cx.theme().green;
    cx.new(|_| DragPreview::new(label, path.is_dir(), offset, text, card, border, light, green))
}

/// The card a held file drag shows (the control socket's `drag-hold`), as a real drag would.
pub(crate) fn drag_card(path: &std::path::Path, cx: &mut App) -> AnyView {
    file_drag(path, Point::default(), cx).into()
}

/// A refused name's card on its way in or out: `to` is 1 while an error shows, 0 once it clears. The
/// fade is `OVERLAY_ENTER` long going in and `OVERLAY_EXIT` going out.
#[derive(Clone)]
struct CardFade {
    /// The reason last shown, kept so the card can fade out saying it.
    text: SharedString,
    to: f32,
    fade: Fade,
}

impl CardFade {
    /// Heads for `to` from the level drawn at `now`, on the length of that direction.
    fn turned(text: SharedString, previous: &Fade, to: f32, now: Instant) -> Self {
        let mut fade = Fade::with(previous.level(now), if to > 0. { OVERLAY_ENTER } else { OVERLAY_EXIT });
        fade.aim(to, now);
        Self { text, to, fade }
    }
}

/// The reason a name was refused, on a floating card under the field. It fades in with the
/// tooltip's 2 pt settle and fades out saying what it said, turning around from where it is drawn.
/// The reason wraps to as many lines as it needs and the card grows down; it keeps its words while it
/// fades out, so it stays one size.
#[derive(IntoElement)]
struct RefusedCard {
    error: Option<SharedString>,
    danger: Hsla,
    ink: Hsla,
    light: bool,
    left: Pixels,
}

impl RenderOnce for RefusedCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let state = window.use_keyed_state("refused-card", cx, |_, _| CardFade { text: SharedString::default(), to: 0., fade: Fade::with(0., OVERLAY_ENTER) });
        let mut card = state.read(cx).clone();
        let to = if self.error.is_some() { 1. } else { 0. };
        let text = self.error.clone().unwrap_or_else(|| card.text.clone());
        if card.to != to || card.text != text {
            // A new direction starts from where the card is drawn; new words keep the fade as it is.
            card = if card.to != to { CardFade::turned(text, &card.fade, to, now) } else { CardFade { text, ..card } };
            state.update(cx, |state, _| *state = card.clone());
        }
        let level = card.fade.level(now);
        if card.fade.is_moving(now) {
            window.request_animation_frame();
        }
        if level <= 0. && to == 0. {
            return div().into_any_element();
        }
        let (fill, border) = crate::tooltip::float_fill(cx.theme().background, self.ink, self.light);
        let RefusedCard { danger, light, left, .. } = self;
        let reason = card.text;
        // The card is laid out as a root of its own, so its height is known before it is placed: it
        // opens below the row, and flips above it, with the same settle mirrored, when the window's
        // room under the row is less than the card and there is room above. It is drawn in a
        // `deferred` so the list's clip does not cut it.
        deferred(
            canvas(
                move |bounds, window, cx| {
                    let width = bounds.size.width - left - ROW_TRAILING;
                    let mut card = div()
                        .w(width)
                        .opacity(level)
                        .child(
                            float_card(f32::from(RADIUS_FIELD), fill, border, light, false)
                                .min_h(TEXT_SUBHEADLINE_LINE + ROW_GAP)
                                .px(ROW_GAP)
                                .py(ROW_GAP / 2.)
                                .flex()
                                .items_start()
                                .gap(ROW_GAP / 2.)
                                .text_color(danger)
                                .text_size(TEXT_SUBHEADLINE)
                                .line_height(TEXT_SUBHEADLINE_LINE)
                                // The icon centres on the first line; the reason wraps in full under it.
                                .child(div().flex_none().h(TEXT_SUBHEADLINE_LINE).flex().items_center().child(symbols::icon("exclamationmark.triangle.fill", ERROR_SYMBOL)))
                                .child(div().flex_1().min_w_0().child(reason)),
                        )
                        .into_any_element();
                    let height = card.layout_as_root(gpui::size(gpui::AvailableSpace::Definite(width), gpui::AvailableSpace::MinContent), window, cx).height;
                    let rise = px(TOOLTIP_RISE * (1. - level));
                    let above = height > window.viewport_size().height - bounds.bottom() && bounds.top() >= height;
                    let top = if above { bounds.top() - height - rise } else { bounds.bottom() + rise };
                    card.prepaint_at(gpui::point(bounds.left() + left, top), window, cx);
                    card
                },
                |_, mut card, window, cx| card.paint(window, cx),
            )
            .absolute()
            .inset_0(),
        )
        .with_priority(1)
        .into_any_element()
    }
}

/// A row's name, drawn as one run: a file's stem cut in the middle with "…" when the width is short,
/// keeping its start, its last two characters and the extension ("caret_l…er.rs"). A folder or a name with no
/// extension is cut at its end. The tab bar's titles use it too. `marks` is the filter's matched
/// characters (by index in the whole name) and the full ink they draw bold in.
pub(crate) fn name_label(label: &SharedString, is_folder: bool, ink: Hsla, marks: Option<(&[usize], Hsla)>) -> Div {
    name_label_breathing(label, is_folder, ink, marks, None)
}

/// `name_label` for a row whose file is being copied (`breath` makes the ink ease up and down while it lasts).
fn name_label_breathing(label: &SharedString, is_folder: bool, ink: Hsla, marks: Option<(&[usize], Hsla)>, breath: Option<Breath>) -> Div {
    let dot = if is_folder { None } else { label.rfind('.').filter(|&dot| dot > 0) };
    let marks = marks.map(|(at, full)| (at.to_vec(), full));
    div().flex_1().min_w_0().pl(ROW_GAP).flex().text_color(ink).child(NameText { text: label.clone(), dot, marks, breath, path: false })
}

/// A folder path, drawn as one run and cut in the middle when the width is short: the top folder, "…",
/// and as many of the last folders as fit ("archive/…/2025-final/appendix-notes"). Open Quickly's rows use it.
pub(crate) fn path_label(path: &SharedString, ink: Hsla) -> Div {
    div().flex_1().min_w_0().pl(ROW_GAP).flex().text_color(ink).child(NameText { text: path.clone(), dot: None, marks: None, breath: None, path: true })
}

/// A name's text as an element that cuts itself to the width layout gives it.
struct NameText {
    text: SharedString,
    /// Where the extension starts (the last dot's byte index), if the name has one.
    dot: Option<usize>,
    marks: Option<(Vec<usize>, Hsla)>,
    breath: Option<Breath>,
    /// A folder path (cut between its folders), not a name.
    path: bool,
}

/// What a name needs to be cut and drawn, read from the text style where it sits.
struct NameSpec {
    text: SharedString,
    dot: Option<usize>,
    marks: Option<(Vec<usize>, Hsla)>,
    breath: Option<Breath>,
    path: bool,
    style: TextStyle,
    font_size: Pixels,
    line_height: Pixels,
}

/// How a name was cut at a width: the text and the matched characters' places in it.
type Cut = (SharedString, Vec<usize>);

/// The most cuts kept. Past it the cache drops the cuts not used in the last two frames; a width glide
/// cuts every name at a new width each frame, so the cache never restarts mid-drag.
const CUTS_KEPT: usize = 2048;

/// The cuts by name, width, font size and weight, and matched letters, so a frame only measures names it
/// hasn't cut before. Each cut carries the frame it was last used in, by the motion clock's reading.
struct CutCache {
    cuts: HashMap<(SharedString, u32, u32, u32, u64), (Cut, Instant)>,
    /// The newest frame stamp seen, and the one before it.
    frame: Instant,
    before: Instant,
}

impl CutCache {
    fn new() -> Self {
        let now = Instant::now();
        Self { cuts: HashMap::new(), frame: now, before: now }
    }

    /// Marks `now` as the frame in use: a newer reading starts a new frame.
    fn tick(&mut self, now: Instant) {
        if now > self.frame {
            self.before = self.frame;
            self.frame = now;
        }
    }

    fn get(&mut self, key: &(SharedString, u32, u32, u32, u64), now: Instant) -> Option<Cut> {
        self.tick(now);
        let frame = self.frame;
        self.cuts.get_mut(key).map(|(cut, used)| {
            *used = frame;
            cut.clone()
        })
    }

    fn insert(&mut self, key: (SharedString, u32, u32, u32, u64), cut: Cut, now: Instant, kept: usize) {
        self.tick(now);
        if self.cuts.len() >= kept {
            let before = self.before;
            self.cuts.retain(|_, (_, used)| *used >= before);
            // Two frames of names past the cap are not a name list: start over rather than grow.
            if self.cuts.len() >= kept {
                self.cuts.clear();
            }
        }
        self.cuts.insert(key, (cut, self.frame));
    }
}

thread_local! {
    static CUTS: RefCell<CutCache> = RefCell::new(CutCache::new());
}

impl NameSpec {
    /// The runs of `text`: the style as it is (or in `tint`), with `marks` (character indexes) bold in the full ink.
    fn runs(&self, text: &str, marks: &[usize], tint: Option<Hsla>) -> Vec<TextRun> {
        let full = self.marks.as_ref().map(|(_, full)| *full);
        let mut out: Vec<TextRun> = Vec::new();
        let mut last_bold = false;
        let mut matched = marks.iter().copied().peekable();
        for (ix, ch) in text.chars().enumerate() {
            let bold = matched.next_if_eq(&ix).is_some();
            match out.last_mut() {
                Some(last) if last_bold == bold => last.len += ch.len_utf8(),
                _ => {
                    let mut run = self.style.to_run(ch.len_utf8());
                    if let Some(tint) = tint {
                        run.color = tint;
                    }
                    if let (true, Some(full)) = (bold, full) {
                        run.font.weight = FontWeight::BOLD;
                        run.color = full;
                    }
                    out.push(run);
                }
            }
            last_bold = bold;
        }
        out
    }

    fn shape(&self, text: SharedString, marks: &[usize], tint: Option<Hsla>, window: &Window) -> ShapedLine {
        let runs = self.runs(&text, marks, tint);
        window.text_system().shape_line(text, self.font_size, &runs, None)
    }

    /// The name cut to fit `width`: whole if it fits, else the fewest stem characters dropped so
    /// stem start, "…", the stem's last characters and extension fit, so "…" never touches the dot.
    fn cut(&self, width: Pixels, window: &Window) -> Cut {
        let marks = self.marks.as_ref().map_or(&[][..], |(at, _)| at.as_slice());
        let mut hasher = DefaultHasher::new();
        marks.hash(&mut hasher);
        self.path.hash(&mut hasher);
        let key = (self.text.clone(), f32::from(width).to_bits(), f32::from(self.font_size).to_bits(), self.style.font_weight.0.to_bits(), hasher.finish());
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        if let Some(cut) = CUTS.with_borrow_mut(|cuts| cuts.get(&key, now)) {
            return cut;
        }
        let cut = self.cut_uncached(marks, width, window);
        CUTS.with_borrow_mut(|cuts| cuts.insert(key, cut.clone(), now, CUTS_KEPT));
        cut
    }

    fn cut_uncached(&self, marks: &[usize], width: Pixels, window: &Window) -> Cut {
        if self.path {
            return (cut_path_to_fit(&self.text, &|text| self.shape(text.clone(), &[], None, window).width <= width), Vec::new());
        }
        cut_to_fit(&self.text, self.dot, marks, &|text, moved| self.shape(text.clone(), moved, None, window).width <= width)
    }
}

/// A folder path cut until `fits` says it does: whole if it fits, else its top folder, "…" and the most
/// of the last folders that fit ("archive/…/2025-final/appendix-notes"); when not even the top folder and
/// the last one fit, "…" and the last folders alone, and at the narrowest the path's last characters.
fn cut_path_to_fit(path: &SharedString, fits: &dyn Fn(&SharedString) -> bool) -> SharedString {
    if fits(path) {
        return path.clone();
    }
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len() - 1;
    let joined = |top: Option<&str>, from: usize| SharedString::from(match top {
        Some(top) => format!("{top}/\u{2026}/{}", parts[from..].join("/")),
        None => format!("\u{2026}/{}", parts[from..].join("/")),
    });
    // Folders in the middle go first, one at a time from the top; the top folder goes last.
    for (top, from) in (2..=last).map(|from| (Some(parts[0]), from)).chain((1..=last).map(|from| (None, from))) {
        let cut = joined(top, from);
        if fits(&cut) {
            return cut;
        }
    }
    // One folder name wider than the room: its last characters.
    let chars = path.chars().count();
    let tail = |count: usize| SharedString::from(format!("\u{2026}{}", path.chars().skip(chars - count).collect::<String>()));
    let (mut low, mut high) = (0, chars);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(&tail(mid)) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    tail(low)
}

/// `text` cut until `fits` says it does: whole if it fits, else the stem cut in the middle with "…",
/// keeping the extension and the stem's last `TAIL_KEPT` characters (one more when those only echo the
/// extension's letters, so "…rs.rs" never shows), so "…" never sits against the
/// dot. With matched letters (`marks`, by character index) the first matched run stays whole: the
/// stem keeps its start and cuts after the run, or, when that is too wide, drops its start with a
/// leading "…" and keeps the run. The marks come back at their places in the cut text.
fn cut_to_fit(text: &SharedString, dot: Option<usize>, marks: &[usize], fits: &dyn Fn(&SharedString, &[usize]) -> bool) -> Cut {
    if fits(text, marks) {
        return (text.clone(), marks.to_vec());
    }
    let (stem, extension) = text.split_at(dot.unwrap_or(text.len()));
    let stem_chars = stem.chars().count();
    // The stem's last characters kept after the cut, only before an extension; a very short stem keeps fewer.
    let mut keep = if dot.is_some() { TAIL_KEPT.min(stem_chars / 2) } else { 0 };
    // A kept tail that only repeats the extension's last letters ("…rs.rs") reads as a second extension:
    // keep one more stem character ("…ers.rs").
    if keep > 0 && keep < stem_chars {
        let tail: String = stem.chars().skip(stem_chars - keep).collect::<String>().to_lowercase();
        let letters = extension.trim_start_matches('.').to_lowercase();
        if letters.ends_with(&tail) {
            keep = (letters.chars().count() + 1).min(stem_chars / 2).max(keep);
        }
    }
    let tail_from = stem_chars - keep;
    let byte = |ix: usize| stem.char_indices().nth(ix).map_or(stem.len(), |(byte, _)| byte);
    // Where the stem's head part ends once `end` is asked for: a cut that leaves only the kept tail
    // out is no cut, so the whole stem shows.
    let cuts_tail = |end: usize| keep > 0 && end < tail_from;
    // The stem's characters `lead..end`, "…" on each side that was cut, the kept tail, and the marks that survive.
    let candidate = |lead: usize, end: usize| -> Cut {
        let head = usize::from(lead > 0);
        let (shown, tail) = if cuts_tail(end) { (end, 1) } else if keep > 0 { (stem_chars, 0) } else { (end, usize::from(end < stem_chars)) };
        let kept = if tail == 1 && keep > 0 { &stem[byte(tail_from)..] } else { "" };
        let cut = SharedString::from(format!(
            "{}{}{}{kept}{extension}",
            if head == 1 { "\u{2026}" } else { "" },
            &stem[byte(lead)..byte(shown)],
            if tail == 1 { "\u{2026}" } else { "" }
        ));
        let after = head + (shown - lead) + tail + kept.chars().count();
        let moved = marks
            .iter()
            .filter_map(|&ix| {
                if (lead..shown).contains(&ix) {
                    Some(ix - lead + head)
                } else if tail == 1 && keep > 0 && (tail_from..stem_chars).contains(&ix) {
                    Some(ix - tail_from + head + (shown - lead) + 1)
                } else if ix >= stem_chars {
                    Some(ix - stem_chars + after)
                } else {
                    None
                }
            })
            .collect();
        (cut, moved)
    };
    let fit = |lead: usize, end: usize| {
        let (cut, moved) = candidate(lead, end);
        fits(&cut, &moved)
    };
    // The most stem characters that fit after dropping `lead`; none still draws "…" and the extension.
    let widest = |lead: usize| {
        let (mut low, mut high) = (lead, stem_chars);
        while low < high {
            let mid = (low + high).div_ceil(2);
            if fit(lead, mid) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        low
    };
    // The first matched run in the stem: where it starts and where it ends.
    let run = marks.iter().copied().find(|&ix| ix < stem_chars).map(|start| {
        let mut end = start + 1;
        while marks.contains(&end) && end < stem_chars {
            end += 1;
        }
        (start, end)
    });
    let end = widest(0);
    let shown = if cuts_tail(end) { end } else if keep > 0 { stem_chars } else { end };
    let Some((start, run_end)) = run.filter(|&(start, run_end)| start > 0 && shown < run_end && !(keep > 0 && start >= tail_from && cuts_tail(end))) else {
        return candidate(0, end);
    };
    // Cutting after the stem's start would hide the run. When the stem's end fits with the run still
    // in it, drop only the leading characters ("…ed-drafts-2025"): one cut, the match and the name's
    // end both readable. Else drop the fewest leading characters that keep all of the run and cut
    // after it; a run wider than the name keeps its start.
    let least_lead = |last: usize, whole: usize| {
        let (mut low, mut high) = (1, last);
        while low < high {
            let mid = (low + high) / 2;
            if fit(mid, whole) {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        low
    };
    if start > 0 && fit(start, stem_chars) {
        return candidate(least_lead(start, stem_chars), stem_chars);
    }
    // The start kept and one cut after the run, the kept tail dropped ("other-draft-of-the….md"): one
    // cut, the match and the extension both readable, when that fits and the leading cut can't.
    let bare = |end: usize| -> Cut {
        let moved = marks.iter().filter_map(|&ix| if ix < end { Some(ix) } else if ix >= stem_chars { Some(ix - stem_chars + end + 1) } else { None }).collect();
        (SharedString::from(format!("{}\u{2026}{extension}", &stem[..byte(end)])), moved)
    };
    let bare_fits = |end: usize| {
        let (cut, moved) = bare(end);
        fits(&cut, &moved)
    };
    if bare_fits(run_end) {
        let (mut low, mut high) = (run_end, stem_chars - 1);
        while low < high {
            let mid = (low + high).div_ceil(2);
            if bare_fits(mid) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        // A separator left against the "…" ("the-….md") says nothing: it goes, down to the run's end.
        while low > run_end && !stem[..byte(low)].chars().next_back().is_some_and(char::is_alphanumeric) {
            low -= 1;
        }
        return bare(low);
    }
    let lead = least_lead(start, run_end);
    candidate(lead, widest(lead))
}

impl Element for NameText {
    type RequestLayoutState = Rc<NameSpec>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, _: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style.line_height_in_pixels(window.rem_size());
        let spec = Rc::new(NameSpec { text: self.text.clone(), dot: self.dot, marks: self.marks.take(), breath: self.breath.take(), path: self.path, style, font_size, line_height });
        let measured = spec.clone();
        let layout = window.request_measured_layout(gpui::Style::default(), move |known, available, window, _| {
            let width = match (known.width, available.width) {
                (Some(width), _) => width,
                (None, gpui::AvailableSpace::Definite(width)) => {
                    let (text, marks) = measured.cut(width, window);
                    measured.shape(text, &marks, None, window).width.min(width)
                }
                // A name asked for its least shrinks to nothing; asked for its most it is whole, its bold
                // matched letters measured too (Open Quickly sizes its names this way).
                (None, gpui::AvailableSpace::MinContent) => px(0.),
                (None, gpui::AvailableSpace::MaxContent) => {
                    let marks = measured.marks.as_ref().map_or(&[][..], |(at, _)| at.as_slice());
                    measured.shape(measured.text.clone(), marks, None, window).width
                }
            };
            size(width, measured.line_height)
        });
        (layout, spec)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Self::RequestLayoutState, _: &mut Window, _: &mut App) {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, spec: &mut Self::RequestLayoutState, _: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        let (text, marks) = spec.cut(bounds.size.width, window);
        // A name whose file is being copied breathes: its ink, from the frame's time, between rest and lit.
        let tint = spec.breath.as_ref().map(|breath| crate::tab_bar::mix(breath.rest, breath.lit, copy_level(window, &breath.path, breath.level)));
        let line = spec.shape(text, &marks, tint, window);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            line.paint(bounds.origin, spec.line_height, gpui::TextAlign::Left, None, window, cx).ok();
        });
    }
}

impl IntoElement for NameText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// What a dimmed row says about why, after its path in the tip.
const IGNORED_TIP: &str = "Hidden by .gitignore";

/// A row's tip: its path with `~` for the home folder, as the tab's tip has it, and for a row shown only because Show ignored files is on, the line saying `.gitignore` hides it.
fn row_tip(id: &SharedString, home: Option<&std::path::Path>, ignored: bool) -> SharedString {
    let path = std::path::Path::new(id.as_ref());
    let shown = if home.is_some_and(|home| path.starts_with(home) && path != home) { SharedString::from(crate::tab_bar::tip_path(path, home)) } else { id.clone() };
    if ignored { SharedString::from(format!("{shown}\n{IGNORED_TIP}")) } else { shown }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui_kit::SharedString;

    use super::{CutCache, IGNORED_TIP, cut_path_to_fit, cut_to_fit, row_tip};

    /// A row the tree shows only because Show ignored files is on says why it is dimmed; a dot file, dimmed on its own, says nothing more.
    #[test]
    fn an_ignored_rows_tip_names_the_gitignore() {
        let path: SharedString = "/work/target/debug".into();
        assert_eq!(row_tip(&path, None, true).to_string(), format!("/work/target/debug\n{IGNORED_TIP}"));
        assert_eq!(row_tip(&path, None, false).to_string(), "/work/target/debug");
        assert_eq!(row_tip(&path, Some(std::path::Path::new("/work")), true).to_string(), format!("~/target/debug\n{IGNORED_TIP}"));
    }

    /// A second git result for an icon that is mid-fade starts from the tints last drawn, never from an end.
    #[test]
    fn a_second_git_change_turns_the_icon_around_from_what_was_drawn() {
        use super::{SHIFTS, Status, git_changed, gpui};
        let (window, path) = (gpui::WindowId::from(7), std::path::PathBuf::from("/r/a.txt"));
        git_changed(window, vec![(path.clone(), None, Some(Status::Modified))]);
        // Nothing drawn yet: a newer result keeps the status the tree last drew as the start.
        git_changed(window, vec![(path.clone(), None, Some(Status::Added))]);
        SHIFTS.with_borrow_mut(|all| {
            let shift = all.get_mut(&(window, path.clone())).unwrap();
            assert_eq!(shift.from, vec![(None, 1.)]);
            assert_eq!(shift.to, Some(Status::Added));
            shift.started = true;
            shift.drawn = Some(0.25);
        });
        git_changed(window, vec![(path.clone(), Some(Status::Added), None)]);
        SHIFTS.with_borrow_mut(|all| {
            let shift = all.remove(&(window, path.clone())).unwrap();
            // A quarter of the way to added: three quarters still plain, one quarter added.
            assert_eq!(shift.from, vec![(None, 0.75), (Some(Status::Added), 0.25)]);
            assert_eq!(shift.to, None);
            assert!(!shift.started && shift.drawn.is_none());
        });
        // Going back to the status it was easing from merges into one share.
        git_changed(window, vec![(path.clone(), None, Some(Status::Added))]);
        SHIFTS.with_borrow_mut(|all| all.get_mut(&(window, path.clone())).unwrap().drawn = Some(0.5));
        git_changed(window, vec![(path.clone(), Some(Status::Added), None)]);
        SHIFTS.with_borrow_mut(|all| {
            let shift = all.remove(&(window, path)).unwrap();
            assert_eq!(shift.from, vec![(None, 0.5), (Some(Status::Added), 0.5)]);
        });
    }

    /// Cuts `name` to `width` characters (every character one unit wide) with the extension at its last dot.
    fn cut(name: &str, marks: &[usize], width: usize) -> (String, Vec<usize>) {
        let text = SharedString::from(name.to_string());
        let dot = name.rfind('.');
        let (text, moved) = cut_to_fit(&text, dot, marks, &|text, _| text.chars().count() <= width);
        (text.to_string(), moved)
    }

    /// The characters the marks point at in `text`.
    fn marked(text: &str, marks: &[usize]) -> String {
        let chars: Vec<char> = text.chars().collect();
        marks.iter().map(|&ix| chars[ix]).collect()
    }

    #[test]
    fn a_kept_tail_that_echoes_the_extension_keeps_one_more_character() {
        // "…rs.rs" would read as a double extension: the tail takes a third character.
        assert_eq!(cut("tokens_and_counters.rs", &[], 20).0, "tokens_and_co\u{2026}ers.rs");
        // A tail that doesn't echo the extension stays at two.
        assert_eq!(cut("second-draft-of-the-planning-summary-v2.md", &[], 10).0, "seco\u{2026}v2.md");
        // Capitals count as the same letters.
        assert_eq!(cut("tokens_and_counterS.RS", &[], 20).0, "tokens_and_co\u{2026}erS.RS");
    }

    #[test]
    fn a_cut_keeps_the_extension_and_the_first_marked_run() {
        let name = "second-draft-of-the-planning-summary-v2.md";
        let marks: Vec<usize> = (7..12).collect();
        // No marks: the stem's start, "…", the stem's last two characters, the extension.
        assert_eq!(cut(name, &[], 10).0, "seco\u{2026}v2.md");
        // The run fits after the stem's start: the cut falls after it.
        let (text, moved) = cut(name, &marks, 18);
        assert_eq!(text, "second-draft\u{2026}v2.md");
        assert_eq!(marked(&text, &moved), "draft");
        // Too narrow for that: the start goes instead, and the run stays whole.
        let (text, moved) = cut(name, &marks, 14);
        assert_eq!(text, "\u{2026}d-draft\u{2026}v2.md");
        assert_eq!(marked(&text, &moved), "draft");
        // Marks in the extension follow it.
        let (text, moved) = cut(name, &[40, 41], 14);
        assert_eq!(marked(&text, &moved), "md");
        // A run wider than the name keeps its start.
        let (text, moved) = cut(name, &marks, 8);
        assert!(text.ends_with(".md") && text.chars().count() <= 8);
        assert!(moved.iter().all(|&ix| ix < text.chars().count()));
    }

    #[test]
    fn a_match_deep_in_a_folder_name_cuts_only_the_start() {
        let name = "archive-of-superseded-drafts-2025";
        let marks: Vec<usize> = (22..27).collect();
        // The name's end fits with the match in it: one leading cut, no trailing one.
        let (text, moved) = cut(name, &marks, 17);
        assert_eq!(text, "\u{2026}eded-drafts-2025");
        assert_eq!(marked(&text, &moved), "draft");
        // Too narrow for the end: the match stays whole, cut after it.
        let (text, moved) = cut(name, &marks, 8);
        assert_eq!(marked(&text, &moved), "draft");
        assert!(text.chars().count() <= 8);
    }

    /// A match deep in a long name: one cut when the start, the match and the extension fit; two only when they can't.
    #[test]
    fn a_match_deep_in_a_file_name_cuts_one_side_when_the_start_and_the_extension_fit() {
        let name = "other-draft-of-the-planning-summary-v2.md";
        let marks: Vec<usize> = (6..18).collect();
        // The leading cut alone is too wide, the start through the match and ".md" fit: one cut, after the match.
        let (text, moved) = cut(name, &marks, 22);
        assert_eq!(text, "other-draft-of-the\u{2026}.md");
        assert_eq!(marked(&text, &moved), "draft-of-the");
        // A separator that would sit against the "…" is dropped.
        assert_eq!(cut(name, &marks, 23).0, "other-draft-of-the\u{2026}.md");
        // The tail kept when the cut can be after the match and still show it.
        let (text, _) = cut(name, &marks, 24);
        assert_eq!(text, "other-draft-of-the\u{2026}v2.md");
        // Too narrow for the start with the match: the match stays whole with a cut on each side.
        let (text, moved) = cut(name, &marks, 20);
        assert_eq!(text, "\u{2026}-draft-of-the\u{2026}v2.md");
        assert_eq!(marked(&text, &moved), "draft-of-the");
    }

    /// Cuts `path` to `width` characters (every character one unit wide).
    fn path(path: &str, width: usize) -> String {
        cut_path_to_fit(&SharedString::from(path.to_string()), &|text| text.chars().count() <= width).to_string()
    }

    #[test]
    fn a_folder_path_loses_its_middle_and_keeps_the_top_folder_and_the_end() {
        let folder = "archive/superseded-drafts/planning/quarterly/appendix-notes";
        assert_eq!(path(folder, 80), folder);
        assert_eq!(path(folder, 45), "archive/\u{2026}/planning/quarterly/appendix-notes");
        assert_eq!(path(folder, 40), "archive/\u{2026}/quarterly/appendix-notes");
        assert_eq!(path(folder, 25), "archive/\u{2026}/appendix-notes");
        // No room for the top folder: "…" and the last folders.
        assert_eq!(path(folder, 20), "\u{2026}/appendix-notes");
        // One folder wider than the room: its last characters, never mid-glyph.
        assert_eq!(path(folder, 12), "\u{2026}endix-notes");
        assert_eq!(path("a/ünïcödé-folder", 6), "\u{2026}older");
        assert_eq!(path("docs", 3), "\u{2026}cs");
    }

    #[test]
    fn a_cut_never_puts_the_ellipsis_against_the_dot() {
        let name = "an-unusually-long-name.md";
        for width in 8..name.chars().count() {
            let (text, _) = cut(name, &[], width);
            assert!(!text.contains("\u{2026}."), "{text} at {width}");
            assert!(text.chars().count() <= width, "{text} at {width}");
        }
        assert_eq!(cut(name, &[], 16).0, "an-unusual\u{2026}me.md");
        // Marks in the kept tail follow it.
        let (text, moved) = cut(name, &[20, 21], 16);
        assert_eq!(marked(&text, &moved), "me");
    }

    fn key(name: &str, width: u32) -> (SharedString, u32, u32, u32, u64) {
        (SharedString::from(name.to_string()), width, 0, 0, 0)
    }

    #[test]
    fn cut_cache_keeps_the_last_two_frames_and_its_cap() {
        let mut cache = CutCache::new();
        let start = Instant::now();
        let kept = 8;
        // A glide: every frame cuts four names at a new width.
        for frame in 0..100u32 {
            let now = start + Duration::from_millis(8 * u64::from(frame) + 1);
            for name in 0..4 {
                let k = key(&format!("n{name}"), frame);
                if cache.get(&k, now).is_none() {
                    cache.insert(k, (SharedString::from("x"), Vec::new()), now, kept);
                }
            }
            assert!(cache.cuts.len() <= kept);
            for name in 0..4 {
                assert!(cache.cuts.contains_key(&key(&format!("n{name}"), frame)), "this frame's cut is kept");
            }
        }
    }
}
