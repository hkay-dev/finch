//! Edit > Undo history: a slim slider over the text that scrubs the file back through this
//! session's edits (finch#832h).
//!
//! Each stop is one undo step. Dragging replays the jump as one batch (`EditorState::history_jump`:
//! one highlighter update, one `Change`, one notify however far it goes), so the changed lines wash
//! as the editor's own Undo washes them. Letting go stays there; Escape puts the text back as it was
//! and closes; closing any other way keeps the scrubbed text and folds the newer steps into one, so a
//! single Undo brings the newest text back and nothing is dropped. The first edit made while scrubbed
//! back asks before it drops the newer steps.
//!
//! The slider is a view of its own, drawn beside the editor pane by the workspace, and it never
//! observes the editor: its state moves with its own mouse and with the guard the editor calls after
//! an Undo or Redo. It draws no frames at rest.

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::base::input::HistoryGuard;
use gpui_kit::component::{ActiveTheme as _, input::EditorState};
use gpui_kit::*;

use crate::{
    document::Document,
    theme, tooltip,
    tokens::{ALPHA_BORDER, ALPHA_PRIMARY, ALPHA_SECONDARY, ALPHA_TERTIARY, FIND_PAD, RADIUS_FLOAT_CAPSULE, TEXT_DETAIL, TEXT_DETAIL_LINE},
};

/// The float's width, capped by the pane's.
pub const WIDTH: Pixels = px(420.);
/// The float's height: the track's hit area plus the float's padding.
pub const HEIGHT: Pixels = px(36.);
/// The track's hit height, and where its line and knob sit in it.
const TRACK: f32 = 20.;
const KNOB: f32 = 10.;

/// How a session ends.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Close {
    /// Keep the text where it stands; newer steps fold into one Undo.
    Keep,
    /// Escape: the text goes back to where it was when the slider opened.
    Restore,
    /// The newer steps were dropped on purpose; nothing to fold.
    Dropped,
}

pub enum UndoSliderEvent {
    Closed,
}

impl EventEmitter<UndoSliderEvent> for UndoSlider {}

/// The key context the slider's keys live in.
const KEY_CONTEXT: &str = "UndoSlider";

actions!(undo_slider, [SliderBack, SliderForward, SliderBackFar, SliderForwardFar, SliderStart, SliderEnd, SliderKeep, SliderCancel]);

/// Binds the slider's keys when the keymap lacks them (the first open, and again after the app
/// clears and rebinds on a key-setting change): Left and Right a stop, Option a tenth of the
/// history, Home and End the ends, Return keeps, Escape restores.
pub fn bind_keys(cx: &mut App) {
    if cx.key_bindings().borrow().bindings_for_action(&SliderBack).next().is_some() {
        return;
    }
    cx.bind_keys([
        KeyBinding::new("left", SliderBack, Some(KEY_CONTEXT)),
        KeyBinding::new("right", SliderForward, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-left", SliderBackFar, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-right", SliderForwardFar, Some(KEY_CONTEXT)),
        KeyBinding::new("home", SliderStart, Some(KEY_CONTEXT)),
        KeyBinding::new("end", SliderEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", SliderKeep, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", SliderCancel, Some(KEY_CONTEXT)),
    ]);
}

pub struct UndoSlider {
    document: WeakEntity<Document>,
    editor: WeakEntity<EditorState>,
    /// Steps applied when the session began (or when the text last changed outside it).
    origin: usize,
    /// Steps in all (applied and waiting).
    total: usize,
    /// Steps applied now.
    position: usize,
    /// When each step was made, oldest first.
    times: Vec<Instant>,
    track: Rc<Cell<Bounds<Pixels>>>,
    dragging: bool,
    asking: bool,
    focus: FocusHandle,
}

impl UndoSlider {
    /// A slider over `editor`, the editor of `document`. The editor's history guard and the
    /// document's scrub state are set here and let go at `shut`.
    pub fn new(document: &Entity<Document>, editor: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let stops = editor.read(cx).history_stops();
        let weak = cx.weak_entity();
        let me = weak.clone();
        let blocked = {
            let weak = weak.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                weak.update(cx, |this, cx| this.ask_to_drop(window, cx)).ok();
            })
        };
        let stepped = Rc::new(move |_: &mut Window, cx: &mut App| {
            weak.update(cx, |this, cx| this.refresh(cx)).ok();
        });
        editor.update(cx, |state, _| state.set_history_guard(Some(HistoryGuard { blocked, stepped })));
        document.update(cx, |document, cx| document.begin_scrub(me, cx));
        Self {
            document: document.downgrade(),
            editor: editor.downgrade(),
            origin: stops.applied,
            total: stops.applied + stops.waiting,
            position: stops.applied,
            times: stops.times,
            track: Rc::new(Cell::new(Bounds::default())),
            dragging: false,
            asking: false,
            focus,
        }
    }

    /// The history as the editor has it now. Steps made since (typing at the newest text) make
    /// this the new baseline for Escape.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else { return };
        let stops = editor.read(cx).history_stops();
        if stops.applied + stops.waiting != self.total {
            self.origin = stops.applied;
            self.total = stops.applied + stops.waiting;
        }
        self.position = stops.applied;
        self.times = stops.times;
        cx.notify();
    }

    /// A press on the track: it picks the stop under the pointer and starts a drag.
    fn press(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
        self.dragging = true;
        self.drag_to(x, window, cx);
    }

    fn drag_to(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = self.track.get();
        let width = f32::from(bounds.size.width).max(1.);
        let fraction = (f32::from(x - bounds.origin.x) / width).clamp(0., 1.);
        self.go((fraction * self.total as f32).round() as usize, window, cx);
    }

    /// Moves the text to `position` steps applied, as one replay.
    fn go(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        let position = position.min(self.total);
        if position == self.position {
            return;
        }
        let Some(editor) = self.editor.upgrade() else { return };
        editor.update(cx, |state, cx| state.history_jump(position, window, cx));
        self.position = position;
        cx.notify();
    }

    /// Letting go: the file is saved or unsaved by the text it stands at, and a draft is written
    /// once the slider closes.
    fn release(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.dragging) {
            return;
        }
        if let Some(document) = self.document.upgrade() {
            document.update(cx, |document, cx| document.scrub_settled(cx));
        }
        cx.notify();
    }

    /// For the control socket: a drag to `position` and letting go.
    pub fn to(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
        self.dragging = true;
        self.go(position, window, cx);
        self.release(cx);
    }

    /// A key: `by` stops from where the text stands (negative goes back), stopping at the ends.
    fn step(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
        let position = self.position.saturating_add_signed(by).min(self.total);
        self.to(position, window, cx);
    }

    /// How far Option-Left and Option-Right move: a tenth of the history, at least one stop.
    fn far(&self) -> usize {
        (self.total / 10).max(1)
    }

    /// An edit was refused because it would drop the newer steps: asks whether to drop them and edit here.
    fn ask_to_drop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.asking, true) {
            return;
        }
        let Some(editor) = self.editor.upgrade() else { return };
        let waiting = editor.read(cx).history_waiting();
        let edits = if waiting == 1 { "1 later edit".to_string() } else { format!("{waiting} later edits") };
        let answer = window.prompt(
            PromptLevel::Warning,
            "Drop the later edits?",
            Some(&format!("Editing from here drops {edits} for good. Cancel keeps them, and the slider stays.")),
            &[PromptButton::cancel("Cancel"), PromptButton::destructive("Drop edits")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let drop = answer.await.ok() == Some(1);
            this.update_in(cx, |this, window, cx| {
                this.asking = false;
                if drop {
                    if let Some(editor) = this.editor.upgrade() {
                        editor.update(cx, |state, _| state.history_drop_redo());
                    }
                    this.shut(Close::Dropped, window, cx, true);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Ends the session. `tell_document` is false when the document itself is ending it (it is
    /// being updated now and does its own part).
    pub fn shut(&mut self, how: Close, window: &mut Window, cx: &mut Context<Self>, tell_document: bool) {
        self.dragging = false;
        if let Some(editor) = self.editor.upgrade() {
            self.refresh(cx);
            let origin = self.origin;
            editor.update(cx, |state, cx| {
                match how {
                    Close::Restore => state.history_jump(origin, window, cx),
                    Close::Keep => {
                        let (applied, waiting) = state.history_counts();
                        if waiting > 0 && applied != origin {
                            state.history_fold_redo();
                        }
                    }
                    Close::Dropped => {}
                }
                state.set_history_guard(None);
            });
        }
        if tell_document {
            if let Some(document) = self.document.upgrade() {
                document.update(cx, |document, cx| document.end_scrub(cx));
            }
        }
        let focused = self.focus.contains_focused(window, cx);
        if focused {
            if let Some(editor) = self.editor.upgrade() {
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
        }
        cx.emit(UndoSliderEvent::Closed);
    }

    /// `{"position":N,"total":N,"origin":N,"label":"..."}`, for the control socket.
    pub fn snapshot(&self) -> String {
        format!("{{\"position\":{},\"total\":{},\"origin\":{},\"label\":{:?}}}", self.position, self.total, self.origin, self.label(Instant::now()))
    }

    fn label(&self, now: Instant) -> String {
        let age = self.position.checked_sub(1).and_then(|step| self.times.get(step)).map(|at| now.saturating_duration_since(*at));
        stop_label(self.position, self.total, age)
    }
}

/// How long ago, in the words the slider uses.
pub fn ago(age: Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..5 => "just now".to_string(),
        5..60 => format!("{seconds} s ago"),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

/// The label of a stop: before any edit, the newest, or an edit by its age.
fn stop_label(position: usize, total: usize, age: Option<Duration>) -> String {
    match (position, age) {
        (0, _) => "Before the first edit".to_string(),
        (position, _) if position == total => "Latest edit".to_string(),
        (position, Some(age)) => format!("{} (edit {position} of {total})", ago(age)),
        (position, None) => format!("Edit {position} of {total}"),
    }
}

impl Render for UndoSlider {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let family = theme.font_family.clone();
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        let muted = theme::ink(ALPHA_SECONDARY, cx);
        let faint = theme::ink(ALPHA_TERTIARY, cx);
        let rim = ink.opacity(ALPHA_BORDER);
        let fraction = |stop: usize| if self.total == 0 { 0. } else { stop as f32 / self.total as f32 };
        let (at, origin) = (fraction(self.position), fraction(self.origin));
        let bounds = self.track.clone();
        let slider = cx.entity();
        let dragging = self.dragging;
        let label = self.label(Instant::now());
        let track = div()
            .id("undo-track")
            .relative()
            .flex_1()
            .h(px(TRACK))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, window, cx| this.press(event.position.x, window, cx)))
            .child(canvas(move |found, _, _| bounds.set(found), |_, (), _, _| {}).absolute().size_full())
            .child(div().absolute().left_0().right_0().top(px((TRACK - 2.) / 2.)).h(px(2.)).rounded(px(1.)).bg(faint))
            .child(div().absolute().left_0().top(px((TRACK - 2.) / 2.)).h(px(2.)).w(relative(at)).rounded(px(1.)).bg(muted))
            .child(div().absolute().left(relative(origin)).top(px((TRACK - 8.) / 2.)).w(px(1.)).h(px(8.)).bg(primary))
            .child(div().absolute().left(relative(at)).ml(px(-KNOB / 2.)).top(px((TRACK - KNOB) / 2.)).size(px(KNOB)).rounded_full().bg(primary).border_1().border_color(rim));
        // While the knob is held the window's mouse events drive it, so a drag that leaves the track still follows.
        let drag = canvas(
            |_, _, _| (),
            move |_, (), window, _| {
                if !dragging {
                    return;
                }
                let moved = slider.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        moved.update(cx, |this, cx| this.drag_to(event.position.x, window, cx));
                    }
                });
                let up = slider.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        up.update(cx, |this, cx| this.release(cx));
                    }
                });
            },
        )
        .absolute()
        .size_0();
        tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("undo-slider")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SliderBack, window, cx| this.step(-1, window, cx)))
            .on_action(cx.listener(|this, _: &SliderForward, window, cx| this.step(1, window, cx)))
            .on_action(cx.listener(|this, _: &SliderBackFar, window, cx| this.step(-(this.far() as isize), window, cx)))
            .on_action(cx.listener(|this, _: &SliderForwardFar, window, cx| this.step(this.far() as isize, window, cx)))
            .on_action(cx.listener(|this, _: &SliderStart, window, cx| this.to(0, window, cx)))
            .on_action(cx.listener(|this, _: &SliderEnd, window, cx| this.to(this.total, window, cx)))
            .on_action(cx.listener(|this, _: &SliderKeep, window, cx| this.shut(Close::Keep, window, cx, true)))
            .on_action(cx.listener(|this, _: &SliderCancel, window, cx| this.shut(Close::Restore, window, cx, true)))
            .occlude()
            .w_full()
            .h(HEIGHT)
            .px(FIND_PAD) // design: `.px` is the horizontal padding method, the value is a token
            .flex()
            .items_center()
            .gap(px(10.))
            .font_family(family)
            .text_size(TEXT_DETAIL)
            .line_height(TEXT_DETAIL_LINE)
            .text_color(muted)
            .child(drag)
            .child(track)
            .child(div().flex_none().min_w(px(150.)).text_right().whitespace_nowrap().text_color(primary).child(label))
            .child(
                div()
                    .id("undo-done")
                    .flex_none()
                    .cursor_pointer()
                    .text_color(primary)
                    .child("Done")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.shut(Close::Keep, window, cx, true))),
            )
    }
}
