//! The caret, drawn by a view of its own over each editor (gpui-editor#grwg).
//!
//! The editor records where its carets are while it prepaints and paints none. This view
//! paints them from that record (`gpui_base`'s `CaretLayerHandle`), runs the glide and the
//! eased blink fade (gpui-editor#8d7h), and asks the frames they need. The workspace draws it
//! as a cached sibling of the editor pane, so a blink flip or a fade frame rebuilds this view
//! and the thin workspace, never the editor pane. It is notified with the editor, so a caret
//! that moves with an edit or a scroll is drawn in the editor's own frame. The editor's
//! scrollbar is drawn here too (gpui-editor#1vmr), for the same reason: its fade frames
//! rebuild this view, not the editor. So are the pinned scope lines (`sticky_scope.rs`), over
//! the caret and under the scrollbar.

use std::{cell::{Cell, RefCell}, rc::Rc};

use gpui_kit::{base::input::CaretLayerHandle, component::input::EditorState, *};

use crate::{scrollbar::{Bar, Marks}, sticky_scope};

pub struct CaretLayer {
    /// Weak: the caret layer doesn't keep a closed tab's document alive.
    state: WeakEntity<EditorState>,
    handle: CaretLayerHandle,
    /// How much of the caret shows while old editors fade out over the new one (the pane sets it).
    cover: Rc<Cell<f32>>,
    /// The scrollbar of the editor shown, with Find's marks and the subscription that wakes this view with the editor.
    bar: Option<(Bar, Marks, Subscription)>,
    /// What the pinned scope lines remember between frames, for the editor shown.
    sticky: Rc<RefCell<sticky_scope::Cache>>,
    _subscriptions: [Subscription; 2],
}

impl CaretLayer {
    pub fn new(state: Entity<EditorState>, cover: Rc<Cell<f32>>, cx: &mut Context<Self>) -> Self {
        let (handle, subscriptions) = Self::follow(&state, cx);
        Self { state: state.downgrade(), handle, cover, bar: None, sticky: sticky_scope::Cache::shared(), _subscriptions: subscriptions }
    }

    /// Takes over the caret of `state`: the editor stops painting it and notifies this view
    /// when it moves.
    fn follow(state: &Entity<EditorState>, cx: &mut Context<Self>) -> (CaretLayerHandle, [Subscription; 2]) {
        let handle = state.update(cx, |state, _| state.enable_caret_layer());
        handle.bind(cx.entity_id());
        let subscriptions = [handle.observe_blink(cx, |_, cx| cx.notify()), handle.observe(state, cx, |_, cx| cx.notify())];
        (handle, subscriptions)
    }

    /// Follows the caret of another editor, when the pane shows another tab.
    pub fn set(&mut self, state: Entity<EditorState>, cx: &mut Context<Self>) {
        if self.state.entity_id() == state.entity_id() {
            return;
        }
        self.handle.unbind(cx.entity_id());
        (self.handle, self._subscriptions) = Self::follow(&state, cx);
        self.state = state.downgrade();
        self.sticky = sticky_scope::Cache::shared();
        cx.notify();
    }

    /// Lets go of the caret for good, when the pane closes: the editor it holds draws its own
    /// bright line number while it glides shut.
    pub fn release(&mut self, cx: &mut Context<Self>) {
        self.handle.unbind(cx.entity_id());
    }

    /// Draws the scrollbar of `state`, the editor this layer follows: its knob and Find's ticks
    /// fade and ease here, so those frames rebuild this view and not the editor pane
    /// (gpui-editor#1vmr). The pane calls it on each render; the layer keeps the bar it has
    /// when it's the same, and follows the editor's notifications so the knob is drawn in the
    /// frame the text scrolls.
    pub fn set_bar(&mut self, state: &Entity<EditorState>, bar: Bar, marks: Marks, cx: &mut Context<Self>) {
        if self.bar.as_ref().is_some_and(|(kept, _, _)| kept.feed.ptr_eq(&bar.feed)) {
            return;
        }
        let watch = cx.observe(state, |_, _, cx| cx.notify());
        self.bar = Some((bar, marks, watch));
        cx.notify();
    }

    /// Lets go of the bar and its marks, when the pane has no editor left.
    pub fn clear_bar(&mut self, cx: &mut Context<Self>) {
        if self.bar.take().is_some() {
            cx.notify();
        }
    }
}

impl Render for CaretLayer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .opacity(self.cover.get())
            .child(self.handle.element())
            .children(self.bar.as_ref().map(|(bar, _, _)| sticky_scope::layer(self.state.clone(), bar.feed.clone(), self.sticky.clone())))
            .children(self.bar.as_ref().map(|(bar, marks, _)| crate::scrollbar::editor_overlay(bar, marks)))
    }
}
