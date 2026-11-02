//! The writing goal (finch#1ayn): a word count to reach in one file. `goal: 500` in the file's front
//! matter sets it (`word_count::front_matter_goal`); Format > Writing goal… sets one from a small field
//! for the open document, for the session only: it is kept here by the document's entity id, never in
//! `settings.toml` and never written into the file. A goal set here wins over the front matter's.
//!
//! The status line draws the goal's hairline and hosts the field, a small float above its left end
//! (the same capsule Go to line uses) that takes a number and Return, or an empty field to clear the goal.

use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Escape, InputEvent, InputState},
};
use gpui_kit::*;

use crate::{
    document::Document,
    float_field::{Reveal, float_field},
    shake::Shake,
    theme, tooltip,
    tokens::{ALPHA_PRIMARY, FIND_PAD, OVERLAY_ENTER_RISE, RADIUS_FLOAT_CAPSULE},
};

/// The goals set from the menu, by the document's entity id. A closed document's entries go when it is released.
#[derive(Default)]
struct Goals {
    goals: HashMap<EntityId, usize>,
    /// One release hook per document seen, kept until that document is released (so a goal set, cleared and set again never adds a second).
    hooks: HashMap<EntityId, Subscription>,
}

impl Goals {
    fn release(&mut self, id: EntityId) {
        self.goals.remove(&id);
        self.hooks.remove(&id);
    }
}

impl Global for Goals {}

/// The goal set for `document` from the menu this session, if any.
pub fn session(document: &Entity<Document>, cx: &App) -> Option<usize> {
    cx.try_global::<Goals>()?.goals.get(&document.entity_id()).copied()
}

/// Sets (or, with `None`, clears) the session goal of `document`.
pub fn set(document: &Entity<Document>, goal: Option<usize>, cx: &mut App) {
    let id = document.entity_id();
    match goal {
        Some(goal) => {
            if !cx.default_global::<Goals>().hooks.contains_key(&id) {
                let hook = cx.observe_release(document, move |_, cx| cx.default_global::<Goals>().release(id));
                cx.default_global::<Goals>().hooks.insert(id, hook);
            }
            cx.default_global::<Goals>().goals.insert(id, goal);
        }
        None => {
            cx.default_global::<Goals>().goals.remove(&id);
        }
    }
}

/// What a typed goal reads as: a whole number from 1 (thousands commas and spaces allowed), `Some(None)` for an empty field (clear the goal), `None` for anything else.
pub fn parse(text: &str) -> Option<Option<usize>> {
    let digits: String = text.chars().filter(|ch| !matches!(ch, ',' | ' ')).collect();
    if digits.is_empty() {
        return Some(None);
    }
    digits.parse::<usize>().ok().filter(|goal| (1..=crate::word_count::MAX_GOAL).contains(goal)).map(Some)
}

/// What the field tells the status line.
pub enum GoalEvent {
    /// Return on a goal that reads (`None`: an empty field, so no goal).
    Set(Option<usize>),
    /// The field closed: the keyboard goes back to the editor.
    Done,
}

pub struct GoalField {
    input: Entity<InputState>,
    reveal: Reveal,
    /// The sideways shake a goal that doesn't read gives the field, beside the beep.
    shake: Option<Shake>,
    _subscription: Subscription,
}

impl EventEmitter<GoalEvent> for GoalField {}

impl GoalField {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Words to write, like 500"));
        crate::control::name_field("writing goal", input.focus_handle(cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.commit(window, cx),
            // A click elsewhere ends it, but never the window losing the key (the field is still the focus then).
            InputEvent::Blur if this.reveal.open && crate::control::window_key(window) => {
                this.shut(window, cx);
            }
            _ => {}
        });
        let reveal = Reveal::shut(crate::motion::drawn_now(window.window_handle().window_id()));
        Self { input, reveal, shake: None, _subscription: subscription }
    }

    /// Shows the field with the keyboard in it, holding `current` (the goal now in force) selected.
    pub fn open(&mut self, current: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        self.reveal.turn(true, now);
        let text = current.map(|goal| goal.to_string()).unwrap_or_default();
        self.input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// Leaves the field and gives the keyboard back to the editor.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shut(window, cx) {
            cx.emit(GoalEvent::Done);
        }
    }

    /// Turns the reveal shut and says whether it was open. The keyboard stays where it went.
    fn shut(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.reveal.open {
            return false;
        }
        self.reveal.turn(false, crate::motion::drawn_now(window.window_handle().window_id()));
        cx.notify();
        true
    }

    /// Return: a goal that reads is set and the field closes; anything else beeps, shakes and stays.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        match parse(&text) {
            Some(goal) => {
                cx.emit(GoalEvent::Set(goal));
                self.close(window, cx);
            }
            None => {
                crate::preview::render::beep();
                self.shake = Some(Shake::new(crate::motion::drawn_now(window.window_handle().window_id())));
                cx.notify();
            }
        }
    }
}

impl Render for GoalField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (level, moving) = self.reveal.level(now);
        if moving {
            window.request_animation_frame();
        }
        if !self.reveal.open && level <= 0. {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let family = theme.font_family.clone();
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        let field = float_field(&self.input, None, 0., cx).w_full();
        let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("writing-goal")
            .occlude()
            .w_full()
            .p(FIND_PAD)
            .font_family(family)
            .text_color(primary)
            .child(field)
            // The field answers Escape itself, ahead of the window's editor.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.close(window, cx);
                cx.stop_propagation();
            }));
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
            place.child(div().id("writing-goal-cover").absolute().inset_0().occlude()).into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::EntityId;

    use super::{Goals, parse};

    #[test]
    fn a_released_document_leaves_no_goal_behind() {
        let mut goals = Goals::default();
        for n in 1..=20u64 {
            goals.goals.insert(EntityId::from(n), 500 + n as usize);
        }
        for n in 1..=10u64 {
            goals.release(EntityId::from(n));
        }
        assert_eq!(goals.goals.len(), 10);
        assert_eq!(goals.goals.get(&EntityId::from(11)), Some(&511), "a document still open keeps its goal");
        assert!(!goals.goals.contains_key(&EntityId::from(10)));
        for n in 11..=20u64 {
            goals.release(EntityId::from(n));
        }
        assert!(goals.goals.is_empty() && goals.hooks.is_empty());
    }

    #[test]
    fn reads_a_whole_number_of_words() {
        assert_eq!(parse("500"), Some(Some(500)));
        assert_eq!(parse(" 1,500 "), Some(Some(1500)));
        assert_eq!(parse("2 000"), Some(Some(2000)));
    }

    #[test]
    fn an_empty_field_clears_the_goal() {
        assert_eq!(parse(""), Some(None));
        assert_eq!(parse("  "), Some(None));
    }

    #[test]
    fn refuses_what_is_not_a_goal() {
        for text in ["0", "-5", "5.5", "five", "10x", "99999999999999999999", "10000001"] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }
}
