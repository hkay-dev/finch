//! Pinned folder names: while the tree scrolls inside a deep folder, the open folders that hold
//! the top row stay at the top of the list, one per depth, outermost first. The deepest slides up
//! and away as its subtree's last row passes under it.
//!
//! The anchor search and the drift are Zed's (`crates/ui/src/components/sticky_items.rs`); the
//! pinned rows are our own rows, drawn by the list as a decoration over the sidebar's material:
//! nothing opaque sits under them, so the rows below fade out themselves (`EdgeFade`) before they
//! reach a pinned name. A pinned row moves with the scroll offset itself, never with an animation
//! of its own.

use std::{cell::Cell, collections::HashSet, ops::Range, path::PathBuf, rc::Rc};

use gpui_kit::base::{TreeEntry, TreeState, input::CaretLayerHandle};
use gpui_kit::*;

use super::{Explorer, git, row};
use crate::{
    hover::FadeHover as _,
    motion,
    scrollbar::Edge,
    theme,
    tokens::{self, ALPHA_HOVER, ALPHA_HOVER_LIGHT, RADIUS_ROW, ROW_HEIGHT, ROW_WASH_INSET_Y},
};

/// The rows the list pins, as the tree's decoration. It holds the tree and the explorer weakly:
/// the tree stores the decoration.
pub struct Sticky {
    tree: WeakEntity<TreeState>,
    scroll: UniformListScrollHandle,
    editor: WeakEntity<Explorer>,
    marked: Rc<HashSet<PathBuf>>,
    ignored: Rc<HashSet<PathBuf>>,
    git: Rc<git::Statuses>,
    ink: row::Ink,
    picked: row::PickedRows,
    /// The caret of a name field open in a row, and that row's id. The caret layer paints over
    /// the rows, so it is told where the pinned rows hide the field, and when the row
    /// isn't drawn at all. The list builds its decoration after its rows, in the same frame.
    name_field: Option<(CaretLayerHandle, SharedString)>,
}

impl Sticky {
    pub fn new(
        tree: WeakEntity<TreeState>,
        scroll: UniformListScrollHandle,
        editor: WeakEntity<Explorer>,
        marked: Rc<HashSet<PathBuf>>,
        ignored: Rc<HashSet<PathBuf>>,
        git: Rc<git::Statuses>,
        ink: row::Ink,
        picked: row::PickedRows,
        name_field: Option<(CaretLayerHandle, SharedString)>,
    ) -> Self {
        Self { tree, scroll, editor, marked, ignored, git, ink, picked, name_field }
    }

    fn cover(&self, pinned: Option<Bounds<Pixels>>) {
        if let Some((field, _)) = &self.name_field {
            field.set_cover(pinned);
        }
    }
}

/// A row's depth, by index.
pub type Depth<'a> = &'a dyn Fn(usize) -> usize;

/// Zed's `find_sticky_anchor`: the first row of `range` that stands clear of its own pinned
/// folders (one per depth), or the last row of a subtree (the next row is shallower) while the
/// folders it ends can still be seen being pushed up. Zed lets only a one-level outdent push; here
/// any outdent does, folder by folder (see `stack`), so the stack leaves one name at a time.
fn find_anchor(depth: Depth, range: Range<usize>) -> Option<usize> {
    for ix in range.clone() {
        let own = depth(ix);
        if own < ix - range.start {
            return Some(ix);
        }
        if ix + 1 < range.end && depth(ix + 1) < own {
            return Some(ix);
        }
    }
    None
}

/// The pinned rows of one frame and where each sits.
struct Stack {
    /// The folders pinned, outermost first, by row index.
    parents: [usize; 32],
    count: usize,
    /// Each pinned row's top, below the viewport's top.
    ys: [Pixels; 32],
    /// The bottom of the lowest pinned row: the stack's height as it shows.
    lowest: Pixels,
}

/// The stack for the rows `visible` of a list of `len` rows scrolled `top` points: the anchor
/// search, the folders holding the anchor, and the slot each rests on. `None` when nothing pins.
fn stack(depth: Depth, len: usize, visible: Range<usize>, top: Pixels, item_height: Pixels) -> Option<Stack> {
    if visible.end > len || visible.is_empty() {
        return None;
    }
    let start = visible.start;
    let anchor = find_anchor(depth, visible)?;
    let anchor_depth = depth(anchor);

    // The folders holding the anchor, deepest first: the nearest earlier row one level up each
    // time. One whose own row is still on screen needs no pinned copy.
    let mut parents: [usize; 32] = [0; 32];
    let mut count = 0;
    let mut want = anchor_depth;
    for ix in (0..anchor).rev() {
        if want == 0 || count == parents.len() {
            break;
        }
        if depth(ix) + 1 == want {
            if ix < start {
                parents[count] = ix;
                count += 1;
            }
            want -= 1;
        }
    }
    if count == 0 {
        return None;
    }
    parents[..count].reverse();

    let rows = count;
    // Each folder's subtree ends where a later row is as shallow as it: that row pushes it up.
    // From the deepest up, a row rests on its slot, or on the row under it, or on the pusher.
    // Only rows within the pinned area's reach below the top are looked at.
    let reach = (anchor + 2).max(start + rows + 2).min(len);
    let mut ys = [Pixels::ZERO; 32];
    for slot in (0..rows).rev() {
        let own = depth(parents[slot]);
        let mut y = item_height * slot;
        if let Some(end) = (anchor + 1..reach).find(|&next| depth(next) <= own) {
            y = y.min(item_height * end - top - item_height);
        }
        if slot + 1 < rows {
            y = y.min(ys[slot + 1] - item_height);
        }
        ys[slot] = y;
    }
    let lowest = ys[..rows].iter().fold(Pixels::ZERO, |low, &y| low.max(y + item_height));
    Some(Stack { parents, count, ys, lowest })
}

/// How tall the pinned stack shows with the list `scrolled` points down and `view` points tall,
/// from each row's depth: the same search the stack is drawn from, for the rows to fade under it.
fn height(depth: Depth, len: usize, scrolled: f32, view: f32) -> f32 {
    let row = f32::from(ROW_HEIGHT);
    // The list's own visible range, from its scroll offset.
    let first = (scrolled / row).floor() as usize;
    let last = (((scrolled + view) / row).ceil() as usize).min(len);
    stack(depth, len, first..last, px(scrolled), ROW_HEIGHT).map_or(0., |stack| f32::from(stack.lowest))
}

/// How many bands trace the smoothstep of a guide line's fade, as `scrollbar::paint_ramp` does.
const FADE_BANDS: usize = 8;

/// The tree's edge fades as alpha: its ink thins at the edges the list still scrolls past, and
/// under the pinned stack, instead of a ground painted over it, so the sidebar's material shows
/// through unbroken. GPUI has no alpha mask for content, so the rows (by their centre) and the
/// washes fade whole, and the indent guides fade along their length. The ramp has the shape and
/// strengths of `scrollbar::paint_ramp` (`LIST_EDGE_FADE`, smoothstep): clear at the stack's bottom
/// (the list's top with no pins) and at the visible bottom, whole `LIST_EDGE_FADE` in. `y` is in
/// points from the list's top. The default fades nothing.
#[derive(Clone, Copy, Default)]
pub struct EdgeFade {
    scrolled: f32,
    /// The most the list scrolls.
    max: f32,
    /// The visible height, without the fold's overdraw.
    view: f32,
    /// The bottom of the pinned stack, 0 with none.
    pinned: f32,
}

/// What `EdgeFade::read` last read, and the scroll offset, list height and overdraw it was read at.
pub type Memo = Cell<Option<([f32; 3], EdgeFade)>>;

impl EdgeFade {
    /// Reads the list's scroll and the pinned stack of its `len` rows (`depth` of each); `overdraw`
    /// is how far the list reaches past its visible bottom. The pinned stack's search walks up to
    /// the top-level folder, so it is redone only when the offset, height or overdraw in `memo`
    /// change. Nothing fades while the list is busy.
    pub fn read(memo: &Memo, scroll: &UniformListScrollHandle, overdraw: f32, depth: Depth, len: usize) -> Self {
        // The list measures a row while it holds its own state; that row is never shown.
        let Ok(state) = scroll.0.try_borrow() else {
            return Self::default();
        };
        let key = [f32::from(state.base_handle.offset().y), f32::from(state.base_handle.bounds().size.height), overdraw];
        drop(state);
        if let Some((was, fade)) = memo.get()
            && was == key
        {
            return fade;
        }
        let edge = Edge::of(scroll, key[1] - overdraw);
        let fade = Self { scrolled: edge.scrolled, max: edge.max, view: edge.height, pinned: height(depth, len, edge.scrolled, key[1]) };
        memo.set(Some((key, fade)));
        fade
    }

    fn strengths(&self) -> (f32, f32) {
        let fade = f32::from(tokens::LIST_EDGE_FADE);
        let top = if self.pinned > 0. { 1. } else { (self.scrolled / fade).clamp(0., 1.) };
        (top, ((self.max - self.scrolled) / fade).clamp(0., 1.))
    }

    /// How much ink shows at `y`, 0 to 1.
    pub fn at(&self, y: f32) -> f32 {
        let fade = f32::from(tokens::LIST_EDGE_FADE);
        let (top, bottom) = self.strengths();
        let smooth = |x: f32| {
            let x = x.clamp(0., 1.);
            x * x * (3. - 2. * x)
        };
        let clear = |strength: f32, distance: f32| 1. - strength * (1. - smooth(distance / fade));
        clear(top, y - self.pinned) * clear(bottom, self.view - y)
    }

    /// How much of row `ix`'s ink shows, read at the row's centre where it is drawn: its slot's,
    /// `shift` points down when a fold draws it that far up (`FoldView::shift`).
    pub fn row(&self, ix: usize, shift: f32) -> f32 {
        self.at(f32::from(ROW_HEIGHT) * (ix as f32 + 0.5) - shift - self.scrolled)
    }

    /// A line from `from` to `to` cut where its fade bends: each piece, as `(from, to, alpha at
    /// from, alpha at to)` to paint as a gradient, is pushed onto `out`. Pieces with no ink are left out.
    pub fn pieces(&self, from: f32, to: f32, out: &mut Vec<(f32, f32, f32, f32)>) {
        let fade = f32::from(tokens::LIST_EDGE_FADE);
        let (top, bottom) = self.strengths();
        // The two ends and, for each band edge of each fade, one cut.
        let mut cuts = [0.; 2 + 2 * (FADE_BANDS + 1)];
        let mut n = 0;
        let mut cut = |at: f32| {
            if (from..=to).contains(&at) {
                cuts[n] = at;
                n += 1;
            }
        };
        cut(from);
        cut(to);
        for band in 0..=FADE_BANDS {
            let along = fade * band as f32 / FADE_BANDS as f32;
            if top > 0. {
                cut(self.pinned + along);
            }
            if bottom > 0. {
                cut(self.view - along);
            }
        }
        let cuts = &mut cuts[..n];
        cuts.sort_by(f32::total_cmp);
        for pair in cuts.windows(2) {
            if pair[1] > pair[0] {
                let piece = (pair[0], pair[1], self.at(pair[0]), self.at(pair[1]));
                if piece.2 > 0. || piece.3 > 0. {
                    out.push(piece);
                }
            }
        }
    }
}

/// How many rows the pinned stack covers at the top when row `ix` is shown as high as it can be
/// without sitting under it, for the tree's keys to scroll a selection to: the fewest rows `n`
/// such that with `ix` `n` rows below the top, the stack drawn there is `n` rows or less. The
/// same search as the drawn stack, so the row lands where the pinned names end. `view` is how
/// many rows the list shows.
pub fn covered_rows(entries: &[TreeEntry], ix: usize, view: usize) -> usize {
    let depth = |at: usize| entries[at].depth();
    // The stack never holds more than its 32 slots.
    let most = ix.min(32);
    for n in 0..most {
        let start = ix - n;
        let end = (start + view + 1).min(entries.len());
        let lowest = stack(&depth, entries.len(), start..end, ROW_HEIGHT * start, ROW_HEIGHT).map_or(Pixels::ZERO, |stack| stack.lowest);
        if (lowest / ROW_HEIGHT).ceil() as usize <= n {
            return n;
        }
    }
    most
}

/// The height of the pinned header's shadow.
const SHADOW_HEIGHT: Pixels = px(6.);
/// The shadow's black at its top edge.
const SHADOW_ALPHA: f32 = 0.18;
/// The shadow's black on a light theme is this share of that.
const SHADOW_LIGHT: f32 = 0.7;
/// The shadow comes in over this much scroll past the first pin.
const SHADOW_IN: f32 = 8.;

impl UniformListDecoration for Sticky {
    fn compute(
        &self,
        visible_range: Range<usize>,
        bounds: Bounds<Pixels>,
        scroll_offset: Point<Pixels>,
        item_height: Pixels,
        _item_count: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let nothing = || div().into_any_element();
        let Some(tree) = self.tree.upgrade() else {
            self.cover(None);
            return nothing();
        };
        let state = tree.read(cx);
        let entries = state.entries();
        // A row scrolled or folded out of view drew no field, so its caret record is last frame's.
        if let Some((field, id)) = &self.name_field
            && !entries.get(visible_range.clone()).is_some_and(|rows| rows.iter().any(|entry| entry.item().id == *id))
        {
            field.hide();
        }
        // Where the pinned area sits: the viewport's top (the list's bounds start at the scrolled content's).
        let top = -scroll_offset.y;
        let depth = |ix: usize| entries[ix].depth();
        let Some(Stack { parents, count, ys, lowest }) = stack(&depth, entries.len(), visible_range, top, item_height) else {
            self.cover(None);
            return nothing();
        };
        self.cover(Some(Bounds::new(point(bounds.origin.x, bounds.origin.y + top), size(bounds.size.width, lowest))));
        let parents = &parents[..count];

        let window_id = window.window_handle().window_id();
        let now = motion::drawn_now(window_id);
        let selected = state.selected_index();
        let light = theme::is_light(cx);
        let hover = self.ink.text.opacity(if light { ALPHA_HOVER_LIGHT } else { ALPHA_HOVER });

        let mut area = div().absolute().top(top).left_0().w_full();
        for (slot, &ix) in parents.iter().enumerate() {
            let entry = &entries[ix];
            let level = row::ink_level(window_id, &entry.item().id, selected == Some(ix), now);
            let content = row::row(ix, entry, &self.marked, &self.ignored, self.ink, &self.editor, None, None, None, row::FoldView::default(), level, &self.git, &self.picked);
            let y = ys[slot];
            let (tree, scroll) = (self.tree.clone(), self.scroll.clone());
            let (editor, id) = (self.editor.clone(), entry.item().id.clone());
            area = area.child(
                div()
                    .id(("sticky-row", slot))
                    .absolute()
                    .top(y)
                    .left_0()
                    .w_full()
                    .h(ROW_HEIGHT)
                    // A click stops here, the wheel still scrolls the list under it.
                    .block_mouse_except_scroll()
                    // The pointer on a pinned name takes the hover from the row hidden under it.
                    .on_hover(move |hovered, window, cx| {
                        if *hovered {
                            row::set_hovered(window, id.clone());
                            if let Ok(shapes) = editor.read_with(cx, |explorer, _| explorer.shapes.entity_id()) {
                                cx.notify(shapes);
                            }
                        }
                    })
                    // Like Zed: scroll so the folder's own row sits right under the folders above it, and select it.
                    .on_click(move |_, _, cx| {
                        tree.update(cx, |tree, cx| tree.set_selected_index(Some(ix), cx)).ok();
                        scroll.scroll_to_item_strict_with_offset(ix, ScrollStrategy::Top, slot);
                    })
                    .child(
                        // The wash fades in and out like every row's, a point in from the row's top and bottom.
                        div()
                            .id(("sticky-hover", ix))
                            .absolute()
                            .top(ROW_WASH_INSET_Y)
                            .left_0()
                            .w_full()
                            .h(ROW_HEIGHT - ROW_WASH_INSET_Y * 2.)
                            .rounded(RADIUS_ROW)
                            .fade_hover(("sticky-hover", ix), None, hover)
                            .child(div().absolute().top(-ROW_WASH_INSET_Y).left_0().w_full().h(ROW_HEIGHT).child(content)),
                    ),
            );
        }
        // A soft shadow under the stack's lowest row, riding with the rows as they are pushed. It
        // comes in over the first `SHADOW_IN` points of scroll past the first pin.
        let into = ((f32::from(top) - f32::from(item_height) * parents[0] as f32) / SHADOW_IN).clamp(0., 1.);
        let black = gpui::black().opacity(SHADOW_ALPHA * if light { SHADOW_LIGHT } else { 1. } * into);
        area = area.child(
            div()
                .absolute()
                .top(lowest)
                .left_0()
                .w_full()
                .h(SHADOW_HEIGHT)
                .bg(linear_gradient(180., linear_color_stop(black, 0.), linear_color_stop(black.opacity(0.), 1.))),
        );

        div().w(bounds.size.width).h(bounds.size.height).child(area).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::base::{TreeEntry, TreeItem};

    use super::covered_rows;

    /// `root/d1/d2` and twenty files in `d2`, as the rows an expanded tree lists.
    fn rows() -> Vec<TreeEntry> {
        let item = |name: String| TreeItem::new(name.clone(), name);
        let mut rows = vec![TreeEntry::new(&item("root".into()), 0), TreeEntry::new(&item("d1".into()), 1), TreeEntry::new(&item("d2".into()), 2)];
        rows.extend((0..20).map(|n| TreeEntry::new(&item(format!("f{n}")), 3)));
        rows
    }

    #[test]
    fn a_row_deep_in_a_folder_clears_the_pinned_names_above_it() {
        let rows = rows();
        // Three folders are pinned over a file well inside d2, so it sits three rows down.
        assert_eq!(covered_rows(&rows, 13, 8), 3);
        // d2's own row is one row down, with only root pinned over d1's row.
        assert_eq!(covered_rows(&rows, 2, 8), 1);
        // Near the top nothing scrolls under anything.
        assert_eq!(covered_rows(&rows, 0, 8), 0);
    }
}
