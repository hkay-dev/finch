use ropey::Rope;
use std::ops::Range;

use gpui::{
    App, AppContext as _, Context, Empty, Entity, FocusHandle, Focusable, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled, Subscription,
    WeakEntity, Window, actions,
    div,
};
use std::{rc::Rc, sync::Arc};

use crate::input::{
    Enter, Escape, FindBar, FindBarRenderer, FindControl, FindReplace, IndentInline, InputBaseState,
    InputEvent, InputState, MoveDown, MoveUp, OutdentInline, Replace, Search,
};

const CONTEXT: &'static str = "SearchPanel";

actions!(input, [Tab]);

#[cfg(test)]
use gpui_base::input::SearchMatcher;

#[derive(Clone, Copy)]
#[cfg(test)]
enum MoveDirection {
    Up,
    Down,
}

#[cfg(test)]
fn next_scroll_direction(
    previous_match_ix: usize,
    current_match_ix: usize,
) -> Option<MoveDirection> {
    if current_match_ix <= previous_match_ix {
        None
    } else {
        Some(MoveDirection::Down)
    }
}

#[cfg(test)]
fn prev_scroll_direction(
    previous_match_ix: usize,
    current_match_ix: usize,
) -> Option<MoveDirection> {
    if current_match_ix >= previous_match_ix {
        None
    } else {
        Some(MoveDirection::Up)
    }
}

/// How many past entries Up in the find or the replace field can reach.
const HISTORY_LIMIT: usize = 20;

/// The queries searched and the replacements made since the app started, newest first, none
/// twice.
#[derive(Default)]
struct FindHistory {
    queries: Vec<String>,
    replacements: Vec<String>,
}

impl Global for FindHistory {}

/// The longest entry history keeps, in characters. Pasted text past this (a whole file in one
/// line) is a search, not a phrase to recall, and the list would hold it until the app quits.
const HISTORY_QUERY_MAX: usize = 512;

/// The two lists, one per field.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Find,
    Replace,
}

impl FindHistory {
    fn list(&self, field: Field) -> &Vec<String> {
        match field {
            Field::Find => &self.queries,
            Field::Replace => &self.replacements,
        }
    }

    fn list_mut(&mut self, field: Field) -> &mut Vec<String> {
        match field {
            Field::Find => &mut self.queries,
            Field::Replace => &mut self.replacements,
        }
    }
}

/// Where Up and Down stand in one field's list.
#[derive(Default)]
struct Recall {
    /// Which past entry Up brought into the field (0 is the newest), `None` while the field
    /// shows what was typed.
    at: Option<usize>,
    /// What was typed, for Down to give back.
    typed: String,
}

/// A query was searched (Return, Cmd-G) or the bar closed on it: it becomes the newest.
pub(super) fn remember_query(query: &str, cx: &mut App) {
    remember(Field::Find, query, cx);
}

/// The searches Up in the find field reaches, newest first: what the magnifier's Recent Searches lists.
pub(super) fn recent_queries(cx: &App) -> Vec<String> {
    cx.try_global::<FindHistory>().map_or_else(Vec::new, |history| history.queries.clone())
}

/// A replacement was made (Replace, Replace and find, Replace all): it becomes the newest.
fn remember_replacement(replacement: &str, cx: &mut App) {
    remember(Field::Replace, replacement, cx);
}

fn remember(field: Field, text: &str, cx: &mut App) {
    // A one-line field can't bring a multi-line entry back whole. `len` is bytes, at least the
    // characters, so only text near the limit pays for counting them.
    if text.is_empty()
        || text.len() > HISTORY_QUERY_MAX * 4
        || text.contains(['\n', '\r'])
        || text.chars().count() > HISTORY_QUERY_MAX
    {
        return;
    }
    if cx.try_global::<FindHistory>().is_some_and(|history| history.list(field).first().is_some_and(|newest| newest == text)) {
        return;
    }
    let list = cx.default_global::<FindHistory>().list_mut(field);
    list.retain(|past| past != text);
    list.insert(0, text.to_string());
    list.truncate(HISTORY_LIMIT);
}

/// What the count label says after Replace all, until the query or the text changes.
enum Replaced {
    /// The replacement is made and its rescan has not been drawn yet: `before` is the scan it replaced.
    Pending { count: usize, before: Arc<Vec<Range<usize>>> },
    /// `scan` is the first scan of the replaced text; a new scan ends the label.
    Shown { count: usize, scan: Arc<Vec<Range<usize>>> },
}

pub(super) struct SearchPanel<M: crate::input::overlay::OverlayMode> {
    editor: WeakEntity<InputBaseState<M>>,
    search_input: Entity<InputState>,
    replace_input: Entity<InputState>,
    session: gpui_base::input::SearchSession,
    /// The user clicked the case toggle: the query's own letters no longer
    /// pick the case mode (smartcase).
    case_touched: bool,
    /// The editor's Regular expression option changed from outside while the bar was open: the
    /// panel re-reads the query for smartcase on its next draw.
    recase: bool,
    /// The bar was opened and not yet closed.
    shown: bool,
    /// The replace row was showing when the bar last opened or was toggled.
    replace_row: bool,
    /// The bar was closed and is still drawn for its exit.
    exiting: bool,
    /// Counts openings, so each is a new element for the bar's entrance.
    epoch: usize,
    /// What the last Replace all replaced, while its label shows.
    replaced: Option<Replaced>,
    /// How many times the current match wrapped past an end of the matches, and which end: the
    /// bar pulses the chevron it wrapped from.
    wraps: u32,
    wrap_forward: bool,
    /// The direction of the last step the panel took, until the next sync reads it.
    step: Option<bool>,
    /// How many times Find next or previous found no match: the bar pulses its count.
    misses: u32,
    /// Up and Down in each field.
    find_recall: Recall,
    replace_recall: Recall,
    /// How many times Replace all found nothing to replace; the bar pulses its count for each.
    replace_misses: u32,

    _subscriptions: Vec<Subscription>,
}

impl<M: crate::input::overlay::OverlayMode> SearchPanel<M> {
    pub(super) fn replace_mode(&self) -> bool {
        self.session.replace_mode
    }

    /// The bar is closed and still drawn for its exit.
    pub(super) fn is_exiting(&self) -> bool {
        self.exiting
    }

    pub(super) fn sync_session(&mut self, session: &gpui_base::input::SearchSession) {
        self.misses = session.misses;
        let new_scan = session.matcher.matched_ranges();
        // A wrap is the current match crossing from one end to the other inside one scan: a new
        // query or edit rescans, so typing and replacing never count.
        let total = session.matcher.len();
        // With two matches either end is both: only a step the panel took itself knows its way.
        let step = self.step.take();
        if total >= 2
            && Arc::ptr_eq(&self.session.matcher.matched_ranges(), &new_scan)
            && self.session.query == session.query
        {
            let known = |direction: bool| total > 2 || step == Some(direction);
            match (self.session.matcher.current(), session.matcher.current()) {
                (Some(from), Some(0)) if from == total - 1 && known(true) => {
                    self.wraps = self.wraps.wrapping_add(1);
                    self.wrap_forward = true;
                }
                (Some(0), Some(to)) if to == total - 1 && known(false) => {
                    self.wraps = self.wraps.wrapping_add(1);
                    self.wrap_forward = false;
                }
                _ => {}
            }
        }
        self.replaced = match self.replaced.take() {
            Some(Replaced::Pending { count, before }) if !Arc::ptr_eq(&before, &new_scan) => {
                Some(Replaced::Shown { count, scan: new_scan })
            }
            Some(Replaced::Shown { count, scan }) if Arc::ptr_eq(&scan, &new_scan) => {
                Some(Replaced::Shown { count, scan })
            }
            Some(Replaced::Shown { .. }) => None,
            other => other,
        };
        // The panel copies every change it makes back from the editor, so a case mode that differs
        // here was set from outside (Option-Cmd-C): it is a choice, and smartcase stops picking it.
        if session.case_insensitive != self.session.case_insensitive {
            self.case_touched = true;
        }
        if session.open && session.regex != self.session.regex && !self.case_touched {
            self.recase = true;
        }
        // A leaving bar keeps the count it showed; closing emptied the editor's matcher.
        let matcher = (self.exiting && !session.open).then(|| self.session.matcher.clone());
        self.session = session.clone();
        if let Some(matcher) = matcher {
            self.session.matcher = matcher;
        }
    }

    /// The query the panel's search input currently holds.
    pub(super) fn query(&self, cx: &App) -> gpui::SharedString {
        self.search_input.read(cx).value()
    }

    /// The panel's search input, so a test can assert what re-invoking search
    /// does to its value and selection.
    #[cfg(test)]
    pub(super) fn search_input(&self) -> &Entity<InputState> {
        &self.search_input
    }

    pub(crate) fn new(
        editor: Entity<InputBaseState<M>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        let replace_input = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));

        let initial = editor.read(cx).search_session().clone();
        cx.new(|cx| {
            let _subscriptions =
                vec![
                    cx.subscribe(&search_input, |this: &mut Self, _, ev: &InputEvent, cx| {
                        // Handle search input changes
                        match ev {
                            InputEvent::Change => {
                                this.find_recall.at = None;
                                this.update_search_query(None, cx);
                            }
                            _ => {}
                        }
                    }),
                    cx.subscribe(&replace_input, |this: &mut Self, _, ev: &InputEvent, _| {
                        if matches!(ev, InputEvent::Change) {
                            this.replace_recall.at = None;
                        }
                    }),
                ];

            Self {
                editor: editor.downgrade(),
                search_input,
                replace_input,
                session: initial,
                case_touched: false,
                shown: false,
                replace_row: false,
                exiting: false,
                epoch: 0,
                replaced: None,
                wraps: 0,
                wrap_forward: true,
                misses: 0,
                find_recall: Recall::default(),
                replace_recall: Recall::default(),
                replace_misses: 0,
                step: None,
                recase: false,
                _subscriptions,
            }
        })
    }

    pub(super) fn show_with_focus(
        &mut self,
        selected_text: &Rope,
        replace_mode: bool,
        visible_range_offset: Option<Range<usize>>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session.open = true;
        self.find_recall.at = None;
        self.replace_recall.at = None;
        if !self.shown || self.exiting {
            self.epoch += 1;
        }
        // Asking for replace again while the row is open goes to its field.
        let again = replace_mode && self.shown && !self.exiting && self.replace_row;
        self.shown = true;
        self.exiting = false;
        self.session.replace_mode = replace_mode;
        self.replace_row = replace_mode;
        if focus {
            let field = if again { &self.replace_input } else { &self.search_input };
            field.update(cx, |input, cx| input.focus(window, cx));
        }

        self.search_input.update(cx, |this, cx| {
            if selected_text.len() > 0 {
                this.set_value(selected_text.to_string(), window, cx);
            }
            this.select_all(window, cx);
        });

        // The `set_value` does not emit `InputEvent::Change`, so update the query
        // here to match the value of the search input.
        self.update_search_query(visible_range_offset, cx);
    }

    /// Update the matcher by the value of the search input.
    ///
    /// The `visible_range_offset` is to select the nearest match of the visible range,
    /// it is passed in, because the editor may be borrowed by the caller.
    fn update_search_query(
        &mut self,
        visible_range_offset: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let field = self.search_input.read(cx).value();
        // The field is one line, so it shows a multi-line query with its line breaks dropped.
        // While it still reads that way the search keeps the breaks, so the field and the search
        // stay one query as Cmd-E and Cmd-F seeded it from the selection.
        let query: gpui::SharedString = if field.as_ref() != self.session.query.as_str()
            && self.session.query.chars().filter(|c| !matches!(c, '\n' | '\r')).eq(field.chars())
        {
            self.session.query.clone().into()
        } else {
            field
        };
        if !self.case_touched {
            self.session.case_insensitive = if self.session.regex {
                gpui_base::input::SearchSession::smart_case_insensitive_regex(&query)
            } else {
                gpui_base::input::SearchSession::smart_case_insensitive(&query)
            };
        }
        let editor = self.editor.clone();
        let _ = editor.update(cx, |state, cx| {
            state.set_search_options(
                query.clone(),
                self.session.case_insensitive,
                self.session.whole_word,
                cx,
            );
        });
        if let Ok(session) = editor.read_with(cx, |state, _| state.search_session().clone()) {
            self.session = session;
        }
        if let Some(visible_range_offset) = visible_range_offset {
            self.session
                .matcher
                .update_cursor_by_offset(visible_range_offset.start);
        }
        cx.notify();
    }

    fn replaceable(&self, cx: &App) -> bool {
        self.editor
            .read_with(cx, |editor, _| editor.is_replaceable())
            .unwrap_or(false)
    }

    pub(super) fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_with_focus(true, window, cx);
    }

    pub(super) fn hide_with_focus(
        &mut self,
        focus_editor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session.open = false;
        remember_query(&self.session.query, cx);
        self.find_recall.at = None;
        self.replace_recall.at = None;
        self.replaced = None;
        let _ = self.editor.update(cx, |state, cx| {
            let selected_match = if focus_editor && !state.search_session().query.is_empty() {
                let matcher = &state.search_session().matcher;
                matcher.matched_ranges().get(matcher.current_match_index()).cloned()
            } else {
                None
            };
            state.close_search(cx);
            if focus_editor {
                if let Some(range) = selected_match {
                    state.set_selected_range(range, cx);
                }
                state.focus(window, cx);
            }
        });
        // A bar that was showing stays drawn for its exit, then the editor draws without it.
        if std::mem::replace(&mut self.shown, false) {
            let exit = cx
                .try_global::<FindBarRenderer>()
                .map_or(std::time::Duration::ZERO, |r| r.exit);
            if !exit.is_zero() {
                self.exiting = true;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(exit).await;
                    let _ = this.update(cx, |this, cx| {
                        if !this.shown {
                            this.exiting = false;
                            this.session.matcher = Default::default();
                            this.clear_cover(cx);
                            let _ = this.editor.update(cx, |_, cx| cx.notify());
                            cx.notify();
                        }
                    });
                })
                .detach();
            } else {
                self.clear_cover(cx);
            }
        } else {
            self.clear_cover(cx);
        }
        cx.notify();
    }
    /// The bar no longer draws, so the carets it covered show again.
    fn clear_cover(&self, cx: &mut App) {
        let _ = self.editor.read_with(cx, |state, _| state.set_caret_cover(None));
    }
    fn on_action_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if !action.shift {
            remember_query(&self.session.query, cx);
            self.find_recall.at = None;
        }
        if action.shift {
            self.prev(window, cx);
        } else if self.session.replace_mode
            && self.replace_input.read(cx).focus_handle(cx).is_focused(window)
        {
            // Return in the replace field replaces the match and moves on, as Xcode and TextEdit.
            self.replace_next(true, window, cx);
        } else {
            self.next(window, cx);
        }
    }

    fn on_action_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.hide(window, cx);
    }

    /// Up and Down in the find or the replace field: step through that field's past entries. A
    /// multi-line query keeps them as caret moves, and so does Down with nothing recalled.
    fn recall_entry(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let field = if self.search_input.read(cx).focus_handle(cx).is_focused(window) {
            Field::Find
        } else if self.replace_input.read(cx).focus_handle(cx).is_focused(window) {
            Field::Replace
        } else {
            return false;
        };
        if field == Field::Find && self.session.query.contains(['\n', '\r']) {
            return false;
        }
        let input = match field {
            Field::Find => self.search_input.clone(),
            Field::Replace => self.replace_input.clone(),
        };
        let past = cx.try_global::<FindHistory>().map_or(&[][..], |history| history.list(field).as_slice());
        let shown = input.read(cx).value().to_string();
        let recall = match field {
            Field::Find => &mut self.find_recall,
            Field::Replace => &mut self.replace_recall,
        };
        // Another panel may have cleared the shared list since this one stepped: a place past its end is no place.
        let at = recall.at.filter(|&at| at < past.len());
        let target = match (at, older) {
            (None, true) => {
                // What the last search left in the field is not an earlier one.
                let first = usize::from(past.first().is_some_and(|newest| *newest == shown));
                if first >= past.len() {
                    return false;
                }
                recall.typed = shown;
                Some(first)
            }
            (None, false) => return false,
            (Some(at), true) => Some((at + 1).min(past.len().saturating_sub(1))),
            (Some(0), false) => None,
            (Some(at), false) => Some(at - 1),
        };
        recall.at = target;
        let text = match target {
            Some(at) => past[at].clone(),
            None => std::mem::take(&mut recall.typed),
        };
        input.update(cx, |input, cx| input.set_value(text, window, cx));
        if field == Field::Find {
            // `set_value` does not emit `InputEvent::Change`.
            self.update_search_query(None, cx);
        }
        true
    }

    fn on_capture_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.recall_entry(true, window, cx) {
            cx.stop_propagation();
        }
    }

    fn on_capture_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.recall_entry(false, window, cx) {
            cx.stop_propagation();
        }
    }

    fn on_action_tab(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_focus(window, cx);
    }

    fn on_action_tab_prev(
        &mut self,
        _: &OutdentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_focus(window, cx);
    }

    /// Cycle focus between the search and the replace input, to keep the Tab key
    /// staying in the panel.
    ///
    /// There are only 2 inputs, so the forward and the backward are the same.
    fn cycle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.session.replace_mode || !self.replaceable(cx) {
            return;
        }

        let search_focus_handle = self.search_input.read(cx).focus_handle(cx);
        let focus_handle = if search_focus_handle.is_focused(window) {
            self.replace_input.read(cx).focus_handle(cx)
        } else {
            search_focus_handle
        };
        focus_handle.focus(window, cx);
    }

    /// Shift-Cmd-F: with the replace row closed it opens and takes focus; with it open it moves
    /// focus from the find field to the replace field, and from the replace field it closes the
    /// row again (as the replace button does).
    fn on_action_replace(&mut self, _: &Replace, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.replace_mode
            && self.replaceable(cx)
            && self.search_input.read(cx).focus_handle(cx).is_focused(window)
        {
            self.replace_input.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        self.toggle_replace_mode(window, cx);
    }

    /// Cmd-F from either field of the open bar: back to plain Find, as on the Mac. The replace row
    /// closes (the editor's session follows), and the caret goes to the find field with its text
    /// selected.
    fn on_action_search(&mut self, _: &Search, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.replace_mode {
            self.session.replace_mode = false;
            self.replace_row = false;
            let _ = self.editor.update(cx, |state, cx| {
                state.set_search_replace_mode(false, cx);
            });
        }
        self.search_input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// Toggle the replace field, and move focus to the field that is going to be used.
    fn toggle_replace_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replaceable(cx) {
            return;
        }

        self.session.replace_mode = !self.session.replace_mode;
        let replace_mode = self.session.replace_mode;
        self.replace_row = replace_mode;
        let _ = self.editor.update(cx, |state, cx| {
            state.set_search_replace_mode(replace_mode, cx);
        });
        let focus_handle = if self.session.replace_mode {
            self.replace_input.read(cx).focus_handle(cx)
        } else {
            self.search_input.read(cx).focus_handle(cx)
        };
        focus_handle.focus(window, cx);
        cx.notify();
    }

    fn prev(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.step = Some(false);
        let _ = self.editor.update(cx, |state, cx| {
            _ = state.previous_search_match(cx);
        });
    }

    fn next(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.step = Some(true);
        let _ = self.editor.update(cx, |state, cx| {
            _ = state.next_search_match(cx);
        });
    }

    fn replace_next(&mut self, jump: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replaceable(cx) {
            self.session.replace_mode = false;
            cx.notify();
            return;
        }

        let replacement = self.replace_input.read(cx).value();
        remember_replacement(&replacement, cx);
        self.replace_recall.at = None;
        let _ = self.editor.update(cx, |state, cx| {
            _ = state.replace_current_search_match(&replacement, jump, window, cx);
        });
    }

    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replaceable(cx) {
            self.session.replace_mode = false;
            cx.notify();
            return;
        }

        let replacement = self.replace_input.read(cx).value();
        remember_replacement(&replacement, cx);
        self.replace_recall.at = None;
        let _ = self.editor.update(cx, |state, cx| {
            let before = state.search_session().matcher.matched_ranges();
            let count = state.replace_all_search_matches(&replacement, window, cx);
            self.replaced = (count > 0).then_some(Replaced::Pending { count, before });
            // No match: the count pulses once, as a Find next that finds nothing does.
            if count == 0 {
                self.replace_misses = self.replace_misses.wrapping_add(1);
            }
        });
    }

    /// Use selection for replace: the Replace field holds `text`, ready for Replace.
    pub(super) fn set_replacement(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_recall.at = None;
        // `set_value` does not emit `InputEvent::Change`.
        self.replace_input.update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    /// Edit > Find > Replace, Replace and find and Replace all: they run with the caret in the
    /// editor, so they never move focus. Returns whether the row was open to take them.
    pub(super) fn replace_command(
        &mut self,
        command: FindReplace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.session.open || !self.session.replace_mode || !self.replaceable(cx) {
            return false;
        }
        match command {
            FindReplace::Replace => self.replace_next(false, window, cx),
            FindReplace::ReplaceAndFind => self.replace_next(true, window, cx),
            FindReplace::ReplaceAll => self.replace_all(window, cx),
        }
        cx.notify();
        true
    }

    /// A press on one of the app's find bar controls.
    fn control(&mut self, control: FindControl, window: &mut Window, cx: &mut Context<Self>) {
        // A bar that is leaving takes nothing but its close.
        if !self.session.open && control != FindControl::Close {
            return;
        }
        match control {
            FindControl::CaseSensitive => {
                self.case_touched = true;
                let _ = self.editor.update(cx, |state, _| state.mark_search_case_chosen());
                self.session.case_insensitive = !self.session.case_insensitive;
                self.update_search_query(None, cx);
            }
            FindControl::WholeWord => {
                self.session.whole_word = !self.session.whole_word;
                self.update_search_query(None, cx);
            }
            FindControl::Regex => {
                let regex = !self.session.regex;
                self.session.regex = regex;
                let _ = self.editor.update(cx, |state, cx| state.set_search_regex(regex, cx));
                self.update_search_query(None, cx);
            }
            FindControl::IgnoreDiacritics => {
                let on = !self.session.ignore_diacritics();
                let _ = self.editor.update(cx, |state, cx| state.set_search_ignore_diacritics(on, cx));
                self.update_search_query(None, cx);
            }
            FindControl::InSelection => {
                let on = !self.session.in_selection();
                let _ = self.editor.update(cx, |state, cx| state.set_search_in_selection(on, cx));
                self.update_search_query(None, cx);
            }
            FindControl::Previous => self.prev(window, cx),
            FindControl::Next => self.next(window, cx),
            FindControl::Close => self.hide(window, cx),
            FindControl::ToggleReplace => self.toggle_replace_mode(window, cx),
            FindControl::ReplaceOne => self.replace_next(true, window, cx),
            FindControl::ReplaceAll => self.replace_all(window, cx),
            FindControl::Recent(ix) => {
                if let Some(query) = recent_queries(cx).into_iter().nth(ix) {
                    self.search_input.update(cx, |input, cx| {
                        input.set_value(query.clone(), window, cx);
                        input.focus(window, cx);
                    });
                    remember_query(&query, cx);
                    self.find_recall.at = None;
                    // `set_value` does not emit `InputEvent::Change`.
                    self.update_search_query(None, cx);
                }
            }
            FindControl::ClearRecent => {
                if cx.has_global::<FindHistory>() {
                    cx.global_mut::<FindHistory>().queries.clear();
                }
                self.find_recall.at = None;
            }
        }
        cx.notify();
    }
}

impl<M: crate::input::overlay::OverlayMode> Focusable for SearchPanel<M> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search_input.read(cx).focus_handle(cx)
    }
}

impl<M: crate::input::overlay::OverlayMode> Render for SearchPanel<M> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let leaving = !self.session.open;
        if leaving && !self.exiting {
            return Empty.into_any_element();
        }
        // Regular expression was flipped from outside (Option-Cmd-R): the query reads differently
        // as a regex (`\S` is syntax), so smartcase picks the case again, as the button's press does.
        if std::mem::take(&mut self.recase) {
            cx.defer_in(window, |this, _, cx| {
                let query = this.session.query.as_str();
                let insensitive = if this.session.regex {
                    gpui_base::input::SearchSession::smart_case_insensitive_regex(query)
                } else {
                    gpui_base::input::SearchSession::smart_case_insensitive(query)
                };
                if insensitive != this.session.case_insensitive {
                    this.update_search_query(None, cx);
                }
            });
        }
        let Some(render) = cx.try_global::<FindBarRenderer>().map(|r| r.render) else {
            return Empty.into_any_element();
        };

        let allow_replace = self.replaceable(cx);
        if !allow_replace {
            self.session.replace_mode = false;
        }
        let panel = cx.entity().downgrade();
        let bar = FindBar {
            search: self.search_input.clone(),
            replace: (self.session.replace_mode && allow_replace)
                .then(|| self.replace_input.clone()),
            can_replace: allow_replace,
            case_sensitive: !self.session.case_insensitive,
            whole_word: self.session.whole_word,
            regex: self.session.regex,
            ignore_diacritics: self.session.ignore_diacritics(),
            width: self
                .editor
                .read_with(cx, |editor, _| editor.input_bounds().size.width)
                .ok()
                .filter(|width| *width > gpui::px(0.))
                .unwrap_or(gpui::px(10_000.)),
            bad_pattern: self.session.matcher.problem(),
            query_empty: self.search_input.read(cx).value().is_empty(),
            current: self.session.matcher.shown_current().map_or(0, |ix| ix + 1),
            total: self.session.matcher.shown_total(),
            in_selection: self.session.in_selection(),
            scope_empty: self.session.matcher.scope().is_some_and(<[_]>::is_empty),
            epoch: self.epoch,
            replaced: match &self.replaced {
                Some(Replaced::Pending { count, .. } | Replaced::Shown { count, .. }) => Some(*count),
                None => None,
            },
            wraps: self.wraps,
            wrap_forward: self.wrap_forward,
            misses: self.misses.wrapping_add(self.replace_misses),
            leaving,
            cover: {
                let editor = self.editor.clone();
                Rc::new(move |bounds, cx| {
                    if let Some(editor) = editor.upgrade() {
                        editor.read(cx).set_caret_cover(bounds);
                    }
                })
            },
            act: Rc::new(move |control, window, cx| {
                let _ = panel.update(cx, |this, cx| this.control(control, window, cx));
            }),
        };

        // The panel spans the editor's top and pushes the bar to the right; it has no hitbox,
        // so nothing here takes a click from the text beside the bar.
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .flex()
            .justify_end()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::on_action_enter))
            .capture_action(cx.listener(Self::on_capture_up))
            .capture_action(cx.listener(Self::on_capture_down))
            .on_action(cx.listener(Self::on_action_escape))
            .on_action(cx.listener(Self::on_action_tab))
            .on_action(cx.listener(Self::on_action_tab_prev))
            .on_action(cx.listener(Self::on_action_replace))
            .on_action(cx.listener(Self::on_action_search))
            .child(render(bar, window, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[gpui::test]
    fn history_skips_huge_queries(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            remember_query(&"x".repeat(1024 * 1024), cx);
            assert!(cx.try_global::<FindHistory>().is_none_or(|history| history.queries.is_empty()));
            remember_query(&"y".repeat(HISTORY_QUERY_MAX + 1), cx);
            assert!(cx.try_global::<FindHistory>().is_none_or(|history| history.queries.is_empty()));
            remember_query("short", cx);
            remember_query(&"z".repeat(HISTORY_QUERY_MAX), cx);
            assert_eq!(cx.global::<FindHistory>().queries.len(), 2);
            assert_eq!(cx.global::<FindHistory>().queries[1], "short");
        });
    }

    #[gpui::test]
    fn closing_find_selects_the_stepped_match(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let editor = cx.new(|cx| {
                crate::input::EditorState::new(window, cx).default_value("foo a foo b foo")
            });
            let panel = SearchPanel::new(editor.clone(), window, cx);
            editor.update(cx, |state, cx| {
                state.set_selected_range(0..0, cx);
                state.set_search_query("foo", true, cx);
                assert_eq!(state.next_search_match(cx), Some(6..9));
                assert_eq!(state.selected_range(), 0..0);
            });
            panel.update(cx, |panel, cx| panel.hide_with_focus(true, window, cx));
            assert_eq!(editor.read(cx).selected_range(), 6..9);
            assert!(!editor.read(cx).search_session().is_active());

            editor.update(cx, |state, cx| state.set_search_query("missing", true, cx));
            panel.update(cx, |panel, cx| panel.hide_with_focus(true, window, cx));
            assert_eq!(editor.read(cx).selected_range(), 6..9);

            editor.update(cx, |state, cx| state.set_search_query("foo", true, cx));
            panel.update(cx, |panel, cx| panel.hide_with_focus(false, window, cx));
            assert_eq!(editor.read(cx).selected_range(), 6..9);
        });
    }

    #[gpui::test]
    fn ignore_diacritics_control_follows_the_editor(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let editor = cx.new(|cx| crate::input::EditorState::new(window, cx).default_value("café cafe"));
            let panel = SearchPanel::new(editor.clone(), window, cx);
            editor.update(cx, |state, cx| {
                state.open_search(false, cx);
                state.set_search_query("cafe", true, cx);
            });
            let sync = |panel: &Entity<SearchPanel<_>>, cx: &mut App| {
                let session = editor.read(cx).search_session().clone();
                panel.update(cx, |panel, _| panel.sync_session(&session));
            };
            sync(&panel, cx);
            // The button turns the option on.
            panel.update(cx, |panel, cx| panel.control(FindControl::IgnoreDiacritics, window, cx));
            assert!(editor.read(cx).search_session().ignore_diacritics());
            // The menu item turns it off: the bar reads it off, and the button turns it on again.
            editor.update(cx, |state, cx| state.set_search_ignore_diacritics(false, cx));
            sync(&panel, cx);
            panel.update(cx, |panel, cx| panel.control(FindControl::IgnoreDiacritics, window, cx));
            assert!(editor.read(cx).search_session().ignore_diacritics());
            panel.update(cx, |panel, cx| panel.control(FindControl::IgnoreDiacritics, window, cx));
            assert!(!editor.read(cx).search_session().ignore_diacritics());
        });
    }

    #[test]
    fn test_search() {
        let mut matcher = SearchMatcher::new();
        matcher.update_query("Is", true, false);
        matcher.update(&Rope::from("Hello 世界 this is a Is test string."));

        assert_eq!(matcher.len(), 3);
        let mut matches = matcher.clone();
        assert_eq!(matches.current_match_index(), 0);
        assert_eq!(matches.next(), Some(18..20));
        assert_eq!(matches.next(), Some(23..25));
        assert_eq!(matches.current_match_index(), 2);
        assert_eq!(matches.next(), Some(15..17));
        assert_eq!(matches.current_match_index(), 0);
        assert_eq!(matches.next_back(), Some(23..25));
        assert_eq!(matches.current_match_index(), 2);
        assert_eq!(matches.next_back(), Some(18..20));
        assert_eq!(matches.current_match_index(), 1);
        assert_eq!(matches.next_back(), Some(15..17));
        assert_eq!(matches.current_match_index(), 0);
        assert_eq!(matches.next_back(), Some(23..25));

        matcher.update_query("IS", false, false);
        matcher.update(&Rope::from("Hello 世界 this is a Is test string."));
        assert_eq!(matcher.len(), 0);
        assert_eq!(matcher.next(), None);
        assert_eq!(matcher.next_back(), None);
    }

    #[test]
    fn test_search_label() {
        let mut matcher = SearchMatcher::new();
        matcher.update_query("Is", true, false);
        matcher.update(&Rope::from("Hello 世界 this is a Is test string."));
        assert_eq!(matcher.label(), "1/3");
        matcher.next();
        assert_eq!(matcher.label(), "2/3");
        matcher.next();
        assert_eq!(matcher.label(), "3/3");
        matcher.next();
        assert_eq!(matcher.label(), "1/3");

        matcher.update_query("IS", false, false);
        matcher.update(&Rope::from("Hello 世界 this is a Is test string."));
        assert_eq!(matcher.label(), "0/0");
    }

    #[test]
    fn test_select_range_start() {
        let mut matcher = SearchMatcher::new();
        matcher.update_query("aaaaa", false, false);
        matcher.update(&Rope::from(".....aaaaa.....aaaaa.....aaaaa"));
        matcher.update_cursor_by_offset(0);
        assert_eq!(matcher.current_match_index(), 0);

        matcher.update_cursor_by_offset(5);
        assert_eq!(matcher.current_match_index(), 0);

        matcher.update_cursor_by_offset(12);
        assert_eq!(matcher.current_match_index(), 1);

        matcher.update_cursor_by_offset(16);
        assert_eq!(matcher.current_match_index(), 1);

        matcher.update_cursor_by_offset(30);
        assert_eq!(matcher.current_match_index(), 2);

        matcher.update_cursor_by_offset(31);
        assert_eq!(matcher.current_match_index(), 2);
    }

    #[test]
    fn test_next_scroll_direction_returns_down_without_wrap() {
        assert!(matches!(
            next_scroll_direction(0, 1),
            Some(MoveDirection::Down)
        ));
    }

    #[test]
    fn test_next_scroll_direction_returns_none_on_wrap() {
        assert!(next_scroll_direction(2, 0).is_none());
    }

    #[test]
    fn test_next_scroll_direction_returns_none_for_single_match() {
        assert!(next_scroll_direction(0, 0).is_none());
    }

    #[test]
    fn test_prev_scroll_direction_returns_up_without_wrap() {
        assert!(matches!(
            prev_scroll_direction(2, 1),
            Some(MoveDirection::Up)
        ));
    }

    #[test]
    fn test_prev_scroll_direction_returns_none_on_wrap() {
        assert!(prev_scroll_direction(0, 2).is_none());
    }

    #[test]
    fn test_prev_scroll_direction_returns_none_for_single_match() {
        assert!(prev_scroll_direction(0, 0).is_none());
    }
}

#[cfg(test)]
mod replace_history_tests {
    use super::*;

    #[gpui::test]
    fn up_in_replace_recalls_earlier_replacements(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let editor = cx.new(|cx| {
                crate::input::EditorState::new(window, cx).default_value("foo foo")
            });
            let panel = SearchPanel::new(editor.clone(), window, cx);
            panel.update(cx, |panel, cx| {
                panel.session.replace_mode = true;
                panel.search_input.update(cx, |input, cx| input.set_value("foo", window, cx));
                panel.update_search_query(None, cx);
                panel.replace_input.update(cx, |input, cx| input.set_value("one", window, cx));
                panel.replace_all(window, cx);
                // The replaced text no longer matches, so the next run is on fresh text.
                editor.update(cx, |state, cx| state.set_value("foo foo", window, cx));
                panel.update_search_query(None, cx);
                panel.replace_input.update(cx, |input, cx| input.set_value("two", window, cx));
                panel.replace_all(window, cx);
                panel.replace_input.update(cx, |input, cx| input.set_value("", window, cx));
                panel.replace_input.update(cx, |input, cx| input.focus(window, cx));
                macro_rules! shown {
                    () => {
                        panel.replace_input.read(cx).value().to_string()
                    };
                }
                assert!(panel.recall_entry(true, window, cx));
                assert_eq!(shown!(), "two");
                assert!(panel.recall_entry(true, window, cx));
                assert_eq!(shown!(), "one");
                assert!(panel.recall_entry(false, window, cx));
                assert_eq!(shown!(), "two");
                assert!(panel.recall_entry(false, window, cx));
                assert_eq!(shown!(), "");
                // Find's list is its own.
                assert!(cx.global::<FindHistory>().queries.is_empty());
            });
        });
    }

    #[gpui::test]
    fn picking_a_recent_search_fills_the_field_and_clearing_empties_the_list(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let editor = cx.new(|cx| {
                crate::input::EditorState::new(window, cx).default_value("alpha beta alpha")
            });
            remember_query("beta", cx);
            remember_query("alpha", cx);
            assert_eq!(recent_queries(cx), vec!["alpha".to_string(), "beta".to_string()]);
            let panel = SearchPanel::new(editor.clone(), window, cx);
            panel.update(cx, |panel, cx| {
                panel.session.open = true;
                panel.control(FindControl::Recent(1), window, cx);
                assert_eq!(panel.search_input.read(cx).value().to_string(), "beta");
                assert_eq!(panel.session.query.as_str(), "beta");
                // Picking makes it the newest.
                assert_eq!(recent_queries(cx), vec!["beta".to_string(), "alpha".to_string()]);
                // The test's editor never opened Find, so its session closes again after the pick; a real bar's stays open.
                panel.session.open = true;
                panel.control(FindControl::ClearRecent, window, cx);
                assert!(recent_queries(cx).is_empty());
                // The field keeps what it shows.
                assert_eq!(panel.search_input.read(cx).value().to_string(), "beta");
            });
        });
    }

    /// The history is shared by every editor's panel but each panel keeps its own place in it: one panel
    /// clearing the list must not leave another's Up or Down indexing past the end.
    #[gpui::test]
    fn recall_after_another_panel_cleared_the_list_does_not_index_past_it(cx: &mut gpui::TestAppContext) {
        cx.update(crate::init);
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let editor = |cx: &mut App, window: &mut Window| {
                cx.new(|cx| crate::input::EditorState::new(window, cx).default_value("alpha beta"))
            };
            remember_query("beta", cx);
            remember_query("alpha", cx);
            let (one, two) = (editor(cx, window), editor(cx, window));
            let first = SearchPanel::new(one, window, cx);
            let second = SearchPanel::new(two, window, cx);
            second.update(cx, |panel, cx| {
                panel.search_input.update(cx, |input, cx| input.focus(window, cx));
                assert!(panel.recall_entry(true, window, cx));
                assert_eq!(panel.find_recall.at, Some(0));
            });
            first.update(cx, |panel, cx| {
                panel.session.open = true;
                panel.control(FindControl::ClearRecent, window, cx);
            });
            second.update(cx, |panel, cx| {
                // Up and Down with nothing left to walk: no panic, and the field is left to its own keys.
                assert!(!panel.recall_entry(true, window, cx));
                assert!(!panel.recall_entry(false, window, cx));
            });
        });
    }
}
