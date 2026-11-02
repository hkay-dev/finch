//! Go to line (Cmd-L): a small field floating over the top middle of the active editor. It takes a
//! line (`120`) or a line and column (`120:8`), both counted from 1; Return moves the caret there
//! through `Document::go_to` (the workspace does that on `GoToLineEvent::Go`), Escape closes. A
//! number that doesn't read as a place beeps and eases a tint toward the danger colour and back over
//! `FADE_QUICK`. A line past the end of the file does the same and says "Only 400 lines" under the
//! field, fading in on the overlay timings until the text changes; Return again on it lands on the last
//! line (without the found wash). A click elsewhere closes it without taking the keyboard back.
//!
//! It stands on the find bar's float (`tooltip::float`: the float fill and rim, a lit top in the dark,
//! contact and ambient shadow) and rises into place over `OVERLAY_ENTER` on the expo-out curve, then leaves over
//! `OVERLAY_EXIT` on the ease-in; closing and reopening mid-flight turns around from where it is drawn.
//! Out of key the field softens to its rest fill, as the find bar's does. The workspace places it
//! (it knows the active pane's bounds) and draws it over everything but the tooltips.

use std::time::Instant;

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Escape, InputEvent, InputState},
};
use gpui_kit::*;

use crate::{
    float_field::{Reveal, float_field},
    motion::Fade,
    start::Caret,
    shake::Shake, symbols, theme, tooltip,
    tokens::{
        ALPHA_PRIMARY, ERROR_SYMBOL, FADE_QUICK, FIND_PAD, OVERLAY_ENTER, OVERLAY_ENTER_RISE, OVERLAY_EXIT, RADIUS_FLOAT_CAPSULE, ROW_GAP, TEXT_SUBHEADLINE,
        TEXT_SUBHEADLINE_LINE,
    },
};

/// What the field tells the window.
pub enum GoToLineEvent {
    /// Return on a place to go: put the caret there. `found` is false when the line was past the end
    /// and the caret lands on the last line (no found wash).
    Go { caret: Caret, found: bool },
    /// The field closed (Return or Escape): the keyboard goes back to the editor.
    Done,
}

/// The tint a place that doesn't read lays on the field: toward the danger colour over `FADE_QUICK`
/// and back over `FADE_QUICK`, on the one ease-out. A second bad Return while it runs turns it
/// around: it rises again from the level drawn. It asks for frames only while it runs.
struct Bad {
    from: f32,
    since: Option<Instant>,
}

impl Bad {
    fn level(&self, now: Instant) -> Option<f32> {
        let secs = now.saturating_duration_since(self.since?).as_secs_f32();
        if secs < FADE_QUICK {
            Some(self.from + (1. - self.from) * crate::motion::ease_out(secs / FADE_QUICK))
        } else if secs < 2. * FADE_QUICK {
            Some(1. - crate::motion::ease_out((secs - FADE_QUICK) / FADE_QUICK))
        } else {
            None
        }
    }
}

pub struct GoToLine {
    input: Entity<InputState>,
    reveal: Reveal,
    bad: Bad,
    /// The sideways shake a place that doesn't read gives the field, beside the beep.
    shake: Option<Shake>,
    /// How many lines the open file has (the workspace sets it at Cmd-L); 0 knows none.
    lines: u32,
    /// The text a line past the end was refused on: Return again on it goes to the last line.
    warned: Option<String>,
    /// The refusal's words under the field, shown until the text changes, and the words last shown so
    /// they can fade out saying them.
    error: Option<SharedString>,
    shown: SharedString,
    /// The words' opacity, over `OVERLAY_ENTER` going in and `OVERLAY_EXIT` going out, and where it is headed.
    reason: Fade,
    reason_to: f32,
    _subscription: Subscription,
}

impl EventEmitter<GoToLineEvent> for GoToLine {}

impl GoToLine {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Line or line:column"));
        crate::control::name_field("go to line", input.focus_handle(cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => {
                this.commit(window, cx).ok();
            }
            // Editing the text takes the words away, and the refused line with them.
            InputEvent::Change => {
                if this.error.is_some() && this.warned.as_deref() != Some(this.input.read(cx).value().trim()) {
                    this.error = None;
                    this.warned = None;
                    cx.notify();
                }
            }
            // A click elsewhere ends it, but never the window losing the key (the field is still the focus then).
            InputEvent::Blur if this.reveal.open && crate::control::window_key(window) => {
                this.shut(window, cx);
            }
            _ => {}
        });
        // Created at the first Cmd-L, so it starts shut and the first `open` turns it toward 1 from 0.
        let reveal = Reveal::shut(crate::motion::drawn_now(window.window_handle().window_id()));
        Self {
            input,
            reveal,
            bad: Bad { from: 0., since: None },
            shake: None,
            lines: 0,
            warned: None,
            error: None,
            shown: SharedString::default(),
            reason: Fade::with(0., OVERLAY_ENTER),
            reason_to: 0.,
            _subscription: subscription,
        }
    }

    /// Shows the field, empty, with the keyboard in it. Shown already, it selects what is typed.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        if self.reveal.open {
            self.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
            return;
        }
        self.reveal.turn(true, now);
        self.error = None;
        self.warned = None;
        self.reason = Fade::with(0., OVERLAY_ENTER);
        self.reason_to = 0.;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Leaves the field and gives the keyboard back to the editor.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shut(window, cx) {
            cx.emit(GoToLineEvent::Done);
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

    pub fn is_open(&self) -> bool {
        self.reveal.open
    }

    /// Return: a place that reads moves the caret and closes; anything else beeps and stays. A line
    /// past the end of the file is refused once, saying how many lines there are (the same tint and
    /// shake); Return again on the same text goes to the last line, without the found wash.
    /// Err holds the words of the refusal (the control socket replies with them).
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let text = self.input.read(cx).value();
        let Some(caret) = parse(&text) else {
            self.refuse(None, window, cx);
            return Err("not a line or line:column".into());
        };
        let past = self.lines > 0 && caret.line > self.lines;
        if past && self.warned.as_deref() != Some(text.trim()) {
            self.warned = Some(text.trim().to_string());
            let why = only_lines(self.lines);
            self.refuse(Some(why.clone()), window, cx);
            return Err(why);
        }
        cx.emit(GoToLineEvent::Go { caret, found: !past });
        self.close(window, cx);
        Ok(())
    }

    /// A place that can't be taken: the beep, the tint and the shake, and `why` under the field when there is one.
    fn refuse(&mut self, why: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        crate::preview::render::beep();
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        self.bad = Bad { from: self.bad.level(now).unwrap_or(0.), since: Some(now) };
        self.shake = Some(Shake::new(now));
        if let Some(why) = why {
            self.shown = why.clone().into();
            self.error = Some(why.into());
        }
        cx.notify();
    }

    /// How many lines the file has, for the refusal's words; 0 knows none (nothing is refused).
    pub fn set_lines(&mut self, lines: u32) {
        self.lines = lines;
    }

    /// For the control socket: types `text` into the open field and presses Return, as keys would.
    /// Err holds why the place was refused.
    pub fn submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        self.input.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
        self.commit(window, cx)
    }
}

/// `Only 400 lines`, and what Return does next.
fn only_lines(lines: u32) -> String {
    let count = if lines == 1 { "Only 1 line".to_string() } else { format!("Only {lines} lines") };
    format!("{count}. Return again goes to the end.")
}

/// `120` or `120:8` (spaces around either number allowed), from 1; `None` for anything else.
fn parse(text: &str) -> Option<Caret> {
    let (line, column) = match text.split_once(':') {
        Some((line, column)) => (line, Some(column)),
        None => (text, None),
    };
    let number = |text: &str| text.trim().parse::<u32>().ok().filter(|&n| n >= 1);
    Some(Caret { line: number(line)?, column: column.map_or(Some(1), number)? })
}

impl Render for GoToLine {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (level, moving) = self.reveal.level(now);
        if moving {
            window.request_animation_frame();
        }
        if !self.reveal.open && level <= 0. {
            // Shut and gone: `rects` has no Go to line rect.
            if crate::control::rects_enabled() {
                crate::control::unrecord(window.window_handle().window_id(), "go-to-line");
            }
            return div().into_any_element();
        }
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let family = theme.font_family.clone();
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        // A place that doesn't read tints the field toward the danger colour once and eases back.
        let tint = self.bad.level(now);
        if tint.is_some() {
            window.request_animation_frame();
        } else {
            self.bad.since = None;
        }
        let field = float_field(&self.input, Some("arrow.right.to.line"), tint.unwrap_or(0.), cx).w_full();
        // A line past the end says how many there are under the field, on the overlay timings, until the text changes.
        let to = if self.error.is_some() { 1. } else { 0. };
        if self.reason_to != to {
            self.reason_to = to;
            self.reason = Fade::with(self.reason.level(now), if to > 0. { OVERLAY_ENTER } else { OVERLAY_EXIT });
            self.reason.aim(to, now);
        }
        if self.reason.is_moving(now) {
            window.request_animation_frame();
        }
        let reason_level = self.reason.level(now);
        let danger = theme.danger;
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
            .id("go-to-line")
            .occlude()
            .w_full()
            .p(FIND_PAD)
            .font_family(family)
            .text_color(primary)
            .child(field)
            .children(reason)
            // The drawn rect, rise included, for `debug rects`.
            .children(crate::control::track("go-to-line"))
            // The field answers Escape itself, ahead of the window's editor.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.close(window, cx);
                cx.stop_propagation();
            }));
        // The shake moves the whole float sideways and asks for frames only while it runs.
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
            place.child(div().id("go-to-line-cover").absolute().inset_0().occlude()).into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    // Named, not `super::*`: that glob carries GPUI's own `test` attribute over the standard one.
    use super::{Caret, parse};

    #[test]
    fn reads_a_line_or_a_line_and_column() {
        assert_eq!(parse("120"), Some(Caret { line: 120, column: 1 }));
        assert_eq!(parse(" 120 : 8 "), Some(Caret { line: 120, column: 8 }));
        assert_eq!(parse("1:1"), Some(Caret { line: 1, column: 1 }));
    }

    #[test]
    fn refuses_what_is_not_a_place() {
        for text in ["", "0", "3:0", "-4", "a", "3:", ":4", "3:4:5", "1.5"] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }
}
