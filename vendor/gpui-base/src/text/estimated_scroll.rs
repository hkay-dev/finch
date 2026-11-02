//! Scrollbar handle for a block list that measures only what shows.
//!
//! A plain `ListState` counts blocks it has not laid out as 0 px, so its
//! scrollbar is sized from the measured part of the document and a thumb drag
//! is clamped to that part. This handle sizes the track from an estimate of
//! the whole document instead: each block weighs its source length, a block
//! laid out so far counts its real height, and the rest count weight times the
//! pixels per source byte learned from the laid-out blocks. A drag position
//! maps back to a block and an offset inside it, so the bottom of the track
//! lands on the last block. Nothing is measured to do this.

use std::{cell::RefCell, rc::Rc};

use gpui::{Bounds, ListOffset, ListState, Pixels, Point, Size, point, px};

use crate::scrollbar::ScrollbarHandle;

/// Pixels per source byte before any block has been laid out.
const DEFAULT_PX_PER_BYTE: f32 = 0.4;
/// Source bytes of weight the default carries against the laid-out blocks, so
/// the first few blocks (often headings and badges) do not set the whole scale.
const PRIOR_BYTES: f32 = 1500.;
/// Lowest weight of a block, so an empty or span-less block still has a height.
const MIN_WEIGHT: f32 = 24.;

/// The overdraw the list is built with (`measure_visible_only`): the blocks
/// that far below the viewport are measured with the visible ones.
const OVERDRAW: Pixels = px(1000.);

#[derive(Default)]
struct Inner {
    /// Source length of each block.
    weights: Vec<f32>,
    /// Height of each block as last laid out.
    heights: Vec<Option<f32>>,
    /// Estimated height above each block, and the total at the end.
    prefix: Vec<f32>,
    /// While dragging the estimate holds still, so the thumb does not move
    /// under the pointer as blocks get measured.
    dragging: bool,
    /// Viewport width the heights were laid out at.
    width: f32,
    /// The document version `weights` were read at (`set_weights`).
    version: u64,
    /// A weight or a height changed since `prefix` was built.
    stale: bool,
    /// Passes over every block made so far (`refresh`'s sums and prefix, `set_weights`).
    #[cfg(test)]
    passes: usize,
}

#[derive(Clone)]
pub(crate) struct EstimatedScroll {
    list: ListState,
    inner: Rc<RefCell<Inner>>,
}

impl EstimatedScroll {
    pub(crate) fn new(list: ListState) -> Self {
        Self {
            list,
            inner: Rc::default(),
        }
    }

    /// How many passes over every block have been made so far.
    #[cfg(test)]
    pub(crate) fn passes(&self) -> usize {
        self.inner.borrow().passes
    }

    /// Sets the source length of every block, in order, when the document is a new `version`
    /// since the last call (the lengths of one version never change).
    pub(crate) fn set_weights(&self, version: u64, lengths: impl ExactSizeIterator<Item = Option<usize>>) {
        let mut inner = self.inner.borrow_mut();
        if inner.dragging || inner.version == version {
            return;
        }
        inner.version = version;
        inner.stale = true;
        #[cfg(test)]
        {
            inner.passes += 1;
        }
        let count = lengths.len();
        inner.weights.clear();
        inner
            .weights
            .extend(lengths.map(|len| (len.unwrap_or(0) as f32).max(MIN_WEIGHT)));
        if inner.heights.len() != count {
            inner.heights.clear();
            inner.heights.resize(count, None);
        }
    }

    /// Records the heights of the blocks laid out now, and rebuilds the sums when a weight or a
    /// height is not what they were built from: scrolling over blocks already measured costs
    /// the few blocks laid out, not a pass over the document.
    fn refresh(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.dragging {
            return;
        }
        // The list drops every height on a width change.
        let viewport = self.list.viewport_bounds();
        let width = f32::from(viewport.size.width);
        if inner.width != width {
            inner.width = width;
            inner.heights.fill(None);
            inner.stale = true;
        }
        let count = inner.weights.len().min(self.list.item_count());
        if inner.heights.len() == count {
            let top = self.list.logical_scroll_top().item_ix;
            for ix in top..count {
                let Some(bounds) = self.list.bounds_for_item(ix) else {
                    break;
                };
                let height = Some(f32::from(bounds.size.height));
                if inner.heights[ix] != height {
                    inner.heights[ix] = height;
                    inner.stale = true;
                }
                // Blocks below the overdraw were measured long ago, if at all.
                if bounds.bottom() >= viewport.bottom() + OVERDRAW {
                    break;
                }
            }
        }
        if !inner.stale && inner.prefix.len() == inner.weights.len() + 1 {
            return;
        }
        inner.stale = false;
        #[cfg(test)]
        {
            inner.passes += 2;
        }

        let (mut measured_height, mut measured_weight) = (0., 0.);
        for (weight, height) in inner.weights.iter().zip(&inner.heights) {
            if let Some(height) = height {
                measured_height += height;
                measured_weight += weight;
            }
        }
        let px_per_byte = (measured_height + PRIOR_BYTES * DEFAULT_PX_PER_BYTE)
            / (measured_weight + PRIOR_BYTES);

        let Inner {
            weights,
            heights,
            prefix,
            ..
        } = &mut *inner;
        prefix.clear();
        prefix.push(0.);
        let mut above = 0.;
        for (ix, weight) in weights.iter().enumerate() {
            above += heights
                .get(ix)
                .copied()
                .flatten()
                .unwrap_or(weight * px_per_byte);
            prefix.push(above);
        }
    }

    fn max_scroll(&self) -> f32 {
        let total = self.inner.borrow().prefix.last().copied().unwrap_or(0.);
        (total - f32::from(self.list.viewport_bounds().size.height)).max(0.)
    }
}

impl ScrollbarHandle for EstimatedScroll {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.list.viewport_bounds()
    }

    fn offset(&self) -> Point<Pixels> {
        self.refresh();
        let max = self.max_scroll();
        let inner = self.inner.borrow();
        let count = inner.weights.len();
        if count == 0 || inner.prefix.len() != count + 1 {
            return Point::default();
        }
        let top = self.list.logical_scroll_top();
        let mut y = inner.prefix[top.item_ix.min(count)] + f32::from(top.offset_in_item);
        // The list stops at the real end of the last block, wherever the
        // estimate puts it: report the end of the track then.
        let viewport = self.list.viewport_bounds();
        if self
            .list
            .bounds_for_item(count - 1)
            .is_some_and(|last| last.bottom() <= viewport.bottom() + px(1.))
        {
            y = max;
        }
        point(px(0.), px(-y.min(max)))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let max = self.max_scroll();
        let inner = self.inner.borrow();
        let count = inner.weights.len();
        if count == 0 || inner.prefix.len() != count + 1 {
            return;
        }
        let y = f32::from(-offset.y).clamp(0., max);
        // The end of the track is the end of the document whatever the
        // estimate says: one past the last block is the list's end sentinel,
        // and the list lays out upward from the real end.
        let (item_ix, within) = if y >= max - 1. {
            (count, 0.)
        } else {
            let item_ix = inner
                .prefix
                .partition_point(|&above| above <= y)
                .saturating_sub(1)
                .min(count - 1);
            let within = y - inner.prefix[item_ix];
            (item_ix, inner.heights[item_ix].map_or(within, |height| within.min(height)))
        };
        self.list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(within),
        });
    }

    fn content_size(&self) -> Size<Pixels> {
        self.refresh();
        let viewport = self.list.viewport_bounds().size;
        Size {
            width: viewport.width,
            height: viewport.height + px(self.max_scroll()),
        }
    }

    fn start_drag(&self) {
        self.refresh();
        self.inner.borrow_mut().dragging = true;
        self.list.scrollbar_drag_started();
    }

    fn end_drag(&self) {
        self.inner.borrow_mut().dragging = false;
        self.list.scrollbar_drag_ended();
    }
}
