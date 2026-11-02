use crate::input::InputModeKind;
use gpui::{Context, Pixels, Point, Window};
use ropey::Rope;
use sum_tree::Bias;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::input::{
    InputBaseState, MoveDown, MoveEnd, MoveHome, MoveLeft, MovePageDown, MovePageUp, MoveRight,
    MoveToEnd, MoveToNextSubwordEnd, MoveToNextWord, MoveToPreviousSubwordStart,
    MoveToPreviousWord, MoveToStart, MoveUp, RopeExt as _, SelectToNextSubwordEnd,
    SelectToPreviousSubwordStart, cursor::CursorSelection,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveDirection {
    Up,
    Down,
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Compute the column anchor for the given `offset`. Wrap/fold-aware.
    pub(super) fn preferred_column_for(&self, offset: usize) -> Option<(Pixels, usize)> {
        self.preferred_column_for_with_affinity(offset, false)
    }

    /// Like [`Self::preferred_column_for`], but resolves an offset on a soft wrap
    /// boundary to the row the caret is drawn on.
    pub(super) fn preferred_column_for_with_affinity(
        &self,
        offset: usize,
        line_end_affinity: bool,
    ) -> Option<(Pixels, usize)> {
        let last_layout = self.last_layout.as_ref()?;
        let point = self.text.offset_to_point(offset);
        let line = last_layout.line(point.row)?;
        let pos = line.position_for_index(point.column, last_layout, line_end_affinity)?;
        Some((pos.x, point.column))
    }

    /// The line-end affinity that applies to `sel`. Only the active cursor
    /// carries one; every other cursor sits at the start of its row.
    pub(super) fn line_end_affinity_for(&self, sel: &CursorSelection) -> bool {
        sel.id == self.active_selection().id && self.cursor_line_end_affinity
    }

    /// The line-end affinity for a cursor known only by its offset. Cursors never share an
    /// offset, so this is the active cursor's affinity when `offset` is where it sits.
    pub(super) fn line_end_affinity_at(&self, offset: usize) -> bool {
        offset == self.cursor() && self.cursor_line_end_affinity
    }

    /// Called after moving the cursor. Updates the active selection's
    /// `column_anchor` if we know where the cursor now is.
    pub(super) fn update_preferred_column(&mut self) {
        let anchor =
            self.preferred_column_for_with_affinity(self.cursor(), self.cursor_line_end_affinity);
        self.active_selection_mut().column_anchor = anchor;
    }

    /// Move the cursor to the given offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Ensure the offset use self.next_boundary or self.previous_boundary to get the correct offset.
    pub(crate) fn move_to(
        &mut self,
        offset: usize,
        direction: Option<MoveDirection>,
        cx: &mut Context<Self>,
    ) {
        self.move_to_with_affinity(offset, direction, false, cx);
    }

    /// Like [`Self::move_to`], but also carries the caret's line-end affinity.
    ///
    /// A soft wrap boundary is one offset shared by the end of one visual line and the start of
    /// the next, so the offset alone cannot say where to draw the caret. Callers that resolved
    /// the offset from a visual position -- a click, a drag, a vertical move -- already know
    /// which of the two rows the user meant, and pass it here. Taking it in the same call as the
    /// move is what keeps the two from drifting apart.
    pub(crate) fn move_to_with_affinity(
        &mut self,
        offset: usize,
        direction: Option<MoveDirection>,
        line_end_affinity: bool,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.selections.remove_all_but_active();
        let offset = self.cursor_boundary(offset, Bias::Left);
        self.cursor_line_end_affinity = line_end_affinity;
        self.set_cursor_to(offset);
        self.scroll_to(offset, direction, cx);
        self.pause_blink_cursor(cx);
        self.update_preferred_column();
        M::hide_context_menu(self, cx);
        M::clear_inline_completion(self, cx);
        cx.notify()
    }

    /// Compute the target offset when moving a cursor at `offset` vertically by
    /// `move_lines`, honoring the remembered `column_anchor`. Wrap/fold-aware.
    ///
    /// Returns the new offset together with the line-end affinity the caret
    /// should carry there.
    pub(super) fn vertical_target(
        &self,
        offset: usize,
        column_anchor: Option<(Pixels, usize)>,
        line_end_affinity: bool,
        move_lines: isize,
    ) -> (usize, bool) {
        let Some(last_layout) = &self.last_layout else {
            return (offset, line_end_affinity);
        };

        // Start from the row the caret is drawn on, not the row the raw offset falls in: on a
        // soft wrap boundary those are two different rows.
        let mut display_point = self
            .display_map
            .offset_to_wrap_display_point_with_affinity(offset, line_end_affinity);

        // Convert wrap row → display row (skips folded rows), move, then convert back
        let current_display_row = self
            .display_map
            .wrap_row_to_display_row(display_point.row)
            .unwrap_or_else(|| {
                self.display_map
                    .nearest_visible_display_row(display_point.row)
            });
        let max_display_row = self.display_map.display_row_count().saturating_sub(1);
        let target_display_row = current_display_row
            .saturating_add_signed(move_lines)
            .min(max_display_row);
        let target_wrap_row = self
            .display_map
            .display_row_to_wrap_row(target_display_row)
            .unwrap_or(display_point.row);

        display_point.row = target_wrap_row;
        display_point.column = 0;
        let mut new_offset = self.display_map.wrap_display_point_to_offset(display_point);

        let column_anchor = column_anchor
            .or_else(|| self.preferred_column_for_with_affinity(offset, line_end_affinity));
        let mut new_affinity = false;
        if let Some((preferred_x, column)) = column_anchor {
            // Get display point again to update local_row.
            let mut next_display_point = self.display_map.offset_to_wrap_display_point(new_offset);
            next_display_point.column = 0;
            let next_point = self
                .display_map
                .wrap_display_point_to_point(next_display_point);
            let line_start_offset = self.text.line_start_offset(next_point.row);

            // If in visible range, prefer to use position to get column.
            if let Some(line) = last_layout.line(next_point.row) {
                if let Some((x, line_end_affinity)) = line.closest_index_for_position(
                    Point {
                        x: preferred_x,
                        y: next_display_point.local_row * last_layout.line_height,
                    },
                    last_layout,
                ) {
                    new_offset = line_start_offset + x;
                    // Landing on a wrap boundary means the preferred column pointed past the
                    // last glyph of the target row, so the caret stays on that row.
                    new_affinity = line_end_affinity;
                }
            } else {
                // Not in visible range, use column directly.
                let max_line_len = self.text.slice_line(next_point.row).len();
                new_offset = line_start_offset + column.min(max_line_len);
            }
        }

        (new_offset, new_affinity)
    }

    /// Extend to the document edge when there is no further visual row. Plain
    /// movement retains its column there, but selection must still reach the
    /// remaining text on the first or last row.
    pub(super) fn vertical_selection_target(
        &self,
        offset: usize,
        column_anchor: Option<(Pixels, usize)>,
        line_end_affinity: bool,
        move_lines: isize,
    ) -> (usize, bool) {
        let target = self.vertical_target(offset, column_anchor, line_end_affinity, move_lines);
        if self.last_layout.is_some() {
            let row = |offset, affinity| {
                self.display_map
                    .offset_to_wrap_display_point_with_affinity(offset, affinity)
                    .row
            };
            if row(offset, line_end_affinity) == row(target.0, target.1) {
                return (if move_lines < 0 { 0 } else { self.text.len() }, false);
            }
        }
        target
    }

    /// Move every cursor through `f`, which maps each selection to a
    /// `(new_offset, column_anchor, line_end_affinity)`, collapsing each to a
    /// cursor. Overlapping cursors are merged, then the standard post-move
    /// sequence runs. Only the active cursor's affinity is kept, see
    /// [`Self::move_to_with_affinity`].
    pub(super) fn move_all_cursors(
        &mut self,
        f: impl Fn(&Self, &CursorSelection) -> (usize, Option<(Pixels, usize)>, bool),
        direction: Option<MoveDirection>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        let mut active_affinity = false;
        let new_selections: Vec<CursorSelection> = self
            .selections
            .iter()
            .map(|sel| {
                let (offset, anchor, line_end_affinity) = f(self, sel);
                if sel.id == self.active_selection().id {
                    active_affinity = line_end_affinity;
                }
                let mut new_sel = *sel;
                new_sel.place_at(self.cursor_boundary(offset, Bias::Left), anchor);
                new_sel
            })
            .collect();
        self.selections.replace_all(new_selections);
        self.selections.merge_overlapping();

        self.cursor_line_end_affinity = active_affinity;
        self.scroll_to(self.cursor(), direction, cx);
        self.pause_blink_cursor(cx);
        M::hide_context_menu(self, cx);
        M::clear_inline_completion(self, cx);
        cx.notify();
    }

    /// Move every cursor vertically by `move_lines`.
    ///
    /// When `collapse` is set, a non-empty selection first collapses to just
    /// outside its start (up) or end (down) before moving, otherwise the cursor
    /// offset is used.
    fn move_vertical(
        &mut self,
        move_lines: isize,
        collapse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_single_line() {
            return;
        }
        self.pause_blink_cursor(cx);

        let direction = if move_lines < 0 {
            MoveDirection::Up
        } else {
            MoveDirection::Down
        };

        self.move_all_cursors(
            move |s, sel| {
                let (effective, anchor, affinity) = if sel.is_empty() || !collapse {
                    (
                        sel.cursor_offset(),
                        sel.column_anchor,
                        s.line_end_affinity_for(sel),
                    )
                } else if move_lines < 0 {
                    let e = s.previous_boundary(sel.start.saturating_sub(1));
                    (e, s.preferred_column_for(e), false)
                } else {
                    let e = s.next_boundary(sel.end.saturating_sub(1));
                    let affinity = s.line_end_affinity_at(e);
                    (
                        e,
                        s.preferred_column_for_with_affinity(e, affinity),
                        affinity,
                    )
                };
                let anchor =
                    anchor.or_else(|| s.preferred_column_for_with_affinity(effective, affinity));
                let (offset, affinity) = s.vertical_target(effective, anchor, affinity, move_lines);
                (offset, anchor, affinity)
            },
            Some(direction),
            window,
            cx,
        );
    }

    pub(super) fn left(&mut self, _: &MoveLeft, window: &mut Window, cx: &mut Context<Self>) {
        // With a lone cursor at the very start there is nowhere to move.
        // Propagate the keystroke so an ancestor (e.g. a navigable command
        // palette) can act on it. This is harmless when nothing is bound there.
        // With multiple cursors the others can still move, so only the
        // single-cursor case propagates.
        if self.selections.is_single() && self.active_selection().is_empty() && self.cursor() == 0 {
            cx.propagate();
            return;
        }

        self.move_all_cursors(
            |s, sel| {
                let offset = if sel.is_empty() {
                    s.previous_boundary(sel.cursor_offset())
                } else {
                    sel.start
                };
                (offset, s.preferred_column_for(offset), false)
            },
            None,
            window,
            cx,
        );
    }

    pub(super) fn right(&mut self, _: &MoveRight, window: &mut Window, cx: &mut Context<Self>) {
        // Mirror `left`: a lone cursor at the end of the text has nowhere to
        // move, so let the keystroke bubble to an ancestor.
        if self.selections.is_single()
            && self.active_selection().is_empty()
            && self.cursor() == self.text.len()
        {
            cx.propagate();
            return;
        }

        self.move_all_cursors(
            |s, sel| {
                let offset = if sel.is_empty() {
                    s.next_boundary(sel.cursor_offset())
                } else {
                    sel.end
                };
                (offset, s.preferred_column_for(offset), false)
            },
            None,
            window,
            cx,
        );
    }

    pub(super) fn up(&mut self, action: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        if M::handle_context_menu_action(self, Box::new(action.clone()), window, cx) {
            return;
        }

        self.move_vertical(-1, true, window, cx);
    }

    pub(super) fn down(&mut self, action: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        if M::handle_context_menu_action(self, Box::new(action.clone()), window, cx) {
            return;
        }

        self.move_vertical(1, true, window, cx);
    }

    pub(super) fn page_up(&mut self, _: &MovePageUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            return;
        }

        let Some(last_layout) = &self.last_layout else {
            return;
        };

        let display_lines = (self.input_bounds.size.height / last_layout.line_height) as isize;
        self.move_vertical(-display_lines, false, window, cx);
    }

    pub(super) fn page_down(
        &mut self,
        _: &MovePageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_single_line() {
            return;
        }

        let Some(last_layout) = &self.last_layout else {
            return;
        };

        let display_lines = (self.input_bounds.size.height / last_layout.line_height) as isize;
        self.move_vertical(display_lines, false, window, cx);
    }

    pub(super) fn home(&mut self, _: &MoveHome, window: &mut Window, cx: &mut Context<Self>) {
        self.move_all_cursors(
            |s, sel| {
                let offset = s.start_of_line_at(sel.cursor_offset(), s.line_end_affinity_for(sel));
                (offset, s.preferred_column_for(offset), false)
            },
            Some(MoveDirection::Up),
            window,
            cx,
        );
    }

    pub(super) fn end(&mut self, _: &MoveEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.move_all_cursors(
            |s, sel| {
                let offset = s.end_of_line_at(sel.cursor_offset(), s.line_end_affinity_for(sel));
                // The caret belongs at the end of the visual row it is on.
                (offset, s.preferred_column_for(offset), true)
            },
            Some(MoveDirection::Down),
            window,
            cx,
        );
    }

    pub(super) fn move_to_start(
        &mut self,
        _: &MoveToStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(0, None, cx);
    }

    pub(super) fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), None, cx);
    }

    pub(super) fn move_to_previous_word(
        &mut self,
        _: &MoveToPreviousWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_all_cursors(
            |s, sel| {
                let offset = s.previous_start_of_word_at(sel.cursor_offset());
                (offset, s.preferred_column_for(offset), false)
            },
            None,
            window,
            cx,
        );
    }

    pub(super) fn move_to_next_word(
        &mut self,
        _: &MoveToNextWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_all_cursors(
            |s, sel| {
                let offset = s.next_end_of_word_at(sel.cursor_offset());
                (offset, s.preferred_column_for(offset), false)
            },
            None,
            window,
            cx,
        );
    }

    pub(super) fn move_to_previous_subword(
        &mut self,
        _: &MoveToPreviousSubwordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_all_cursors(
            |s, sel| {
                let offset = s.previous_subword_start_at(sel.cursor_offset());
                (offset, s.preferred_column_for(offset), false)
            },
            None,
            window,
            cx,
        );
    }

    pub(super) fn move_to_next_subword(
        &mut self,
        _: &MoveToNextSubwordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_all_cursors(
            |s, sel| {
                let offset = s.next_subword_end_at(sel.cursor_offset());
                (offset, s.preferred_column_for(offset), false)
            },
            None,
            window,
            cx,
        );
    }

    pub(super) fn select_to_previous_subword(
        &mut self,
        _: &SelectToPreviousSubwordStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(|s, sel| s.previous_subword_start_at(sel.cursor_offset()), cx);
    }

    pub(super) fn select_to_next_subword(
        &mut self,
        _: &SelectToNextSubwordEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(|s, sel| s.next_subword_end_at(sel.cursor_offset()), cx);
    }

    /// Start of the subword before `offset`; see [`previous_subword_start`].
    pub(super) fn previous_subword_start_at(&self, offset: usize) -> usize {
        if self.masked {
            return 0;
        }
        let offset = self.text.clip_offset(offset, Bias::Left);
        self.token_boundary(previous_subword_start(&self.text, offset), Bias::Left)
    }

    /// End of the subword after `offset`; see [`next_subword_end`].
    pub(super) fn next_subword_end_at(&self, offset: usize) -> usize {
        if self.masked {
            return self.text.len();
        }
        let offset = self.text.clip_offset(offset, Bias::Left);
        self.cursor_boundary(next_subword_end(&self.text, offset), Bias::Right)
    }

    /// Offset to delete back to from `offset` for a word delete: the word
    /// start, cut short by [`adjust_greedy_deletion`].
    pub(super) fn word_delete_start_at(&self, offset: usize) -> usize {
        let offset = self.text.clip_offset(offset, Bias::Left);
        let start = self.previous_start_of_word_at(offset);
        if self.masked {
            return start;
        }
        self.token_boundary(adjust_greedy_deletion(&self.text, offset, start), Bias::Left)
    }

    /// Offset to delete forward to from `offset` for a word delete.
    pub(super) fn word_delete_end_at(&self, offset: usize) -> usize {
        let offset = self.text.clip_offset(offset, Bias::Left);
        let end = self.next_end_of_word_at(offset);
        if self.masked {
            return end;
        }
        self.token_boundary(adjust_greedy_deletion(&self.text, offset, end), Bias::Right)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Whitespace,
    Punctuation,
    Word,
}

/// Zed's `CharClassifier` reduced to what a plain input needs: letters and
/// digits (any script) are word characters; `_`, `.`, `-` and every other
/// symbol are punctuation, so they are stops of their own. Combining marks
/// never reach here: the walks below class them with their base char.
fn char_kind(c: char) -> CharKind {
    if c.is_whitespace() {
        CharKind::Whitespace
    } else if c.is_alphanumeric() {
        CharKind::Word
    } else {
        CharKind::Punctuation
    }
}

/// A combining accent, virama, tone mark or variation selector: it belongs
/// to the char before it, so no stop falls in front of it.
fn is_mark(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Mark
}

fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

/// Whether a word stop lies between `left` and `right`. A run of punctuation
/// is one stop, except brackets, which stop one by one.
fn kind_break(left: char, right: char) -> bool {
    char_kind(left) != char_kind(right) || (is_bracket(left) && is_bracket(right))
}

fn is_word_start(left: char, right: char) -> bool {
    kind_break(left, right) && !right.is_whitespace()
}

fn is_word_end(left: char, right: char) -> bool {
    kind_break(left, right) && !left.is_whitespace()
}

fn is_subword_boundary(left: char, right: char) -> bool {
    left == '_' && right != '_'
        || left != '_' && right == '_'
        || left == '-' && right != '-'
        || left != '-' && right == '-'
        || left.is_lowercase() && right.is_uppercase()
}

fn is_subword_start(left: char, right: char) -> bool {
    is_word_start(left, right) || (is_subword_boundary(left, right) && !right.is_whitespace())
}

fn is_subword_end(left: char, right: char) -> bool {
    is_word_end(left, right) || (is_subword_boundary(left, right) && !left.is_whitespace())
}

/// Walk back from `offset` over chars (never allocating) to the first spot
/// between `left` and `right` where `is_boundary(left, right)` holds; the
/// pair that straddles `offset` is not tested. Combining marks take their
/// base char's place. Start of text if none.
fn find_preceding_boundary(
    text: &Rope,
    offset: usize,
    is_boundary: impl Fn(char, char) -> bool,
) -> usize {
    let mut at = offset;
    // Left edge of the cluster read last; going back, its marks come before its base.
    let mut cluster_start = offset;
    let mut previous: Option<char> = None;
    for c in text.chars_at(offset).reversed() {
        at -= c.len_utf8();
        if is_mark(c) {
            continue;
        }
        if let Some(right) = previous
            && is_boundary(c, right)
        {
            return cluster_start;
        }
        cluster_start = at;
        previous = Some(c);
    }
    at
}

/// Walk forward from `offset` to the first spot between `left` and `right`
/// where `is_boundary(left, right)` holds; marks take their base's place.
/// End of text if none.
fn find_boundary(text: &Rope, offset: usize, is_boundary: impl Fn(char, char) -> bool) -> usize {
    let mut at = offset;
    let mut previous: Option<char> = None;
    for c in text.chars_at(offset) {
        if !is_mark(c) {
            if let Some(left) = previous
                && is_boundary(left, c)
            {
                return at;
            }
            previous = Some(c);
        }
        at += c.len_utf8();
    }
    at
}

/// Start of the word before `offset`: whitespace, newlines included, is
/// skipped, then the run of letters, or of punctuation, is crossed.
pub(crate) fn previous_word_start(text: &Rope, offset: usize) -> usize {
    find_preceding_boundary(text, offset, is_word_start)
}

/// End of the word after `offset`, mirror of [`previous_word_start`].
pub(crate) fn next_word_end(text: &Rope, offset: usize) -> usize {
    find_boundary(text, offset, is_word_end)
}

/// Start of the subword before `offset`: like a word start, and also at
/// `_`, `-` and lower-to-upper case changes.
pub(crate) fn previous_subword_start(text: &Rope, offset: usize) -> usize {
    find_preceding_boundary(text, offset, is_subword_start)
}

/// End of the subword after `offset`, mirror of [`previous_subword_start`].
pub(crate) fn next_subword_end(text: &Rope, offset: usize) -> usize {
    find_boundary(text, offset, is_subword_end)
}

/// Word moves are greedy, so a word delete would be too. Cut a deletion from
/// `delete_from` to `delete_until` short at a bracket inside it, and after
/// the nearest run of two or more whitespace chars, so a delete over
/// indentation or a gap takes that and not the word behind it.
pub(crate) fn adjust_greedy_deletion(
    text: &Rope,
    delete_from: usize,
    delete_until: usize,
) -> usize {
    if delete_from == delete_until {
        return delete_until;
    }
    let is_backward = delete_from > delete_until;
    let (start, end) = if is_backward {
        (delete_until, delete_from)
    } else {
        (delete_from, delete_until)
    };

    // Brackets are one byte, so a bracket at `at` offers `at` and `at + 1`.
    let mut bracket: Option<usize> = None;
    let mut at = start;
    for c in text.slice(start..end).chars() {
        if is_bracket(c) {
            for cut in [at, at + 1] {
                if start < cut && cut < end {
                    bracket = Some(match bracket {
                        Some(b) if is_backward => b.max(cut),
                        Some(b) => b.min(cut),
                        None => cut,
                    });
                }
            }
        }
        at += c.len_utf8();
    }
    let (start, end) = match bracket {
        Some(b) if is_backward => (b, end),
        Some(b) => (start, b),
        None => (start, end),
    };

    // Nearest whitespace run of two or more chars to the caret. A CRLF counts
    // once, so a line break cuts the same way in CRLF and LF files.
    let mut found: Option<usize> = None;
    let mut run_len = 0usize;
    let mut run_start = start;
    let mut at = start;
    let mut previous: Option<char> = None;
    for c in text.slice(start..end).chars() {
        if c.is_whitespace() {
            if run_len == 0 {
                run_start = at;
            }
            if !(c == '\n' && previous == Some('\r')) {
                run_len += 1;
            }
        } else {
            if run_len >= 2 {
                found = Some(if is_backward { run_start } else { found.unwrap_or(at) });
            }
            run_len = 0;
        }
        at += c.len_utf8();
        previous = Some(c);
    }
    if run_len >= 2 {
        found = Some(if is_backward { run_start } else { found.unwrap_or(at) });
    }

    found.unwrap_or(if is_backward { start } else { end })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    /// `|` marks the caret in `text`; returns the text without it and the offset.
    fn caret(text: &str) -> (Rope, usize) {
        let at = text.find('|').unwrap();
        (rope(&text.replacen('|', "", 1)), at)
    }

    /// Press the move repeatedly from the caret, return the text with every stop marked.
    fn stops(text: &str, step: fn(&Rope, usize) -> usize) -> Vec<usize> {
        let (r, mut at) = caret(text);
        let mut out = vec![];
        loop {
            let next = step(&r, at);
            if next == at {
                break;
            }
            out.push(next);
            at = next;
            assert!(out.len() < 100);
        }
        out
    }

    #[test]
    fn punctuation_is_a_stop_going_right() {
        // foo _ bar . baz ( )
        assert_eq!(stops("|foo_bar.baz()", next_word_end), [3, 4, 7, 8, 11, 12, 13]);
    }

    #[test]
    fn punctuation_is_a_stop_going_left() {
        assert_eq!(
            stops("foo_bar.baz()|", previous_word_start),
            [12, 11, 8, 7, 4, 3, 0]
        );
    }

    #[test]
    fn dash_separates_words() {
        assert_eq!(stops("|a-b", next_word_end), [1, 2, 3]);
        assert_eq!(stops("a-b|", previous_word_start), [2, 1, 0]);
    }

    #[test]
    fn camel_case_subwords() {
        assert_eq!(stops("|fooBarBaz", next_subword_end), [3, 6, 9]);
        assert_eq!(stops("fooBarBaz|", previous_subword_start), [6, 3, 0]);
        assert_eq!(stops("|my_var_name", next_subword_end), [2, 3, 6, 7, 11]);
        assert_eq!(
            stops("my_var_name|", previous_subword_start),
            [7, 6, 3, 2, 0]
        );
        // A plain word move does not stop at the case change.
        assert_eq!(stops("|fooBar", next_word_end), [6]);
    }

    #[test]
    fn unicode_letters_are_word_characters() {
        // "héllo wörld": e-acute and o-umlaut are two bytes each.
        assert_eq!(stops("|héllo wörld", next_word_end), [6, 13]);
        assert_eq!(stops("héllo wörld|", previous_word_start), [7, 0]);
        assert_eq!(stops("|日本語 テスト", next_word_end), [9, 19]);
        // A decomposed accent stays inside its word.
        assert_eq!(stops("|cafe\u{0301} x", next_word_end), [6, 8]);
    }

    #[test]
    fn leading_whitespace_is_skipped() {
        assert_eq!(stops("|   foo bar", next_word_end), [6, 10]);
        assert_eq!(stops("   foo|", previous_word_start), [3, 0]);
        assert_eq!(stops("  |", previous_word_start), [0]);
    }

    #[test]
    fn start_and_end_of_text() {
        assert_eq!(stops("|", next_word_end), Vec::<usize>::new());
        assert_eq!(stops("|", previous_word_start), Vec::<usize>::new());
        assert_eq!(previous_word_start(&rope("foo"), 0), 0);
        assert_eq!(next_word_end(&rope("foo"), 3), 3);
        assert_eq!(next_word_end(&rope("foo  "), 3), 5);
    }

    #[test]
    fn crlf_never_splits_and_is_skipped() {
        let r = rope("one\r\ntwo\r\n");
        assert_eq!(next_word_end(&r, 0), 3);
        assert_eq!(next_word_end(&r, 3), 8);
        assert_eq!(previous_word_start(&r, 8), 5);
        assert_eq!(previous_word_start(&r, 5), 0);
        assert_eq!(next_subword_end(&r, 3), 8);
        assert_eq!(previous_subword_start(&r, 5), 0);
    }

    #[test]
    fn greedy_deletion_stops_after_a_wide_gap() {
        // Backward over "foo    " takes the gap only.
        let (r, at) = caret("foo    |");
        let start = previous_word_start(&r, at);
        assert_eq!(start, 0);
        assert_eq!(adjust_greedy_deletion(&r, at, start), 3);
        // A single space goes with the word.
        let (r, at) = caret("foo |");
        assert_eq!(adjust_greedy_deletion(&r, at, 0), 0);
        // Forward over "    foo" takes the gap only.
        let (r, at) = caret("|    foo");
        let end = next_word_end(&r, at);
        assert_eq!(end, 7);
        assert_eq!(adjust_greedy_deletion(&r, at, end), 4);
    }

    #[test]
    fn greedy_deletion_stops_at_a_bracket() {
        let (r, at) = caret("f(abc|");
        assert_eq!(adjust_greedy_deletion(&r, at, 0), 2);
        let (r, at) = caret("|abc)d");
        assert_eq!(adjust_greedy_deletion(&r, at, 5), 3);
    }
}

#[cfg(test)]
mod mark_tests {
    use super::*;

    fn walk(text: &str, step: fn(&Rope, usize) -> usize, from: usize) -> Vec<usize> {
        let r = Rope::from_str(text);
        let (mut at, mut out) = (from, vec![]);
        loop {
            let next = step(&r, at);
            if next == at {
                return out;
            }
            out.push(next);
            at = next;
        }
    }

    #[test]
    fn combining_marks_stay_with_their_word() {
        // Hindi: the virama is a mark and not a letter, but the word is one stop.
        assert_eq!(walk("नमस्ते x", next_word_end, 0), [18, 20]);
        assert_eq!(walk("नमस्ते x", previous_word_start, 20), [19, 0]);
        // A heart with a variation selector is one stop.
        assert_eq!(walk("❤\u{fe0f} x", next_word_end, 0), [6, 8]);
        assert_eq!(walk("❤\u{fe0f} x", previous_word_start, 8), [7, 0]);
    }

    #[test]
    fn crlf_counts_once_in_greedy_deletion() {
        let lf = Rope::from_str("foo\nbar");
        let crlf = Rope::from_str("foo\r\nbar");
        assert_eq!(adjust_greedy_deletion(&lf, 4, 0), 0);
        assert_eq!(adjust_greedy_deletion(&crlf, 5, 0), 0);
        assert_eq!(adjust_greedy_deletion(&lf, 3, 7), 7);
        assert_eq!(adjust_greedy_deletion(&crlf, 3, 8), 8);
    }
}
