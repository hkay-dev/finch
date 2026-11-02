//! The carets of the explorer's text fields, the filter and a row's name field, drawn by a view of
//! their own (gpui-editor#em8z), as `caret_layer.rs` draws the editor's.
//!
//! The fields draw inside the cached explorer and record their carets in its prepaint, painting
//! none; this view paints them, so a blink flip or a fade frame rebuilds this view and the thin
//! workspace, never the explorer's rows. The workspace draws it as a cached sibling over the
//! explorer column. It is notified with the explorer, so a field that moves with a scroll, a fold
//! or an edit has its caret drawn in the explorer's own frame.

use gpui_kit::{base::input::CaretLayerHandle, component::input::InputState, *};

use super::Explorer;

/// One field's caret record and the subscriptions that wake this view for it.
struct Field {
    handle: CaretLayerHandle,
    _subscriptions: [Subscription; 2],
}

pub struct FieldCarets {
    explorer: WeakEntity<Explorer>,
    filter: Field,
    /// The name field open in a row, for a rename or a new item.
    name: Option<Field>,
}

impl FieldCarets {
    pub fn new(explorer: &Entity<Explorer>, filter: &Entity<InputState>, cx: &mut Context<Self>) -> Self {
        let filter = Self::follow(explorer, filter, cx);
        Self { explorer: explorer.downgrade(), filter, name: None }
    }

    /// Takes over the caret of `input`: the field stops painting it, and its blink wakes this view.
    fn follow(explorer: &Entity<Explorer>, input: &Entity<InputState>, cx: &mut Context<Self>) -> Field {
        let handle = input.update(cx, |input, _| input.enable_caret_layer());
        handle.bind(cx.entity_id());
        let subscriptions = [handle.observe_blink(cx, |_, cx| cx.notify()), handle.observe(explorer, cx, |_, cx| cx.notify())];
        Field { handle, _subscriptions: subscriptions }
    }

    /// Takes over the caret of the name field that opened in a row, or lets go of the one that
    /// closed (`None`).
    pub fn set_name_field(&mut self, input: Option<&Entity<InputState>>, cx: &mut Context<Self>) {
        if let Some(old) = self.name.take() {
            old.handle.unbind(cx.entity_id());
        }
        self.name = input.zip(self.explorer.upgrade()).map(|(input, explorer)| Self::follow(&explorer, input, cx));
        cx.notify();
    }

    /// The name field's caret record, for the pinned rows to cover.
    pub fn name_field(&self) -> Option<CaretLayerHandle> {
        self.name.as_ref().map(|field| field.handle.clone())
    }
}

impl Render for FieldCarets {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let layer = |field: &Field| div().absolute().inset_0().child(field.handle.element());
        div().size_full().child(layer(&self.filter)).children(self.name.as_ref().map(layer))
    }
}
