//! File > Rename: a small field floating over the top middle of the active editor, filled with the
//! open file's name, the stem selected and the extension not. Return hands the typed name to the
//! workspace (`RenameEvent::Submit`), which renames the file and either closes the field or says
//! why it refused (`refuse`: the explorer's refusal words and warning sign under the field, the field
//! tinted toward the danger colour until the name is edited). The tint eases over `FADE_QUICK` both
//! ways and the reason fades on the overlay timings, as the tree's refused-name card does; a change
//! mid-fade turns around. Escape closes; a click elsewhere closes it without taking the keyboard back.
//!
//! It stands on the same float and field capsule as Go to line (`tooltip::float`, `float_field`,
//! the shared `Reveal`) and rises into place over `OVERLAY_ENTER`, then leaves over `OVERLAY_EXIT`.
//! The workspace places it (it knows the active pane's bounds) and draws it over everything but the tooltips.

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Escape, InputEvent, InputState},
};
use gpui_kit::*;

use crate::{
    float_field::{Reveal, float_field},
    shake::Shake,
    motion::Fade,
    symbols, theme, tooltip,
    tokens::{
        ALPHA_PRIMARY, ERROR_SYMBOL, FADE_QUICK, FIND_PAD, OVERLAY_ENTER, OVERLAY_ENTER_RISE, OVERLAY_EXIT, RADIUS_FLOAT_CAPSULE, ROW_GAP, TEXT_SUBHEADLINE,
        TEXT_SUBHEADLINE_LINE,
    },
};

/// The float's width: room for a long file name beside the find bar's side insets.
pub const RENAME_WIDTH: Pixels = px(300.);

/// What the field tells the window.
pub enum RenameEvent {
    /// Return: rename the file to this name.
    Submit(String),
    /// The field closed (Return that renamed, or Escape): the keyboard goes back to the editor.
    Done,
}

pub struct RenameField {
    input: Entity<InputState>,
    reveal: Reveal,
    /// Why the last Return was refused, shown under the field until the name changes.
    error: Option<SharedString>,
    /// The reason last shown, kept so it can fade out saying it.
    shown: SharedString,
    /// The danger tint on the field, 0 to 1, over `FADE_QUICK`.
    tint: Fade,
    /// The reason's opacity, over `OVERLAY_ENTER` going in and `OVERLAY_EXIT` going out, and where it is headed.
    reason: Fade,
    reason_to: f32,
    /// The sideways shake a refused name gives the field, beside the beep; `shake_asked` is a refusal
    /// not yet drawn (it carries no window to read the frame time from), which render starts.
    shake: Option<Shake>,
    shake_asked: bool,
    _subscription: Subscription,
}

impl EventEmitter<RenameEvent> for RenameField {}

impl RenameField {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        crate::control::name_field("rename field", input.focus_handle(cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => {
                let name = this.input.read(cx).value().to_string();
                cx.emit(RenameEvent::Submit(name));
            }
            InputEvent::Change => {
                if this.error.take().is_some() {
                    cx.notify();
                }
            }
            // A click elsewhere ends it, but never the window losing the key (the field is still the focus then).
            InputEvent::Blur if this.reveal.open && crate::control::window_key(window) => {
                this.shut(window, cx);
            }
            _ => {}
        });
        // Created at the first File > Rename, so it starts shut and the first `open` turns it toward 1 from 0.
        let reveal = Reveal::shut(crate::motion::drawn_now(window.window_handle().window_id()));
        Self {
            input,
            reveal,
            error: None,
            shown: SharedString::default(),
            tint: Fade::with(0., FADE_QUICK),
            reason: Fade::with(0., OVERLAY_ENTER),
            reason_to: 0.,
            shake: None,
            shake_asked: false,
            _subscription: subscription,
        }
    }

    /// Shows the field filled with `name`, its stem selected (a name with no extension, or only a
    /// leading dot, selects all of it), with the keyboard in it.
    pub fn open(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        self.reveal.turn(true, now);
        self.error = None;
        self.tint = Fade::with(0., FADE_QUICK);
        self.reason = Fade::with(0., OVERLAY_ENTER);
        self.reason_to = 0.;
        let stem = name.rfind('.').filter(|&dot| dot > 0).unwrap_or(name.len());
        let name = name.to_string();
        self.input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
            input.set_selected_range(0..stem, cx);
        });
        cx.notify();
    }

    /// Leaves the field and gives the keyboard back to the editor.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shut(window, cx) {
            cx.emit(RenameEvent::Done);
        }
    }

    /// Turns the reveal shut and says whether it was open. The keyboard stays where it went: a blur
    /// closes the field because a click or a key put it elsewhere, so it never takes it back.
    fn shut(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.reveal.open {
            return false;
        }
        self.reveal.turn(false, crate::motion::drawn_now(window.window_handle().window_id()));
        cx.notify();
        true
    }

    /// A name that was refused: the words show under the field and the field stays open.
    pub fn refuse(&mut self, why: String, cx: &mut Context<Self>) {
        crate::preview::render::beep();
        self.shown = why.clone().into();
        self.error = Some(why.into());
        self.shake_asked = true;
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.reveal.open
    }
}

impl Render for RenameField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (level, moving) = self.reveal.level(now);
        if moving {
            window.request_animation_frame();
        }
        if !self.reveal.open && level <= 0. {
            return div().into_any_element();
        }
        // The tint and the reason follow the refusal: the tint eases over `FADE_QUICK` both ways, the
        // reason fades on the overlay timings, and either turns around from where it is drawn.
        let to = if self.error.is_some() { 1. } else { 0. };
        self.tint.aim(to, now);
        if self.reason_to != to {
            self.reason_to = to;
            self.reason = Fade::with(self.reason.level(now), if to > 0. { OVERLAY_ENTER } else { OVERLAY_EXIT });
            self.reason.aim(to, now);
        }
        if self.tint.is_moving(now) || self.reason.is_moving(now) {
            window.request_animation_frame();
        }
        let reason_level = self.reason.level(now);
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let danger = theme.danger;
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let family = theme.font_family.clone();
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        let field = float_field(&self.input, Some("pencil"), self.tint.level(now), cx).w_full();
        // The reason keeps its words while it fades out, so the float shrinks only once it is gone.
        let reason = (reason_level > 0. || self.error.is_some()).then(|| {
            div()
                .px(FIND_PAD)
                .pt(FIND_PAD)
                .opacity(reason_level)
                .flex()
                .items_start()
                .gap(ROW_GAP / 2.)
                .text_size(TEXT_SUBHEADLINE)
                .line_height(TEXT_SUBHEADLINE_LINE)
                .text_color(danger)
                .child(div().flex_none().h(TEXT_SUBHEADLINE_LINE).flex().items_center().child(symbols::icon("exclamationmark.triangle.fill", ERROR_SYMBOL)))
                .child(div().flex_1().min_w_0().child(self.shown.clone()))
        });

        let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("rename-field")
            .occlude()
            .w_full()
            .p(FIND_PAD)
            .font_family(family)
            .text_color(primary)
            .child(field)
            .children(reason)
            // The field answers Escape itself, ahead of the window's editor.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.close(window, cx);
                cx.stop_propagation();
            }));
        // The shake moves the whole float sideways and asks for frames only while it runs.
        if std::mem::take(&mut self.shake_asked) {
            self.shake = Some(Shake::new(now));
        }
        let swing = self.shake.and_then(|shake| shake.offset(now));
        if swing.is_some() {
            window.request_animation_frame();
        } else {
            self.shake = None;
        }
        let place = div().relative().left(swing.unwrap_or_default()).mt(px(OVERLAY_ENTER_RISE * (1. - level))).opacity(level).child(body);
        if self.reveal.open {
            place.into_any_element()
        } else {
            // A leaving field takes no clicks and no focus: a cover over it eats them until it is gone.
            place.child(div().id("rename-field-cover").absolute().inset_0().occlude()).into_any_element()
        }
    }
}
