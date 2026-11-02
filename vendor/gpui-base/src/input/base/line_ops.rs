//! Duplicate line, move lines up or down, delete line, join lines and
//! transpose characters. Duplicate and move are ported from Zed's `duplicate`,
//! `move_line_up` and `move_line_down`.
//!
//! The planning functions work on the rope and plain offsets, so they are
//! tested without a window. `InputBaseState` applies a plan as one undo step.

use std::collections::BTreeMap;
use std::ops::Range;

use gpui::{Context, Window};
use ropey::Rope;

use crate::input::{
    DeleteLine, DuplicateLine, InputBaseState, InputModeKind, InsertLineAbove, InsertLineBelow, JoinLines, MoveLineDown,
    MoveLineUp, OpenLine, RopeExt as _, Transpose,
};

/// Where one row sits in the rope. `content_end..end` is its line terminator
/// (`\n` or `\r\n`), empty on the last row when the text has no final newline.
#[derive(Clone, Copy)]
pub(super) struct RowSpan {
    pub(super) start: usize,
    pub(super) content_end: usize,
    pub(super) end: usize,
}

impl RowSpan {
    fn content_len(&self) -> usize {
        self.content_end - self.start
    }

    fn terminator_len(&self) -> usize {
        self.end - self.content_end
    }
}

pub(super) fn row_span(rope: &Rope, row: usize) -> RowSpan {
    let start = rope.line_start_offset(row);
    let newline = start + rope.slice_line(row).len();
    if row + 1 >= rope.lines_len() {
        return RowSpan {
            start,
            content_end: newline,
            end: newline,
        };
    }
    let crlf = newline > start && rope.char_at(newline - 1) == Some('\r');
    RowSpan {
        start,
        content_end: if crlf { newline - 1 } else { newline },
        end: newline + 1,
    }
}

/// Rows that hold text. The empty row after a final newline is a place for the
/// caret, not a line, so it is neither moved nor moved onto.
pub(super) fn real_rows(rope: &Rope) -> usize {
    let rows = rope.lines_len();
    if rows > 1 && rope.line_start_offset(rows - 1) == rope.len() {
        rows - 1
    } else {
        rows
    }
}

/// The first and last row a selection touches, `None` when it starts past the
/// last real row. A selection ending at the start of a row does not touch it.
pub(super) fn touched_rows(rope: &Rope, rows: usize, &(start, end): &(usize, usize)) -> Option<(usize, usize)> {
    let first = rope.offset_to_point(start).row;
    if first >= rows {
        return None;
    }
    let mut last = rope.offset_to_point(end).row;
    if end > start && last > first && end == rope.line_start_offset(last) {
        last -= 1;
    }
    Some((first, last.min(rows - 1)))
}

/// Sort row ranges and merge those that overlap, or, with `adjacent`, those
/// that also touch end to start.
pub(super) fn merge_rows(mut ranges: Vec<(usize, usize)>, adjacent: bool) -> Vec<RowBlock> {
    ranges.sort_unstable();
    let gap = usize::from(adjacent);
    let mut blocks: Vec<RowBlock> = Vec::new();
    for (first, last) in ranges {
        match blocks.last_mut() {
            Some(block) if first <= block.last + gap => block.last = block.last.max(last),
            _ => blocks.push(RowBlock { first, last }),
        }
    }
    blocks
}

/// The edits for one key press and the selections that result, in the order
/// the selections were given. Edit ranges are disjoint and in the document
/// before any edit.
pub(super) struct LinePlan {
    pub(super) edits: Vec<(Range<usize>, String)>,
    pub(super) selections: Vec<(usize, usize)>,
}

/// A whole line copied below its row.
struct RowCopy {
    text: String,
    terminator_len: usize,
    start: usize,
    content_len: usize,
}

/// Everything inserted at one offset: the selected text copied after a
/// selection ending there, then the row copy for a bare cursor on a row ending
/// there.
#[derive(Default)]
struct DuplicateEdit {
    selected: String,
    row: Option<RowCopy>,
}

impl DuplicateEdit {
    fn len(&self) -> usize {
        self.selected.len() + self.row.as_ref().map_or(0, |row| row.text.len())
    }
}

/// Duplicate each non-empty selection after itself, and each row holding a
/// bare cursor below itself. Cursors on one row share one copy.
pub(super) fn plan_duplicate(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    let mut edits: BTreeMap<usize, DuplicateEdit> = BTreeMap::new();
    for &(start, end) in selections {
        if start != end {
            let edit = edits.entry(end).or_default();
            if edit.selected.is_empty() {
                edit.selected = rope.slice(start..end).to_string();
            }
            continue;
        }
        let row = rope.offset_to_point(start).row;
        let span = row_span(rope, row);
        let edit = edits.entry(span.content_end).or_default();
        if edit.row.is_some() {
            continue;
        }
        let mut text = if span.terminator_len() > 0 {
            rope.slice(span.content_end..span.end).to_string()
        } else if row > 0 {
            let above = row_span(rope, row - 1);
            rope.slice(above.content_end..above.end).to_string()
        } else {
            "\n".to_string()
        };
        let terminator_len = text.len();
        text.push_str(&rope.slice(span.start..span.content_end).to_string());
        edit.row = Some(RowCopy {
            text,
            terminator_len,
            start: span.start,
            content_len: span.content_len(),
        });
    }
    if edits.is_empty() {
        return None;
    }

    // Offset of each edit and the text inserted before it.
    let offsets: Vec<usize> = edits.keys().copied().collect();
    let mut before = Vec::with_capacity(offsets.len());
    let mut total = 0;
    for edit in edits.values() {
        before.push(total);
        total += edit.len();
    }

    let resulting = selections
        .iter()
        .map(|&(start, end)| {
            let key = if start == end {
                row_span(rope, rope.offset_to_point(start).row).content_end
            } else {
                end
            };
            let index = offsets
                .binary_search(&key)
                .expect("every selection has an edit at its key");
            let edit = &edits[&key];
            let copy_start = key + before[index];
            if start != end {
                return (copy_start, copy_start + edit.selected.len());
            }
            let row = edit.row.as_ref().expect("a cursor has a row copy");
            let column = (start - row.start).min(row.content_len);
            let caret = copy_start + edit.selected.len() + row.terminator_len + column;
            (caret, caret)
        })
        .collect();

    Some(LinePlan {
        edits: edits
            .into_iter()
            .map(|(offset, edit)| {
                let mut text = edit.selected;
                if let Some(row) = edit.row {
                    text.push_str(&row.text);
                }
                (offset..offset, text)
            })
            .collect(),
        selections: resulting,
    })
}

/// A run of consecutive rows that move together.
pub(super) struct RowBlock {
    pub(super) first: usize,
    pub(super) last: usize,
}

/// Swap every row the selections touch with the row above or below it.
/// Overlapping and adjacent row ranges move as one block. A block at the edge
/// stays put. Returns `None` when nothing moves.
pub(super) fn plan_move(
    rope: &Rope,
    selections: &[(usize, usize)],
    up: bool,
) -> Option<LinePlan> {
    let rows = real_rows(rope);
    let row_range = |selection: &(usize, usize)| touched_rows(rope, rows, selection);

    let ranges: Vec<(usize, usize)> = selections.iter().filter_map(row_range).collect();
    let blocks = merge_rows(ranges, true);

    // Each block's shift in bytes, `None` when it cannot move.
    let mut shifts: Vec<Option<isize>> = Vec::with_capacity(blocks.len());
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for block in &blocks {
        let first = row_span(rope, block.first);
        let last = row_span(rope, block.last);
        let movable = if up {
            block.first > 0
        } else {
            block.last + 1 < rows
        };
        if !movable {
            shifts.push(None);
            continue;
        }
        let moved = rope.slice(first.start..last.content_end).to_string();
        if up {
            let other = row_span(rope, block.first - 1);
            let separator = rope.slice(other.content_end..first.start).to_string();
            let neighbour = rope.slice(other.start..other.content_end).to_string();
            let shift = other.content_len() + separator.len();
            edits.push((
                other.start..last.content_end,
                format!("{moved}{separator}{neighbour}"),
            ));
            shifts.push(Some(-(shift as isize)));
        } else {
            let other = row_span(rope, block.last + 1);
            let separator = rope.slice(last.content_end..other.start).to_string();
            let neighbour = rope.slice(other.start..other.content_end).to_string();
            let shift = other.content_len() + separator.len();
            edits.push((
                first.start..other.content_end,
                format!("{neighbour}{separator}{moved}"),
            ));
            shifts.push(Some(shift as isize));
        }
    }
    if edits.is_empty() {
        return None;
    }

    let len = rope.len();
    let resulting = selections
        .iter()
        .map(|selection| {
            let Some((first, _)) = row_range(selection) else {
                return *selection;
            };
            let index = blocks.partition_point(|block| block.first <= first) - 1;
            let Some(shift) = shifts[index] else {
                return *selection;
            };
            let block_end = row_span(rope, blocks[index].last).content_end;
            let map = |position: usize| -> usize {
                let moved = if position <= block_end {
                    position as isize + shift
                } else {
                    // In the terminator after the block, which moves with it.
                    (block_end as isize + shift) + (position - block_end) as isize
                };
                (moved as usize).min(len)
            };
            (map(selection.0), map(selection.1))
        })
        .collect();

    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// One run of rows joined into the first row's line.
struct Join {
    first: usize,
    last: usize,
    /// Start of the first row.
    start: usize,
    /// The replaced range: the first row's content end to the last row's.
    from: usize,
    to: usize,
    text: String,
    /// Where each join sits in `text` (before its space), for row `first + i`.
    seams: Vec<usize>,
}

/// Join every row a selection touches with the row below it: one space, the
/// next row's indent dropped. A selection over several rows joins just those
/// rows. The last row has nothing to join. A cursor lands where its row was
/// joined; a selection becomes the joined line. Returns `None` when nothing
/// joins.
pub(super) fn plan_join(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    let rows = real_rows(rope);
    let ranges: Vec<(usize, usize)> = selections
        .iter()
        .filter_map(|selection| {
            let (first, last) = touched_rows(rope, rows, selection)?;
            let last = if last == first { first + 1 } else { last };
            (last < rows).then_some((first, last))
        })
        .collect();
    let joins: Vec<Join> = merge_rows(ranges, false)
        .iter()
        .map(|block| {
            let first = row_span(rope, block.first);
            let mut text = String::new();
            let mut seams = Vec::new();
            let mut last_char = (first.content_end > first.start)
                .then(|| rope.chars_at(first.content_end).prev())
                .flatten();
            for row in block.first + 1..=block.last {
                let span = row_span(rope, row);
                let line = rope.slice(span.start..span.content_end).to_string();
                let piece = line.trim_start();
                seams.push(text.len());
                if !piece.is_empty() && last_char.is_some_and(|c| !c.is_whitespace()) {
                    text.push(' ');
                }
                text.push_str(piece);
                if let Some(c) = piece.chars().next_back() {
                    last_char = Some(c);
                }
            }
            Join {
                first: block.first,
                last: block.last,
                start: first.start,
                from: first.content_end,
                to: row_span(rope, block.last).content_end,
                text,
                seams,
            }
        })
        .collect();
    if joins.is_empty() {
        return None;
    }

    // A position after the joins before it.
    let shift = |position: usize| -> usize {
        let delta: isize = joins
            .iter()
            .filter(|join| join.to <= position)
            .map(|join| join.text.len() as isize - (join.to - join.from) as isize)
            .sum();
        (position as isize + delta) as usize
    };
    let resulting = selections
        .iter()
        .map(|selection| {
            let (start, end) = *selection;
            let join = touched_rows(rope, rows, selection).and_then(|(first, _)| {
                joins
                    .iter()
                    .find(|join| join.first <= first && first <= join.last)
                    .map(|join| (first, join))
            });
            let Some((first, join)) = join else {
                return (shift(start), shift(end));
            };
            let from = shift(join.from);
            if start != end {
                return (shift(join.start), from + join.text.len());
            }
            let seam = join.seams.get(first - join.first).copied().unwrap_or(join.text.len());
            (from + seam, from + seam)
        })
        .collect();

    Some(LinePlan {
        edits: joins
            .into_iter()
            .map(|join| (join.from..join.to, join.text))
            .collect(),
        selections: resulting,
    })
}

/// Delete every row a selection touches, terminator included (a last row
/// without one takes the terminator before it). The caret goes to the same
/// column of the row that took its place, else the row above.
pub(super) fn plan_delete(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    let rows = real_rows(rope);
    let ranges: Vec<(usize, usize)> = selections
        .iter()
        .filter_map(|selection| touched_rows(rope, rows, selection))
        .collect();
    let blocks = merge_rows(ranges, true);
    if blocks.is_empty() {
        return None;
    }
    let deletions: Vec<Range<usize>> = blocks
        .iter()
        .map(|block| {
            let last = row_span(rope, block.last);
            if last.terminator_len() == 0 && block.first > 0 {
                row_span(rope, block.first - 1).content_end..last.end
            } else {
                row_span(rope, block.first).start..last.end
            }
        })
        .collect();
    // A position after the deletions before it; one inside a deletion lands on its start.
    let removed = |position: usize| -> usize {
        let gone: usize = deletions
            .iter()
            .map(|range| position.min(range.end).saturating_sub(range.start))
            .sum();
        position - gone
    };

    let resulting = selections
        .iter()
        .map(|selection| {
            let (start, end) = *selection;
            let Some((first, _)) = touched_rows(rope, rows, selection) else {
                return (removed(start), removed(end));
            };
            let block = &blocks[blocks.partition_point(|block| block.first <= first) - 1];
            let span = row_span(rope, first);
            let column = rope.slice(span.start..start.min(span.content_end)).chars().count();
            let next = block.last + 1;
            let target = if next < rope.lines_len() {
                Some(next)
            } else if block.first > 0 {
                Some(block.first - 1)
            } else {
                None
            };
            let position = target.map_or(0, |row| {
                let row = row_span(rope, row);
                let line = rope.slice(row.start..row.content_end);
                row.start + line.chars().take(column).map(char::len_utf8).sum::<usize>()
            });
            let caret = removed(position);
            (caret, caret)
        })
        .collect();

    Some(LinePlan {
        edits: deletions.into_iter().map(|range| (range, String::new())).collect(),
        selections: resulting,
    })
}

/// Cocoa's transpose for each bare cursor: swap the characters on either side
/// of it and step past them; at the end of a row swap the two before it and
/// stay. A cursor at the start of a row, or after a single character at its
/// end, does nothing, and neither does a selection. Swapping keeps every
/// offset, so only the moved cursors change. Returns `None` when nothing swaps.
pub(super) fn plan_transpose(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    let mut order: Vec<usize> = (0..selections.len())
        .filter(|&index| selections[index].0 == selections[index].1)
        .collect();
    order.sort_by_key(|&index| selections[index].0);

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut resulting = selections.to_vec();
    for index in order {
        let position = selections[index].0;
        let span = row_span(rope, rope.offset_to_point(position).row);
        let (range, text, caret) = if position == span.content_end {
            let mut before = rope.chars_at(position);
            let (Some(second), Some(first)) = (before.prev(), before.prev()) else {
                continue;
            };
            let width = second.len_utf8() + first.len_utf8();
            // Both characters must be on this row.
            if position < span.start + width {
                continue;
            }
            let start = position - width;
            (start..position, format!("{second}{first}"), position)
        } else if position > span.start && position < span.content_end {
            let Some(before) = rope.chars_at(position).prev() else {
                continue;
            };
            let Some(after) = rope.char_at(position) else {
                continue;
            };
            let range = position - before.len_utf8()..position + after.len_utf8();
            let caret = range.end;
            (range, format!("{after}{before}"), caret)
        } else {
            continue;
        };
        if let Some((last, _)) = edits.last() {
            if *last == range {
                // Two cursors at one place share the swap.
                resulting[index] = (caret, caret);
                continue;
            }
            if last.end > range.start {
                continue;
            }
        }
        edits.push((range, text));
        resulting[index] = (caret, caret);
    }
    if edits.is_empty() {
        return None;
    }
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// Control-O: a newline at each caret, which stays in front of it, so the text
/// after the caret moves to a new row below. A selection opens the line at its
/// end. Carets at one place share the newline. Selections in the plan are
/// positions after the edits.
pub(super) fn plan_open_line(selections: &[(usize, usize)]) -> Option<LinePlan> {
    if selections.is_empty() {
        return None;
    }
    let mut order: Vec<usize> = (0..selections.len()).collect();
    order.sort_by_key(|&index| selections[index].0.max(selections[index].1));

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut resulting = selections.to_vec();
    let mut inserted = 0;
    let mut last: Option<usize> = None;
    for index in order {
        let position = selections[index].0.max(selections[index].1);
        if last != Some(position) {
            edits.push((position..position, "\n".to_string()));
            last = Some(position);
            inserted += 1;
        }
        let caret = position + inserted - 1;
        resulting[index] = (caret, caret);
    }
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// The row a selection opens a line next to: its last row for a line below,
/// its first for one above. A caret on the empty row after a final newline is
/// on that row.
fn insert_row(rope: &Rope, sel: &(usize, usize), below: bool) -> usize {
    match touched_rows(rope, real_rows(rope), sel) {
        Some((first, last)) => if below { last } else { first },
        None => rope.offset_to_point(sel.0).row,
    }
}

/// The spaces and tabs a row starts with.
fn leading_indent(rope: &Rope, span: RowSpan) -> String {
    let mut indent = String::new();
    for c in rope.slice(span.start..span.content_end).chars() {
        if c != ' ' && c != '\t' {
            break;
        }
        indent.push(c);
    }
    indent
}

/// Open a line below (or above) the row each selection sits on and put the
/// caret on it after `indents[i]`, the indent for selection `i`. The new line
/// ends with the row's own line ending (the one before it, for a last row
/// with none). Selections on one row share one new line.
pub(super) fn plan_insert_line(rope: &Rope, selections: &[(usize, usize)], below: bool, indents: &[String]) -> Option<LinePlan> {
    if selections.is_empty() {
        return None;
    }
    // Per row: where the line goes, what it holds, and where the caret sits in it.
    struct Opened {
        row: usize,
        at: usize,
        text: String,
        caret: usize,
    }
    let mut opened: Vec<Opened> = Vec::new();
    let mut slot: Vec<usize> = Vec::with_capacity(selections.len());
    // Row to the index of the line opened for it. Two rows can meet at one
    // offset (a last line that ends in a newline and the empty row after it),
    // and each still gets its own line.
    let mut order: Vec<(usize, usize)> = Vec::new();
    for (index, sel) in selections.iter().enumerate() {
        let row = insert_row(rope, sel, below);
        let span = row_span(rope, row);
        let at = if below { span.end } else { span.start };
        let eol = if span.terminator_len() > 0 {
            rope.slice(span.content_end..span.end).to_string()
        } else if row > 0 {
            let before = row_span(rope, row - 1);
            rope.slice(before.content_end..before.end).to_string()
        } else {
            "\n".to_string()
        };
        let indent = &indents[index];
        let (text, caret) = if below && span.terminator_len() == 0 {
            (format!("{eol}{indent}"), eol.len() + indent.len())
        } else {
            (format!("{indent}{eol}"), indent.len())
        };
        match order.iter().find(|&&(open_row, _)| open_row == row) {
            Some(&(_, existing)) => slot.push(existing),
            None => {
                order.push((row, opened.len()));
                slot.push(opened.len());
                opened.push(Opened { row, at, text, caret });
            }
        }
    }
    let mut by_position: Vec<usize> = (0..opened.len()).collect();
    by_position.sort_by_key(|&i| (opened[i].at, opened[i].row));
    // Bytes inserted before each opened line, in document order.
    let mut before = vec![0usize; opened.len()];
    let mut total = 0;
    for &i in &by_position {
        before[i] = total;
        total += opened[i].text.len();
    }
    let resulting = slot
        .iter()
        .map(|&i| {
            let caret = opened[i].at + before[i] + opened[i].caret;
            (caret, caret)
        })
        .collect();
    // Lines that meet at one offset go in as one edit, in row order.
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for &i in &by_position {
        match edits.last_mut() {
            Some((range, text)) if range.start == opened[i].at => text.push_str(&opened[i].text),
            _ => edits.push((opened[i].at..opened[i].at, opened[i].text.clone())),
        }
    }
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

#[derive(Clone, Copy)]
enum LineOp {
    Duplicate,
    MoveUp,
    MoveDown,
    Join,
    Transpose,
    Delete,
    OpenLine,
}

impl<M: InputModeKind> InputBaseState<M> {
    pub(super) fn duplicate_line(
        &mut self,
        _: &DuplicateLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.line_op(LineOp::Duplicate, window, cx);
    }

    pub(super) fn move_line_up(
        &mut self,
        _: &MoveLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.line_op(LineOp::MoveUp, window, cx);
    }

    pub(super) fn move_line_down(
        &mut self,
        _: &MoveLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.line_op(LineOp::MoveDown, window, cx);
    }

    pub(super) fn join_lines(&mut self, _: &JoinLines, window: &mut Window, cx: &mut Context<Self>) {
        self.line_op(LineOp::Join, window, cx);
    }

    pub(super) fn transpose(&mut self, _: &Transpose, window: &mut Window, cx: &mut Context<Self>) {
        self.line_op(LineOp::Transpose, window, cx);
    }

    pub(super) fn delete_line(&mut self, _: &DeleteLine, window: &mut Window, cx: &mut Context<Self>) {
        self.line_op(LineOp::Delete, window, cx);
    }

    pub(super) fn open_line(&mut self, _: &OpenLine, window: &mut Window, cx: &mut Context<Self>) {
        self.line_op(LineOp::OpenLine, window, cx);
    }

    pub(super) fn insert_line_below(&mut self, _: &InsertLineBelow, window: &mut Window, cx: &mut Context<Self>) {
        self.insert_line(true, window, cx);
    }

    pub(super) fn insert_line_above(&mut self, _: &InsertLineAbove, window: &mut Window, cx: &mut Context<Self>) {
        self.insert_line(false, window, cx);
    }

    /// Open a line below (or above) each caret's line, as Return would at the
    /// end of it: below gets the next line's indent, above the line's own.
    fn insert_line(&mut self, below: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text.clone();
        let code = self.is_code_editor();
        let rows: Vec<usize> = self
            .selections
            .iter()
            .map(|sel| insert_row(&text, &(sel.start, sel.end), below))
            .collect();
        let indents: Vec<String> = rows
            .into_iter()
            .map(|row| {
                let span = row_span(&text, row);
                match (code, below) {
                    (false, _) => String::new(),
                    (true, true) => self.indent_of_next_line_at(span.content_end),
                    (true, false) => leading_indent(&text, span),
                }
            })
            .collect();
        self.apply_line_plan(false, |text, ranges| plan_insert_line(text, ranges, below, &indents), window, cx);
    }

    fn line_op(&mut self, op: LineOp, window: &mut Window, cx: &mut Context<Self>) {
        // Duplicate, Move and Join wash the rows they leave changed.
        match op {
            LineOp::Duplicate => self.apply_washed_line_plan(plan_duplicate, window, cx),
            LineOp::MoveUp => self.apply_washed_line_plan(|text, ranges| plan_move(text, ranges, true), window, cx),
            LineOp::MoveDown => self.apply_washed_line_plan(|text, ranges| plan_move(text, ranges, false), window, cx),
            LineOp::Join => self.apply_washed_line_plan(plan_join, window, cx),
            // Transpose also works in a one-line field.
            LineOp::Transpose => self.apply_line_plan(true, plan_transpose, window, cx),
            LineOp::Delete => self.apply_line_plan(false, plan_delete, window, cx),
            LineOp::OpenLine => self.apply_line_plan(false, |_, ranges| plan_open_line(ranges), window, cx),
        }
    }

    /// Plan against the current text and apply the plan as one undo step.
    pub(super) fn apply_line_plan(
        &mut self,
        one_line_ok: bool,
        plan: impl FnOnce(&Rope, &[(usize, usize)]) -> Option<LinePlan>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if (!self.is_multi_line() && !one_line_ok) || !self.is_editable() || self.edit_blocked(window, cx) {
            return;
        }
        let cursors: Vec<_> = self.selections.iter().copied().collect();
        let ranges: Vec<(usize, usize)> = cursors.iter().map(|c| (c.start, c.end)).collect();
        let Some(plan) = plan(&self.text, &ranges) else {
            return;
        };

        let resulting: Vec<_> = cursors
            .iter()
            .zip(&plan.selections)
            .map(|(cursor, &(start, end))| {
                let mut cursor = *cursor;
                cursor.start = start;
                cursor.end = end;
                cursor.column_anchor = None;
                cursor
            })
            .collect();

        self.pause_blink_cursor(cx);
        self.undo_manager.begin_transaction();
        self.undo_manager
            .record_selections(cursors.clone(), cursors.clone());
        self.replace_text_in_ranges(&plan.edits, window, cx);
        self.selections.replace_all(resulting);
        self.selections.merge_overlapping();
        let after = self.selections.iter().copied().collect();
        self.undo_manager.record_selections(cursors, after);
        self.undo_manager.commit_transaction();
        self.update_preferred_column();
        self.scroll_to(self.cursor(), None, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply a plan the way `replace_text_in_ranges` does: highest edit first.
    fn apply(text: &str, plan: &LinePlan) -> String {
        let mut out = text.to_string();
        let mut edits: Vec<_> = plan.edits.iter().collect();
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, new_text) in edits {
            out.replace_range(range.clone(), new_text);
        }
        out
    }

    fn moved(text: &str, selections: &[(usize, usize)], up: bool) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_move(&Rope::from(text), selections, up)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    fn duplicated(text: &str, selections: &[(usize, usize)]) -> (String, Vec<(usize, usize)>) {
        let plan = plan_duplicate(&Rope::from(text), selections).unwrap();
        (apply(text, &plan), plan.selections.clone())
    }

    #[test]
    fn move_up_the_last_line_without_a_newline() {
        let (text, selections) = moved("a\nb\nc", &[(5, 5)], true).unwrap();
        assert_eq!(text, "a\nc\nb");
        assert_eq!(selections, vec![(3, 3)]);
    }

    #[test]
    fn move_down_onto_the_last_line_without_a_newline() {
        let (text, selections) = moved("a\nb\nc", &[(2, 3)], false).unwrap();
        assert_eq!(text, "a\nc\nb");
        assert_eq!(selections, vec![(4, 5)]);
    }

    #[test]
    fn first_line_does_not_move_up_and_last_does_not_move_down() {
        assert!(moved("a\nb", &[(1, 1)], true).is_none());
        assert!(moved("a\nb", &[(3, 3)], false).is_none());
        // The empty row after a final newline is not a line to move onto.
        assert!(moved("a\nb\n", &[(2, 2)], false).is_none());
        assert!(moved("a\nb\n", &[(4, 4)], true).is_none());
    }

    #[test]
    fn selection_covering_a_whole_block_moves_with_its_rows() {
        // "b\nc" selected up to the start of row 3.
        let (text, selections) = moved("a\nb\nc\nd", &[(2, 6)], true).unwrap();
        assert_eq!(text, "b\nc\na\nd");
        assert_eq!(selections, vec![(0, 4)]);
    }

    #[test]
    fn selection_ending_at_a_row_start_leaves_that_row_alone() {
        // Rows 1 only: the selection ends at the start of row 2.
        let (text, _) = moved("a\nb\nc\nd", &[(2, 4)], false).unwrap();
        assert_eq!(text, "a\nc\nb\nd");
    }

    #[test]
    fn cursors_on_overlapping_rows_move_as_one_block() {
        // Cursors on rows 1 and 2 (adjacent), and a selection from row 2 to 3.
        let text = "a\nb\nc\nd\ne";
        let (out, selections) = moved(text, &[(2, 2), (5, 7), (4, 4)], true).unwrap();
        assert_eq!(out, "b\nc\nd\na\ne");
        assert_eq!(selections, vec![(0, 0), (3, 5), (2, 2)]);
    }

    #[test]
    fn separate_blocks_each_move_and_a_block_at_the_edge_stays() {
        let text = "a\nb\nc\nd\ne";
        // Cursors on rows 0 and 3, up: row 0 stays, row 3 swaps with row 2.
        let (out, selections) = moved(text, &[(0, 0), (6, 6)], true).unwrap();
        assert_eq!(out, "a\nb\nd\nc\ne");
        assert_eq!(selections, vec![(0, 0), (4, 4)]);
        // Cursors on rows 1 and 3, down.
        let (out, selections) = moved(text, &[(2, 2), (6, 6)], false).unwrap();
        assert_eq!(out, "a\nc\nb\ne\nd");
        assert_eq!(selections, vec![(4, 4), (8, 8)]);
    }

    #[test]
    fn crlf_lines_keep_their_terminators() {
        let (text, selections) = moved("a\r\nb\r\nc", &[(3, 3)], false).unwrap();
        assert_eq!(text, "a\r\nc\r\nb");
        assert_eq!(selections, vec![(6, 6)]);
        let (text, selections) = moved("a\r\nb\r\nc", &[(7, 7)], true).unwrap();
        assert_eq!(text, "a\r\nc\r\nb");
        assert_eq!(selections, vec![(4, 4)]);
    }

    #[test]
    fn unicode_lines_move_by_bytes() {
        let (text, selections) = moved("é\n世界", &[(3, 3)], true).unwrap();
        assert_eq!(text, "世界\né");
        assert_eq!(selections, vec![(0, 0)]);
    }

    #[test]
    fn duplicate_a_line_keeps_the_caret_column() {
        let (text, selections) = duplicated("ab\ncd\n", &[(1, 1)]);
        assert_eq!(text, "ab\nab\ncd\n");
        assert_eq!(selections, vec![(4, 4)]);
    }

    #[test]
    fn duplicate_the_last_line_without_a_newline() {
        let (text, selections) = duplicated("ab\ncd", &[(4, 4)]);
        assert_eq!(text, "ab\ncd\ncd");
        assert_eq!(selections, vec![(7, 7)]);
    }

    #[test]
    fn duplicate_a_crlf_line_and_the_only_line() {
        let (text, selections) = duplicated("a\r\nbc", &[(4, 4)]);
        assert_eq!(text, "a\r\nbc\r\nbc");
        assert_eq!(selections, vec![(8, 8)]);
        let (text, _) = duplicated("x", &[(0, 0)]);
        assert_eq!(text, "x\nx");
    }

    #[test]
    fn duplicate_a_selection_copies_just_the_text() {
        let (text, selections) = duplicated("hello world", &[(0, 5)]);
        assert_eq!(text, "hellohello world");
        assert_eq!(selections, vec![(5, 10)]);
    }

    #[test]
    fn duplicate_a_selection_and_a_cursor_sharing_an_insert_point() {
        // The cursor's row ends where the selection `cd` ends.
        let (text, selections) = duplicated("abcd\nef", &[(1, 1), (2, 4)]);
        assert_eq!(text, "abcdcd\nabcd\nef");
        assert_eq!(selections, vec![(8, 8), (4, 6)]);
    }

    #[test]
    fn duplicate_with_several_cursors_and_selections() {
        // Two cursors on one row share a copy; a selection on another row.
        let (text, selections) = duplicated("ab\ncd", &[(0, 0), (2, 2), (3, 4)]);
        assert_eq!(text, "ab\nab\nccd");
        assert_eq!(selections, vec![(3, 3), (5, 5), (7, 8)]);
    }

    fn planned(plan: Option<LinePlan>, text: &str) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    fn joined(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        planned(plan_join(&Rope::from(text), selections), text)
    }

    fn deleted(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        planned(plan_delete(&Rope::from(text), selections), text)
    }

    fn transposed(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        planned(plan_transpose(&Rope::from(text), selections), text)
    }

    #[test]
    fn join_a_line_with_the_next_trims_its_indent() {
        let (text, selections) = joined("a\n  b\nc", &[(0, 0)]).unwrap();
        assert_eq!(text, "a b\nc");
        assert_eq!(selections, vec![(1, 1)]);
        let (text, _) = joined("a\n\t\tb", &[(1, 1)]).unwrap();
        assert_eq!(text, "a b");
    }

    #[test]
    fn join_adds_no_space_beside_a_space_or_an_empty_line() {
        assert_eq!(joined("a \n b", &[(0, 0)]).unwrap().0, "a b");
        assert_eq!(joined("a\n\nb", &[(0, 0)]).unwrap().0, "a\nb");
        assert_eq!(joined("\nb", &[(0, 0)]).unwrap().0, "b");
    }

    #[test]
    fn join_the_last_line_without_a_newline_does_nothing() {
        assert!(joined("a\nb", &[(3, 3)]).is_none());
        // The empty row after a final newline is no line either.
        assert!(joined("a\n", &[(2, 2)]).is_none());
        assert!(joined("a\nb", &[(1, 1)]).is_some());
    }

    #[test]
    fn join_with_several_cursors() {
        let (text, selections) = joined("a\nb\nc\nd", &[(0, 0), (4, 4)]).unwrap();
        assert_eq!(text, "a b\nc d");
        assert_eq!(selections, vec![(1, 1), (5, 5)]);
        // Cursors on neighbouring rows join all three rows.
        let (text, selections) = joined("a\nb\nc\nd", &[(0, 0), (2, 2)]).unwrap();
        assert_eq!(text, "a b c\nd");
        assert_eq!(selections, vec![(1, 1), (3, 3)]);
    }

    #[test]
    fn join_a_selection_over_rows_joins_just_those_rows() {
        let (text, selections) = joined("a\n  b\n\tc\nd", &[(0, 7)]).unwrap();
        assert_eq!(text, "a b c\nd");
        assert_eq!(selections, vec![(0, 5)]);
    }

    #[test]
    fn join_crlf_lines_and_move_later_cursors() {
        let (text, selections) = joined("a\r\nb\r\nc\r\nd", &[(0, 0), (6, 6)]).unwrap();
        assert_eq!(text, "a b\r\nc d");
        assert_eq!(selections, vec![(1, 1), (6, 6)]);
        // A cursor on the row that stays last shifts with the joins above it.
        let (text, selections) = joined("a\n b\nc", &[(0, 0), (5, 5)]).unwrap();
        assert_eq!(text, "a b\nc");
        assert_eq!(selections, vec![(1, 1), (4, 4)]);
    }

    #[test]
    fn delete_a_line_and_keep_the_column() {
        let (text, selections) = deleted("a\nb\nc", &[(2, 2)]).unwrap();
        assert_eq!(text, "a\nc");
        assert_eq!(selections, vec![(2, 2)]);
        let (text, selections) = deleted("ab\ncd\nef", &[(4, 4)]).unwrap();
        assert_eq!(text, "ab\nef");
        assert_eq!(selections, vec![(4, 4)]);
    }

    #[test]
    fn delete_the_last_line_without_a_newline_takes_the_terminator_before_it() {
        let (text, selections) = deleted("a\nb", &[(3, 3)]).unwrap();
        assert_eq!(text, "a");
        assert_eq!(selections, vec![(1, 1)]);
        let (text, selections) = deleted("a\r\nb\r\nc", &[(6, 6)]).unwrap();
        assert_eq!(text, "a\r\nb");
        assert_eq!(selections, vec![(3, 3)]);
        let (text, selections) = deleted("only", &[(2, 2)]).unwrap();
        assert_eq!(text, "");
        assert_eq!(selections, vec![(0, 0)]);
    }

    #[test]
    fn delete_a_line_before_a_final_newline_lands_on_the_empty_row() {
        let (text, selections) = deleted("a\nb\n", &[(2, 2)]).unwrap();
        assert_eq!(text, "a\n");
        assert_eq!(selections, vec![(2, 2)]);
        assert!(deleted("a\n", &[(2, 2)]).is_none());
    }

    #[test]
    fn delete_with_several_cursors_and_selections() {
        let (text, selections) = deleted("ab\ncd\nef\ngh", &[(1, 1), (7, 7)]).unwrap();
        assert_eq!(text, "cd\ngh");
        assert_eq!(selections, vec![(1, 1), (4, 4)]);
        // A selection over two rows, ending inside the second, deletes both.
        let (text, selections) = deleted("a\nb\nc\nd", &[(2, 5)]).unwrap();
        assert_eq!(text, "a\nd");
        assert_eq!(selections, vec![(2, 2)]);
        // A selection ending at a row start leaves that row.
        let (text, _) = deleted("a\nb\nc\nd", &[(2, 4)]).unwrap();
        assert_eq!(text, "a\nc\nd");
        // Two cursors on one row delete it once.
        let (text, _) = deleted("a\nbb\nc", &[(2, 2), (4, 4)]).unwrap();
        assert_eq!(text, "a\nc");
    }

    #[test]
    fn transpose_swaps_the_characters_around_each_cursor() {
        let (text, selections) = transposed("abc", &[(1, 1)]).unwrap();
        assert_eq!(text, "bac");
        assert_eq!(selections, vec![(2, 2)]);
        let (text, selections) = transposed("abcd\nefgh", &[(1, 1), (6, 6)]).unwrap();
        assert_eq!(text, "bacd\nfegh");
        assert_eq!(selections, vec![(2, 2), (7, 7)]);
        let (text, selections) = transposed("é世x", &[(2, 2)]).unwrap();
        assert_eq!(text, "世éx");
        assert_eq!(selections, vec![(5, 5)]);
    }

    #[test]
    fn transpose_at_a_row_end_swaps_the_two_before_it() {
        let (text, selections) = transposed("abc\ndef", &[(3, 3)]).unwrap();
        assert_eq!(text, "acb\ndef");
        assert_eq!(selections, vec![(3, 3)]);
        // The last row without a newline.
        let (text, selections) = transposed("ab\ncd", &[(5, 5)]).unwrap();
        assert_eq!(text, "ab\ndc");
        assert_eq!(selections, vec![(5, 5)]);
    }

    #[test]
    fn transpose_needs_two_characters_on_the_row() {
        assert!(transposed("abc", &[(0, 0)]).is_none());
        assert!(transposed("a\nbc", &[(1, 1)]).is_none());
        assert!(transposed("a\nbc", &[(2, 2)]).is_none());
        assert!(transposed("abc", &[(0, 2)]).is_none());
    }

    #[test]
    fn transpose_skips_a_cursor_whose_pair_overlaps_another() {
        let (text, selections) = transposed("abcd", &[(1, 1), (2, 2)]).unwrap();
        assert_eq!(text, "bacd");
        assert_eq!(selections, vec![(2, 2), (2, 2)]);
        let (text, selections) = transposed("abcd", &[(1, 1), (1, 1)]).unwrap();
        assert_eq!(text, "bacd");
        assert_eq!(selections, vec![(2, 2), (2, 2)]);
    }

    fn opened(text: &str, selections: &[(usize, usize)]) -> (String, Vec<(usize, usize)>) {
        let plan = plan_open_line(selections).unwrap();
        (apply(text, &plan), plan.selections.clone())
    }

    #[test]
    fn open_line_mid_line_leaves_the_caret_before_the_new_newline() {
        let (text, selections) = opened("abcd", &[(2, 2)]);
        assert_eq!(text, "ab\ncd");
        assert_eq!(selections, vec![(2, 2)]);
    }

    #[test]
    fn open_line_at_line_end_adds_an_empty_row_below() {
        let (text, selections) = opened("ab\ncd", &[(2, 2)]);
        assert_eq!(text, "ab\n\ncd");
        assert_eq!(selections, vec![(2, 2)]);
    }

    #[test]
    fn open_line_with_two_carets_shifts_the_later_one() {
        let (text, selections) = opened("abcd\nefgh", &[(7, 7), (2, 2)]);
        assert_eq!(text, "ab\ncd\nef\ngh");
        assert_eq!(selections, vec![(8, 8), (2, 2)]);
        // Two carets at one place share one newline.
        let (text, selections) = opened("abcd", &[(2, 2), (2, 2)]);
        assert_eq!(text, "ab\ncd");
        assert_eq!(selections, vec![(2, 2), (2, 2)]);
    }

    #[gpui::test]
    fn each_line_edit_undoes_in_one_step(cx: &mut gpui::TestAppContext) {
        use crate::input::{TextareaMode, TextareaState, Undo, cursor::CursorSelection};

        let original = "a\n  b\ncd\ne";
        cx.update(crate::init);
        let (state, cx) = cx.add_window_view(|window, cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value(original, window, cx);
            state
        });
        let value = |state: &gpui::Entity<InputBaseState<TextareaMode>>,
                     cx: &mut gpui::VisualTestContext| {
            state.read_with(cx, |state, _| state.value().to_string())
        };
        // Cursor at the start, or at the end of row 2 for transpose.
        for (op, offset, expected) in [
            (LineOp::Join, 0, "a b\ncd\ne"),
            (LineOp::Delete, 0, "  b\ncd\ne"),
            (LineOp::Transpose, 8, "a\n  b\ndc\ne"),
            (LineOp::OpenLine, 4, "a\n  \nb\ncd\ne"),
        ] {
            cx.update(|window, cx| {
                state.update(cx, |state, cx| {
                    let id = state.selections.generate_id();
                    state.selections.replace_all(vec![CursorSelection::new(id, offset, offset)]);
                    state.line_op(op, window, cx);
                });
            });
            assert_eq!(value(&state, cx), expected);
            cx.update(|window, cx| {
                state.update(cx, |state, cx| state.undo(&Undo, window, cx));
            });
            assert_eq!(value(&state, cx), original);
        }
    }

    fn inserted(text: &str, selections: &[(usize, usize)], below: bool, indents: &[&str]) -> (String, Vec<(usize, usize)>) {
        let indents: Vec<String> = indents.iter().map(|indent| indent.to_string()).collect();
        let plan = plan_insert_line(&Rope::from(text), selections, below, &indents).unwrap();
        (apply(text, &plan), plan.selections.clone())
    }

    #[test]
    fn insert_line_below_from_the_middle_of_a_line() {
        let (text, selections) = inserted("ab\ncd", &[(1, 1)], true, &[""]);
        assert_eq!(text, "ab\n\ncd");
        assert_eq!(selections, vec![(3, 3)]);
        let (text, selections) = inserted("ab\ncd", &[(1, 1)], false, &[""]);
        assert_eq!(text, "\nab\ncd");
        assert_eq!(selections, vec![(0, 0)]);
    }

    #[test]
    fn insert_line_at_the_first_and_last_line() {
        // Above the first line, and below the last one, which has no newline.
        let (text, selections) = inserted("ab\ncd", &[(4, 4)], true, &[""]);
        assert_eq!(text, "ab\ncd\n");
        assert_eq!(selections, vec![(6, 6)]);
        let (text, selections) = inserted("ab\ncd\n", &[(0, 0)], false, &[""]);
        assert_eq!(text, "\nab\ncd\n");
        assert_eq!(selections, vec![(0, 0)]);
        // The empty row after a final newline gets a row of its own below.
        let (text, selections) = inserted("ab\n", &[(3, 3)], true, &[""]);
        assert_eq!(text, "ab\n\n");
        assert_eq!(selections, vec![(4, 4)]);
    }

    #[test]
    fn insert_line_keeps_the_indent_and_the_line_ending() {
        let (text, selections) = inserted("  ab\r\ncd", &[(3, 3)], true, &["  "]);
        assert_eq!(text, "  ab\r\n  \r\ncd");
        assert_eq!(selections, vec![(8, 8)]);
        let (text, selections) = inserted("x\n  ab\r\ncd", &[(4, 4)], false, &["  "]);
        assert_eq!(text, "x\n  \r\n  ab\r\ncd");
        assert_eq!(selections, vec![(4, 4)]);
        // Without a newline of its own, the last row borrows the one before it.
        let (text, selections) = inserted("a\r\n  b", &[(6, 6)], true, &["  "]);
        assert_eq!(text, "a\r\n  b\r\n  ");
        assert_eq!(selections, vec![(10, 10)]);
    }

    #[test]
    fn insert_line_with_two_carets_gives_each_its_own_row() {
        let (text, selections) = inserted("ab\ncd\nef", &[(1, 1), (7, 7)], true, &["", ""]);
        assert_eq!(text, "ab\n\ncd\nef\n");
        assert_eq!(selections, vec![(3, 3), (10, 10)]);
        // Two carets on one row share one new line; a selection counts by its last row.
        let (text, selections) = inserted("ab\ncd", &[(0, 0), (1, 1), (0, 4)], true, &["", "", ""]);
        assert_eq!(text, "ab\n\ncd\n");
        assert_eq!(selections, vec![(3, 3), (3, 3), (7, 7)]);
        let (text, selections) = inserted("ab\ncd\nef", &[(1, 1), (4, 4)], false, &["", ""]);
        assert_eq!(text, "\nab\n\ncd\nef");
        assert_eq!(selections, vec![(0, 0), (4, 4)]);
    }

    #[test]
    fn insert_line_below_a_final_newline_row_and_the_row_after_it_gives_each_its_own_line() {
        // The last line ends in a newline and the empty row after it starts at the same offset.
        let (text, selections) = inserted("a\n", &[(1, 1), (2, 2)], true, &["", ""]);
        assert_eq!(text, "a\n\n\n");
        assert_eq!(selections, vec![(2, 2), (4, 4)]);
        // The two lines go in as one edit, row 0's first, each caret on its own line.
        let (text, selections) = inserted("  a\n", &[(3, 3), (4, 4)], true, &["  ", ""]);
        assert_eq!(text, "  a\n  \n\n");
        assert_eq!(selections, vec![(6, 6), (8, 8)]);
    }
}
