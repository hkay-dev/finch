//! The markdown preview shown right of the editor.
//!
//! Every open markdown document holds a `Preview`: a `TextViewState` parsed
//! when the file opens, so showing it paints at once; a file over 256 KB opened while the panel
//! is hidden stays cold instead, and parses when the panel shows. The state parses in a
//! background task of its own; the main thread only copies the editor's text
//! into it, once per quiet moment after typing (and every `MAX_WAIT` while typing never stops). While the panel is hidden
//! nothing is copied: the preview is marked stale and fed when it shows.
//!
//! The two panes scroll together, one leading (`Driver`): a wheel or a click in a pane makes it
//! lead, and so does a move of the editor that the preview didn't ask for (keys, find). The
//! editor leading, the preview puts the editor's top source byte at its top (`Preview::follow`);
//! the preview leading, the editor puts the preview's top byte at its top (`Preview::lead`). The
//! follower never moves the leader back: the editor's move that the preview asked for is an echo,
//! and the preview never follows while it leads.

mod pele;
pub mod anchor;
pub mod diagrams;
mod math;
pub mod render;
pub mod task;

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::RopeExt as _;
use gpui_kit::component::{input::EditorState, text::TextViewState};
use gpui_kit::*;

use crate::document::Document;
use crate::motion::{self, ScrollGlide};
use crate::settings;
use crate::panes::PreviewPane;
use crate::theme;
use crate::tokens::{ALPHA_SELECTED, ALPHA_SELECTED_LIGHT, RADIUS_ROW, SCROLL_GLIDE, SELECTED_FLASH};

/// How long typing must pause before the preview is fed the new text.
const DEBOUNCE: Duration = Duration::from_millis(80);

/// The longest a burst of typing holds the preview: each edit restarts the `DEBOUNCE` wait, but
/// a wait never runs past this long after the burst's first edit, so continuous typing still
/// shows a new preview about four times a second.
const MAX_WAIT: Duration = Duration::from_millis(250);

/// A file this long keeps no parse while the panel is hidden: it opens cold and stays cold on a
/// tab switch until the panel shows (the same bound as the pane's `BIG_FILE_BYTES`).
const COLD_BYTES: usize = 256 * 1024;

/// How long the panel stays hidden before a big file's parsed tree goes: toggling the panel back and forth within it keeps the
/// tree, so showing it again is instant.
const HIDDEN_COOL: Duration = Duration::from_secs(5);

/// The longest line that can open or close front matter: `---` or `...` and a `\r`, not counting
/// the line break.
const FENCE_LINE: usize = 4;

/// A markdown document's warm parsed state.
pub struct Preview {
    pub state: Entity<TextViewState>,
    /// The one task waiting to feed, alive while `due` is set: an edit moves `due` and the task
    /// sleeps on to the latest one, so a burst of keys spawns once.
    pending: Option<Task<()>>,
    /// When the waiting task feeds: `DEBOUNCE` after the last edit, never past `MAX_WAIT`
    /// after the burst's first. `None` while no task waits.
    due: Option<Instant>,
    /// The edit burst being fed, for `MAX_WAIT`: when its window began and when the last edit
    /// came. A feed cut short by `MAX_WAIT` starts the next window at once, so typing that
    /// never pauses feeds every `MAX_WAIT`; a pause past `DEBOUNCE` ends the burst.
    burst: Option<(Instant, Instant)>,
    /// What the preview last followed the editor to: the top source byte and
    /// the block count then. `None` until it follows, and again while the
    /// panel is hidden, so showing it follows at once.
    followed: Rc<Cell<Option<(usize, usize)>>>,
    /// The preview's width at its last follow: a new one re-wraps the blocks, so the preview
    /// follows again.
    width: Rc<Cell<Pixels>>,
    /// The preview's list top and block count as of its last prepaint, to tell a scroll of the
    /// preview from a frame that left it where it was or reparsed it.
    seen: Rc<Cell<Option<(ListOffset, usize)>>>,
    /// The editor to read when the text changed while the panel was hidden; `follow` feeds it
    /// in the first frame that shows the panel.
    stale: Rc<RefCell<Option<Entity<EditorState>>>>,
    /// Its parsed tree was let go (`cool`); it parses again when its tab is among the last shown
    /// (`warm`), panel shown or not, except a file over 256 KB, which stays cold while the panel
    /// is hidden and parses when it shows.
    cold: Rc<Cell<bool>>,
    /// A heading or footnote link's glide of the list's scroll offset, run a frame at a time by
    /// `follow`.
    glide: Rc<RefCell<ScrollGlide>>,
    /// Where the glide ends, placed exactly once it lands (the glide runs on estimated heights
    /// for blocks not measured yet).
    landing: Rc<RefCell<Option<Landing>>>,
    /// The wash on the block a heading or footnote link landed on, until it ends.
    wash: Rc<RefCell<Option<Wash>>>,
    /// The text a reload changed on disk, until its parse lands and `start_reload_wash` washes it.
    reloaded: Rc<RefCell<Option<Reloaded>>>,
    /// The wait that lets go of a big file's parsed tree once the panel has stayed hidden (`watch_hidden`).
    cooling: Option<Task<()>>,
    _settings: Subscription,
}

/// What a link's glide lands on.
enum Landing {
    /// A source byte put at the list's top.
    Source(usize),
    /// A footnote anchor (`fn-N` or `fnref-N`).
    Footnote(String),
}

impl Landing {
    /// Puts the target at the list's top at once: `None` when it is gone, else whether the
    /// placement is exact (a block not measured yet is placed next frame).
    fn place(&self, view: &TextViewState) -> Option<bool> {
        match self {
            Landing::Source(start) => view.scroll_source_to_top(*start),
            Landing::Footnote(anchor) => view.scroll_to_footnote(anchor),
        }
    }
}

/// One wash of the block a link landed on: the flash of a jump to a row in view (a sine swell
/// over `SCROLL_GLIDE`, at the selection's alpha times `1 + SELECTED_FLASH` at its peak). A second
/// link clicked mid-wash starts from the level drawn at that moment, so nothing jumps.
struct Wash {
    /// The list items washed: one for a link's landing, the blocks a reload changed for a reload.
    items: Range<usize>,
    /// The list's block count then, to tell a reparse that moved the indices.
    count: usize,
    /// The level drawn when it started, falling away as the new swell rises.
    from: f32,
    since: Instant,
}

impl Wash {
    /// The level, 0 to 1, drawn at `now`; `None` once it has ended.
    fn level(&self, now: Instant) -> Option<f32> {
        let u = now.saturating_duration_since(self.since).as_secs_f32() / SCROLL_GLIDE;
        (u < 1.).then(|| (self.from * (1. - u)).max((std::f32::consts::PI * u).sin()))
    }
}

/// Draws the wash over its block, clipped to the pane, and asks for the next frame only while it
/// runs.
fn paint_wash(slot: &RefCell<Option<Wash>>, state: &Entity<TextViewState>, pane: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let mut slot = slot.borrow_mut();
    let Some(wash) = slot.as_ref() else {
        return;
    };
    let id = window.window_handle().window_id();
    // A reparse that changed the block count shifted the indices: the wash is over.
    let level = Some(wash).filter(|wash| wash.count == state.read(cx).list_state().item_count()).and_then(|wash| wash.level(motion::drawn_now(id)));
    let Some(level) = level else {
        *slot = None;
        if crate::control::rects_enabled() {
            crate::control::unrecord(id, "preview-wash");
        }
        return;
    };
    window.request_animation_frame();
    let list = state.read(cx).list_state();
    let alpha = if theme::is_light(cx) { ALPHA_SELECTED_LIGHT } else { ALPHA_SELECTED };
    let color = theme::ink(alpha * (1. + SELECTED_FLASH) * level, cx);
    // From the first block in view to the first one below the pane: a wash over a long run of
    // blocks never walks the ones off screen.
    let first = wash.items.start.max(list.logical_scroll_top().item_ix);
    window.with_content_mask(Some(ContentMask { bounds: pane }), |window| {
        for item in first..wash.items.end {
            let Some(block) = list.bounds_for_item(item) else {
                break;
            };
            if block.origin.y >= pane.bottom() {
                break;
            }
            window.paint_quad(fill(block, color).corner_radii(Corners::all(RADIUS_ROW)));
            // `rects`: the first washed block as drawn, present only while the wash runs.
            if crate::control::rects_enabled() && item == first {
                crate::control::record(window, "preview-wash", block);
            }
        }
    });
}

/// How long a reload's wash waits for the parse of the new text before it gives up (a parse that
/// takes longer is no longer "the change landing").
const RELOAD_WAIT: Duration = Duration::from_secs(5);

/// The text a reload changed, waiting for its parse: the new text's span in the editor's bytes,
/// the editor's length then (the parse of later typing has another length, and then there is
/// nothing to wash), and when it came.
struct Reloaded {
    span: Range<usize>,
    len: usize,
    at: Instant,
}

/// Starts the wash over the blocks a reload changed, in the frame after their parse lands (frames
/// are asked for while it runs). One wash per reload: the span is taken when it starts.
fn start_reload_wash(reloaded: &RefCell<Option<Reloaded>>, wash: &RefCell<Option<Wash>>, state: &Entity<TextViewState>, window: &mut Window, cx: &mut App) {
    let mut slot = reloaded.borrow_mut();
    let Some(waiting) = slot.as_ref() else {
        return;
    };
    if waiting.at.elapsed() > RELOAD_WAIT {
        *slot = None;
        return;
    }
    let view = state.read(cx);
    if !view.parse_settled() {
        window.request_animation_frame();
        return;
    }
    let Some(Reloaded { span, len, .. }) = slot.take() else {
        return;
    };
    let items = (view.parsed_len() == len).then(|| view.blocks_in_source(span)).flatten();
    let Some(items) = items else {
        return;
    };
    let (count, now) = (view.list_state().item_count(), motion::drawn_now(window.window_handle().window_id()));
    let mut wash = wash.borrow_mut();
    // A wash still running on these blocks turns around from where it is drawn.
    let from = wash.as_ref().filter(|wash| wash.items == items).and_then(|wash| wash.level(now)).unwrap_or(0.);
    *wash = Some(Wash { items, count, from, since: now });
    window.request_animation_frame();
}

/// How long the caret wash holds after the caret's last move, then how long it fades out: the
/// wash is gone about two seconds after the caret stops.
const CARET_HOLD: f32 = 1.5;
const CARET_FADE: f32 = 0.5;

/// The wash over the top-level block holding the caret: it rises over `SCROLL_GLIDE` on the
/// link wash's easing, holds while the caret keeps moving, fades out after it stops, and slides
/// from the block it was on to the next over `SCROLL_GLIDE` (a cut under Reduce Motion).
struct CaretWash {
    item: usize,
    /// The list's block count then, to tell a reparse that moved the indices.
    count: usize,
    /// The level drawn at the last caret move, and when that was.
    level0: f32,
    touched: Instant,
    /// Where the slide to this block started (the rectangle drawn then), and when.
    from: Option<Bounds<Pixels>>,
    slid: Instant,
    /// The rectangle drawn last, which a caret move to another block slides from.
    drawn: Option<Bounds<Pixels>>,
}

impl CaretWash {
    /// The level, 0 to 1, drawn at `now`; `None` once it has faded out.
    fn level(&self, now: Instant) -> Option<f32> {
        let dt = now.saturating_duration_since(self.touched).as_secs_f32();
        if dt >= CARET_HOLD + CARET_FADE {
            return None;
        }
        let rise = self.level0 + (1. - self.level0) * motion::ease_out((dt / SCROLL_GLIDE).min(1.));
        Some(if dt < CARET_HOLD { rise } else { rise * (1. - motion::ease_out((dt - CARET_HOLD) / CARET_FADE)) })
    }

    /// The rectangle drawn at `now`: `to` once the slide has landed.
    fn rect(&self, to: Bounds<Pixels>, now: Instant) -> Bounds<Pixels> {
        let Some(from) = self.from else {
            return to;
        };
        let u = motion::ease_out((now.saturating_duration_since(self.slid).as_secs_f32() / SCROLL_GLIDE).min(1.));
        let lerp = |a: Pixels, b: Pixels| a + (b - a) * u;
        Bounds::new(
            point(lerp(from.origin.x, to.origin.x), lerp(from.origin.y, to.origin.y)),
            size(lerp(from.size.width, to.size.width), lerp(from.size.height, to.size.height)),
        )
    }
}

/// The caret wash and the caret moves it has seen.
#[derive(Default)]
struct CaretSlot {
    wash: Option<CaretWash>,
    seen: u64,
}

/// The caret wash's view. It sits beside the preview pane in the window, not inside it: a view's
/// frames dirty its ancestors, so one inside the pane would render the pane on every frame of
/// every wash. It paints nothing, and asks for no frame, while there is no wash.
pub struct WashLayer {
    pane: WeakEntity<PreviewPane>,
    slot: Rc<RefCell<CaretSlot>>,
}

impl WashLayer {
    pub fn new(pane: WeakEntity<PreviewPane>) -> Self {
        Self { pane, slot: Rc::default() }
    }
}

impl Render for WashLayer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (pane, slot) = (self.pane.clone(), self.slot.clone());
        canvas(
            |_, _, _| {},
            move |_, (), window, cx| {
                if let Some(link) = pane.upgrade().map(|pane| pane.read(cx).link()) {
                    paint_caret_wash(&slot, &link, window, cx);
                }
            },
        )
        .absolute()
        .size_full()
    }
}

/// Draws the caret wash over its block, clipped to the preview's text, and asks for the next
/// frame only while the wash lasts. The caret's block comes from a binary search over the block
/// spans; a reparse that changed the block count ends the wash, so it never lands on a block
/// the indices moved to. No wash while the preview leads or when the caret is in no block.
fn paint_caret_wash(slot: &RefCell<CaretSlot>, link: &EditorLink, window: &mut Window, cx: &mut App) {
    let mut slot = slot.borrow_mut();
    let slot = &mut *slot;
    let moved = std::mem::replace(&mut slot.seen, link.moves.get()) != link.moves.get();
    let state = link.target.borrow().as_ref().and_then(|weak| weak.upgrade());
    let (Some(state), Some(caret)) = (state, link.caret.get()) else {
        slot.wash = None;
        return;
    };
    let (item, count) = {
        let view = state.read(cx);
        (view.block_at_source((caret + view.parsed_len()).saturating_sub(link.len.get())), view.list_state().item_count())
    };
    let item = item.filter(|_| link.driver.get() == Driver::Editor);
    let Some(item) = item else {
        slot.wash = None;
        return;
    };
    if slot.wash.as_ref().is_some_and(|wash| wash.count != count) {
        slot.wash = None;
    }
    let now = motion::drawn_now(window.window_handle().window_id());
    let retarget = slot.wash.as_ref().is_some_and(|wash| wash.item != item);
    if moved || retarget {
        let level0 = slot.wash.as_ref().and_then(|wash| wash.level(now)).unwrap_or(0.);
        match slot.wash.as_mut().filter(|wash| wash.item == item) {
            // The same block: the caret moved inside it, so the wash holds from the level it has.
            Some(wash) => (wash.level0, wash.touched) = (level0, now),
            None => {
                // Another block: slide from where the last one is drawn, unless it has all but gone.
                let from = slot.wash.as_ref().filter(|_| level0 > 0.05 && !cx.reduce_motion()).and_then(|wash| wash.drawn);
                slot.wash = Some(CaretWash { item, count, level0, touched: now, from, slid: now, drawn: None });
            }
        }
    }
    let Some(wash) = slot.wash.as_mut() else {
        return;
    };
    let Some(level) = wash.level(now) else {
        slot.wash = None;
        return;
    };
    window.request_animation_frame();
    let Some(block) = state.read(cx).list_state().bounds_for_item(item) else {
        wash.drawn = None;
        return;
    };
    let rect = wash.rect(block, now);
    wash.drawn = Some(rect);
    let alpha = if theme::is_light(cx) { ALPHA_SELECTED_LIGHT } else { ALPHA_SELECTED };
    let color = theme::ink(alpha * level, cx);
    window.with_content_mask(Some(ContentMask { bounds: link.pane.get() }), |window| {
        window.paint_quad(fill(rect, color).corner_radii(Corners::all(RADIUS_ROW)));
    });
}

impl Preview {
    /// Starts parsing `text` in the background.
    pub fn new(text: &str, cx: &mut Context<Document>) -> Self {
        let text = blank_front_matter(text);
        let state = cx.new(|cx| TextViewState::markdown_with(&text, render::extensions(), cx).measure_visible_only());
        Self {
            state,
            pending: None,
            due: None,
            burst: None,
            followed: Rc::default(),
            width: Rc::default(),
            seen: Rc::default(),
            stale: Rc::default(),
            cold: Rc::default(),
            glide: Rc::default(),
            landing: Rc::default(),
            wash: Rc::default(),
            reloaded: Rc::default(),
            cooling: None,
            _settings: cx.observe_global::<settings::Store>(|document, cx| {
                let visible = settings::get(cx).preview.visible;
                if let Some(preview) = document.preview.as_mut() {
                    preview.watch_hidden(visible, cx);
                }
            }),
        }
    }

    /// The element that keeps the preview's top at the editor's top while the editor leads, and
    /// makes the preview lead on a wheel or a click over it. Put it first in the preview pane
    /// while the preview shows: its capture listeners then run before the text's, which stops the
    /// wheel. It acts in the frame's prepaint, after the editor pane's, which leaves the editor's
    /// top in `link`, so the preview moves in the same frame as the editor, and does nothing
    /// while neither scroll position nor block count changed, so nothing asks for frames at rest.
    /// While the preview leads it only notes where the editor went; the first follow after the
    /// panel shows, the tab changes or a reparse changes the block count lines the preview up with
    /// the editor and gives the lead back to the editor.
    pub fn follow(&self, link: &Rc<EditorLink>) -> impl IntoElement {
        let (state, link, followed) = (self.state.clone(), link.clone(), self.followed.clone());
        let mark = link.clone();
        let stale = self.stale.clone();
        let seen = self.seen.clone();
        let cold = self.cold.clone();
        let (glide, landing_slot) = (self.glide.clone(), self.landing.clone());
        let (hand_glide, hand_landing) = (self.glide.clone(), self.landing.clone());
        let (wash_slot, paint_wash_slot, paint_state) = (self.wash.clone(), self.wash.clone(), self.state.clone());
        let paint_reloaded = self.reloaded.clone();
        let width = self.width.clone();
        canvas(
            move |bounds, window, cx| {
                let editor = stale.borrow_mut().take();
                if let Some(editor) = editor {
                    cold.set(false);
                    feed_editor_text(&state, &editor, cx);
                }
                // A heading link clicked before the parse could answer, or in the frame the
                // preview shows: the preview leads from the heading, gliding there, and the
                // editor follows it frame by frame.
                let id = window.window_handle().window_id();
                let settled = link.top.get().filter(|_| state.read(cx).pending_anchor().is_some()).and_then(|top| {
                    let landing = settle_anchor(&state, &mut glide.borrow_mut(), motion::drawn_now(id), cx)?;
                    *landing_slot.borrow_mut() = Some(landing);
                    Some(top)
                });
                if link.driver.get() != Driver::Preview && settled.is_none() {
                    // The editor took the lead (a key, find): its position wins over the glide.
                    glide.borrow_mut().cancel();
                    landing_slot.borrow_mut().take();
                }
                // The glide's frame: the list moves a step toward the target; the frame it lands
                // in places the target exactly, and once that holds no more frames are asked for.
                let stepped = glide.borrow_mut().step(motion::drawn_now(id));
                if let Some(at) = stepped {
                    state.read(cx).scroll_source_to_top(at as usize);
                }
                let moving = glide.borrow().is_moving();
                let landing = if moving { None } else { landing_slot.borrow_mut().take() };
                if moving {
                    window.request_animation_frame();
                } else if let Some(landing) = landing {
                    match landing.place(state.read(cx)) {
                        Some(false) => {
                            *landing_slot.borrow_mut() = Some(landing);
                            window.request_animation_frame();
                        }
                        // Landed: the block it put at the top washes once.
                        Some(true) => {
                            let (item, count) = {
                                let list = state.read(cx).list_state();
                                (list.logical_scroll_top().item_ix, list.item_count())
                            };
                            let now = motion::drawn_now(id);
                            let mut wash = wash_slot.borrow_mut();
                            // A wash still running on this block turns around from where it is drawn.
                            let from = wash.as_ref().filter(|wash| wash.items == (item..item + 1)).and_then(|wash| wash.level(now)).unwrap_or(0.);
                            *wash = Some(Wash { items: item..item + 1, count, from, since: now });
                            window.request_animation_frame();
                        }
                        None => {}
                    }
                }
                if let Some(top) = settled {
                    link.driver.set(Driver::Preview);
                    // `lead` ignores a first look at the list and a changed block count; a list
                    // top that can't match, with the real count, makes it hand the heading to
                    // the editor.
                    let count = state.read(cx).list_state().item_count();
                    seen.set(Some((ListOffset { item_ix: usize::MAX, offset_in_item: Pixels::ZERO }, count)));
                    followed.set(Some((top, count)));
                    window.request_animation_frame();
                    return;
                }
                let Some(top) = link.top.get() else {
                    return;
                };
                let state = state.read(cx);
                let key = (top, state.list_state().item_count());
                // A reparse that changed the block count resets the list to the top, so the
                // editor leads again and the preview is put back at the editor's top.
                if followed.get().is_none_or(|(_, count)| count != key.1) {
                    link.driver.set(Driver::Editor);
                }
                // A new width re-wraps the blocks and changes their heights, so the editor's top
                // lands somewhere else in them: follow again, once (the retry below covers
                // heights that settle over a frame or two). A preview that leads stays put.
                if width.replace(bounds.size.width) != bounds.size.width {
                    followed.set(followed.get().map(|(_, count)| (usize::MAX, count)));
                }
                if followed.get() == Some(key) {
                    return;
                }
                if link.driver.get() == Driver::Preview {
                    followed.set(Some(key));
                } else if state.scroll_source_to_top(top) == Some(false) {
                    // An unmeasured block lands at its top and is placed
                    // exactly next frame, once it has been laid out.
                    window.request_animation_frame();
                } else {
                    followed.set(Some(key));
                }
            },
            move |bounds, (), window, cx| {
                mark.mark(Driver::Preview, bounds, window);
                mark.aim_wash(&paint_state, bounds, window, cx);
                start_reload_wash(&paint_reloaded, &paint_wash_slot, &paint_state, window, cx);
                paint_wash(&paint_wash_slot, &paint_state, bounds, window, cx);
                // A hand on the preview (wheel, press) stops a glide where it is drawn.
                let (glide, landing) = (hand_glide.clone(), hand_landing.clone());
                window.on_mouse_event(move |event: &ScrollEvent, phase, _, _| {
                    if phase == DispatchPhase::Capture && bounds.contains(&event.position) {
                        glide.borrow_mut().cancel();
                        landing.borrow_mut().take();
                    }
                });
                let (glide, landing) = (hand_glide.clone(), hand_landing.clone());
                window.on_mouse_event(move |event: &MouseDownEvent, phase, _, _| {
                    if phase == DispatchPhase::Capture && bounds.contains(&event.position) {
                        glide.borrow_mut().cancel();
                        landing.borrow_mut().take();
                    }
                });
            },
        )
        .absolute()
        .size_full()
    }

    /// The element that keeps the editor's top at the preview's top while the preview leads. Put
    /// it after the preview's text: it reads the list's top in the frame's prepaint, after the
    /// list has laid out its wheel's glide or knob drag, and hands the editor its new top at the
    /// start of the next frame (a notify in this frame's draw is lost), which draws it. A frame
    /// that reparsed the preview moves nothing, so typing never scrolls the editor.
    pub fn lead(&self, link: &Rc<EditorLink>) -> impl IntoElement {
        let (state, seen, link) = (self.state.clone(), self.seen.clone(), link.clone());
        canvas(
            move |_, window, cx| {
                let byte = {
                    let view = state.read(cx);
                    let list = view.list_state();
                    let now = (list.logical_scroll_top(), list.item_count());
                    let before = seen.replace(Some(now));
                    if link.driver.get() != Driver::Preview || before.is_none_or(|before| before.1 != now.1 || before.0 == now.0) {
                        return;
                    }
                    view.source_at_top()
                };
                let (Some(byte), Some(editor)) = (byte, link.editor.borrow().as_ref().and_then(WeakEntity::upgrade)) else {
                    return;
                };
                let Some(y) = editor_scroll_for(editor.read(cx), byte) else {
                    return;
                };
                link.wrote.set(true);
                window.on_next_frame(move |_, cx| editor.update(cx, |editor, cx| editor.set_scroll_offset(point(editor.scroll_offset().x, y), cx)));
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    }

    /// Forgets where the preview followed to, for a frame without the panel.
    pub fn stop_following(&self) {
        self.followed.set(None);
    }

    /// Feeds the editor's text to the state after the debounce, or marks the preview stale
    /// while the panel is hidden. An edit made by ticking a box in the preview feeds at once,
    /// so the tick shows in the frame the click draws; typing keeps its debounce.
    pub fn schedule(&mut self, editor: Entity<EditorState>, cx: &mut Context<Document>) {
        if !settings::get(cx).preview.visible {
            self.pending = None;
            self.due = None;
            self.burst = None;
            *self.stale.borrow_mut() = Some(editor);
            return;
        }
        if task::take_ticked() {
            self.pending = None;
            self.due = None;
            self.feed(&editor, cx);
            return;
        }
        // Each edit restarts the quiet wait, cut short so the feed lands within `MAX_WAIT` of
        // the window's start.
        let now = Instant::now();
        let began = match self.burst {
            Some((began, last)) if now.duration_since(last) < DEBOUNCE => began,
            _ => now,
        };
        self.burst = Some((began, now));
        let due = now + DEBOUNCE.min(MAX_WAIT.saturating_sub(now.duration_since(began)));
        if self.due.replace(due).is_some() {
            return;
        }
        self.pending = Some(cx.spawn(async move |this, cx| {
            loop {
                let due = this.update(cx, |document, _| document.preview.as_ref().and_then(|preview| preview.due));
                let Ok(Some(due)) = due else { return };
                let now = Instant::now();
                if due > now {
                    cx.background_executor().timer(due - now).await;
                    continue;
                }
                this.update(cx, |document, cx| {
                    if let Some(preview) = document.preview.as_mut() {
                        preview.due = None;
                        preview.feed(&editor, cx);
                    }
                })
                .ok();
                return;
            }
        }));
    }

    /// A reload (or a tail's append) from disk put new text at `span` (bytes of the editor's text
    /// now, `len` long) and the editor washed it: once the parse of that text lands, the preview
    /// washes the blocks that hold `span`, once. Nothing while the panel is hidden or the tree is
    /// let go, since the wash would come long after the change. Call before `feed`.
    pub fn note_reload(&self, span: Option<Range<usize>>, len: usize, cx: &App) {
        let showing = settings::get(cx).preview.visible && !self.cold.get();
        *self.reloaded.borrow_mut() = span.filter(|_| showing).map(|span| Reloaded { span, len, at: Instant::now() });
    }

    /// The tab is shown again after a reload in the background: the editor's own wash is long
    /// gone, so the preview's waiting one goes too.
    pub fn forget_reload(&self) {
        self.reloaded.borrow_mut().take();
    }

    /// Feeds the editor's text to the state now, or marks the preview stale while the panel is
    /// hidden; `follow` feeds a stale preview in the first frame that shows it.
    pub fn feed(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Document>) {
        // Edits still coming (the last under `DEBOUNCE` ago): this feed was cut short by
        // `MAX_WAIT`, and the next window starts now.
        self.burst = self
            .burst
            .filter(|(_, last)| last.elapsed() < DEBOUNCE)
            .map(|(_, last)| (Instant::now(), last));
        if !settings::get(cx).preview.visible || self.cold.get() {
            *self.stale.borrow_mut() = Some(editor.clone());
            return;
        }
        self.stale.borrow_mut().take();
        feed_editor_text(&self.state, editor, cx);
    }

    /// Lets go of the parsed tree, for a tab past the kept-warm few: the state is emptied and
    /// `warm` (or the first frame that shows the panel) feeds it the editor's text again. The
    /// editor's text is untouched.
    pub fn cool(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Document>) {
        if self.cold.replace(true) {
            return;
        }
        self.pending = None;
        self.due = None;
        self.burst = None;
        self.followed.set(None);
        self.seen.set(None);
        self.glide.borrow_mut().cancel();
        self.landing.borrow_mut().take();
        self.reloaded.borrow_mut().take();
        *self.stale.borrow_mut() = Some(editor.clone());
        self.state.update(cx, |state, cx| state.set_text("", cx));
    }

    /// Parses again what `cool` let go of, panel shown or not, so the preview is ready when it
    /// shows; a warm preview stays as it is. A big file (`COLD_BYTES`) stays cold while the
    /// panel is hidden: the first frame that shows it parses.
    pub fn warm(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Document>) {
        if !self.cold.get() || (!settings::get(cx).preview.visible && Self::is_big(editor, cx)) {
            return;
        }
        self.cold.set(false);
        self.stale.borrow_mut().take();
        feed_editor_text(&self.state, editor, cx);
    }

    /// The panel's visibility changed (a settings change): once a big file's preview has stayed hidden `HIDDEN_COOL`, the parsed
    /// tree goes (`cool`) and the first frame that shows the panel parses it again, as a big file opened with the panel hidden
    /// does. A show before then keeps it. The size is read when the wait ends, since a parse started just before the hide is not in yet; a cold preview starts no wait.
    fn watch_hidden(&mut self, visible: bool, cx: &mut Context<Document>) {
        if visible {
            self.cooling = None;
            return;
        }
        if self.cooling.is_some() || self.cold.get() {
            return;
        }
        self.cooling = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIDDEN_COOL).await;
            this.update(cx, |document, cx| {
                if let Some(preview) = document.preview.as_mut() {
                    preview.cooling = None;
                }
                let big = document.preview.as_ref().is_some_and(|preview| preview.state.read(cx).parsed_len() > COLD_BYTES);
                if big && !settings::get(cx).preview.visible {
                    document.keep_preview_warm(false, cx);
                }
            })
            .ok();
        }));
    }

    /// Whether `editor`'s text is past `COLD_BYTES`.
    fn is_big(editor: &Entity<EditorState>, cx: &App) -> bool {
        editor.read(cx).text().len() > COLD_BYTES
    }

    /// Whether a preview made for `len` bytes of text starts cold: a big file opened while the
    /// panel is hidden. Make it with `new` on empty text, then `cool`.
    pub fn starts_cold(len: usize, cx: &App) -> bool {
        len > COLD_BYTES && !settings::get(cx).preview.visible
    }
}

/// Aims `glide` at the heading `state`'s pending anchor names, from where the list is drawn, and
/// says what to land on; `None` when there is no such target. The anchor stays pending until the
/// text is parsed; once it is, an anchor with no target is dropped. A footnote link's anchor is
/// `fn-N` (to the note) or `fnref-N` (back to the reference), which the parser's links carry; the
/// notes have no source span to glide through, so those place at once. A heading's glide moves
/// the list a frame at a time through source bytes (`TextViewState::scroll_source_to_top`), since
/// pixel offsets of blocks not measured yet are only estimates and a glide in them would pop at
/// its end.
fn settle_anchor(state: &Entity<TextViewState>, glide: &mut ScrollGlide, now: Instant, cx: &mut App) -> Option<Landing> {
    let view = state.read(cx);
    let anchor = view.pending_anchor()?;
    let headings = view.headings()?;
    let landing = if view.scroll_to_footnote(anchor).is_some() {
        Some(Landing::Footnote(anchor.to_owned()))
    } else if let Some(from) = view.source_at_top() {
        let was = view.list_state().logical_scroll_top();
        // Place it to read where it is, then put the list back.
        let start = anchor::heading_start(&headings, anchor).filter(|start| view.scroll_source_to_top(*start).is_some());
        if start.is_none() {
            render::beep(); // no such heading in the file: the click says so
        }
        let to = view.source_at_top();
        view.list_state().scroll_to(was);
        if let (Some(_), Some(to)) = (start, to.filter(|to| *to != from)) {
            glide.aim(from as f32, to as f32, now);
        }
        start.map(Landing::Source)
    } else {
        None
    };
    state.update(cx, |state, _| state.set_pending_anchor(None));
    landing
}

/// Feeds `state` the editor's text with a leading front matter block blanked, straight from the
/// rope's chunks: the editor's text is never gathered into a string of its own, and only the
/// lines of the front matter check are read. The rope is shared, not copied, so the state can be
/// updated while the text is read.
fn feed_editor_text(state: &Entity<TextViewState>, editor: &Entity<EditorState>, cx: &mut App) {
    let rope = editor.read(cx).text().clone();
    let mut rows = rope.iter_lines();
    let end = front_matter_end(|line| {
        let row = rows.next()?;
        if row.len() <= FENCE_LINE {
            row.chunks().for_each(|chunk| line.push_str(chunk));
        }
        Some(row.len() + 1)
    });
    let (end, blank) = match end {
        Some(end) => (end, blanked(rope.slice(..end).chars(), end)),
        None => (0, String::new()),
    };
    state.update(cx, |state, cx| {
        state.set_text_pieces(|| std::iter::once(blank.as_str()).chain(rope.slice(end..).chunks()), cx);
    });
}

/// Where a leading front matter block (a `---` line to the next `---` or `...` line) ends, in
/// bytes: the end of its closing line, before the line break. `next` gives the next line's
/// length, break included, or `None` at the end; it puts the line into the buffer only when it
/// is `FENCE_LINE` bytes or fewer, since no longer line is a fence.
fn front_matter_end(mut next: impl FnMut(&mut String) -> Option<usize>) -> Option<usize> {
    let mut line = String::new();
    let mut pos = 0;
    for i in 0.. {
        line.clear();
        let len = next(&mut line)?;
        let body = line.trim_end_matches(['\r', '\n']);
        if i == 0 {
            if body != "---" {
                return None;
            }
        } else if body == "---" || body == "..." {
            return Some(pos + body.len());
        }
        pos += len;
    }
    None
}

/// `chars` as spaces, line breaks kept, each as many bytes as it was, so every later byte keeps
/// its offset.
fn blanked(chars: impl Iterator<Item = char>, capacity: usize) -> String {
    let mut blanked = String::with_capacity(capacity);
    for c in chars {
        match c {
            '\n' | '\r' => blanked.push(c),
            _ => blanked.extend(std::iter::repeat(' ').take(c.len_utf8())),
        }
    }
    blanked
}

/// `text` with a leading front matter block replaced by spaces, newlines kept, so every later
/// byte keeps its offset. Text without a closed block comes back as it is, uncopied.
fn blank_front_matter(text: &str) -> Cow<'_, str> {
    let mut lines = text.split_inclusive('\n');
    let end = front_matter_end(|line| {
        let next = lines.next()?;
        line.push_str(next);
        Some(next.len())
    });
    let Some(end) = end else {
        return Cow::Borrowed(text);
    };
    let mut blank = blanked(text[..end].chars(), text.len());
    blank.push_str(&text[end..]);
    Cow::Owned(blank)
}

/// Which pane the other one follows.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum Driver {
    #[default]
    Editor,
    Preview,
}

/// What the editor pane leaves each frame it builds, for the panes that can't read the editor
/// without being rebuilt by its every caret blink: the preview pane (`Preview::follow`) and the
/// window's input handler. It also holds which of the editor and the preview leads.
#[derive(Default)]
pub struct EditorLink {
    /// The editor's top source byte (`editor_top_offset`), `None` before its first layout.
    pub top: Cell<Option<usize>>,
    /// Where the editor element sits in the window.
    pub bounds: Cell<Bounds<Pixels>>,
    /// The pane that leads.
    driver: Cell<Driver>,
    /// The showing editor, which the preview scrolls while it leads.
    editor: RefCell<Option<WeakEntity<EditorState>>>,
    /// The preview set the editor's scroll offset since the editor's last prepaint, so the
    /// editor's next move is that write's echo, not a move of its own.
    wrote: Cell<bool>,
    /// A wheel went over the editor since its last prepaint, and the way it went (`take_wheel`).
    wheeled: Cell<Option<f32>>,
    /// The caret's head byte and the editor it was read from (`caret_moved`).
    caret: Cell<Option<usize>>,
    caret_of: Cell<Option<EntityId>>,
    /// How many times the caret has moved, for the wash to tell a new move from a repaint.
    moves: Cell<u64>,
    /// The caret wash's view, woken by the editor probe on a caret move.
    pub layer: Cell<Option<EntityId>>,
    /// The preview's list and the pane its text draws in, left by `follow`'s paint.
    target: RefCell<Option<WeakEntity<TextViewState>>>,
    pane: Cell<Bounds<Pixels>>,
    /// The editor's text length at the caret's last notify: the preview's blocks are parsed from
    /// an older text, so the caret is looked up shifted by the difference in lengths.
    len: Cell<usize>,
    /// When the caret last moved, and the preview's (parsed length, block count) at its last paint.
    last_move: Cell<Option<Instant>>,
    parsed: Cell<(usize, usize)>,
}

impl EditorLink {
    /// Records `editor`'s top, from its prepaint, and says whether it moved. A move the preview
    /// didn't ask for (a key, find, a resize) makes the editor lead.
    pub fn editor_moved(&self, editor: &Entity<EditorState>, cx: &App) -> bool {
        if self.editor.borrow().as_ref().map(|weak| weak.entity_id()) != Some(editor.entity_id()) {
            *self.editor.borrow_mut() = Some(editor.downgrade());
        }
        let echo = self.wrote.take();
        let top = editor_top_offset(editor.read(cx));
        let moved = self.top.replace(top) != top;
        if moved && !echo {
            self.driver.set(Driver::Editor);
        }
        moved
    }

    /// Records the caret's head byte and the editor's text length from the editor's notify and
    /// says whether the caret moved. The first sight of an editor (a tab switch) is no move.
    pub fn caret_moved(&self, editor: &Entity<EditorState>, head: usize, len: usize) -> bool {
        self.len.set(len);
        let same = self.caret_of.replace(Some(editor.entity_id())) == Some(editor.entity_id());
        let moved = self.caret.replace(Some(head)) != Some(head) && same;
        if moved {
            self.moves.set(self.moves.get() + 1);
            self.last_move.set(Some(Instant::now()));
        }
        moved
    }

    /// Notes the preview's list and where its text draws, from the follow canvas's paint, for the
    /// caret wash's own view (which can't be inside the preview pane: its frames would render it).
    /// A parse that just landed, soon after a caret move, wakes that view once, so a caret that
    /// was looked up in the old blocks is looked up again in the new ones.
    fn aim_wash(&self, state: &Entity<TextViewState>, pane: Bounds<Pixels>, window: &mut Window, cx: &App) {
        if self.target.borrow().as_ref().map(|weak| weak.entity_id()) != Some(state.entity_id()) {
            *self.target.borrow_mut() = Some(state.downgrade());
        }
        self.pane.set(pane);
        let key = {
            let view = state.read(cx);
            (view.parsed_len(), view.list_state().item_count())
        };
        if self.parsed.replace(key) != key
            && self.last_move.get().is_some_and(|at| at.elapsed().as_secs_f32() < CARET_HOLD + CARET_FADE)
            && let Some(layer) = self.layer.get()
        {
            self.moves.set(self.moves.get() + 1);
            window.on_next_frame(move |_, cx| cx.notify(layer));
        }
    }

    /// The way a wheel went over the editor since the editor's last prepaint (positive up, 0 for an
    /// event with none), if one did (it stays set when that prepaint doesn't come). The editor's
    /// glide steps in its prepaint, a frame after the wheel's own, so the pane that follows is
    /// woken for the frame the glide first moves, not only once the move has been seen.
    pub fn take_wheel(&self) -> Option<f32> {
        self.wheeled.take()
    }

    /// Makes `driver` lead on a wheel or a mouse press over `bounds`, seen in the capture phase,
    /// before the pane under it scrolls. Call in paint.
    pub fn mark(self: &Rc<Self>, driver: Driver, bounds: Bounds<Pixels>, window: &mut Window) {
        let link = self.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, _, _| {
            if phase == DispatchPhase::Capture && bounds.contains(&event.position) {
                link.driver.set(driver);
                if driver == Driver::Editor {
                    let dy = match event.delta {
                        ScrollDelta::Pixels(delta) => f32::from(delta.y),
                        ScrollDelta::Lines(delta) => delta.y,
                    };
                    // A real way is kept over a gesture event with none (a trackpad's start or end).
                    if dy != 0. || link.wheeled.get().is_none() {
                        link.wheeled.set(Some(dy.signum() * f32::from(dy != 0.)));
                    }
                }
            }
        });
        let link = self.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, _| {
            if phase == DispatchPhase::Capture && bounds.contains(&event.position) {
                link.driver.set(driver);
            }
        });
    }
}

/// The byte offset in the editor's text at the top of its viewport: the
/// buffer line under the top display row (a wrapped line spans several rows,
/// and folded lines have none), plus the share of that line the viewport has
/// scrolled past. `None` before the editor's first layout.
pub fn editor_top_offset(editor: &EditorState) -> Option<usize> {
    let line_height = editor.line_height()?;
    let map = editor.display_map();
    let rows = map.display_row_count();
    if rows == 0 {
        return None;
    }
    let top = (-editor.scroll_offset().y).max(px(0.)) / line_height; // design: zero floor for a scroll offset, not a size
    let row = (top.floor() as usize).min(rows - 1);
    let line = map.display_row_to_buffer_line(row);
    let share = match map.buffer_line_to_display_row_range(line) {
        Some(range) => ((top - range.start as f32) / range.len() as f32).clamp(0., 1.),
        None => 0.,
    };
    let text = editor.text();
    let start = text.line_start_offset(line);
    let len = text.line_end_offset(line).saturating_sub(start);
    Some(start + (len as f32 * share) as usize)
}

/// The editor's vertical scroll offset that puts the byte `offset` at its top, the inverse of
/// `editor_top_offset`: the display rows of the buffer line holding it, entered by the share of the
/// line's bytes before it. A folded line counts as the nearest shown line above. `None` before the
/// editor's first layout.
fn editor_scroll_for(editor: &EditorState, offset: usize) -> Option<Pixels> {
    let line_height = editor.line_height()?;
    let map = editor.display_map();
    let text = editor.text();
    let line = text.offset_to_point(offset).row;
    let start = text.line_start_offset(line);
    let len = text.line_end_offset(line).saturating_sub(start);
    let share = if len == 0 { 0. } else { (offset.saturating_sub(start) as f32 / len as f32).clamp(0., 1.) };
    let (range, share) = match map.buffer_line_to_display_row_range(line) {
        Some(range) => (range, share),
        None => ((0..=line).rev().find_map(|line| map.buffer_line_to_display_row_range(line))?, 1.),
    };
    Some(-(line_height * (range.start as f32 + range.len() as f32 * share)))
}

#[cfg(test)]
mod wash_tests {
    use std::time::{Duration, Instant};

    use super::{SCROLL_GLIDE, Wash};

    #[test]
    fn link_wash_swells_once_and_a_second_click_restarts_from_the_drawn_level() {
        let start = Instant::now();
        let at = |seconds: f32| start + Duration::from_secs_f32(seconds);
        let first = Wash { items: 3..4, count: 9, from: 0., since: start };
        assert_eq!(first.level(start), Some(0.));
        let peak = first.level(at(SCROLL_GLIDE / 2.)).unwrap();
        assert!((peak - 1.).abs() < 1e-3);
        assert_eq!(first.level(at(SCROLL_GLIDE)), None);

        // Clicked again at a quarter of the way: the new wash opens on the level drawn there.
        let drawn = first.level(at(SCROLL_GLIDE / 4.)).unwrap();
        let second = Wash { items: 3..4, count: 9, from: drawn, since: at(SCROLL_GLIDE / 4.) };
        assert!((second.level(at(SCROLL_GLIDE / 4.)).unwrap() - drawn).abs() < 1e-6);
        assert_eq!(second.level(at(SCROLL_GLIDE / 4. + SCROLL_GLIDE)), None);
    }
}
