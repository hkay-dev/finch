use crate::input::InputModeKind;
use aho_corasick::AhoCorasick;
use gpui::{App, ClipboardItem, Context, Window};
use ropey::Rope;
use std::{borrow::Cow, io, ops::Range, sync::Arc};

use super::{
    InputBaseState, Replace, RopeExt as _, Search, movement::MoveDirection, state::ScrollPadding,
    selection::TextSelector,
    cursor::CursorSelection,
};

/// What became of a background scan: taken, dropped as out of date, or to run again.
pub enum Landing {
    Adopted,
    Dropped,
    Again,
}

/// Stateful, presentation-independent search engine used by text inputs.
#[derive(Debug, Clone)]
pub struct SearchMatcher {
    pattern: Option<Pattern>,
    whole_word: bool,
    /// The query is a regular expression (the Regex option), so a replacement expands `$1`.
    regex: bool,
    /// The query is a regular expression that does not compile: it matches nothing. Says what is
    /// wrong, in a few plain words.
    invalid: Option<&'static str>,
    /// The query or the text changed since the last scan.
    stale: bool,
    matched_ranges: Arc<Vec<Range<usize>>>,
    current_match_ix: usize,
    replacing: bool,
    /// Where the next scan after a single Replace picks the current match from.
    resume_at: Option<usize>,
    /// The text `matched_ranges` was found in, which the next scan compares with the text it scans
    /// to rescan only what an edit touched. `None` after a new query or a release.
    scanned: Option<Rope>,
    /// Changed span in the last scanned text and the current text, composed as edits are applied.
    edit_span: Option<(Range<usize>, Range<usize>)>,
    /// Find in selection: the ranges a match must lie in.
    scope: Option<Scope>,
    /// Set while the matches are let go (a hidden tab): how many there were, which the bar keeps
    /// showing until a scan lands. The current index is kept too.
    released_len: Option<usize>,
    /// Counts releases and scans, so a background scan's result is adopted only while it is the
    /// newest.
    scan_gen: u64,
    /// A background scan began for this release and has not landed.
    scanning: bool,
    /// The query as last set, which the next scan may narrow from (see `found`).
    query: String,
    /// The case mode of `query`.
    case_insensitive: bool,
    /// Letters match whatever accents they carry (the Ignore diacritics option): the query is
    /// read as a regex over each letter's accented forms. Plain-text queries only.
    ignore_diacritics: bool,
    /// The folded query, when Ignore diacritics reads it and its matches never overlap.
    fold: Option<Fold>,
    /// What `matched_ranges` answers when it holds every occurrence of a plain literal: a longer
    /// query that starts with it reads those matches instead of scanning the text again.
    found: Option<Found>,
}

/// The ranges Find in selection limits a search to. `text` is what they were last placed in, so
/// the next scan can move them through whatever changed since; `None` when an edit was already
/// applied to them range by range (`apply`), and the next scan only takes the new text. The
/// ranges are shared, as the matches are, since the session is cloned on every render.
#[derive(Debug, Clone)]
struct Scope {
    ranges: Arc<Vec<Range<usize>>>,
    text: Option<Rope>,
}

impl Scope {
    /// Whether `range` lies wholly inside one of the scope's ranges.
    fn holds(&self, range: &Range<usize>) -> bool {
        let ix = self.ranges.partition_point(|scope| scope.end < range.end);
        self.ranges.get(ix).is_some_and(|scope| scope.start <= range.start && range.end <= scope.end)
    }

    /// Move the ranges through one edit: `old` was replaced by `new_len` bytes. An edge inside the
    /// edit goes to its edge, text inserted exactly at a range's start or end stays outside it, and
    /// a range the edit empties is dropped.
    fn apply(&mut self, old: &Range<usize>, new_len: usize) {
        let (start, end) = (old.start, old.end);
        let shift = |at: usize| at - end + start + new_len;
        let ranges = Arc::make_mut(&mut self.ranges);
        for range in ranges.iter_mut() {
            range.start = if range.start < start { range.start } else if range.start >= end { shift(range.start) } else { start };
            range.end = if range.end <= start { range.end } else if range.end >= end { shift(range.end) } else { start + new_len };
        }
        ranges.retain(|range| !range.is_empty());
    }

    /// Move the ranges through whatever changed between the text they were placed in and `text`,
    /// read as one edit (the text both share at each end is kept).
    fn follow(&mut self, text: &Rope) {
        if let Some(old) = self.text.take() {
            let (prefix, suffix) = common_ends(&old, text);
            let (old_end, new_end) = (old.len() - suffix, text.len() - suffix);
            if old_end != new_end || old_end != prefix {
                self.apply(&(prefix..old_end), new_end - prefix);
            }
        }
        self.text = Some(text.clone());
    }
}

/// The query and the text `SearchMatcher::matched_ranges` answers, kept while it holds every
/// occurrence of a plain literal query (see `SearchMatcher::narrow`).
#[derive(Debug, Clone)]
struct Found {
    query: String,
    case_insensitive: bool,
    text: Rope,
}

/// What a folded query (Ignore diacritics) adds to `Found`'s narrowing: the query's letters
/// without their accents, which a longer query must start with, and the pattern held to the start
/// of the text it is given. `letters` is how many characters the key holds, which bounds how far a
/// match reaches (see `SearchMatcher::narrow`).
#[derive(Debug, Clone)]
struct Fold {
    key: String,
    anchored: regex::Regex,
    letters: usize,
}

/// How a query is looked up. A literal streams over the rope's chunks. A
/// case-insensitive query with non-ASCII letters needs Unicode case folding,
/// which Aho-Corasick lacks, so it runs as an escaped `(?i)` regex.
#[derive(Debug, Clone)]
enum Pattern {
    /// `borderless`: no match of it overlaps another.
    Literal { finder: AhoCorasick, len: usize, borderless: bool },
    Regex {
        regex: regex::Regex,
        /// The query holds a line break, so it cannot be matched line by line.
        multiline: bool,
        /// How many line breaks the query holds: a match spans that many lines more than one.
        rows: usize,
        /// No match of it overlaps another, even across the letter case it ignores.
        borderless: bool,
    },
}

/// One search over an input: the query, how the built-in panel shows it, and
/// its matches. Read it through [`InputBaseState::search_session`]; it is
/// written only through the input state's search methods, and it grows, so
/// build it with `Default` and do not destructure it exhaustively.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SearchSession {
    /// The built-in search panel is showing.
    pub open: bool,
    pub replace_mode: bool,
    pub case_insensitive: bool,
    /// The case mode was set on purpose (the case toggle, Option-Cmd-C), so smartcase no longer
    /// picks it from the query.
    pub case_chosen: bool,
    /// A match must start and end on a word boundary.
    pub whole_word: bool,
    /// The query is a regular expression; case and whole word still apply.
    pub regex: bool,
    pub query: String,
    pub replacement: String,
    pub anchor_offset: Option<usize>,
    pub matcher: SearchMatcher,
    /// A search is in progress and its matches are highlighted: the panel is
    /// open, or a query was set without it and not closed since.
    active: bool,
    /// The text last read from or written to the Mac's find pasteboard, to
    /// tell a change made by another app from this input's own.
    pasteboard_seen: Option<String>,
    /// Preserve whole-word semantics across a Cmd-D sequence started at a caret.
    /// A changed primary selection or document revision invalidates the origin.
    occurrence_word: Option<(Range<usize>, u64)>,
    /// How many times Find next or previous found no match for a non-empty query.
    pub misses: u32,
}

impl Default for SearchSession {
    fn default() -> Self {
        Self {
            open: false,
            active: false,
            pasteboard_seen: None,
            occurrence_word: None,
            misses: 0,
            replace_mode: false,
            case_insensitive: true,
            case_chosen: false,
            whole_word: false,
            regex: false,
            query: String::new(),
            replacement: String::new(),
            anchor_offset: None,
            matcher: SearchMatcher::new(),
        }
    }
}

impl SearchSession {
    pub(crate) fn open(&mut self, replace_mode: bool, replaceable: bool) {
        self.open = true;
        self.active = true;
        self.occurrence_word = None;
        self.replace_mode = replace_mode && replaceable;
    }

    /// Start a search without the built-in panel. A custom search UI drives
    /// the session through [`InputBaseState::set_search_query`], and the
    /// editor highlights the matches the same way it does for the panel.
    pub(crate) fn activate(&mut self) {
        self.active = true;
        self.occurrence_word = None;
    }

    /// Close the search. The query stays, so reopening resumes it, but the
    /// matches are dropped: they hold a range per match of the whole document
    /// and are rescanned when the search is next opened or stepped.
    pub(crate) fn close(&mut self) {
        self.open = false;
        self.active = false;
        self.matcher.release();
        self.matcher.finish_release();
        self.matcher.clear_scope();
    }

    /// Whether a search is in progress: the built-in panel is open, or a
    /// query was set without it and [`InputBaseState::close_search`] has not
    /// run since. Matches are highlighted while this holds.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Find in selection is on: the search is limited to the ranges `matcher.scope()` holds.
    pub fn in_selection(&self) -> bool {
        self.matcher.scope().is_some()
    }

    /// Smartcase: a query without an uppercase letter ignores case, one with
    /// an uppercase letter matches case.
    pub fn smart_case_insensitive(query: &str) -> bool {
        !query.chars().any(char::is_uppercase)
    }

    /// [`SearchSession::smart_case_insensitive`] for a regular expression: the escapes `\S`,
    /// `\D`, `\W`, `\p{Lu}`, `\pL` and `\x41` are syntax, not capitals the user typed.
    pub fn smart_case_insensitive_regex(query: &str) -> bool {
        let mut chars = query.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let digits = match chars.next() {
                    Some('p' | 'P') => 1,
                    Some('x') => 2,
                    Some('u') => 4,
                    Some('U') => 8,
                    _ => continue,
                };
                if chars.peek() == Some(&'{') {
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                } else {
                    for _ in 0..digits {
                        chars.next();
                    }
                }
            } else if c.is_uppercase() {
                return false;
            }
        }
        true
    }

    pub(crate) fn update_query(
        &mut self,
        query: impl Into<String>,
        case_insensitive: bool,
        whole_word: bool,
    ) {
        let query = query.into();
        if self.query == query
            && self.case_insensitive == case_insensitive
            && self.whole_word == whole_word
        {
            return;
        }

        self.query = query;
        self.case_insensitive = case_insensitive;
        self.whole_word = whole_word;
        self.matcher
            .update_pattern(&self.query, case_insensitive, whole_word, self.regex);
    }

    /// The query a selection seeds: the text itself, escaped under the Regex option so the
    /// pattern finds exactly what was selected.
    pub(crate) fn seeded(&self, selected: &str) -> String {
        if self.regex { regex::escape(selected) } else { selected.to_string() }
    }

    /// Turn the Regex option on or off, rereading the query under it.
    pub(crate) fn set_regex(&mut self, regex: bool) {
        self.regex = regex;
        self.matcher
            .update_pattern(&self.query, self.case_insensitive, self.whole_word, regex);
    }

    /// Turn the Ignore diacritics option on or off, rereading the query under it.
    pub(crate) fn set_ignore_diacritics(&mut self, on: bool) {
        self.matcher.set_ignore_diacritics(on);
    }

    /// Letters match whatever accents they carry (`cafe` finds `café`). A regular expression
    /// query ignores it.
    pub fn ignore_diacritics(&self) -> bool {
        self.matcher.ignore_diacritics
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Open the search session, or re-invoke it if it is already open.
    ///
    /// This is not idempotent: every call advances
    /// [`InputBaseState::search_activation_revision`], and the presentation
    /// layer answers that by re-focusing the search field and selecting its
    /// contents, the same as pressing the shortcut a second time. Call it from
    /// an action or another user gesture, never from a render pass or an
    /// observer that runs every frame — that would re-select the field under
    /// the user on every frame and make it impossible to type.
    pub fn open_search(&mut self, replace_mode: bool, cx: &mut Context<Self>) {
        self.open_search_with(replace_mode, true, cx);
    }

    /// [`InputBaseState::open_search`] that takes the query from the selection only when
    /// `from_selection` is set; without it the selection counts as empty, so the query, the
    /// scope, the selection and the cursors are left as they are.
    pub fn open_search_with(&mut self, replace_mode: bool, from_selection: bool, cx: &mut Context<Self>) {
        if !self.searchable {
            return;
        }
        self.search_activation_revision = self.search_activation_revision.wrapping_add(1);
        self.search_session
            .open(replace_mode, self.is_replaceable());
        let selected = if from_selection { self.selected_text().to_string() } else { String::new() };
        // A new selection starts a new search: it limits nothing until Find in selection is asked again.
        if !selected.is_empty() {
            let same = self.search_session.matcher.scope().is_some_and(|scope| scope == self.selection_scope());
            if !same {
                self.search_session.matcher.clear_scope();
            }
        }
        // Read first, so `pasteboard_seen` is current even when a selection
        // wins and gets written over another app's text.
        let shared = self.changed_find_pasteboard(cx);
        // Under the Regex option a selection is text to find, not a pattern: it goes in escaped,
        // while the find pasteboard keeps it as it was selected.
        let (query, sent) = if !selected.is_empty() {
            (self.search_session.seeded(&selected), selected)
        } else if let Some(shared) = shared {
            (shared.clone(), shared)
        } else {
            let query = self.search_session.query.clone();
            (query.clone(), query)
        };
        self.share_find_query(&sent, cx);
        let query_changed = query != self.search_session.query;
        // A retained query resumes its previous occurrence. Only a new query
        // is anchored to the current viewport.
        self.search_session.anchor_offset = if query_changed {
            self.last_layout
                .as_ref()
                .map(|layout| layout.visible_range_offset.start)
        } else {
            None
        };
        let case_insensitive = self.search_session.case_insensitive;
        let whole_word = self.search_session.whole_word;
        self.search_session
            .update_query(query, case_insensitive, whole_word);
        self.sync_search_matcher();
        if query_changed && let Some(anchor) = self.search_session.anchor_offset {
            self.search_session.matcher.update_cursor_by_offset(anchor);
        }
        cx.notify();
    }

    pub fn search_session(&self) -> &SearchSession {
        &self.search_session
    }

    /// A counter that advances every time [`InputBaseState::open_search`] runs,
    /// including while the session is already open.
    ///
    /// Re-invoking search leaves the session itself identical, so a presentation
    /// layer that decides what to rebuild by comparing session state cannot see
    /// the second request. Fold this into that comparison to notice it.
    pub fn search_activation_revision(&self) -> u64 {
        self.search_activation_revision
    }

    #[doc(hidden)]
    pub fn set_search_replace_mode(&mut self, replace_mode: bool, cx: &mut Context<Self>) {
        self.search_session.replace_mode = replace_mode && self.is_replaceable();
        cx.notify();
    }

    /// Returns true if the search panel can replace the matches.
    ///
    /// This is false when the input is not `replaceable`, or when it is
    /// `disabled` or `readonly`.
    pub fn is_replaceable(&self) -> bool {
        self.replaceable && self.is_editable()
    }

    /// Set the search query and highlight its matches.
    ///
    /// This is the entry point for a custom search UI: it needs neither
    /// `searchable` nor the built-in panel. Navigate the matches with
    /// [`InputBaseState::next_search_match`] and
    /// [`InputBaseState::previous_search_match`], read the count and the
    /// current index from [`InputBaseState::search_session`], and end the
    /// search with [`InputBaseState::close_search`].
    ///
    /// A changed query or case mode starts at the current viewport. Setting
    /// the same query and case mode again retains the current occurrence.
    pub fn set_search_query(
        &mut self,
        query: impl Into<String>,
        case_insensitive: bool,
        cx: &mut Context<Self>,
    ) {
        let whole_word = self.search_session.whole_word;
        self.set_search_options(query, case_insensitive, whole_word, cx);
    }

    /// [`InputBaseState::set_search_query`] with the Whole Word option: a
    /// match must start and end on a word boundary.
    pub fn set_search_options(
        &mut self,
        query: impl Into<String>,
        case_insensitive: bool,
        whole_word: bool,
        cx: &mut Context<Self>,
    ) {
        let query = query.into();
        let query_changed = query != self.search_session.query
            || case_insensitive != self.search_session.case_insensitive
            || whole_word != self.search_session.whole_word;
        self.search_session.anchor_offset = if query_changed {
            self.last_layout
                .as_ref()
                .map(|layout| layout.visible_range_offset.start)
        } else {
            None
        };
        self.search_session.activate();
        // The bar echoes a seeded selection back escaped under Regex: the pasteboard keeps the
        // selection as it was selected.
        let echo = self.search_session.regex
            && self.search_session.pasteboard_seen.as_deref().is_some_and(|seen| is_escaped(seen, &query));
        if !echo {
            self.share_find_query(&query, cx);
        }
        self.search_session
            .update_query(query, case_insensitive, whole_word);
        self.sync_search_matcher();
        if query_changed && let Some(anchor) = self.search_session.anchor_offset {
            self.search_session.matcher.update_cursor_by_offset(anchor);
        }
        self.reveal_search_match(query_changed, cx);
        cx.notify();
    }

    /// Typing in the open bar (or changing an option) shows the match the count names, on the
    /// search reveal (a jump over a screen glides), leaving the caret and the selection for Return
    /// and Escape. A match already wholly in view is left where it is, so a common letter does not
    /// walk the view a few lines a key; one in a fold or out of view reveals. A followed file lets
    /// go of its end unless the match is on the last line.
    fn reveal_search_match(&mut self, changed: bool, cx: &mut Context<Self>) {
        if !changed || !self.search_session.open || self.search_session.query.is_empty() {
            return;
        }
        let matcher = &self.search_session.matcher;
        let Some(range) = matcher.matched_ranges().get(matcher.current_match_index()).cloned() else { return };
        if self.offset_shown(range.end) {
            return;
        }
        if self.text.offset_to_point(range.end).row + 1 < self.text.lines_len() {
            self.stop_following(cx);
        }
        self.scroll_to_with_padding(range.end, None, ScrollPadding::SurroundingLines, cx);
    }

    /// Turn the Regex option on or off: the query is read as a regular expression, and a
    /// replacement expands `$1` and `${name}` from the match's groups. A changed option starts at
    /// the current viewport, like a changed query.
    pub fn set_search_regex(&mut self, regex: bool, cx: &mut Context<Self>) {
        if self.search_session.regex == regex {
            return;
        }
        self.search_session.anchor_offset = self
            .last_layout
            .as_ref()
            .map(|layout| layout.visible_range_offset.start);
        self.search_session.activate();
        self.search_session.set_regex(regex);
        self.sync_search_matcher();
        if let Some(anchor) = self.search_session.anchor_offset {
            self.search_session.matcher.update_cursor_by_offset(anchor);
        }
        self.reveal_search_match(true, cx);
        cx.notify();
    }

    /// Turn the Ignore diacritics option on or off: a letter in the query finds the same letter
    /// with any accent, precomposed or as a combining mark, and the match covers the accent.
    /// A changed option starts at the current viewport, like a changed query.
    pub fn set_search_ignore_diacritics(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.search_session.ignore_diacritics() == on {
            return;
        }
        self.search_session.anchor_offset = self
            .last_layout
            .as_ref()
            .map(|layout| layout.visible_range_offset.start);
        self.search_session.activate();
        self.search_session.set_ignore_diacritics(on);
        self.sync_search_matcher();
        if let Some(anchor) = self.search_session.anchor_offset {
            self.search_session.matcher.update_cursor_by_offset(anchor);
        }
        self.reveal_search_match(true, cx);
        cx.notify();
    }

    /// The selections as a Find in selection scope: the ranges that select something, sorted and
    /// apart.
    fn selection_scope(&self) -> Vec<Range<usize>> {
        let mut ranges: Vec<Range<usize>> =
            self.selected_ranges().map(|range| range.start.min(range.end)..range.start.max(range.end)).collect();
        ranges.retain(|range| !range.is_empty());
        ranges.sort_unstable_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        merged
    }

    /// Find in selection: limit the search to the selections as they are now, or lift the limit.
    /// With only a caret there is nothing to limit to, so the search finds nothing until the limit
    /// is lifted. A changed limit starts at the current viewport, like a changed option.
    pub fn set_search_in_selection(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.search_session.matcher.scope().is_some() == on {
            return;
        }
        let scope = on.then(|| self.selection_scope());
        self.search_session.anchor_offset = self
            .last_layout
            .as_ref()
            .map(|layout| layout.visible_range_offset.start);
        self.search_session.activate();
        self.search_session.matcher.set_scope(scope, &self.text);
        self.sync_search_matcher();
        if let Some(anchor) = self.search_session.anchor_offset {
            self.search_session.matcher.update_cursor_by_offset(anchor);
        }
        self.reveal_search_match(true, cx);
        cx.notify();
    }

    /// The case mode was set on purpose (the Match case button, Option-Cmd-C): smartcase stops
    /// picking it from the query, for Find and for the occurrence selections.
    pub fn mark_search_case_chosen(&mut self) {
        self.search_session.case_chosen = true;
    }

    /// Use Selection for Find: the selection becomes the query and goes to
    /// the Mac's find pasteboard, and the next Find resumes with it. The
    /// panel stays closed and no match is highlighted. Without a selection,
    /// uses the word at the caret.
    pub fn use_selection_for_find(&mut self, cx: &mut Context<Self>) {
        if !self.searchable {
            return;
        }
        let Some(range) = self.selection_or_word() else { return };
        let selected = self.text.slice(range).to_string();
        if !selected.is_empty() {
            // Note another app's find text first, so a selection equal to the
            // text this input last saw is still written over it.
            self.changed_find_pasteboard(cx);
            self.set_find_query(self.search_session.seeded(&selected), &selected, cx);
        }
    }

    /// Take the query from the Mac's find pasteboard when another app changed
    /// it since this input last used it. Runs when the input gains focus. An
    /// open panel keeps its own query: it is what the user is typing in.
    pub(super) fn adopt_find_pasteboard(&mut self, cx: &mut Context<Self>) {
        if !self.searchable || self.search_session.open {
            return;
        }
        if let Some(shared) = self.changed_find_pasteboard(cx) {
            self.set_find_query(shared.clone(), &shared, cx);
        }
    }

    /// Set the query without starting a search, and share it.
    fn set_find_query(&mut self, query: String, sent: &str, cx: &mut Context<Self>) {
        self.share_find_query(sent, cx);
        let case_insensitive = self.search_session.case_insensitive;
        let whole_word = self.search_session.whole_word;
        self.search_session.anchor_offset = None;
        self.search_session
            .update_query(query, case_insensitive, whole_word);
        if self.search_session.is_active() {
            self.sync_search_matcher();
        }
        cx.notify();
    }

    /// The query on the Mac's find pasteboard, if another app (or input)
    /// changed it since this input last read or wrote it.
    fn changed_find_pasteboard(&mut self, cx: &mut App) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            let shared = cx.read_from_find_pasteboard()?.text()?;
            if shared.is_empty() || self.search_session.pasteboard_seen.as_deref() == Some(&shared)
            {
                return None;
            }
            self.search_session.pasteboard_seen = Some(shared.clone());
            Some(shared)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = cx;
            None
        }
    }

    /// Put the query on the Mac's find pasteboard so other apps' Find fields
    /// show it.
    fn share_find_query(&mut self, query: &str, cx: &mut App) {
        #[cfg(target_os = "macos")]
        {
            if query.is_empty() || self.search_session.pasteboard_seen.as_deref() == Some(query) {
                return;
            }
            cx.write_to_find_pasteboard(ClipboardItem::new_string(query.to_string()));
            self.search_session.pasteboard_seen = Some(query.to_string());
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (query, cx);
    }

    /// End the search: hide the built-in panel and the match highlights. The
    /// query is kept so the next [`InputBaseState::open_search`] resumes it.
    pub fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_session.close();
        cx.notify();
    }

    /// The editor stopped showing: let go of the matches an active search holds (a range per
    /// match of the whole text), keeping the query, the options, Find in selection's scope and
    /// the current match's index. [`InputBaseState::restore_search_matches`] scans again.
    pub fn release_search_matches(&mut self, cx: &mut Context<Self>) {
        let matcher = &self.search_session.matcher;
        if self.search_session.is_active() && (!matcher.is_empty() || matcher.is_scanning()) {
            self.search_session.matcher.release();
            cx.notify();
        }
    }

    /// The editor shows again: scan the matches [`InputBaseState::release_search_matches`] let go
    /// of on a background thread. The bar and the ticks keep the old count until it lands, then
    /// the matches fill in at the same current match.
    pub fn restore_search_matches(&mut self, cx: &mut Context<Self>) {
        let matcher = &self.search_session.matcher;
        if !self.search_session.is_active()
            || self.search_session.query.is_empty()
            || !matcher.is_released()
            || matcher.is_scanning()
        {
            return;
        }
        let revision = self.document_revision;
        let text = self.text.clone();
        let (mut scan, scan_gen) = self.search_session.matcher.begin_scan();
        cx.spawn(async move |this, cx| {
            let scan = cx
                .background_executor()
                .spawn(async move {
                    scan.refresh(&text);
                    scan
                })
                .await;
            this.update(cx, |state, cx| state.land_search_scan(scan, scan_gen, revision, cx)).ok();
        })
        .detach();
    }

    fn land_search_scan(&mut self, scan: SearchMatcher, scan_gen: u64, revision: u64, cx: &mut Context<Self>) {
        match self.search_session.matcher.adopt_scan(scan, scan_gen, revision != self.document_revision) {
            // Text that changed during the scan is searched at once, so the next edit builds on it.
            Landing::Adopted => {
                self.sync_search_matcher();
                cx.notify();
            }
            Landing::Again => self.restore_search_matches(cx),
            Landing::Dropped => {}
        }
    }

    fn caret_word(&self) -> Option<Range<usize>> {
        let word_at = |offset| {
            TextSelector::word_range(&self.text, offset).filter(|range| {
                self.text.slice(range.clone()).chars().next().is_some_and(is_word_char)
            })
        };
        word_at(self.cursor()).or_else(|| self.cursor().checked_sub(1).and_then(word_at))
    }

    fn selection_or_word(&self) -> Option<Range<usize>> {
        let range = self.selected_range();
        if range.is_empty() {
            self.caret_word()
        } else {
            Some(range)
        }
    }

    /// The case mode for selecting occurrences of `text`: the one Find uses, so smartcase
    /// (a capital matches case) unless the case mode was chosen.
    fn occurrence_case_insensitive(&self, text: &str) -> bool {
        if self.search_session.case_chosen {
            self.search_session.case_insensitive
        } else {
            SearchSession::smart_case_insensitive(text)
        }
    }

    fn occurrence_from_word(&self) -> bool {
        let selected = self.selected_range();
        selected.is_empty() || self.search_session.occurrence_word.as_ref().is_some_and(
            |(range, revision)| *range == selected && *revision == self.document_revision,
        )
    }

    /// Whether Select all matches has a query or word to select.
    pub fn can_select_all_search_matches(&self) -> bool {
        if self.disabled || !self.is_multi_line() {
            return false;
        }
        if self.search_session.is_active() && !self.search_session.query.is_empty() {
            !self.search_session.matcher.is_empty()
        } else {
            self.selection_or_word().is_some()
        }
    }

    /// Select every result of the active search, or occurrences of the selection
    /// or caret's word using stored options when no search is active. Closes Find.
    pub fn select_all_search_matches(&mut self, cx: &mut Context<Self>) -> bool {
        if self.disabled || !self.is_multi_line() {
            return false;
        }
        let active = self.search_session.is_active() && !self.search_session.query.is_empty();
        let from_word = !active && self.occurrence_from_word();
        let ranges = if active {
            self.sync_search_matcher();
            self.search_session.matcher.matched_ranges()
        } else {
            let Some(range) = self.selection_or_word() else { return false };
            let mut matcher = SearchMatcher::new();
            let text = self.text.slice(range).to_string();
            matcher.update_query(
                &text,
                self.occurrence_case_insensitive(&text),
                self.search_session.whole_word || from_word,
            );
            matcher.update(&self.text);
            matcher.matched_ranges()
        };
        if ranges.is_empty() {
            return false;
        }
        // Bound caret construction and subsequent editing costs for common queries.
        let selections: Vec<_> = ranges.iter().take(10_000).map(|range| {
            let range = self.normalize_token_range(range.clone());
            let start = self.cursor_boundary(range.start, sum_tree::Bias::Left);
            let end = self.cursor_boundary(range.end, sum_tree::Bias::Right);
            CursorSelection::new(self.selections.generate_id(), start, end)
        }).collect();
        let target = selections[0].end;
        self.apply_occurrence_selections(selections, target, from_word, cx);
        true
    }

    fn next_occurrence_range(&self) -> Option<Range<usize>> {
        let selected = self.selected_range();
        if selected.is_empty() {
            return self.selection_or_word();
        }
        let mut matcher = SearchMatcher::new();
        let text = self.text.slice(selected).to_string();
        matcher.update_query(
            &text,
            self.occurrence_case_insensitive(&text),
            self.search_session.whole_word || self.occurrence_from_word(),
        );
        matcher.update(&self.text);
        let ranges = matcher.matched_ranges();
        let mut selected: Vec<Range<usize>> = self.selected_ranges().collect();
        selected.sort_unstable_by_key(|range| range.start);
        let after = selected.last().map_or(0, |range| range.end);
        let available = |range: &&Range<usize>| {
            let index = selected.partition_point(|selection| selection.end <= range.start);
            selected.get(index).is_none_or(|selection| selection.start >= range.end)
        };
        let split = ranges.partition_point(|range| range.start < after);
        ranges[split..].iter().chain(ranges[..split].iter()).find(available).cloned()
    }

    /// Cheap menu eligibility; the action checks whether a result remains.
    pub fn can_select_next_occurrence(&self) -> bool {
        !self.disabled && self.is_multi_line() && self.selection_or_word().is_some()
    }

    /// Select the caret's word first, otherwise add the next occurrence using
    /// stored options, wrapping and skipping already selected ranges.
    pub fn select_next_occurrence(&mut self, cx: &mut Context<Self>) -> bool {
        self.add_occurrence(false, cx)
    }

    /// Cheap menu eligibility for [`InputBaseState::skip_next_occurrence`]: there is a selected
    /// pick to drop.
    pub fn can_skip_next_occurrence(&self) -> bool {
        !self.selected_range().is_empty() && self.can_select_next_occurrence()
    }

    /// Drop the newest pick (the last selection) and add the next occurrence after it instead:
    /// the occurrence Cmd-D chose is not wanted, the one after it is. The earlier picks stay.
    /// With only a caret there is no pick, so it selects the word as Cmd-D does. False when no
    /// other occurrence remains, and then nothing changes.
    pub fn skip_next_occurrence(&mut self, cx: &mut Context<Self>) -> bool {
        self.add_occurrence(true, cx)
    }

    fn add_occurrence(&mut self, skip: bool, cx: &mut Context<Self>) -> bool {
        if self.disabled || !self.is_multi_line() {
            return false;
        }
        let from_word = self.occurrence_from_word();
        // Found with the skipped pick still selected, so the search starts after it.
        let Some(range) = self.next_occurrence_range() else { return false };
        let range = self.normalize_token_range(range);
        let start = self.cursor_boundary(range.start, sum_tree::Bias::Left);
        let end = self.cursor_boundary(range.end, sum_tree::Bias::Right);
        let mut selections = if self.selected_range().is_empty() {
            Vec::new()
        } else {
            self.selections.iter().copied().collect()
        };
        if skip {
            selections.pop();
        }
        selections.push(CursorSelection::new(self.selections.generate_id(), start, end));
        self.apply_occurrence_selections(selections, end, from_word, cx);
        true
    }

    fn apply_occurrence_selections(
        &mut self,
        selections: Vec<CursorSelection>,
        target: usize,
        from_word: bool,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.selected_word_range = None;
        self.cursor_line_end_affinity = false;
        self.pause_blink_cursor(cx);
        M::hide_context_menu(self, cx);
        M::clear_inline_completion(self, cx);
        // A caret never sits on a hidden line: open the folds the new selections land in.
        for sel in &selections {
            for offset in [sel.start, sel.end] {
                let row = self.text.offset_to_point(offset).row;
                self.unfold_line(row);
            }
        }
        self.selections.replace_all(selections);
        self.search_session.occurrence_word =
            from_word.then(|| (self.selected_range(), self.document_revision));
        self.close_search(cx);
        self.scroll_to(target, None, cx);
    }

    pub fn next_search_match(&mut self, cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.sync_search_matcher();
        let Some(range) = self.search_session.matcher.next() else {
            self.count_search_miss(cx);
            return None;
        };
        // Match order does not describe viewport direction after a manual
        // scroll. Always allow search navigation to reveal the active match.
        self.scroll_to_with_padding(range.end, None, ScrollPadding::SurroundingLines, cx);
        self.start_search_halo(range.clone(), cx);
        Some(range)
    }

    pub fn previous_search_match(&mut self, cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.sync_search_matcher();
        let Some(range) = self.search_session.matcher.next_back() else {
            self.count_search_miss(cx);
            return None;
        };
        // Match order does not describe viewport direction after a manual
        // scroll. Always allow search navigation to reveal the active match.
        self.scroll_to_with_padding(range.start, None, ScrollPadding::SurroundingLines, cx);
        self.start_search_halo(range.clone(), cx);
        Some(range)
    }

    /// A step with a query and no match to step to: counted so a view can answer it. An empty
    /// query is no miss.
    fn count_search_miss(&mut self, cx: &mut Context<Self>) {
        if !self.search_session.query.is_empty() {
            self.search_session.misses = self.search_session.misses.wrapping_add(1);
            cx.notify();
        }
    }

    /// Replace the current match. With `jump` the view moves on to the next match (the find bar's
    /// Replace button, Return in the replace field, Replace and find); without it the view stays
    /// where it is (Edit > Find > Replace). The next match is the current one either way. Returns
    /// whether there was a match to replace.
    pub fn replace_current_search_match(
        &mut self,
        replacement: &str,
        jump: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_replaceable() || self.edit_blocked(window, cx) {
            return false;
        }
        self.sync_search_matcher();
        let matcher = &mut self.search_session.matcher;
        let Some(range) = matcher
            .matched_ranges()
            .get(matcher.current_match_index())
            .cloned()
        else {
            return false;
        };
        let replacement = matcher.replacement_for(&self.text, &range, replacement).into_owned();
        let next = matcher.peek().unwrap_or_else(|| range.clone());
        let direction = matcher
            .has_next_without_wrap()
            .then_some(MoveDirection::Down);
        matcher.begin_replacement_at(range.start + replacement.len());
        let range_utf16 = self.range_to_utf16(&range);
        if jump {
            self.scroll_to(next.end, direction, cx);
        }
        self.replace_text_in_range_silent(Some(range_utf16), &replacement, window, cx);
        true
    }

    /// Replace every match as one undo step, keeping each cursor on the same
    /// text and the scroll where it was. Returns how many were replaced.
    pub fn replace_all_search_matches(
        &mut self,
        replacement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        if !self.is_replaceable() || self.edit_blocked(window, cx) {
            return 0;
        }
        self.sync_search_matcher();
        let ranges = self.search_session.matcher.matched_ranges();
        if ranges.is_empty() {
            return 0;
        }
        let count = ranges.len();
        let edits: Vec<(Range<usize>, String)> = {
            let replacements = self.search_session.matcher.replacements_for(&self.text, &ranges, replacement);
            ranges.iter().cloned().zip(replacements).collect()
        };
        let before: Vec<_> = self.selections.iter().copied().collect();
        let mapped: Vec<_> = before
            .iter()
            .map(|cursor| {
                let mut cursor = *cursor;
                cursor.start = map_through_replacements(&edits, cursor.start);
                cursor.end = map_through_replacements(&edits, cursor.end);
                cursor.column_anchor = None;
                cursor
            })
            .collect();

        self.search_session.matcher.begin_replacement();
        self.undo_manager.begin_transaction();
        self.undo_manager
            .record_selections(before.clone(), before.clone());
        self.replace_text_in_ranges(&edits, window, cx);
        self.selections.replace_all(mapped);
        self.selections.merge_overlapping();
        let after = self.selections.iter().copied().collect();
        self.undo_manager.record_selections(before, after);
        self.undo_manager.commit_transaction();
        self.update_preferred_column();
        cx.notify();
        count
    }

    /// Keep the matches in step with an edit. A closed search skips the scan:
    /// it searches the whole document, and nothing reads the matches until the
    /// search is resumed or navigated, which sync first.
    pub(super) fn update_search(&mut self, _cx: &mut gpui::App) {
        self.search_session.matcher.invalidate();
        if self.undo_manager.is_ignoring() {
            return;
        }
        // A hidden tab's reload only marks the matches out of date: a scan here would build the
        // whole list again, and the one on show finds what changed.
        if self.search_session.matcher.is_released() {
            return;
        }
        if !self.search_session.is_active() {
            self.search_session.matcher.forget_text();
            return;
        }
        self.sync_search_matcher();
    }

    /// Mark the matches stale after the text changed. Cheap: the scan runs
    /// when the matches are next read.
    pub(super) fn invalidate_search(&mut self) {
        self.search_session.matcher.invalidate();
    }

    /// Recompute the matches if the query or the text changed since the last
    /// scan.
    fn sync_search_matcher(&mut self) {
        self.search_session.matcher.refresh(&self.text);
    }

    /// An input that is not `searchable` leaves the shortcut to its
    /// ancestors, so a custom search UI can take it.
    pub(super) fn on_action_search(&mut self, _: &Search, _: &mut Window, cx: &mut Context<Self>) {
        if !self.searchable {
            cx.propagate();
            return;
        }
        self.open_search(false, cx);
    }

    pub(super) fn on_action_replace(
        &mut self,
        _: &Replace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.searchable {
            cx.propagate();
            return;
        }
        self.open_search(true, cx);
    }
}

impl Default for SearchMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchMatcher {
    pub fn new() -> Self {
        Self {
            pattern: None,
            whole_word: false,
            regex: false,
            invalid: None,
            stale: false,
            matched_ranges: Arc::new(Vec::new()),
            current_match_ix: 0,
            replacing: false,
            resume_at: None,
            scanned: None,
            edit_span: None,
            scope: None,
            released_len: None,
            scan_gen: 0,
            scanning: false,
            query: String::new(),
            case_insensitive: false,
            ignore_diacritics: false,
            fold: None,
            found: None,
        }
    }

    /// Scan `text` for the query and recompute the matches. The matcher keeps
    /// no copy of the text; the caller passes its current rope in.
    pub fn update(&mut self, text: &Rope) {
        self.finish_release();
        self.stale = false;
        self.update_matches(text);
    }

    /// Scan again only when the query or the text changed since the last scan
    /// (see [`SearchMatcher::invalidate`]); otherwise keep the current match.
    pub fn refresh(&mut self, text: &Rope) {
        if self.stale {
            self.update(text);
        } else {
            self.replacing = false;
        }
    }

    /// The text changed: the next [`SearchMatcher::refresh`] scans again.
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    /// Let go of the text the last scan saw; the next scan is a full one.
    pub fn forget_text(&mut self) {
        self.scanned = None;
        self.edit_span = None;
        self.found = None;
    }

    /// Drop the matches and mark them stale, keeping the query and the current
    /// index, so the next scan finds the same matches and resumes at the same
    /// one.
    pub fn release(&mut self) {
        // Released twice before a scan landed: the first count is the one the bar showed.
        self.released_len = Some(self.released_len.unwrap_or_else(|| self.len()));
        self.scan_gen += 1;
        self.found = None;
        self.scanning = false;
        self.matched_ranges = Arc::new(Vec::new());
        self.scanned = None;
        self.edit_span = None;
        self.stale = true;
        self.replacing = true;
        self.resume_at = None;
    }

    /// A scan, or a new query, ends the release: the matches are real again, and a scan still
    /// running for the old release is dropped.
    fn finish_release(&mut self) {
        self.released_len = None;
        self.scan_gen += 1;
        self.scanning = false;
    }

    /// Set a literal query. The matches are stale until the next
    /// [`SearchMatcher::update`] or [`SearchMatcher::refresh`].
    pub fn update_query(&mut self, query: &str, case_insensitive: bool, whole_word: bool) {
        self.update_pattern(query, case_insensitive, whole_word, false);
    }

    /// Turn the Ignore diacritics option on or off. The matches are stale until the next scan.
    pub fn set_ignore_diacritics(&mut self, on: bool) {
        if self.ignore_diacritics == on {
            return;
        }
        self.ignore_diacritics = on;
        self.found = None;
        let query = std::mem::take(&mut self.query);
        self.update_pattern(&query, self.case_insensitive, self.whole_word, self.regex);
    }

    /// Set the query, read as a regular expression when `regex` is set. A pattern that does not
    /// compile matches nothing and reads true from [`SearchMatcher::invalid`]. The matches are
    /// stale until the next [`SearchMatcher::update`] or [`SearchMatcher::refresh`].
    pub fn update_pattern(&mut self, query: &str, case_insensitive: bool, whole_word: bool, regex: bool) {
        self.finish_release();
        self.whole_word = whole_word;
        self.regex = regex;
        self.query.clear();
        self.query.push_str(query);
        self.case_insensitive = case_insensitive;
        self.invalid = None;
        self.fold = None;
        self.stale = true;
        self.replacing = false;
        self.scanned = None;
        self.edit_span = None;
        self.pattern = if query.is_empty() {
            None
        } else if regex {
            // `^` and `$` match at every line, and a line may end in `\r\n`.
            let build = |query: &str| {
                regex::RegexBuilder::new(query)
                    .case_insensitive(case_insensitive)
                    .multi_line(true)
                    .crlf(true)
                    .build()
            };
            match build(query) {
                Ok(raw) => {
                    // Whole word wraps the pattern in word boundaries, so an alternation such as
                    // `int|integer` still finds `integer`; a pattern that breaks when wrapped
                    // keeps its own form and the word check after the match.
                    let regex = if whole_word {
                        build(&format!(r"\b(?:{query})\b")).unwrap_or(raw)
                    } else {
                        raw
                    };
                    // Rows are read one line at a time unless the pattern names a line break
                    // (`\n` or `\r`, escaped or typed); then a match may run on, as far as the
                    // pattern's longest match goes (every following row when it has no limit).
                    let multiline = query.contains(['\n', '\r']) || query.contains("\\n") || query.contains("\\r");
                    let rows = if multiline { longest_match(query, case_insensitive) } else { 0 };
                    Some(Pattern::Regex {
                        regex,
                        multiline,
                        rows,
                        borderless: false,
                    })
                }
                Err(error) => {
                    self.invalid = Some(regex_problem(query, &error));
                    None
                }
            }
        } else if let Some((pattern, fold)) = self.ignore_diacritics.then(|| diacritic_pattern(query, case_insensitive)).flatten() {
            self.fold = fold;
            Some(pattern)
        } else if let Some(regex) = case_insensitive
            .then_some(query)
            .filter(|query| !query.is_ascii() && query.len() <= FOLDED_QUERY_BYTES)
            .and_then(|query| {
                // A long query is slow to compile (and can pass the regex size limit): past
                // `FOLDED_QUERY_BYTES`, or when it does not build, it reads as a plain string.
                regex::RegexBuilder::new(&regex::escape(query)).case_insensitive(true).build().ok()
            })
        {
            Some(Pattern::Regex {
                regex,
                multiline: query.contains(['\n', '\r']),
                rows: query.matches('\n').count(),
                borderless: borderless_folded(&query.chars().collect::<Vec<_>>()),
            })
        } else {
            let bytes = if case_insensitive { query.to_ascii_lowercase() } else { query.to_string() };
            Some(Pattern::Literal {
                finder: AhoCorasick::builder()
                    .ascii_case_insensitive(case_insensitive)
                    .build([query])
                    .expect("failed to build input search query"),
                len: query.len(),
                borderless: borderless(bytes.as_bytes()),
            })
        };
    }

    /// The query is a regular expression that does not compile.
    pub fn invalid(&self) -> bool {
        self.invalid.is_some()
    }

    /// What is wrong with a regular expression that does not compile, in a few plain words.
    pub fn problem(&self) -> Option<&'static str> {
        self.invalid
    }

    /// The text that replaces the match at `range`: `replacement`, under the Regex option with
    /// `\n`, `\t` and `\\` read as a newline, a tab and a backslash and with `$1` and `${name}`
    /// expanded from the match's groups (`$$` is a dollar sign). A plain replacement is literal.
    pub fn replacement_for<'a>(&self, text: &Rope, range: &Range<usize>, replacement: &'a str) -> Cow<'a, str> {
        let replacement = self.unescaped(replacement);
        let Some(regex) = self.expanding(&replacement) else {
            return replacement;
        };
        let (base, haystack) = Self::lines_of(text, range);
        let expanded = Self::expand(regex, &haystack, base, range, &Self::braced(&replacement));
        match expanded {
            Some(expanded) => Cow::Owned(expanded),
            None => replacement,
        }
    }

    /// [`SearchMatcher::replacement_for`] for every match of `ranges` (ascending), reading each
    /// row's text once however many matches it holds.
    pub fn replacements_for(&self, text: &Rope, ranges: &[Range<usize>], replacement: &str) -> Vec<String> {
        let replacement = self.unescaped(replacement);
        let Some(regex) = self.expanding(&replacement) else {
            return ranges.iter().map(|_| replacement.to_string()).collect();
        };
        let braced = Self::braced(&replacement);
        let mut rows = (usize::MAX, usize::MAX);
        let (mut base, mut haystack) = (0, String::new());
        ranges
            .iter()
            .map(|range| {
                let span = (text.offset_to_point(range.start).row, text.offset_to_point(range.end).row);
                if span != rows {
                    rows = span;
                    (base, haystack) = Self::lines_of(text, range);
                }
                Self::expand(regex, &haystack, base, range, &braced).unwrap_or_else(|| replacement.to_string())
            })
            .collect()
    }

    /// `replacement` with `\n`, `\t` and `\\` read as a newline, a tab and one backslash when the
    /// Regex option is on, left to right so `\\n` is a backslash and an `n`; any other backslash
    /// stays. Done before `$` groups expand, so text a group brings in is never read this way.
    fn unescaped<'a>(&self, replacement: &'a str) -> Cow<'a, str> {
        if !self.regex || !replacement.contains('\\') {
            return Cow::Borrowed(replacement);
        }
        let mut out = String::with_capacity(replacement.len());
        let mut chars = replacement.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.clone().next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                _ => {
                    out.push('\\');
                    continue;
                }
            }
            chars.next();
        }
        Cow::Owned(out)
    }

    /// The regex that expands `replacement`, when the Regex option is on and `replacement` holds a `$`.
    fn expanding(&self, replacement: &str) -> Option<&regex::Regex> {
        match &self.pattern {
            Some(Pattern::Regex { regex, .. }) if self.regex && replacement.contains('$') => Some(regex),
            _ => None,
        }
    }

    /// The lines `range` sits on and where they start, so `^`, `$` and the groups read what the
    /// scan read.
    fn lines_of(text: &Rope, range: &Range<usize>) -> (usize, String) {
        let base = text.line_start_offset(text.offset_to_point(range.start).row);
        let end = text.line_end_offset(text.offset_to_point(range.end).row);
        (base, text.slice(base..end).to_string())
    }

    /// `replacement` with each `$` and digits read as that group, so `$1_old` is group 1 then
    /// `_old` (the regex crate would read a group named `1_old`). `$$` and `${..}` stay as they are.
    fn braced(replacement: &str) -> Cow<'_, str> {
        let bytes = replacement.as_bytes();
        let mut out = String::new();
        let (mut copied, mut at) = (0, 0);
        while at < bytes.len() {
            if bytes[at] != b'$' {
                at += 1;
                continue;
            }
            match bytes.get(at + 1) {
                Some(b'$') => at += 2,
                Some(digit) if digit.is_ascii_digit() => {
                    let end = (at + 1..bytes.len()).find(|&i| !bytes[i].is_ascii_digit()).unwrap_or(bytes.len());
                    out.push_str(&replacement[copied..at + 1]);
                    out.push('{');
                    out.push_str(&replacement[at + 1..end]);
                    out.push('}');
                    copied = end;
                    at = end;
                }
                _ => at += 1,
            }
        }
        if copied == 0 {
            return Cow::Borrowed(replacement);
        }
        out.push_str(&replacement[copied..]);
        Cow::Owned(out)
    }

    /// `replacement` expanded from the groups of the match at `range`, `None` when the match
    /// cannot be read again from its lines.
    fn expand(regex: &regex::Regex, haystack: &str, base: usize, range: &Range<usize>, replacement: &str) -> Option<String> {
        let at = range.start - base..range.end - base;
        let groups = regex
            .captures_at(haystack, at.start)
            .filter(|groups| groups.get(0).is_some_and(|whole| whole.range() == at))?;
        let mut expanded = String::new();
        groups.expand(replacement, &mut expanded);
        Some(expanded)
    }

    pub fn matched_ranges(&self) -> Arc<Vec<Range<usize>>> {
        self.matched_ranges.clone()
    }

    pub fn current_match_index(&self) -> usize {
        self.current_match_ix
    }

    /// The index of the current match into [`SearchMatcher::matched_ranges`],
    /// `None` while there is no match.
    pub fn current(&self) -> Option<usize> {
        (!self.is_empty()).then_some(self.current_match_ix)
    }

    /// The ranges the search is limited to (Find in selection), sorted and apart, `None` for the
    /// whole text. An empty slice limits it to nothing.
    pub fn scope(&self) -> Option<&[Range<usize>]> {
        self.scope.as_ref().map(|scope| scope.ranges.as_slice())
    }

    pub(crate) fn shared_scope(&self) -> Option<&Arc<Vec<Range<usize>>>> {
        self.scope.as_ref().map(|scope| &scope.ranges)
    }

    /// Record one applied edit in current-text coordinates. Several edits compose to a
    /// conservative span; unchanged text outside it never needs a rope comparison.
    pub(crate) fn record_edit(&mut self, range: &Range<usize>, new_len: usize) {
        self.scope_apply(range, new_len);
        if self.scanned.is_none() {
            return;
        }
        self.edit_span = Some(match self.edit_span.take() {
            None => (range.clone(), range.start..range.start + new_len),
            Some((old, new)) => {
                let start = old.start.min(range.start);
                let old_end = old.end + range.end.saturating_sub(new.end);
                let new_end = range.start + new_len + new.end.saturating_sub(range.end);
                (start..old_end, start..new_end)
            }
        });
    }

    /// Lift the limit without a text to place it in.
    pub fn clear_scope(&mut self) {
        if self.scope.take().is_some() {
            self.stale = true;
            self.scanned = None;
            self.found = None;
        }
    }

    /// An edit that replaced `old` with `new_len` bytes, applied to the scope on its own: a batch
    /// of edits read as one span would stretch a scope of several ranges over the text between them.
    pub fn scope_apply(&mut self, old: &Range<usize>, new_len: usize) {
        if let Some(scope) = &mut self.scope {
            scope.apply(old, new_len);
            scope.text = None;
            self.stale = true;
        }
    }

    /// Limit the search to `ranges` of `text` (`None` lifts the limit). The matches are stale until
    /// the next scan, which is a full one.
    pub fn set_scope(&mut self, ranges: Option<Vec<Range<usize>>>, text: &Rope) {
        self.found = None;
        self.scope = ranges.map(|mut ranges| {
            ranges.retain(|range| !range.is_empty());
            ranges.sort_unstable_by_key(|range| range.start);
            let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
            for range in ranges {
                match merged.last_mut() {
                    Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                    _ => merged.push(range),
                }
            }
            Scope { ranges: Arc::new(merged), text: Some(text.clone()) }
        });
        self.stale = true;
        self.replacing = false;
        self.scanned = None;
    }

    pub fn len(&self) -> usize {
        self.matched_ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matched_ranges.is_empty()
    }

    /// The matches were let go and no scan has landed yet: [`SearchMatcher::shown_total`] and
    /// [`SearchMatcher::shown_current`] still give what the matches were.
    pub fn is_released(&self) -> bool {
        self.released_len.is_some()
    }

    /// How many matches the bar shows: the count before a release until the scan lands.
    pub fn shown_total(&self) -> usize {
        self.released_len.unwrap_or_else(|| self.len())
    }

    /// The current match as the bar shows it: kept through a release that had matches.
    pub fn shown_current(&self) -> Option<usize> {
        match self.released_len {
            Some(len) => (len > 0).then_some(self.current_match_ix),
            None => self.current(),
        }
    }

    /// A background scan for a released matcher is under way.
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    /// Start a background scan: the matcher to scan on another thread and the number its result
    /// must still carry to be adopted.
    pub fn begin_scan(&mut self) -> (Self, u64) {
        self.scanning = true;
        (self.clone(), self.scan_gen)
    }

    /// Take a finished background scan, unless the matcher was scanned, released again or given
    /// a new query since it began. `moved`: the text changed while it ran; the scan is taken
    /// stale, so the next one only rescans what the edits touched, but a scope limited to
    /// ranges the edits moved is scanned again from scratch.
    pub fn adopt_scan(&mut self, scanned: Self, scan_gen: u64, moved: bool) -> Landing {
        if self.scan_gen != scan_gen || self.released_len.is_none() {
            return Landing::Dropped;
        }
        if moved && self.scope.is_some() {
            self.scanning = false;
            return Landing::Again;
        }
        *self = scanned;
        self.stale |= moved;
        Landing::Adopted
    }

    /// `2/5`: the current match and the total, `0/0` without matches.
    pub fn label(&self) -> String {
        match self.current() {
            Some(ix) => format!("{}/{}", ix + 1, self.len()),
            None => "0/0".into(),
        }
    }

    fn peek(&self) -> Option<Range<usize>> {
        self.next_index()
            .and_then(|ix| self.matched_ranges.get(ix).cloned())
    }

    fn has_next_without_wrap(&self) -> bool {
        self.current_match_ix < self.matched_ranges.len().saturating_sub(1)
    }

    /// Make the first match that ends at or after `offset` the current one (the last match when
    /// none does), by binary search: the matches are sorted and never overlap.
    pub fn update_cursor_by_offset(&mut self, offset: usize) {
        if self.matched_ranges.is_empty() {
            return;
        }
        let ix = self.matched_ranges.partition_point(|range| range.end < offset);
        self.current_match_ix = ix.min(self.matched_ranges.len() - 1);
    }

    /// Preserve the current logical match while a replacement mutates text.
    fn begin_replacement(&mut self) {
        self.replacing = true;
        self.resume_at = None;
    }

    /// [`SearchMatcher::begin_replacement`] for one match replaced: the next scan makes the first
    /// match at or after `at` (past the new text) the current one, so a replacement that holds the
    /// query does not leave the current match inside what was just put in.
    fn begin_replacement_at(&mut self, at: usize) {
        self.replacing = true;
        self.resume_at = Some(at);
    }

    #[cfg(test)]
    fn set_current_match_index(&mut self, index: usize) {
        self.current_match_ix = index.min(self.matched_ranges.len().saturating_sub(1));
    }

    fn next_index(&self) -> Option<usize> {
        if self.is_empty() {
            None
        } else if self.has_next_without_wrap() {
            Some(self.current_match_ix + 1)
        } else {
            Some(0)
        }
    }

    fn update_matches(&mut self, text: &Rope) {
        if let Some(scope) = &mut self.scope {
            scope.follow(text);
        }
        let narrowed = if self.scope.is_none() { self.narrow(text) } else { None };
        let mut ranges = narrowed
            .or_else(|| self.scanned.as_ref().and_then(|old| self.find_changed(old, text)))
            .unwrap_or_else(|| self.find_all(text));
        if let Some(scope) = &self.scope {
            ranges.retain(|range| scope.holds(range));
        }
        self.scanned = Some(text.clone());
        self.edit_span = None;
        self.matched_ranges = Arc::new(ranges);
        self.found = self.narrowable().then(|| Found {
            query: self.fold.as_ref().map_or_else(|| self.query.clone(), |fold| fold.key.clone()),
            case_insensitive: self.case_insensitive,
            text: text.clone(),
        });
        match self.resume_at.take() {
            _ if self.is_empty() || !self.replacing => self.current_match_ix = 0,
            // The first match past the text a Replace put in, wrapping to the first one.
            Some(at) => {
                let ix = self.matched_ranges.partition_point(|range| range.start < at);
                self.current_match_ix = if ix < self.len() { ix } else { 0 };
            }
            None => self.current_match_ix = self.current_match_ix.min(self.len() - 1),
        }
        self.replacing = false;
    }

    /// Whether the matches hold every occurrence of the query, so a longer query can start from
    /// them: a plain literal, no whole word, no scope, and none of its occurrences overlap.
    fn narrowable(&self) -> bool {
        !self.regex
            && !self.whole_word
            && self.scope.is_none()
            && match (&self.pattern, &self.fold) {
                (Some(Pattern::Literal { borderless: true, .. }), None) => true,
                (Some(Pattern::Regex { borderless: true, .. }), Some(_)) => true,
                _ => false,
            }
    }

    /// The matches of the query when it extends the one `found` answers, with the same case mode,
    /// in the same text: each earlier match that is still a match at its start with the longer
    /// length. Both queries are borderless, so every occurrence of the longer query starts at an
    /// occurrence of the shorter one, and the pick is what a scan of the text would find. `None`
    /// when any of that fails (a regex, whole word, Find in selection, a case flip, an edit).
    fn narrow(&self, text: &Rope) -> Option<Vec<Range<usize>>> {
        let found = self.found.as_ref()?;
        let key = self.fold.as_ref().map_or(self.query.as_str(), |fold| fold.key.as_str());
        if !self.narrowable()
            || found.case_insensitive != self.case_insensitive
            || key.len() <= found.query.len()
            || !key.starts_with(&found.query)
            || found.text.len() != text.len()
            || common_ends(&found.text, text).0 != text.len()
        {
            return None;
        }
        if let Some(fold) = &self.fold {
            // Each earlier match's start is read again with the longer pattern. Where the key is
            // ASCII and the text from the start through one byte past the key is too, no accent
            // can be involved and the bytes are compared as a literal is; anything else (an accent,
            // a combining mark that may follow) is read by the pattern over the text that can hold
            // the longest match.
            let (bytes, ascii) = (key.as_bytes(), key.is_ascii());
            let need = bytes.len() + 1;
            let mut out = Vec::new();
            let (mut window, mut head) = (String::new(), Vec::with_capacity(need));
            let (mut chunk, mut chunk_start): (&str, usize) = ("", 0);
            for old in self.matched_ranges.iter() {
                head.clear();
                if ascii {
                    let mut at = old.start;
                    while head.len() < need && at < text.len() {
                        if at < chunk_start || at >= chunk_start + chunk.len() {
                            let (mut chunks, start) = text.chunks_at(at);
                            chunk = chunks.next().unwrap_or("");
                            chunk_start = start;
                            if chunk.is_empty() {
                                break;
                            }
                        }
                        let from = at - chunk_start;
                        let take = (need - head.len()).min(chunk.len() - from);
                        head.extend_from_slice(&chunk.as_bytes()[from..from + take]);
                        at += take;
                    }
                    if head.is_ascii() {
                        if head.len() >= bytes.len() {
                            let seen = &head[..bytes.len()];
                            if if self.case_insensitive { seen.eq_ignore_ascii_case(bytes) } else { seen == bytes } {
                                out.push(old.start..old.start + bytes.len());
                            }
                        }
                        continue;
                    }
                }
                // The most a match can reach: through the key's letters, each with its marks, so up
                // to just before the next character the marks don't cover.
                let (mut end, mut seen) = (old.start, 0);
                let mut chars = text.chars_at(old.start);
                for c in chars.by_ref() {
                    if !is_mark(c) {
                        if seen == fold.letters {
                            break;
                        }
                        seen += 1;
                    }
                    end += c.len_utf8();
                }
                window.clear();
                text.slice(old.start..end).chunks().for_each(|chunk| window.push_str(chunk));
                if let Some(found) = fold.anchored.find(&window) {
                    out.push(old.start..old.start + found.end());
                }
            }
            out.shrink_to_fit();
            return Some(out);
        }
        let query = self.query.as_bytes();
        let fold = self.case_insensitive;
        let mut out = Vec::new();
        let (mut chunk, mut chunk_start): (&str, usize) = ("", 0);
        'matches: for old in self.matched_ranges.iter() {
            let (mut at, mut done) = (old.start, 0);
            if at + query.len() > text.len() {
                // Sorted: no later match fits either.
                break;
            }
            while done < query.len() {
                if at < chunk_start || at >= chunk_start + chunk.len() {
                    let (mut chunks, start) = text.chunks_at(at);
                    chunk = chunks.next()?;
                    chunk_start = start;
                }
                let from = at - chunk_start;
                let take = (query.len() - done).min(chunk.len() - from);
                let (seen, wanted) = (&chunk.as_bytes()[from..from + take], &query[done..done + take]);
                if if fold { !seen.eq_ignore_ascii_case(wanted) } else { seen != wanted } {
                    continue 'matches;
                }
                at += take;
                done += take;
            }
            out.push(old.start..old.start + query.len());
        }
        out.shrink_to_fit();
        Some(out)
    }

    /// Every match of the query in `text`, streamed over the rope's chunks
    /// (literal) or lines (regex) without materializing the document.
    fn find_all(&self, text: &Rope) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        match &self.pattern {
            None => {}
            Some(Pattern::Literal { finder, .. }) => {
                self.scan_literal(finder, text, 0..text.len(), &mut ranges);
            }
            Some(Pattern::Regex { regex, multiline, rows, .. }) => {
                if *multiline {
                    self.scan_blocks(regex, *rows, text, 0..text.lines_len(), &mut ranges);
                } else {
                    self.scan_rows(regex, text, 0..text.lines_len(), &mut ranges);
                }
            }
        }
        ranges.shrink_to_fit();
        ranges
    }

    /// The matches of `text` given those of `old`, the text the current matches were found in:
    /// matches clear of the edit are kept (shifted past it) and only the text around it is scanned.
    /// `None` where a match could reach across the edit unseen (a query that overlaps itself, or a
    /// regex across lines), which wants a full scan.
    fn find_changed(&self, old: &Rope, text: &Rope) -> Option<Vec<Range<usize>>> {
        let before = &*self.matched_ranges;
        let (prefix, old_end, new_end) = if let Some((old, new)) = &self.edit_span {
            (old.start, old.end, new.end)
        } else {
            let (prefix, suffix) = common_ends(old, text);
            (prefix, old.len() - suffix, text.len() - suffix)
        };
        let mut found = Vec::new();
        let (left, right) = match self.pattern.as_ref()? {
            Pattern::Literal { finder, len, borderless: true } => {
                // A match is kept when it and the character beside it lie wholly outside the edit.
                let left = before.partition_point(|range| range.end + REACH <= prefix);
                let right = before.partition_point(|range| range.start < old_end + REACH);
                let from = floor_boundary(text, prefix.saturating_sub(REACH + len));
                let to = ceil_boundary(text, new_end + REACH + len);
                self.scan_literal(finder, text, from..to, &mut found);
                found.retain(|range| range.end + REACH > prefix && range.start < new_end + REACH);
                (left, right)
            }
            Pattern::Regex { regex, multiline: false, .. } => {
                // Whole lines, from the edit's first line to the end of its last.
                let first = text.offset_to_point(prefix).row;
                let last = text.offset_to_point(new_end).row;
                let line_end = text.line_end_offset(last) + old_end - new_end;
                let first_start = text.line_start_offset(first);
                let left = before.partition_point(|range| range.start < first_start);
                let right = before.partition_point(|range| range.start <= line_end);
                self.scan_rows(regex, text, first..last + 1, &mut found);
                (left, right)
            }
            Pattern::Regex { regex, multiline: true, rows, borderless: true } => {
                // Whole lines again, from `rows` lines before the edit, where a match can begin and
                // still reach it, to the end of the edit's last line.
                let first = text.offset_to_point(prefix).row.saturating_sub(*rows);
                let last = text.offset_to_point(new_end).row;
                let line_end = text.line_end_offset(last) + old_end - new_end;
                let first_start = text.line_start_offset(first);
                let left = before.partition_point(|range| range.start < first_start);
                let right = before.partition_point(|range| range.start <= line_end);
                self.scan_blocks(regex, *rows, text, first..last + 1, &mut found);
                (left, right)
            }
            _ => return None,
        };
        let mut ranges = Vec::with_capacity(left + found.len() + before.len() - right);
        ranges.extend_from_slice(&before[..left]);
        ranges.extend(found);
        ranges.extend(before[right..].iter().map(|range| {
            range.start + new_end - old_end..range.end + new_end - old_end
        }));
        Some(ranges)
    }

    /// The literal matches inside `window`, which sits on character boundaries.
    fn scan_literal(&self, finder: &AhoCorasick, text: &Rope, window: Range<usize>, out: &mut Vec<Range<usize>>) {
        let base = window.start;
        let reader = ChunkReader::new(text.slice(window).chunks());
        for result in finder.stream_find_iter(reader) {
            let found = result.expect("input search match").range();
            let range = base + found.start..base + found.end;
            if self.accepts(text, &range) {
                out.push(range);
            }
        }
    }

    /// The regex matches on the lines `rows`, line by line. A line is split on `\n` only, so it
    /// starts one byte after the previous one ends.
    fn scan_rows(&self, regex: &regex::Regex, text: &Rope, rows: Range<usize>, out: &mut Vec<Range<usize>>) {
        let mut start = text.line_start_offset(rows.start);
        for row in rows {
            let line = text.slice_line(row);
            let line_len = line.len();
            let haystack = match line.as_str() {
                Some(line) => Cow::Borrowed(line),
                None => Cow::Owned(line.to_string()),
            };
            for found in regex.find_iter(&haystack) {
                let range = start + found.start()..start + found.end();
                if !range.is_empty() && self.accepts(text, &range) {
                    out.push(range);
                }
            }
            start += line_len + 1;
        }
    }

    /// The regex matches that start on the rows `starts`, for a query that holds line breaks. The
    /// text goes through one buffer a block of rows at a time, with `reach` rows more after it for a
    /// match to run into; a pattern with no bound on its length reads the whole text into one
    /// buffer. A match that starts in one block and ends in the next is found once, and the next
    /// block resumes after it, as one scan would.
    fn scan_blocks(&self, regex: &regex::Regex, reach: usize, text: &Rope, starts: Range<usize>, out: &mut Vec<Range<usize>>) {
        // A few rows more than the query names, so a repeat such as `\n{3}` sees all it needs.
        let reach = reach.saturating_add(8);
        let lines = text.lines_len();
        // A pattern with no bound on its length (or a long one) reads the rest of the text from
        // wherever it starts, so one buffer holds all of it and each match is searched once.
        let whole = reach >= lines;
        let mut buffer = String::new();
        let (mut row, mut resume) = (starts.start, 0);
        while row < starts.end {
            // A match that ran on from an earlier block already covered the rows up to where it
            // ended; the search resumes there, inside this block's buffer.
            row = row.max(text.offset_to_point(resume).row);
            if row >= starts.end {
                break;
            }
            let last = if whole { starts.end } else { (row + BLOCK_ROWS).min(starts.end) } - 1;
            let base = text.line_start_offset(row);
            let mut tail = last.saturating_add(reach).saturating_add(1).min(lines);
            let mut stop = if tail < lines { text.line_start_offset(tail) } else { text.len() };
            buffer.clear();
            for chunk in text.slice(base..stop).chunks() {
                buffer.push_str(chunk);
            }
            let limit = text.line_end_offset(last);
            while let Some(found) = regex.find_at(&buffer, resume.max(base) - base) {
                if found.end() == buffer.len() && stop < text.len() && base + found.start() <= limit {
                    // The match runs to the buffer's end, so it may run on: read more rows, as
                    // many again as the buffer holds so the work stays linear, and look again.
                    tail = (tail + (tail - row).max(EXTEND_ROWS)).min(lines);
                    let grown = if tail < lines { text.line_start_offset(tail) } else { text.len() };
                    for chunk in text.slice(stop..grown).chunks() {
                        buffer.push_str(chunk);
                    }
                    stop = grown;
                    continue;
                }
                let range = base + found.start()..base + found.end();
                if range.start > limit {
                    break;
                }
                resume = range.end;
                if range.is_empty() {
                    // A pattern that can match nothing: step over one character.
                    let Some(next) = buffer[range.end - base..].chars().next() else { break };
                    resume = range.end + next.len_utf8();
                } else if self.accepts(text, &range) {
                    out.push(range);
                }
            }
            row = last + 1;
        }
    }

    /// Whether a found range passes the Whole Word option.
    fn accepts(&self, text: &Rope, range: &Range<usize>) -> bool {
        !self.whole_word || on_word_boundaries(text, range)
    }
}

/// How many bytes beside an edit a match's Whole Word check can read: one character.
const REACH: usize = 4;

/// How many rows a match that reaches the end of a block's buffer reads in at a time.
const EXTEND_ROWS: usize = 256;

/// How many rows a scan of text across lines copies into its buffer at a time.
const BLOCK_ROWS: usize = 4096;

/// The longest query, in bytes, matched ignoring the case of non-ASCII letters: the regex that
/// folds the case takes seconds to compile for a selection of tens of thousands of characters.
const FOLDED_QUERY_BYTES: usize = 2048;

/// The longest query, in characters, read with Ignore diacritics: each letter carries a mark class,
/// and the regex passes its size limit between 500 and 550 letters, so a longer query would silently
/// match as plain text. Measured with `é`, `o` and `ố`.
const FOLDED_LETTERS: usize = 400;

/// The query as a regex that reads each letter as itself or any accented form of it, each followed
/// by any combining marks (`e` then U+0301 is `é` too). `None` for a query of more than
/// `FOLDED_LETTERS` letters, one with nothing but combining marks, or one that does not build: it
/// reads as plain text.
fn diacritic_pattern(query: &str, case_insensitive: bool) -> Option<(Pattern, Option<Fold>)> {
    if query.chars().take(FOLDED_LETTERS + 1).count() > FOLDED_LETTERS {
        return None;
    }
    let (bases, forms) = &*FOLD_TABLES;
    let mut source = String::with_capacity(query.len() * 8);
    let mut letters = false;
    for c in query.chars().filter(|&c| !is_combining_mark(c)) {
        letters = true;
        let base = bases.get(&c).copied().unwrap_or(c);
        match forms.get(&base) {
            Some(accented) => {
                source.push('[');
                source.push(base);
                source.push_str(accented);
                source.push(']');
            }
            None => source.push_str(&regex::escape(base.encode_utf8(&mut [0; 4]))),
        }
        if c.is_alphabetic() {
            // Not case folded: U+0345 folds to a Greek iota, so `κα` would match `και`.
            source.push_str(r"(?-i:\p{Mn})*");
        }
    }
    if !letters {
        return None;
    }
    let regex = regex::RegexBuilder::new(&source).case_insensitive(case_insensitive).build().ok()?;
    let key: String = query.chars().filter(|&c| !is_combining_mark(c)).map(|c| bases.get(&c).copied().unwrap_or(c)).collect();
    let borderless = borderless_folded(&key.chars().collect::<Vec<_>>());
    // The same pattern held to the start of a window: a longer query reads each earlier match's
    // start with it (see `SearchMatcher::narrow`). Not needed when the matches can overlap.
    let anchored = if borderless {
        regex::RegexBuilder::new(&format!(r"\A(?:{source})")).case_insensitive(case_insensitive).build().ok()
    } else {
        None
    };
    let pattern = Pattern::Regex {
        regex,
        multiline: query.contains(['\n', '\r']),
        rows: query.matches('\n').count(),
        borderless,
    };
    let letters = key.chars().count();
    Some((pattern, anchored.map(|anchored| Fold { letters, key, anchored })))
}

/// A nonspacing mark, as the `\p{Mn}` of a folded pattern reads it.
fn is_mark(c: char) -> bool {
    static MARK: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\A\p{Mn}\z").expect("mark class"));
    MARK.is_match(c.encode_utf8(&mut [0; 4]))
}

/// A combining mark: it rides on the letter before it and is no letter of its own.
fn is_combining_mark(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}' | '\u{fe20}'..='\u{fe2f}')
}

type FoldTables = (std::collections::HashMap<char, char>, std::collections::HashMap<char, &'static str>);

/// The accent fold of the Latin, Greek and Cyrillic letters: each accented letter to its base, and
/// each base to the accented letters that fold to it.
static FOLD_TABLES: std::sync::LazyLock<FoldTables> = std::sync::LazyLock::new(|| {
    let mut bases = std::collections::HashMap::new();
    let mut forms = std::collections::HashMap::new();
    for &(base, accented) in ACCENTED {
        forms.insert(base, accented);
        for c in accented.chars() {
            bases.insert(c, base);
        }
    }
    (bases, forms)
});

/// Each base letter and the precomposed letters that decompose to it and combining marks only
/// (Unicode canonical decomposition, Latin to Cyrillic).
const ACCENTED: &[(char, &str)] = &[
    ('A', "ÀÁÂÃÄÅĀĂĄǍǞǠǺȀȂȦḀẠẢẤẦẨẪẬẮẰẲẴẶ"),
    ('B', "ḂḄḆ"),
    ('C', "ÇĆĈĊČḈ"),
    ('D', "ĎḊḌḎḐḒ"),
    ('E', "ÈÉÊËĒĔĖĘĚȄȆȨḔḖḘḚḜẸẺẼẾỀỂỄỆ"),
    ('F', "Ḟ"),
    ('G', "ĜĞĠĢǦǴḠ"),
    ('H', "ĤȞḢḤḦḨḪ"),
    ('I', "ÌÍÎÏĨĪĬĮİǏȈȊḬḮỈỊ"),
    ('J', "Ĵ"),
    ('K', "ĶǨḰḲḴ"),
    ('L', "ĹĻĽḶḸḺḼ"),
    ('M', "ḾṀṂ"),
    ('N', "ÑŃŅŇǸṄṆṈṊ"),
    ('O', "ÒÓÔÕÖŌŎŐƠǑǪǬȌȎȪȬȮȰṌṎṐṒỌỎỐỒỔỖỘỚỜỞỠỢ"),
    ('P', "ṔṖ"),
    ('R', "ŔŖŘȐȒṘṚṜṞ"),
    ('S', "ŚŜŞŠȘṠṢṤṦṨ"),
    ('T', "ŢŤȚṪṬṮṰ"),
    ('U', "ÙÚÛÜŨŪŬŮŰŲƯǓǕǗǙǛȔȖṲṴṶṸṺỤỦỨỪỬỮỰ"),
    ('V', "ṼṾ"),
    ('W', "ŴẀẂẄẆẈ"),
    ('X', "ẊẌ"),
    ('Y', "ÝŶŸȲẎỲỴỶỸ"),
    ('Z', "ŹŻŽẐẒẔ"),
    ('a', "àáâãäåāăąǎǟǡǻȁȃȧḁạảấầẩẫậắằẳẵặ"),
    ('b', "ḃḅḇ"),
    ('c', "çćĉċčḉ"),
    ('d', "ďḋḍḏḑḓ"),
    ('e', "èéêëēĕėęěȅȇȩḕḗḙḛḝẹẻẽếềểễệ"),
    ('f', "ḟ"),
    ('g', "ĝğġģǧǵḡ"),
    ('h', "ĥȟḣḥḧḩḫẖ"),
    ('i', "ìíîïĩīĭįǐȉȋḭḯỉị"),
    ('j', "ĵǰ"),
    ('k', "ķǩḱḳḵ"),
    ('l', "ĺļľḷḹḻḽ"),
    ('m', "ḿṁṃ"),
    ('n', "ñńņňǹṅṇṉṋ"),
    ('o', "òóôõöōŏőơǒǫǭȍȏȫȭȯȱṍṏṑṓọỏốồổỗộớờởỡợ"),
    ('p', "ṕṗ"),
    ('r', "ŕŗřȑȓṙṛṝṟ"),
    ('s', "śŝşšșṡṣṥṧṩ"),
    ('t', "ţťțṫṭṯṱẗ"),
    ('u', "ùúûüũūŭůűųưǔǖǘǚǜȕȗṳṵṷṹṻụủứừửữự"),
    ('v', "ṽṿ"),
    ('w', "ŵẁẃẅẇẉẘ"),
    ('x', "ẋẍ"),
    ('y', "ýÿŷȳẏẙỳỵỷỹ"),
    ('z', "źżžẑẓẕ"),
    ('Æ', "ǢǼ"),
    ('Ø', "Ǿ"),
    ('æ', "ǣǽ"),
    ('ø', "ǿ"),
    ('ſ', "ẛ"),
    ('Ʒ', "Ǯ"),
    ('ʒ', "ǯ"),
    ('Α', "ΆἈἉἊἋἌἍἎἏᾈᾉᾊᾋᾌᾍᾎᾏᾸᾹᾺΆᾼ"),
    ('Ε', "ΈἘἙἚἛἜἝῈΈ"),
    ('Η', "ΉἨἩἪἫἬἭἮἯᾘᾙᾚᾛᾜᾝᾞᾟῊΉῌ"),
    ('Ι', "ΊΪἸἹἺἻἼἽἾἿῘῙῚΊ"),
    ('Ο', "ΌὈὉὊὋὌὍῸΌ"),
    ('Ρ', "Ῥ"),
    ('Υ', "ΎΫὙὛὝὟῨῩῪΎ"),
    ('Ω', "ΏὨὩὪὫὬὭὮὯᾨᾩᾪᾫᾬᾭᾮᾯῺΏῼ"),
    ('α', "άἀἁἂἃἄἅἆἇὰάᾀᾁᾂᾃᾄᾅᾆᾇᾰᾱᾲᾳᾴᾶᾷ"),
    ('ε', "έἐἑἒἓἔἕὲέ"),
    ('η', "ήἠἡἢἣἤἥἦἧὴήᾐᾑᾒᾓᾔᾕᾖᾗῂῃῄῆῇ"),
    ('ι', "ΐίϊἰἱἲἳἴἵἶἷὶίῐῑῒΐῖῗ"),
    ('ο', "όὀὁὂὃὄὅὸό"),
    ('ρ', "ῤῥ"),
    ('υ', "ΰϋύὐὑὒὓὔὕὖὗὺύῠῡῢΰῦῧ"),
    ('ω', "ώὠὡὢὣὤὥὦὧὼώᾠᾡᾢᾣᾤᾥᾦᾧῲῳῴῶῷ"),
    ('ϒ', "ϓϔ"),
    ('І', "Ї"),
    ('А', "ӐӒ"),
    ('Г', "Ѓ"),
    ('Е', "ЀЁӖ"),
    ('Ж', "ӁӜ"),
    ('З', "Ӟ"),
    ('И', "ЍЙӢӤ"),
    ('К', "Ќ"),
    ('О', "Ӧ"),
    ('У', "ЎӮӰӲ"),
    ('Ч', "Ӵ"),
    ('Ы', "Ӹ"),
    ('Э', "Ӭ"),
    ('а', "ӑӓ"),
    ('г', "ѓ"),
    ('е', "ѐёӗ"),
    ('ж', "ӂӝ"),
    ('з', "ӟ"),
    ('и', "йѝӣӥ"),
    ('к', "ќ"),
    ('о', "ӧ"),
    ('у', "ўӯӱӳ"),
    ('ч', "ӵ"),
    ('ы', "ӹ"),
    ('э', "ӭ"),
    ('і', "ї"),
    ('Ѵ', "Ѷ"),
    ('ѵ', "ѷ"),
    ('Ә', "Ӛ"),
    ('ә', "ӛ"),
    ('Ө', "Ӫ"),
    ('ө', "ӫ"),
];

/// How many bytes the longest match of the regular expression `query` has, `usize::MAX` when it
/// has no limit or does not parse. No match holds more rows than bytes.
fn longest_match(query: &str, case_insensitive: bool) -> usize {
    regex_syntax::ParserBuilder::new()
        .case_insensitive(case_insensitive)
        .multi_line(true)
        .crlf(true)
        .build()
        .parse(query)
        .ok()
        .and_then(|hir| hir.properties().maximum_len())
        .unwrap_or(usize::MAX)
}

/// What is wrong with the regular expression `query`, which did not compile, in a few plain words
/// for the find bar to show.
fn regex_problem(query: &str, error: &regex::Error) -> &'static str {
    use regex_syntax::{ast::ErrorKind as Parse, hir::ErrorKind as Translate};
    let parsed = regex_syntax::ParserBuilder::new().build().parse(query);
    match parsed {
        Err(regex_syntax::Error::Parse(error)) => match error.kind() {
            Parse::ClassUnclosed => "Unclosed bracket",
            Parse::GroupUnclosed => "Unclosed parenthesis",
            Parse::GroupUnopened => "Extra closing parenthesis",
            Parse::RepetitionMissing => "Nothing to repeat",
            Parse::RepetitionCountUnclosed => "Unclosed brace",
            Parse::RepetitionCountInvalid => "Repeat count runs backwards",
            Parse::RepetitionCountDecimalEmpty | Parse::DecimalEmpty => "Repeat count needs a number",
            Parse::DecimalInvalid => "Number too big",
            Parse::ClassRangeInvalid | Parse::ClassRangeLiteral => "Bad range in brackets",
            Parse::ClassEscapeInvalid => "Bad escape in brackets",
            Parse::EscapeUnexpectedEof => "Ends with a lone backslash",
            Parse::EscapeUnrecognized => "Unknown escape",
            Parse::EscapeHexEmpty | Parse::EscapeHexInvalid | Parse::EscapeHexInvalidDigit => "Bad hex escape",
            Parse::FlagDanglingNegation | Parse::FlagUnexpectedEof | Parse::FlagUnrecognized => "Bad option in parentheses",
            Parse::GroupNameEmpty | Parse::GroupNameInvalid | Parse::GroupNameUnexpectedEof => "Bad group name",
            Parse::NestLimitExceeded(_) => "Nested too deeply",
            Parse::SpecialWordBoundaryUnclosed | Parse::SpecialWordBoundaryUnrecognized => "Bad word boundary",
            Parse::SpecialWordOrRepetitionUnexpectedEof => "Pattern ends too soon",
            Parse::UnicodeClassInvalid => "Unknown character class",
            Parse::UnsupportedBackreference => "Backreferences aren't supported",
            Parse::UnsupportedLookAround => "Lookaround isn't supported",
            Parse::CaptureLimitExceeded => "Too many groups",
            _ => "Not a valid pattern",
        },
        Err(regex_syntax::Error::Translate(error)) => match error.kind() {
            Translate::UnicodePropertyNotFound | Translate::UnicodePropertyValueNotFound | Translate::UnicodePerlClassNotFound => "Unknown character class",
            _ => "Not a valid pattern",
        },
        _ if matches!(error, regex::Error::CompiledTooBig(_)) => "Pattern too big",
        _ => "Not a valid pattern",
    }
}

/// Whether `escaped` is `raw` as `regex::escape` writes it: a backslash before each regex
/// metacharacter, and nothing else added. Reads both once, without building the escaped text.
fn is_escaped(raw: &str, escaped: &str) -> bool {
    let mut escaped = escaped.chars();
    for c in raw.chars() {
        if regex_syntax::is_meta_character(c) && escaped.next() != Some('\\') {
            return false;
        }
        if escaped.next() != Some(c) {
            return false;
        }
    }
    escaped.next().is_none()
}

/// The length of the text both ropes start with and of the text they end with, the two never
/// overlapping. Chunks both ropes share (an edit copies only the path to its leaf) are skipped by
/// pointer, so a one-key edit costs one comparison per chunk, not per byte.
fn common_ends(old: &Rope, new: &Rope) -> (usize, usize) {
    let (mut a, mut b) = (old.chunks(), new.chunks());
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut prefix = 0;
    loop {
        if x.is_empty() {
            match a.next() {
                Some(chunk) => x = chunk.as_bytes(),
                None => break,
            }
        }
        if y.is_empty() {
            match b.next() {
                Some(chunk) => y = chunk.as_bytes(),
                None => break,
            }
        }
        if x.as_ptr() == y.as_ptr() && x.len() == y.len() {
            prefix += x.len();
            (x, y) = (&[], &[]);
            continue;
        }
        let n = x.len().min(y.len());
        if let Some(at) = x[..n].iter().zip(&y[..n]).position(|(p, q)| p != q) {
            prefix += at;
            break;
        }
        prefix += n;
        (x, y) = (&x[n..], &y[n..]);
    }
    let room = old.len().min(new.len()) - prefix;
    let (mut a, mut b) = (old.chunks_at(old.len()).0, new.chunks_at(new.len()).0);
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut suffix = 0;
    while suffix < room {
        if x.is_empty() {
            match a.prev() {
                Some(chunk) => x = chunk.as_bytes(),
                None => break,
            }
        }
        if y.is_empty() {
            match b.prev() {
                Some(chunk) => y = chunk.as_bytes(),
                None => break,
            }
        }
        if x.as_ptr() == y.as_ptr() && x.len() == y.len() {
            suffix += x.len();
            (x, y) = (&[], &[]);
            continue;
        }
        let n = x.len().min(y.len());
        let same = x[x.len() - n..].iter().rev().zip(y[y.len() - n..].iter().rev()).position(|(p, q)| p != q);
        if let Some(at) = same {
            suffix += at;
            break;
        }
        suffix += n;
        (x, y) = (&x[..x.len() - n], &y[..y.len() - n]);
    }
    (prefix, suffix.min(room))
}

/// Whether no proper prefix of `query` is also its suffix, so two matches never overlap and a scan
/// can start anywhere before a match and still find it.
fn borderless(query: &[u8]) -> bool {
    (1..query.len()).all(|k| query[..k] != query[query.len() - k..])
}

/// [`borderless`] for a case-folded query: two characters are the same where either case matches.
fn borderless_folded(query: &[char]) -> bool {
    let fold = |c: char| c.to_uppercase().flat_map(char::to_lowercase).collect::<Vec<_>>();
    let same = |a: char, b: char| {
        a == b
            || a.to_lowercase().eq(b.to_lowercase())
            || a.to_uppercase().eq(b.to_uppercase())
            || fold(a) == fold(b)
    };
    let n = query.len();
    (1..n).all(|k| !(0..k).all(|i| same(query[i], query[n - k + i])))
}

fn floor_boundary(text: &Rope, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &Rope, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// A letter, a digit, an underscore, or a combining mark, which belongs to the letter before it.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || is_combining_mark(c)
}

/// Both ends of `range` sit on a word boundary: the character outside an end
/// differs in word-ness from the matched character inside it.
fn on_word_boundaries(text: &Rope, range: &Range<usize>) -> bool {
    let first = text.chars_at(range.start).next();
    let last = text.chars_at(range.end).prev();
    let before = text.chars_at(range.start).prev();
    let after = text.chars_at(range.end).next();
    let boundary = |outside: Option<char>, inside: Option<char>| {
        outside.is_some_and(is_word_char) != inside.is_some_and(is_word_char)
    };
    boundary(before, first) && boundary(after, last)
}

/// Feeds a rope's chunks to a stream search one `read` at a time.
struct ChunkReader<'a, I: Iterator<Item = &'a str>> {
    chunks: I,
    current: &'a [u8],
}

impl<'a, I: Iterator<Item = &'a str>> ChunkReader<'a, I> {
    fn new(chunks: I) -> Self {
        Self {
            chunks,
            current: &[],
        }
    }
}

impl<'a, I: Iterator<Item = &'a str>> io::Read for ChunkReader<'a, I> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.current.is_empty() {
            match self.chunks.next() {
                Some(chunk) => self.current = chunk.as_bytes(),
                None => return Ok(0),
            }
        }
        let n = self.current.len().min(buf.len());
        buf[..n].copy_from_slice(&self.current[..n]);
        self.current = &self.current[n..];
        Ok(n)
    }
}

/// Where `offset` lands once every range in `edits` (ascending, disjoint) is
/// replaced by its text. An offset inside a replaced range moves to the end
/// of its replacement.
fn map_through_replacements(edits: &[(Range<usize>, String)], offset: usize) -> usize {
    let mut delta = 0isize;
    for (range, replacement) in edits {
        let replacement_len = replacement.len();
        if range.end <= offset {
            delta += replacement_len as isize - range.len() as isize;
        } else if range.start < offset {
            return (range.start as isize + delta) as usize + replacement_len;
        } else {
            break;
        }
    }
    (offset as isize + delta) as usize
}

impl Iterator for SearchMatcher {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let ix = self.next_index()?;
        self.current_match_ix = ix;
        self.matched_ranges.get(ix).cloned()
    }
}

impl DoubleEndedIterator for SearchMatcher {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.is_empty() {
            return None;
        }
        if self.current_match_ix == 0 {
            self.current_match_ix = self.len();
        }
        self.current_match_ix -= 1;
        self.matched_ranges.get(self.current_match_ix).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(query: &str, case_insensitive: bool, whole_word: bool) -> SearchMatcher {
        let mut matcher = SearchMatcher::new();
        matcher.update_query(query, case_insensitive, whole_word);
        matcher
    }

    /// Edits `text` one random replacement at a time, each rescanned incrementally, and checks
    /// every result against a scan from scratch.
    fn edits_match_full_scans(query: &str, case_insensitive: bool, whole_word: bool, seed: u64) {
        let alphabet = ["a", "b", "e", "é", "É", " ", "\n", "\n", "_", "ab", "aa", "a\nb", "ee\n", "ϑ", "ϴ", "θ", "ϑ\n"];
        let mut state = seed;
        let mut next = |bound: usize| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as usize % bound
        };
        let mut text = String::new();
        for _ in 0..40 {
            text.push_str(alphabet[next(alphabet.len())]);
        }
        let mut rope = Rope::from(text.as_str());
        let mut live = matcher(query, case_insensitive, whole_word);
        live.update(&rope);
        for step in 0..300 {
            let boundaries: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
            let a = boundaries[next(boundaries.len())];
            let b = boundaries[next(boundaries.len())];
            let (start, end) = (a.min(b), if next(3) == 0 { a.max(b) } else { a.min(b) });
            let mut insert = String::new();
            for _ in 0..next(4) {
                insert.push_str(alphabet[next(alphabet.len())]);
            }
            text.replace_range(start..end, &insert);
            let mut edited = rope.clone();
            if step % 2 == 0 {
                live.record_edit(&(start..end), insert.len());
            }
            edited.replace(start..end, &insert);
            rope = edited;
            live.update(&rope);
            let mut fresh = matcher(query, case_insensitive, whole_word);
            fresh.update(&rope);
            assert_eq!(
                *live.matched_ranges(),
                *fresh.matched_ranges(),
                "{query:?} ci={case_insensitive} ww={whole_word} step {step}: {start}..{end} -> {insert:?} in {text:?}"
            );
        }
    }

    #[test]
    fn recorded_edits_compose_in_current_text_coordinates() {
        for edits in [
            vec![(12..12, "foo"), (1..1, "xx")],
            vec![(1..1, "foo"), (12..12, "xx")],
            vec![(4..7, "foofoo"), (5..8, "")],
            vec![(4..7, ""), (1..10, "foo")],
            vec![(4..7, "xx"), (0..16, "")],
        ] {
            let mut rope = Rope::from("foo foo foo foo foo");
            let mut live = matcher("foo", false, false);
            live.update(&rope);
            for (range, text) in edits {
                live.record_edit(&range, text.len());
                rope.replace(range, text);
            }
            live.update(&rope);
            let mut fresh = matcher("foo", false, false);
            fresh.update(&rope);
            assert_eq!(live.matched_ranges(), fresh.matched_ranges());
        }
    }

    #[test]
    fn incremental_scans_equal_full_scans_after_random_edits() {
        for seed in 1..=12 {
            for (query, ci, ww) in [
                ("e", true, false),
                ("e", false, true),
                ("ab", true, false),
                ("a\nb", false, false),
                ("aa", false, false),
                ("aa", true, true),
                ("é", false, false),
                ("é", true, false),
                ("É", true, true),
                ("é\na", true, false),
                ("é\né", true, false),
                ("a\né\n", true, true),
                ("ϑ\nϴ", true, false),
            ] {
                edits_match_full_scans(query, ci, ww, seed);
            }
        }
    }

    #[test]
    fn a_scope_of_several_ranges_follows_a_batch_edit_by_edit() {
        let mut rope = Rope::from("a foo b\nfoo\na foo b\n");
        let mut live = matcher("foo", true, false);
        live.set_scope(Some(vec![0..7, 12..19]), &rope);
        live.update(&rope);
        assert_eq!(live.len(), 2);
        // Replace all: both matches in the scope go at once, the middle line is out of it.
        let edits = vec![(2..5, "X".to_string()), (14..17, "X".to_string())];
        for (range, text) in edits.iter().rev() {
            live.record_edit(range, text.len());
            rope.replace(range.clone(), text);
        }
        live.update(&rope);
        assert_eq!(live.scope().unwrap(), [0..5, 10..15]);
        assert_eq!(live.len(), 0);
        // Its undo replays the changes one after the other.
        for (range, text) in [(2..3, "foo"), (14..15, "foo")] {
            live.scope_apply(&range, text.len());
            rope.replace(range, text);
        }
        live.update(&rope);
        assert_eq!(live.scope().unwrap(), [0..7, 12..19]);
        assert_eq!(live.len(), 2);
        assert_eq!(rope.to_string(), "a foo b\nfoo\na foo b\n");
        // Text inserted exactly at a scope's start stays outside it.
        live.scope_apply(&(0..0), 3);
        assert_eq!(live.scope().unwrap()[0], 3..10);
    }

    #[test]
    fn blocks_of_rows_find_what_one_scan_of_the_text_finds() {
        for (query, text) in [
            ("é\na", "é\na b é\né\na\n".repeat(5000)),
            ("é\né", "é\né\né\né\né\n".repeat(5000)),
            ("É\n", "é\n".repeat(9000)),
        ] {
            let rope = Rope::from(text.as_str());
            let live = matcher(query, true, false);
            let Some(Pattern::Regex { regex, rows, .. }) = &live.pattern else { panic!("not a regex") };
            let mut blocks = Vec::new();
            live.scan_blocks(regex, *rows, &rope, 0..rope.lines_len(), &mut blocks);
            let whole: Vec<_> = regex.find_iter(&text).map(|found| found.range()).collect();
            assert_eq!(blocks, whole, "{query:?}");
            assert!(blocks.len() > 4000);
        }
    }

    #[test]
    fn incremental_scan_after_deleting_across_a_match_and_replacing_all() {
        let old = Rope::from("foo bar foo baz foo\nfoo");
        let mut live = matcher("foo", true, false);
        live.update(&old);
        // Deleting from inside one match to inside the next removes both.
        let mut text = old.clone();
        text.replace(1..9, "");
        live.update(&text);
        let mut fresh = matcher("foo", true, false);
        fresh.update(&text);
        assert_eq!(*live.matched_ranges(), *fresh.matched_ranges());
        // Replace all: every match changes in one edit.
        let ranges = live.matched_ranges();
        let mut replaced = text.clone();
        for range in ranges.iter().rev() {
            replaced.replace(range.clone(), "quux");
        }
        live.update(&replaced);
        assert!(live.is_empty());
        // Undo brings them back.
        live.update(&text);
        assert_eq!(*live.matched_ranges(), *fresh.matched_ranges());
    }

    #[test]
    fn finds_navigates_and_preserves_replacement_position() {
        let mut matcher = SearchMatcher::new();
        matcher.update_query("foo", true, false);
        matcher.update(&Rope::from("foo FOO foo"));
        assert_eq!(&*matcher.matched_ranges(), &[0..3, 4..7, 8..11]);
        assert_eq!(matcher.next(), Some(4..7));
        assert_eq!(matcher.next_back(), Some(0..3));

        matcher.set_current_match_index(2);
        matcher.begin_replacement();
        matcher.update(&Rope::from("foo FOO bar"));
        assert_eq!(matcher.current_match_index(), 1);
    }

    #[test]
    fn next_wraps_to_start() {
        let mut matcher = SearchMatcher::new();
        matcher.update_query("aaaaa", false, false);
        matcher.update(&Rope::from(".....aaaaa.....aaaaa.....aaaaa"));
        matcher.set_current_match_index(2);
        assert_eq!(matcher.next(), Some(5..10));
    }

    #[test]
    fn a_query_set_without_the_panel_keeps_the_session_active_until_closed() {
        let mut session = SearchSession::default();
        assert!(!session.is_active());

        session.open(false, true);
        assert!(session.is_active());
        session.close();
        assert!(!session.is_active());

        // A custom search UI never opens the panel; setting a query is what
        // turns the match highlights on, and closing turns them off again.
        session.activate();
        assert!(session.is_active());
        assert!(!session.open);
        session.close();
        assert!(!session.is_active());
    }

    #[test]
    fn identical_query_keeps_the_current_match() {
        let mut session = SearchSession::default();
        session.update_query("foo", true, false);
        session.matcher.update(&Rope::from("foo bar foo baz foo"));
        session.matcher.update_cursor_by_offset(12);
        assert_eq!(session.matcher.current_match_index(), 2);

        // Reopening Find and the styled search panel's initial query echo both
        // update the session with the same query. Neither should reset the
        // previously active occurrence.
        session.update_query("foo", true, false);

        assert_eq!(session.matcher.current_match_index(), 2);
        assert_eq!(session.matcher.label(), "3/3");
    }

    #[test]
    fn closing_drops_the_matches_and_reopening_finds_them_again() {
        let text = Rope::from("foo bar foo baz foo");
        let mut session = SearchSession::default();
        session.open(false, true);
        session.update_query("foo", true, false);
        session.matcher.refresh(&text);
        session.matcher.update_cursor_by_offset(12);
        assert_eq!(session.matcher.len(), 3);

        session.close();
        assert!(session.matcher.matched_ranges().is_empty());
        assert_eq!(session.matcher.matched_ranges().capacity(), 0);
        assert_eq!(session.query, "foo");

        session.open(false, true);
        session.update_query("foo", true, false);
        session.matcher.refresh(&text);
        assert_eq!(session.matcher.label(), "3/3");
    }

    #[test]
    fn a_released_search_keeps_scope_and_current_match_and_rescans_to_the_same_label() {
        let text = Rope::from("foo bar foo baz foo");
        let mut session = SearchSession::default();
        session.open(false, true);
        session.update_query("foo", true, false);
        session.matcher.refresh(&text);
        session.matcher.update_cursor_by_offset(12);
        assert!(!session.matcher.is_released());

        session.matcher.release();
        assert!(session.matcher.matched_ranges().is_empty());
        assert!(session.matcher.is_released());
        assert_eq!(session.matcher.shown_total(), 3);
        assert_eq!(session.matcher.shown_current(), Some(2));
        assert!(session.is_active());
        assert_eq!(session.query, "foo");

        session.matcher.refresh(&text);
        assert_eq!(session.matcher.label(), "3/3");
        assert!(!session.matcher.is_released());
    }

    #[test]
    fn a_scan_that_landed_on_changed_text_refreshes_to_a_fresh_scan() {
        let old = Rope::from("foo bar foo baz foo");
        let new = Rope::from("foo foo bar foo baz foo foo");
        let mut matcher = SearchMatcher::new();
        matcher.update_query("foo", true, false);
        matcher.refresh(&old);
        matcher.release();
        let (mut scan, scan_gen) = matcher.begin_scan();
        scan.refresh(&old);
        assert!(matches!(matcher.adopt_scan(scan, scan_gen, true), Landing::Adopted));
        matcher.refresh(&new);
        let mut fresh = SearchMatcher::new();
        fresh.update_query("foo", true, false);
        fresh.refresh(&new);
        assert_eq!(matcher.matched_ranges(), fresh.matched_ranges());
    }

    #[test]
    fn a_background_scan_lands_at_the_same_match_and_is_dropped_once_stale() {
        let text = Rope::from("foo bar foo baz foo");
        let mut matcher = SearchMatcher::new();
        matcher.update_query("foo", true, false);
        matcher.refresh(&text);
        matcher.update_cursor_by_offset(12);
        matcher.release();
        // The bar keeps the old count and match while the scan runs.
        let (mut scan, scan_gen) = matcher.begin_scan();
        assert!(matcher.is_scanning());
        scan.refresh(&text);
        assert!(matches!(matcher.adopt_scan(scan, scan_gen, false), Landing::Adopted));
        assert_eq!(matcher.label(), "3/3");
        assert!(!matcher.is_released() && !matcher.is_scanning() && !matcher.stale);

        // Released again before a scan landed: its result is dropped.
        matcher.release();
        let (mut scan, scan_gen) = matcher.begin_scan();
        matcher.release();
        scan.refresh(&text);
        assert!(matches!(matcher.adopt_scan(scan, scan_gen, false), Landing::Dropped));
        assert!(matcher.is_empty() && matcher.is_released());

        // Text changed while it ran: taken stale, so the next refresh rescans what changed.
        let (mut scan, scan_gen) = matcher.begin_scan();
        scan.refresh(&text);
        assert!(matches!(matcher.adopt_scan(scan, scan_gen, true), Landing::Adopted));
        assert!(matcher.stale);
    }

    #[test]
    fn replacement_keeps_current_match_index_on_next_match() {
        let mut matcher = SearchMatcher::new();
        matcher.update_query("foo", true, false);
        matcher.update(&Rope::from("foo foo foo"));
        assert_eq!(matcher.label(), "1/3");

        assert!(matcher.has_next_without_wrap());
        matcher.begin_replacement();
        matcher.update(&Rope::from("bar foo foo"));
        assert_eq!(matcher.current_match_index(), 0);
        assert_eq!(matcher.matched_ranges()[0], 4..7);
        assert_eq!(matcher.label(), "1/2");

        matcher.set_current_match_index(1);
        assert!(!matcher.has_next_without_wrap());
        matcher.set_current_match_index(0);
        matcher.begin_replacement();
        matcher.update(&Rope::from("bar foo bar"));
        assert_eq!(matcher.current_match_index(), 0);
        assert_eq!(matcher.matched_ranges()[0], 4..7);
        assert_eq!(matcher.label(), "1/1");
    }

    #[test]
    fn a_regex_running_to_the_end_of_a_long_text_is_one_match() {
        let text = Rope::from("a\n".repeat(9_000).as_str());
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern(r"\n[\s\S]*", false, false, true);
        matcher.update(&text);
        assert_eq!(matcher.matched_ranges().to_vec(), vec![1..text.len()]);
    }

    #[test]
    fn a_bounded_multiline_regex_finds_matches_at_block_edges() {
        let line = BLOCK_ROWS - 1;
        let mut text = "x\n".repeat(line);
        text.push_str(&"\n".repeat(20));
        text.push_str("end\n");
        text.push_str(&"x\n".repeat(10));
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern(r"x\n{21}end", false, false, true);
        matcher.update(&Rope::from(text.as_str()));
        assert_eq!(matcher.len(), 1);
    }

    #[test]
    fn a_replacement_with_digits_then_text_keeps_the_group() {
        let text = Rope::from("foo bar");
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern(r"(\w+)", false, false, true);
        matcher.update(&text);
        assert_eq!(matcher.replacement_for(&text, &(0..3), "$1_old"), "foo_old");
        assert_eq!(matcher.replacement_for(&text, &(0..3), "$${1}$1"), "${1}foo");
        assert_eq!(matcher.replacements_for(&text, &[0..3, 4..7], "US$$ $1x"), vec!["US$ foox", "US$ barx"]);
    }

    #[test]
    fn a_replacement_holding_the_query_moves_past_the_new_text() {
        let mut matcher = matcher("log", true, false);
        matcher.update(&Rope::from("log(a); log(b);"));
        matcher.begin_replacement_at("console.log".len());
        matcher.update(&Rope::from("console.log(a); log(b);"));
        assert_eq!(matcher.current_match_index(), 1);
        assert_eq!(matcher.matched_ranges()[1], 16..19);
    }

    #[test]
    fn a_long_accented_query_still_searches() {
        let query = "é".repeat(1_500);
        let mut matcher = SearchMatcher::new();
        matcher.update_query(&query, true, false);
        matcher.update(&Rope::from(query.as_str()));
        assert_eq!(matcher.len(), 1);
    }

    #[test]
    fn update_matches_clamps_current_match_index_while_replacing() {
        let mut matcher = SearchMatcher::new();
        matcher.update_query("foo", true, false);
        matcher.update(&Rope::from("foo foo foo"));
        matcher.set_current_match_index(2);
        matcher.begin_replacement();

        matcher.update(&Rope::from("foo xoo foo"));

        assert_eq!(matcher.len(), 2);
        assert_eq!(matcher.current_match_index(), 1);
        assert_eq!(matcher.label(), "2/2");
    }

    fn found(
        text: &str,
        query: &str,
        case_insensitive: bool,
        whole_word: bool,
    ) -> Vec<Range<usize>> {
        let mut matcher = SearchMatcher::new();
        matcher.update_query(query, case_insensitive, whole_word);
        matcher.update(&Rope::from(text));
        matcher.matched_ranges().to_vec()
    }

    #[test]
    fn accented_letters_match_ignoring_case() {
        // `é` and `É` are two bytes each; the ranges are byte offsets.
        assert_eq!(
            found("un café, un CAFÉ", "café", true, false),
            [3..8, 13..18]
        );
        assert_eq!(
            found("un café, un CAFÉ", "CAFÉ", true, false),
            [3..8, 13..18]
        );
        assert_eq!(found("un café, un CAFÉ", "café", false, false), [3..8]);
        // Folding stays per character: `ß` does not match `SS`.
        assert_eq!(found("straße STRASSE", "straße", true, false), [0..7]);
        // A match spanning a line break still works for a folded query.
        assert_eq!(found("a É\nb", "É\nB", true, false), [2..6]);
        // Ranges on later lines count the preceding line breaks.
        assert_eq!(found("x\nCAFÉ", "café", true, false), [2..7]);
    }

    /// `found` with Ignore diacritics on.
    fn found_folded(text: &str, query: &str, case_insensitive: bool, whole_word: bool) -> Vec<Range<usize>> {
        let mut matcher = SearchMatcher::new();
        matcher.set_ignore_diacritics(true);
        matcher.update_query(query, case_insensitive, whole_word);
        matcher.update(&Rope::from(text));
        matcher.matched_ranges().to_vec()
    }

    #[test]
    fn ignore_diacritics_finds_accented_letters_by_their_base() {
        // Off, `cafe` is not `café`.
        assert!(found("un café", "cafe", true, false).is_empty());
        // `é` is two bytes: the ranges are the real text's.
        assert_eq!(found_folded("un café, cafe", "cafe", true, false), [3..8, 10..14]);
        assert_eq!(found_folded("Résumé resume", "resume", true, false), [0..8, 9..15]);
        // The query's own accents fold too.
        assert_eq!(found_folded("cafe café", "CAFÉ", true, false), [0..4, 5..10]);
        assert_eq!(found_folded("Café café", "cafe", true, false), [0..5, 6..11]);
        assert_eq!(found_folded("Café café", "cafe", false, false), [6..11]);
        assert_eq!(found_folded("Café café", "Cafe", false, false), [0..5]);
        // A line break in the query still spans lines.
        assert_eq!(found_folded("a é\nb", "e\nb", true, false), [2..6]);
    }

    #[test]
    fn ignore_diacritics_reads_a_combining_mark_as_part_of_its_letter() {
        // `e` and U+0301 are one letter to the eye, and the match takes the mark with it.
        assert_eq!(found_folded("cafe\u{301}", "cafe", true, false), [0..6]);
        assert_eq!(found_folded("cafe\u{301}", "caf\u{e9}", true, false), [0..6]);
        assert_eq!(found_folded("caf\u{e9} cafe\u{301}", "cafe\u{301}", true, false), [0..5, 6..12]);
        // Two marks on one letter.
        assert_eq!(found_folded("e\u{301}\u{323}x", "ex", true, false), [0..6]);
    }

    #[test]
    fn ignore_diacritics_keeps_a_mark_from_folding_into_a_letter() {
        // U+0345 folds to a Greek iota under case folding; the mark run must not.
        assert_eq!(found_folded("και καιρός", "κα", true, false), [0..4, 7..11]);
        assert_eq!(found_folded("cafeι", "cafe", true, false), [0..4]);
    }

    #[test]
    fn a_long_folded_query_folds_up_to_the_letter_cap_and_reads_as_plain_text_past_it() {
        let folds = |letters: usize| {
            let query = "é".repeat(letters);
            let mut matcher = SearchMatcher::new();
            matcher.set_ignore_diacritics(true);
            matcher.update_query(&query, true, false);
            matcher.update(&Rope::from("e".repeat(letters).as_str()));
            matcher.len() == 1
        };
        assert!(folds(FOLDED_LETTERS));
        assert!(!folds(1_000));
    }

    #[test]
    fn ignore_diacritics_keeps_whole_word_and_the_mark_inside_the_word() {
        assert_eq!(found_folded("café cafés cafe\u{301}", "cafe", true, true), [0..5, 13..19]);
    }

    #[test]
    fn ignore_diacritics_does_not_touch_a_regular_expression() {
        let mut matcher = SearchMatcher::new();
        matcher.set_ignore_diacritics(true);
        matcher.update_pattern("caf.", true, false, true);
        matcher.update(&Rope::from("café cafe"));
        assert_eq!(matcher.matched_ranges().to_vec(), [0..5, 6..10]);
        matcher.update_pattern("cafe", true, false, true);
        matcher.update(&Rope::from("café cafe"));
        assert_eq!(matcher.matched_ranges().to_vec(), [6..10]);
    }

    #[test]
    fn switching_ignore_diacritics_rescans_the_same_query() {
        let text = Rope::from("café cafe");
        let mut matcher = SearchMatcher::new();
        matcher.update_query("cafe", true, false);
        matcher.update(&text);
        assert_eq!(matcher.matched_ranges().to_vec(), [6..10]);
        matcher.set_ignore_diacritics(true);
        matcher.update(&text);
        assert_eq!(matcher.matched_ranges().to_vec(), [0..5, 6..10]);
        matcher.set_ignore_diacritics(false);
        matcher.update(&text);
        assert_eq!(matcher.matched_ranges().to_vec(), [6..10]);
    }

    #[gpui::test]
    fn replace_on_a_folded_match_lands_on_the_real_bytes(cx: &mut gpui::TestAppContext) {
        use crate::input::EditorState;
        use gpui::Focusable as _;
        cx.update(crate::init);
        let text = "un caf\u{e9}, un cafe\u{301}!";
        let (state, cx) = cx.add_window_view(move |window, cx| EditorState::new(window, cx).default_value(text));
        // One at a time: the first match, then the second (with its combining mark).
        let replaced = cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.focus_handle(cx).focus(window, cx);
                state.set_search_ignore_diacritics(true, cx);
                state.set_search_query("cafe", true, cx);
                assert!(state.search_session().ignore_diacritics());
                assert_eq!(state.search_session().matcher.len(), 2);
                assert!(state.replace_current_search_match("tea", false, window, cx));
                state.value().to_string()
            })
        });
        assert_eq!(replaced, "un tea, un cafe\u{301}!");
        let all = cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                assert!(state.replace_current_search_match("tea", false, window, cx));
                state.value().to_string()
            })
        });
        assert_eq!(all, "un tea, un tea!");
        // Replace all in one go.
        let (state, cx) = cx.add_window_view(move |window, cx| EditorState::new(window, cx).default_value(text));
        let (count, edited) = cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.focus_handle(cx).focus(window, cx);
                state.set_search_ignore_diacritics(true, cx);
                state.set_search_query("cafe", true, cx);
                let count = state.replace_all_search_matches("tea", window, cx);
                (count, state.value().to_string())
            })
        });
        assert_eq!((count, edited.as_str()), (2, "un tea, un tea!"));
    }

    #[test]
    fn whole_word_skips_matches_inside_longer_words() {
        let text = "foobar foo _foo foo_ foo.";
        assert_eq!(found(text, "foo", true, false).len(), 5);
        assert_eq!(found(text, "foo", true, true), [7..10, 21..24]);
        assert_eq!(found("été été2 été", "été", true, true), [0..5, 13..18]);
    }

    #[test]
    fn smartcase_follows_the_query() {
        assert!(SearchSession::smart_case_insensitive("foo"));
        assert!(!SearchSession::smart_case_insensitive("Foo"));
        assert!(!SearchSession::smart_case_insensitive("fÉo"));
        assert!(SearchSession::smart_case_insensitive(""));
    }

    #[test]
    fn offsets_map_through_replaced_ranges() {
        let edits = |len: usize| -> Vec<(Range<usize>, String)> {
            [2..4, 6..8].into_iter().map(|range| (range, "x".repeat(len))).collect()
        };
        // A longer replacement shifts later offsets; a shorter one pulls them back.
        assert_eq!(map_through_replacements(&edits(5), 0), 0);
        assert_eq!(map_through_replacements(&edits(5), 2), 2);
        assert_eq!(map_through_replacements(&edits(5), 3), 7);
        assert_eq!(map_through_replacements(&edits(5), 4), 7);
        assert_eq!(map_through_replacements(&edits(5), 5), 8);
        assert_eq!(map_through_replacements(&edits(5), 9), 15);
        assert_eq!(map_through_replacements(&edits(0), 7), 4);
        assert_eq!(map_through_replacements(&edits(0), 9), 5);
    }

    /// Replace all in a window: the count, the text, the selection, then the
    /// text and selection after one undo.
    fn replace_all_run(
        cx: &mut gpui::TestAppContext,
        text: &str,
        query: &str,
        replacement: &str,
        caret: usize,
    ) -> (usize, String, Range<usize>, String, Range<usize>) {
        use crate::input::{EditorState, Undo};
        use gpui::Focusable as _;
        cx.update(crate::init);
        let text = text.to_string();
        let (state, cx) = cx.add_window_view(move |window, cx| {
            EditorState::new(window, cx).default_value(text)
        });
        let (query, replacement) = (query.to_string(), replacement.to_string());
        let count = cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.focus_handle(cx).focus(window, cx);
                state.set_selected_range(caret..caret, cx);
                state.set_search_query(query, false, cx);
                state.replace_all_search_matches(&replacement, window, cx)
            })
        });
        let edited = state.read_with(cx, |s, _| (s.value().to_string(), s.selected_range()));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.dispatch_action(Box::new(Undo), cx);
        });
        let undone = state.read_with(cx, |s, _| (s.value().to_string(), s.selected_range()));
        (count, edited.0, edited.1, undone.0, undone.1)
    }

    #[gpui::test]
    fn replace_all_keeps_the_caret_on_its_text_and_undoes_in_one_step(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "foo one foo two foo three";
        // Caret inside "two": it moves with the text, not to the top.
        let (count, edited, selected, undone, restored) =
            replace_all_run(cx, text, "foo", "quux", 13);
        assert_eq!(count, 3);
        assert_eq!(edited, "quux one quux two quux three");
        assert_eq!(selected, 15..15);
        assert_eq!(undone, text);
        assert_eq!(restored, 13..13);
    }

    #[gpui::test]
    fn replace_all_handles_adjacent_matches_and_a_replacement_holding_the_query(
        cx: &mut gpui::TestAppContext,
    ) {
        let (count, edited, _, undone, _) = replace_all_run(cx, "aaaa b aa", "aa", "aaa", 7);
        assert_eq!(count, 3);
        assert_eq!(edited, "aaaaaa b aaa");
        assert_eq!(undone, "aaaa b aa");
    }

    #[gpui::test]
    fn replace_all_does_not_overlap_matches(cx: &mut gpui::TestAppContext) {
        let (count, edited, selected, undone, _) = replace_all_run(cx, "aaaaa", "aaa", "x", 5);
        assert_eq!(count, 1);
        assert_eq!(edited, "xaa");
        assert_eq!(selected, 3..3);
        assert_eq!(undone, "aaaaa");
    }

    #[gpui::test]
    fn undo_after_replace_all_leaves_the_matches_current(cx: &mut gpui::TestAppContext) {
        use crate::input::{EditorState, Undo};
        use gpui::Focusable as _;
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).default_value("foo bar foo baz foo")
        });
        cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.focus_handle(cx).focus(window, cx);
                state.set_search_query("foo", false, cx);
                assert_eq!(state.replace_all_search_matches("q", window, cx), 3);
            });
        });
        assert!(state.read_with(cx, |s, _| s.search_session.matcher.matched_ranges().is_empty()));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.dispatch_action(Box::new(Undo), cx);
        });
        let ranges = state.read_with(cx, |s, _| s.search_session.matcher.matched_ranges());
        assert_eq!(*ranges, vec![0..3, 8..11, 16..19]);
    }

    #[gpui::test]
    fn occurrence_selection_follows_smartcase(cx: &mut gpui::TestAppContext) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).default_value("foo Foo foo Foo")
        });
        let run = |cx: &mut gpui::VisualTestContext, selection: Range<usize>, all: bool| {
            cx.update(|_, cx| {
                state.update(cx, |state, cx| {
                    state.set_selected_range(selection, cx);
                    assert!(if all {
                        state.select_all_search_matches(cx)
                    } else {
                        state.select_next_occurrence(cx)
                    });
                    let mut ranges: Vec<_> = state.selected_ranges().collect();
                    ranges.sort_unstable_by_key(|range| range.start);
                    ranges
                })
            })
        };
        // "foo" ignores case: Foo is taken too. "Foo" matches case: only Foo.
        assert_eq!(run(cx, 0..3, true), vec![0..3, 4..7, 8..11, 12..15]);
        assert_eq!(run(cx, 4..7, true), vec![4..7, 12..15]);
        assert_eq!(run(cx, 0..3, false), vec![0..3, 4..7]);
        assert_eq!(run(cx, 4..7, false), vec![4..7, 12..15]);
        // A chosen case mode wins: Case Sensitive on makes "foo" match only foo.
        cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.set_search_options("x", false, false, cx);
                state.mark_search_case_chosen();
                state.close_search(cx);
            })
        });
        assert_eq!(run(cx, 0..3, true), vec![0..3, 8..11]);
    }

    #[gpui::test]
    fn find_on_a_capitalized_selection_leaves_the_case_mode_to_smartcase(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).searchable(true).default_value("foo Foo foo Foo")
        });
        let chosen = cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.set_selected_range(4..7, cx);
                state.open_search(false, cx);
                // The panel sends the query with the case smartcase picks for it.
                state.set_search_options("Foo", false, false, cx);
                state.search_session().case_chosen
            })
        });
        assert!(!chosen);
    }

    #[gpui::test]
    fn find_on_a_selection_with_regex_on_matches_exactly_that_text(cx: &mut gpui::TestAppContext) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).searchable(true).default_value("a.b axb a.b (x) (x)")
        });
        let run = |cx: &mut gpui::VisualTestContext, selection: Range<usize>, regex: bool| {
            cx.update(|_, cx| {
                state.update(cx, |state, cx| {
                    state.close_search(cx);
                    state.set_search_regex(regex, cx);
                    state.set_selected_range(selection, cx);
                    state.open_search(false, cx);
                    (
                        state.search_session().query.clone(),
                        state.search_session().matcher.matched_ranges().to_vec(),
                        state.search_session().matcher.invalid(),
                    )
                })
            })
        };
        let (query, found, invalid) = run(cx, 0..3, true);
        assert_eq!((query.as_str(), found, invalid), (r"a\.b", vec![0..3, 8..11], false));
        // An unbalanced bracket in a selection is text too, not a pattern that does not compile.
        let (query, found, invalid) = run(cx, 12..15, true);
        assert_eq!((query.as_str(), found, invalid), (r"\(x\)", vec![12..15, 16..19], false));
        // Plain Find takes the selection as it is.
        let (query, found, _) = run(cx, 0..3, false);
        assert_eq!((query.as_str(), found), ("a.b", vec![0..3, 8..11]));
    }

    fn found_regex(
        text: &str,
        query: &str,
        case_insensitive: bool,
        whole_word: bool,
    ) -> Vec<Range<usize>> {
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern(query, case_insensitive, whole_word, true);
        matcher.update(&Rope::from(text));
        matcher.matched_ranges().to_vec()
    }

    #[test]
    fn a_regex_query_matches_the_pattern_with_case_and_whole_word_applying() {
        assert_eq!(found_regex("a1 b22 c333", r"\d+", true, false), [1..2, 4..6, 8..11]);
        assert_eq!(found_regex("Foo foo", "foo", false, false), [4..7]);
        assert_eq!(found_regex("Foo foo", "foo", true, false), [0..3, 4..7]);
        assert_eq!(found_regex("cat concat cat", "cat", true, true), [0..3, 11..14]);
        assert_eq!(found_regex("ab\nab\n", "^ab$", true, false), [0..2, 3..5]);
        assert_eq!(found_regex("a\r\nb\r\n", "a$", true, false), [0..1]);
        // The same text is no pattern without the option.
        assert_eq!(found("a.c abc", "a.c", true, false), [0..3]);
        assert_eq!(found_regex("a.c abc", "a.c", true, false), [0..3, 4..7]);
    }

    #[test]
    fn a_regex_query_with_a_line_break_matches_across_lines() {
        assert_eq!(found_regex("a\nb a\nb", r"a\nb", true, false), [0..3, 4..7]);
        assert_eq!(found_regex("x\n\n\ny", r"\n+", true, false), [1..4]);
    }

    #[test]
    fn a_regex_that_can_match_nothing_finds_only_its_real_matches() {
        assert!(found_regex("abc", "x*", true, false).is_empty());
        assert_eq!(found_regex("ab\ncd", r"(?:\n)*", true, false), [2..3]);
    }

    #[test]
    fn multi_line_regexes_find_the_same_matches_across_block_edges() {
        let text = "x\n".repeat(BLOCK_ROWS - 3) + &"\n".repeat(20) + "y\n\n\n\n\nz\n";
        for query in [r"\n+", r"\n{3}", r"x\n\n\n"] {
            let whole = regex::Regex::new(query).unwrap();
            let expected: Vec<_> = whole.find_iter(&text).map(|found| found.range()).collect();
            assert!(!expected.is_empty());
            assert_eq!(found_regex(&text, query, true, false), expected, "{query}");
        }
    }

    #[test]
    fn whole_word_wraps_a_regex_so_an_alternation_finds_the_longer_word() {
        assert_eq!(found_regex("integer x int", "int|integer", true, true), [0..7, 10..13]);
        assert!(found_regex("integer x", "int", true, true).is_empty());
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern("(a", true, true, true);
        assert!(matcher.invalid());
    }

    #[test]
    fn regex_smartcase_ignores_escapes() {
        for query in [r"\S+", r"\D\W", r"\p{Lu}x", r"\pLx", r"\x41", r"a\\b"] {
            assert!(SearchSession::smart_case_insensitive_regex(query), "{query}");
        }
        assert!(!SearchSession::smart_case_insensitive_regex(r"\SFoo"));
        assert!(!SearchSession::smart_case_insensitive_regex(r"\\Foo"));
    }

    #[test]
    fn replacements_read_each_row_once_and_agree_with_one_at_a_time() {
        let text = Rope::from("a1 b2 c3\nd4\n");
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern(r"([a-z])(\d)", true, false, true);
        matcher.update(&text);
        let ranges = matcher.matched_ranges();
        let all = matcher.replacements_for(&text, &ranges, "$2$1");
        let each: Vec<_> = ranges.iter().map(|range| matcher.replacement_for(&text, range, "$2$1").into_owned()).collect();
        assert_eq!(all, ["1a", "2b", "3c", "4d"]);
        assert_eq!(all, each);
        assert_eq!(matcher.replacements_for(&text, &ranges, "-"), ["-", "-", "-", "-"]);
    }

    #[test]
    fn a_pattern_that_does_not_compile_matches_nothing_and_says_so() {
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern("(a", true, false, true);
        matcher.update(&Rope::from("(a (a"));
        assert!(matcher.invalid());
        assert_eq!(matcher.len(), 0);
        // The same text is a fine literal query.
        matcher.update_pattern("(a", true, false, false);
        matcher.update(&Rope::from("(a (a"));
        assert!(!matcher.invalid());
        assert_eq!(matcher.len(), 2);
    }

    #[test]
    fn a_pattern_that_does_not_compile_says_what_is_wrong_in_plain_words() {
        let problem = |query: &str| {
            let mut matcher = SearchMatcher::new();
            matcher.update_pattern(query, true, false, true);
            matcher.problem()
        };
        for query in ["[a", "(a", "a)", "*a", "a\\", "(?<=a)b"] {
            let said = problem(query).unwrap_or_else(|| panic!("{query} compiles"));
            assert_ne!(said, "Not a valid pattern", "{query}");
        }
        assert_ne!(problem("[a"), problem("(a"));
        assert_eq!(problem("a(b)"), None);
        assert_eq!(problem(""), None);
    }

    #[test]
    fn a_replacement_expands_the_groups_of_a_regex_match_only() {
        let text = Rope::from("ann@x\nbob@yy");
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern(r"(\w+)@(?P<host>\w+)", true, false, true);
        matcher.update(&text);
        let ranges = matcher.matched_ranges();
        assert_eq!(matcher.replacement_for(&text, &ranges[1], "${host}:$1$$"), "yy:bob$");
        matcher.update_query("ann", true, false);
        matcher.update(&text);
        let ranges = matcher.matched_ranges();
        assert_eq!(matcher.replacement_for(&text, &ranges[0], "$1"), "$1");
    }

    #[test]
    fn a_regex_replacement_reads_n_t_and_backslash_and_a_plain_one_stays_literal() {
        let text = Rope::from("ann@x bob@yy");
        let mut matcher = SearchMatcher::new();
        matcher.update_pattern(r"(\w+)@(\w+)", true, false, true);
        matcher.update(&text);
        let ranges = matcher.matched_ranges();
        assert_eq!(matcher.replacement_for(&text, &ranges[0], r"x\ny"), "x\ny");
        assert_eq!(matcher.replacement_for(&text, &ranges[0], r"a\tb\\c\\n\q"), "a\tb\\c\\n\\q");
        // Groups still expand, and what a group brings in is not read as an escape.
        assert_eq!(matcher.replacement_for(&text, &ranges[1], r"$2\n${1}\t$$"), "yy\nbob\t$");
        assert_eq!(matcher.replacements_for(&text, &ranges, r"$1\n"), ["ann\n", "bob\n"]);
        assert_eq!(matcher.replacements_for(&text, &ranges, r"-\t-"), ["-\t-", "-\t-"]);
        matcher.update_pattern("ann", true, false, false);
        matcher.update(&text);
        let ranges = matcher.matched_ranges();
        assert_eq!(matcher.replacement_for(&text, &ranges[0], r"x\ny\\"), r"x\ny\\");
        assert_eq!(matcher.replacements_for(&text, &ranges, r"x\ny"), [r"x\ny"]);
    }

    #[gpui::test]
    fn regex_replace_puts_in_a_newline_and_plain_replace_puts_in_the_backslash(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::EditorState;
        use gpui::Focusable as _;
        cx.update(crate::init);
        let replaced = |cx: &mut gpui::TestAppContext, regex: bool, query: &str, all: bool| {
            let (state, cx) = cx.add_window_view(|window, cx| {
                EditorState::new(window, cx).default_value("a1 b2")
            });
            cx.update(|window, cx| {
                state.update(cx, |state, cx| {
                    state.focus_handle(cx).focus(window, cx);
                    state.set_search_regex(regex, cx);
                    state.set_search_query(query, true, cx);
                    if all {
                        state.replace_all_search_matches(r"x\ny", window, cx);
                    } else {
                        state.replace_current_search_match(r"x\ny", true, window, cx);
                    }
                })
            });
            state.read_with(cx, |s, _| s.value().to_string())
        };
        assert_eq!(replaced(cx, true, r"\d", false), "ax\ny b2");
        assert_eq!(replaced(cx, true, r"\d", true), "ax\ny bx\ny");
        assert_eq!(replaced(cx, false, "a", true), r"x\ny1 b2");
    }

    #[gpui::test]
    fn regex_replace_expands_groups_for_the_current_match_and_for_all(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::{EditorState, Undo};
        use gpui::Focusable as _;
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).default_value("ann@x bob@yy cy@z")
        });
        let replaced = cx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.focus_handle(cx).focus(window, cx);
                state.set_search_regex(true, cx);
                state.set_search_query(r"(\w+)@(\w+)", true, cx);
                state.replace_current_search_match("$2:$1", true, window, cx)
            })
        });
        assert!(replaced);
        assert_eq!(state.read_with(cx, |s, _| s.value().to_string()), "x:ann bob@yy cy@z");
        let count = cx.update(|window, cx| {
            state.update(cx, |state, cx| state.replace_all_search_matches("${2}-$1", window, cx))
        });
        assert_eq!(count, 2);
        assert_eq!(state.read_with(cx, |s, _| s.value().to_string()), "x:ann yy-bob z-cy");
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.dispatch_action(Box::new(Undo), cx);
        });
        assert_eq!(state.read_with(cx, |s, _| s.value().to_string()), "x:ann bob@yy cy@z");
    }

    /// Types `query` one character at a time into `live` (the case mode as `case` picks it for each
    /// prefix) and checks each key's matches against a scan from scratch. Returns how many keys
    /// narrowed the matches instead of scanning.
    fn typed_matches_equal_full_scans(text: &Rope, query: &str, case: impl Fn(&str) -> bool) -> usize {
        let mut live = SearchMatcher::new();
        let mut narrowed = 0;
        for end in query.char_indices().map(|(at, c)| at + c.len_utf8()) {
            let prefix = &query[..end];
            let ci = case(prefix);
            live.update_pattern(prefix, ci, false, false);
            let reading = live.narrow(text);
            live.update(text);
            let mut fresh = SearchMatcher::new();
            fresh.update_pattern(prefix, ci, false, false);
            fresh.update(text);
            assert_eq!(live.matched_ranges(), fresh.matched_ranges(), "{prefix:?} ci={ci}");
            assert_eq!(live.current_match_index(), fresh.current_match_index(), "{prefix:?} ci={ci}");
            if let Some(reading) = reading {
                assert_eq!(reading, *live.matched_ranges(), "{prefix:?} ci={ci}");
                narrowed += 1;
            }
        }
        narrowed
    }

    #[test]
    fn narrowed_matches_equal_a_full_scan_for_every_case_mode() {
        let texts = [
            "ab ab aab abab ABab Abc abcabc aaaa aaa ab AB aB\nabc ABC aBc abd",
            "é É ée ÉÉ ϑ θ ée\nabc ABC aBc éé ÉÉ ée ϴ ϑ\u{212a} k K",
            "aaaa\naaaaaa\nab\nabab\nababab a\n",
            "",
        ];
        let queries = ["ab", "abc", "Abc", "aaa", "abab", "ab a", "ée", "ÉÉ", "ϑ θ", "k", "kK", "xyz"];
        let smart = |prefix: &str| SearchSession::smart_case_insensitive(prefix);
        let mut narrowed = 0;
        for text in texts {
            let rope = Rope::from(text);
            for query in queries {
                narrowed += typed_matches_equal_full_scans(&rope, query, |_| true);
                narrowed += typed_matches_equal_full_scans(&rope, query, |_| false);
                narrowed += typed_matches_equal_full_scans(&rope, query, smart);
            }
        }
        assert!(narrowed > 20, "{narrowed}");
        // Typing on an overlapping query is a full scan, since its matches do not hold every occurrence.
        let rope = Rope::from("aab aaab");
        assert_eq!(typed_matches_equal_full_scans(&rope, "aab", |_| false), 0);
    }

    #[test]
    fn narrowing_is_left_to_a_full_scan_when_the_text_or_the_options_changed() {
        let text = Rope::from("abc abd ab Abc");
        let mut live = SearchMatcher::new();
        live.update_pattern("ab", true, false, false);
        live.update(&text);
        for (query, ci, whole_word, regex) in [("abc", false, false, false), ("abc", true, true, false), ("abc", true, false, true)] {
            live.update_pattern(query, ci, whole_word, regex);
            assert!(live.narrow(&text).is_none(), "{query} {ci} {whole_word} {regex}");
            live.update_pattern("ab", true, false, false);
            live.update(&text);
        }
        live.update_pattern("abc", true, false, false);
        assert!(live.narrow(&Rope::from("abc abd ab Abd")).is_none());
        assert!(live.narrow(&text).is_some());
        live.set_scope(Some(vec![0..7]), &text);
        live.update_pattern("abc", true, false, false);
        assert!(live.narrow(&text).is_none());
    }

    #[test]
    fn narrowing_a_random_text_equals_full_scans() {
        let alphabet = ["a", "b", "c", "ab", "abc", "A", "B", " ", "\n", "é", "É", "ϑ"];
        let mut state = 7u64;
        let mut next = |bound: usize| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as usize % bound
        };
        for _ in 0..40 {
            let text: String = (0..300).map(|_| alphabet[next(alphabet.len())]).collect();
            let query: String = (0..2 + next(5)).map(|_| alphabet[next(alphabet.len())]).collect();
            let rope = Rope::from(text.as_str());
            typed_matches_equal_full_scans(&rope, &query, |_| true);
            typed_matches_equal_full_scans(&rope, &query, |_| false);
            typed_matches_equal_full_scans(&rope, &query, SearchSession::smart_case_insensitive);
        }
    }

    #[test]
    fn narrowing_a_folded_query_equals_full_scans() {
        let alphabet = ["c", "a", "f", "e", "é", "É", "e\u{301}", "è", "ca", "fe", "x", " ", "\n", "E"];
        let mut state = 5u64;
        let mut next = |bound: usize| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as usize % bound
        };
        let mut narrowed = 0;
        // A letter that carries many marks is one letter: the longer query still finds it.
        let marked = format!("e{}x e{}x ex", "\u{301}".repeat(64), "\u{323}".repeat(3));
        let fixed = [(marked, "ex".to_string())];
        let random: Vec<(String, String)> = (0..60)
            .map(|_| {
                let text: String = (0..300).map(|_| alphabet[next(alphabet.len())]).collect();
                let query: String = (0..2 + next(5)).map(|_| alphabet[next(alphabet.len())]).collect();
                (text, query)
            })
            .collect();
        for (text, query) in fixed.iter().chain(random.iter()) {
            let rope = Rope::from(text.as_str());
            for ci in [true, false] {
                let mut live = SearchMatcher::new();
                live.set_ignore_diacritics(true);
                for end in query.char_indices().map(|(at, c)| at + c.len_utf8()) {
                    let prefix = &query[..end];
                    live.update_pattern(prefix, ci, false, false);
                    let reading = live.narrow(&rope);
                    live.update(&rope);
                    let mut fresh = SearchMatcher::new();
                    fresh.set_ignore_diacritics(true);
                    fresh.update_pattern(prefix, ci, false, false);
                    fresh.update(&rope);
                    assert_eq!(live.matched_ranges(), fresh.matched_ranges(), "{prefix:?} ci={ci} in {text:?}");
                    if let Some(reading) = reading {
                        assert_eq!(reading, *live.matched_ranges(), "{prefix:?} ci={ci} in {text:?}");
                        narrowed += 1;
                    }
                }
            }
        }
        assert!(narrowed > 20, "{narrowed}");
    }

    #[test]
    fn the_current_match_at_an_offset_is_the_one_the_walk_picks() {
        let alphabet = ["a", "ab", " ", "\n", "é", "abc"];
        let mut state = 11u64;
        let mut next = |bound: usize| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as usize % bound
        };
        for round in 0..30 {
            let text: String = (0..(round * 20)).map(|_| alphabet[next(alphabet.len())]).collect();
            let mut matcher = SearchMatcher::new();
            matcher.update_pattern(["ab", "a", "é", "abc"][round % 4], true, false, false);
            matcher.update(&Rope::from(text.as_str()));
            let ranges = matcher.matched_ranges();
            for _ in 0..200 {
                let offset = next(text.len() + 3);
                // The walk this replaces: the first match that holds or ends after `offset`, else the last.
                let mut walked = 0;
                for (ix, range) in ranges.iter().enumerate() {
                    walked = ix;
                    if range.contains(&offset) || range.end >= offset {
                        break;
                    }
                }
                matcher.current_match_ix = 3;
                matcher.update_cursor_by_offset(offset);
                if ranges.is_empty() {
                    assert_eq!(matcher.current_match_ix, 3);
                } else {
                    assert_eq!(matcher.current_match_ix, walked, "{offset} in {ranges:?}");
                }
            }
        }
    }

    #[gpui::test]
    fn typing_in_the_open_bar_reveals_the_first_match_and_leaves_the_caret(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let text = "line\n".repeat(300) + "needle here\n" + &"line\n".repeat(50);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).searchable(true).default_value(text.as_str())
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let (caret, aimed, current) = cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.open_search(false, cx);
                state.set_search_options("needle", true, false, cx);
                let matcher = &state.search_session().matcher;
                (
                    state.selected_range(),
                    state.reveal_glide.is_some() || state.deferred_scroll_offset.is_some(),
                    matcher.current_match_index(),
                )
            })
        });
        assert_eq!(caret, 0..0);
        assert_eq!(current, 0);
        assert!(aimed, "typing did not aim a reveal at the first match");
    }

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn the_bar_echoing_an_escaped_selection_leaves_the_pasteboard_as_selected(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).searchable(true).default_value("a.b axb a.b")
        });
        cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.set_search_regex(true, cx);
                state.set_selected_range(0..3, cx);
                state.open_search(false, cx);
                let query = state.search_session().query.clone();
                assert_eq!(query, r"a\.b");
                // What the find bar does after it shows the seeded query.
                state.set_search_options(query, true, false, cx);
            })
        });
        let shared = cx.update(|_, cx| cx.read_from_find_pasteboard().and_then(|item| item.text()));
        assert_eq!(shared.as_deref(), Some("a.b"));
    }

    #[test]
    fn is_escaped_agrees_with_regex_escape() {
        for raw in ["a.b", "(x) [y]", "plain", "", "a\\b", "é.É", "^$|?*+{}#&-~"] {
            assert!(is_escaped(raw, &regex::escape(raw)), "{raw}");
            assert!(!is_escaped(raw, &format!("{}x", regex::escape(raw))), "{raw}");
        }
        assert!(!is_escaped("a.b", "a.b"));
        assert!(!is_escaped("a", "a\\"));
    }

    #[gpui::test]
    fn typing_leaves_the_view_alone_when_the_match_is_already_on_screen(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let text = "line\n".repeat(300);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).searchable(true).default_value(text.as_str())
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let (glide, deferred) = cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.open_search(false, cx);
                state.set_search_options("line", true, false, cx);
                (state.reveal_glide.is_some(), state.deferred_scroll_offset.is_some())
            })
        });
        assert!(!glide && !deferred, "a match in view moved the text");
    }

    #[gpui::test]
    fn typing_leaves_the_view_alone_for_a_match_in_view_with_typewriter_on(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let text = "line\n".repeat(300);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).searchable(true).default_value(text.as_str())
        });
        cx.update(|_, cx| state.update(cx, |state, cx| state.set_typewriter(true, cx)));
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let (glide, deferred) = cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.open_search(false, cx);
                state.set_search_options("line", true, false, cx);
                (state.reveal_glide.is_some(), state.deferred_scroll_offset.is_some())
            })
        });
        assert!(!glide && !deferred, "a match in view moved the text under typewriter");
    }

    #[gpui::test]
    fn skip_next_occurrence_drops_the_newest_pick_and_adds_the_one_after(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::input::EditorState;
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            EditorState::new(window, cx).searchable(true).default_value("foo a foo b foo c foo")
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let picks = cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.set_selected_range(0..3, cx);
                // Cmd-D picks the second foo; skipping it takes the third and keeps the first.
                assert!(state.select_next_occurrence(cx));
                assert_eq!(state.selected_ranges().collect::<Vec<_>>(), vec![0..3, 6..9]);
                assert!(state.can_skip_next_occurrence());
                assert!(state.skip_next_occurrence(cx));
                let skipped = state.selected_ranges().collect::<Vec<_>>();
                // Skipping again moves on to the fourth.
                assert!(state.skip_next_occurrence(cx));
                let again = state.selected_ranges().collect::<Vec<_>>();
                (skipped, again)
            })
        });
        assert_eq!(picks.0, vec![0..3, 12..15]);
        assert_eq!(picks.1, vec![0..3, 18..21]);
        // With every occurrence picked there is nothing to skip to, and the picks stay.
        let stayed = cx.update(|_, cx| {
            state.update(cx, |state, cx| {
                assert!(state.select_next_occurrence(cx));
                assert!(state.select_next_occurrence(cx));
                assert!(!state.skip_next_occurrence(cx));
                state.selected_ranges().collect::<Vec<_>>()
            })
        });
        assert_eq!(stayed.len(), 4);
    }
}
