//! The count float of Find next and Find previous (Cmd-G, Shift-Cmd-G) with the find bar closed: a
//! small float in the editor's top right corner saying "3 of 12", and, when the step came round past
//! the end (or the start) of the file, an arrow and "Wrapped". It stands on Go to line's float
//! (`tooltip::float`) and the shared `Reveal`: it rises in over `OVERLAY_ENTER`, stays `FIND_FLOAT_HOLD`
//! after the last step and leaves over `OVERLAY_EXIT`. A step while it shows updates it where it is
//! and starts the hold over, not the fade-in; a step during the fade-out turns it around from the
//! level drawn. Opening the find bar hides it at once. It asks for frames only while it moves, and
//! the hold is one timer, so nothing runs while it is gone. The workspace places it (it knows the
//! active pane's bounds) and draws it over the editor.

use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, input::EditorState};
use gpui_kit::*;

use crate::{
    float_field::Reveal,
    symbols, theme, tooltip,
    tokens::{
        ALPHA_PRIMARY, ALPHA_SECONDARY, FIELD_SYMBOL_GAP, FILTER_PAD_X, FIND_FIELD_HEIGHT, FIND_FLOAT_HOLD, FIND_PAD, FIND_SYMBOL, OVERLAY_ENTER_RISE,
        RADIUS_FLOAT_CAPSULE, TEXT_BODY, TEXT_BODY_LINE,
    },
};

/// Where a step stands: the match it landed on, counted from 1, of how many, and which way it came
/// round when it wrapped (`Some(true)` forward past the last match, `Some(false)` backward past the first).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Step {
    pub current: usize,
    pub total: usize,
    pub wrapped: Option<bool>,
}

pub struct FindFloat {
    reveal: Reveal,
    /// The step last shown, kept so the float fades out saying it.
    step: Option<Step>,
    /// A step landed and no frame has painted it yet: the next one turns the fade open.
    armed: bool,
    /// The hold before the fade-out; replacing it cancels the one before.
    hold: Option<Task<()>>,
    /// Watches the editor while the float shows, to hide it the moment the find bar opens.
    watch: Option<Subscription>,
}

impl FindFloat {
    pub fn new(window: &mut Window) -> Self {
        Self { reveal: Reveal::shut(crate::motion::drawn_now(window.window_handle().window_id())), step: None, armed: false, hold: None, watch: None }
    }

    /// A step of `editor`'s search with the bar closed landed as `step`: show it (or update it in
    /// place) and hold it `FIND_FLOAT_HOLD` from now. The fade starts at the first frame that
    /// paints it, not at the key, so a slow first frame doesn't skip part of it.
    pub fn show(&mut self, editor: &Entity<EditorState>, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.armed = !self.reveal.open;
        self.step = Some(step);
        self.watch = Some(cx.observe_in(editor, window, |this, editor, window, cx| {
            if (this.reveal.open || this.armed) && editor.read(cx).search_session().open {
                this.hide_now(window, cx);
            }
        }));
        self.hold = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(FIND_FLOAT_HOLD)).await;
            this.update_in(cx, |this, window, cx| {
                this.armed = false;
                this.reveal.turn(false, crate::motion::drawn_now(window.window_handle().window_id()));
                this.watch = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Gone this frame, with no fade.
    fn hide_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal = Reveal::shut(crate::motion::drawn_now(window.window_handle().window_id()));
        self.armed = false;
        self.hold = None;
        self.watch = None;
        cx.notify();
    }
}

impl Render for FindFloat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        if std::mem::take(&mut self.armed) {
            // The first frame that paints this step starts the fade, from the level drawn (0 when it was gone).
            self.reveal.turn(true, now);
        }
        let (level, moving) = self.reveal.level(now);
        if moving {
            window.request_animation_frame();
        }
        let Some(step) = self.step.filter(|_| self.reveal.open || level > 0.) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, theme.sidebar_foreground, light);
        let family = theme.font_family.clone();
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        let muted = theme::ink(ALPHA_SECONDARY, cx);
        let wrap = step.wrapped.map(|forward| {
            let name = if forward { "arrow.clockwise" } else { "arrow.counterclockwise" };
            div()
                .flex()
                .items_center()
                .gap(FIELD_SYMBOL_GAP)
                .text_color(muted)
                .child(symbols::icon(name, FIND_SYMBOL).flex_none())
                .child("Wrapped")
        });
        let content = div()
            .h(FIND_FIELD_HEIGHT)
            .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
            .flex()
            .items_center()
            .gap(FILTER_PAD_X)
            .text_size(TEXT_BODY)
            .line_height(TEXT_BODY_LINE)
            .child(format!("{} of {}", step.current, step.total))
            .children(wrap);
        let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false).p(FIND_PAD).font_family(family).text_color(primary).child(content);
        div().mt(px(OVERLAY_ENTER_RISE * (1. - level))).opacity(level).child(body).into_any_element()
    }
}
