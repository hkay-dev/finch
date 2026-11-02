/// FoldMap: Folding projection layer (Wrap rows → Display rows).
///
/// This module manages code folding by:
/// - Filtering out wrap rows that belong to folded regions
/// - Maintaining bidirectional mapping: wrap_row ↔ display_row
/// - Handling fold state changes and rebuilding the projection
use std::ops::Range;

use super::folding::FoldRange;
use super::wrap_map::WrapMap;

/// A run of wrap rows hidden by folding.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HiddenRows {
    wrap_rows: Range<usize>,
    /// How many wrap rows the runs before this one hide.
    hidden_before: usize,
}

impl HiddenRows {
    /// The display row of the first visible wrap row after this run.
    fn display_start(&self) -> usize {
        self.wrap_rows.start - self.hidden_before
    }
}

/// FoldMap projects wrap rows to display rows by hiding folded regions.
pub(super) struct FoldMap {
    /// The wrap rows hidden by folding: sorted, disjoint and non-adjacent.
    ///
    /// The projection is kept as these runs rather than a table per wrap row,
    /// so rebuilding it after an edit costs the number of folds, not the
    /// length of the document, and both directions are a binary search.
    hidden: Vec<HiddenRows>,

    /// How many wrap rows `hidden` hides in total.
    total_hidden: usize,

    /// The wrap row count the projection was last built for.
    projected_wrap_row_count: usize,

    /// Candidate fold ranges (from tree-sitter/LSP)
    /// Sorted by start_line, unique start_line
    candidates: Vec<FoldRange>,

    /// Currently folded ranges
    /// Subset of candidates, sorted by start_line
    folded: Vec<FoldRange>,

    /// Bumped on every change to `folded`.
    generation: u64,

    /// Flag indicating if the fold projection needs rebuilding
    /// Used for lazy evaluation to avoid expensive rebuilds on every text change
    needs_rebuild: bool,

    /// Cached wrap_row_count from last rebuild
    /// Used to detect if WrapMap changed and rebuild is needed
    cached_wrap_row_count: usize,

    /// Bumps whenever the candidate list changes (a parse, an edit's merge or shift).
    candidates_generation: u64,
}

impl FoldMap {
    pub(super) fn new() -> Self {
        Self {
            hidden: Vec::new(),
            total_hidden: 0,
            projected_wrap_row_count: 0,
            candidates: Vec::new(),
            folded: Vec::new(),
            generation: 0,
            needs_rebuild: true,
            cached_wrap_row_count: 0,
            candidates_generation: 0,
        }
    }

    /// Update cached wrap_row_count without full rebuild.
    /// Used when no folds are active (identity mapping assumed).
    pub(super) fn mark_dirty_with_wrap_count(&mut self, wrap_row_count: usize) {
        self.needs_rebuild = true;
        self.cached_wrap_row_count = wrap_row_count;
    }

    /// Get total number of visible display rows
    pub(super) fn display_row_count(&self) -> usize {
        if self.folded.is_empty() {
            return self.cached_wrap_row_count;
        }
        self.projected_wrap_row_count - self.total_hidden
    }

    /// How many wrap rows the runs before `index` hide.
    fn hidden_before(&self, index: usize) -> usize {
        self.hidden
            .get(index)
            .map_or(self.total_hidden, |run| run.hidden_before)
    }

    /// Convert wrap_row to display_row
    /// Returns None if the wrap_row is hidden by folding
    pub(super) fn wrap_row_to_display_row(&self, wrap_row: usize) -> Option<usize> {
        if self.folded.is_empty() {
            return if wrap_row < self.cached_wrap_row_count {
                Some(wrap_row)
            } else {
                None
            };
        }
        if wrap_row >= self.projected_wrap_row_count {
            return None;
        }
        let index = self
            .hidden
            .partition_point(|run| run.wrap_rows.end <= wrap_row);
        if self
            .hidden
            .get(index)
            .is_some_and(|run| run.wrap_rows.start <= wrap_row)
        {
            return None;
        }
        Some(wrap_row - self.hidden_before(index))
    }

    /// Convert display_row to wrap_row
    pub(super) fn display_row_to_wrap_row(&self, display_row: usize) -> Option<usize> {
        if self.folded.is_empty() {
            return if display_row < self.cached_wrap_row_count {
                Some(display_row)
            } else {
                None
            };
        }
        if display_row >= self.display_row_count() {
            return None;
        }
        let index = self
            .hidden
            .partition_point(|run| run.display_start() <= display_row);
        Some(display_row + self.hidden_before(index))
    }

    /// Find the nearest visible display_row for a given wrap_row
    pub(super) fn nearest_visible_display_row(&self, wrap_row: usize) -> usize {
        if self.folded.is_empty() {
            return wrap_row.min(self.cached_wrap_row_count.saturating_sub(1));
        }

        if let Some(dr) = self.wrap_row_to_display_row(wrap_row) {
            return dr;
        }

        // A hidden row maps to the last visible row before it.
        let visible_before = if wrap_row >= self.projected_wrap_row_count {
            self.display_row_count()
        } else {
            let index = self
                .hidden
                .partition_point(|run| run.wrap_rows.end <= wrap_row);
            self.hidden
                .get(index)
                .map_or(self.display_row_count(), HiddenRows::display_start)
        };
        visible_before.saturating_sub(1)
    }

    /// Set fold candidates (from tree-sitter/LSP), full replacement.
    pub(super) fn set_candidates(&mut self, mut candidates: Vec<FoldRange>) {
        // Sort and deduplicate by start_line
        candidates.sort_by_key(|r| r.start_line);
        candidates.dedup_by_key(|r| r.start_line);
        self.candidates = candidates;
        self.bump_candidates();

        // Remove any folded ranges that are no longer in candidates
        let candidates = &self.candidates;
        self.generation += 1;
        self.folded
            .retain(|fold| candidates.binary_search_by_key(&fold.start_line, |c| c.start_line).is_ok());
    }

    fn bump_candidates(&mut self) {
        self.candidates_generation += 1;
    }

    /// Counts every change to the candidate list.
    pub(super) fn candidates_generation(&self) -> u64 {
        self.candidates_generation
    }

    /// A batch replaces candidates after its final parse, but keeps active folds.
    pub(super) fn clear_candidates(&mut self) {
        self.candidates.clear();
        self.bump_candidates();
    }

    /// Merge new candidates extracted from an edited region into existing candidates.
    ///
    /// Replaces candidates within [edit_start_line, edit_end_line] with `new_candidates`,
    /// keeping candidates outside the edit range intact.
    pub(super) fn merge_candidates_for_edit(
        &mut self,
        edit_start_line: usize,
        edit_end_line: usize,
        mut new_candidates: Vec<FoldRange>,
    ) {
        // The list is sorted by start line, so the edited span is found by
        // binary search and only that span is replaced.
        new_candidates.sort_by_key(|c| c.start_line);
        new_candidates.dedup_by_key(|c| c.start_line);
        self.bump_candidates();

        let lo = self
            .candidates
            .partition_point(|c| c.start_line < edit_start_line);
        let hi = lo
            + self.candidates[lo..].partition_point(|c| c.start_line <= edit_end_line);
        let split =
            new_candidates.partition_point(|c| c.start_line < edit_start_line);
        let (head, body) = new_candidates.split_at(split);

        // Candidates that start inside the edited span: the span's old ones go.
        let last_new = body.last().map_or(0, |c| c.start_line);
        if last_new <= edit_end_line {
            self.candidates.splice(lo..hi, body.iter().copied());
        } else {
            // New candidates reaching past the span interleave with survivors.
            let window_end =
                hi + self.candidates[hi..].partition_point(|c| c.start_line <= last_new);
            let mut merged = Vec::with_capacity(body.len() + window_end - hi);
            let (mut kept, mut fresh) = (self.candidates[hi..window_end].iter(), body.iter());
            let (mut a, mut b) = (kept.next(), fresh.next());
            while let (Some(old), Some(new)) = (a, b) {
                if old.start_line <= new.start_line {
                    merged.push(*old);
                    if old.start_line == new.start_line {
                        b = fresh.next();
                    }
                    a = kept.next();
                } else {
                    merged.push(*new);
                    b = fresh.next();
                }
            }
            merged.extend(a.into_iter().chain(kept).copied());
            merged.extend(b.into_iter().chain(fresh).copied());
            self.candidates.splice(lo..window_end, merged);
        }

        // Enclosing candidates start before the span; an existing one with the
        // same start line wins.
        let mut searched = lo;
        for candidate in head {
            if let Err(at) = self.candidates[..searched]
                .binary_search_by_key(&candidate.start_line, |c| c.start_line)
            {
                self.candidates.insert(at, *candidate);
                searched += 1;
            }
        }
    }

    /// Set a fold at the given start_line (must be in candidates).
    ///
    /// Candidates and folds are both sorted by start line with one entry per start line, so
    /// each lookup and the sorted insert are binary searches.
    pub(super) fn set_folded(&mut self, start_line: usize, folded: bool) {
        let at = self.folded.partition_point(|f| f.start_line < start_line);
        let present = self.folded.get(at).is_some_and(|f| f.start_line == start_line);
        if folded {
            if present {
                return;
            }
            let index = self.candidates.partition_point(|c| c.start_line < start_line);
            if let Some(candidate) = self.candidates.get(index).filter(|c| c.start_line == start_line) {
                self.generation += 1;
                self.folded.insert(at, *candidate);
                self.needs_rebuild = true;
            }
        } else {
            if present {
                self.generation += 1;
                self.folded.remove(at);
            }
            self.needs_rebuild = true;
        }
    }

    /// Fold every candidate that hides at least one line, in one pass (no sort per push).
    pub(super) fn fold_all(&mut self) {
        self.generation += 1;
        self.folded.clear();
        self.folded
            .extend(self.candidates.iter().filter(|c| c.end_line > c.start_line + 1).copied());
        self.needs_rebuild = true;
    }

    /// Toggle fold at the given start_line
    pub(super) fn toggle_fold(&mut self, start_line: usize) {
        let is_folded = self.is_folded_at(start_line);
        self.set_folded(start_line, !is_folded);
    }

    /// Check if a line is currently folded
    pub(super) fn is_folded_at(&self, start_line: usize) -> bool {
        let at = self.folded.partition_point(|f| f.start_line < start_line);
        self.folded.get(at).is_some_and(|f| f.start_line == start_line)
    }

    /// Check if a line is a fold candidate
    pub(super) fn is_fold_candidate(&self, start_line: usize) -> bool {
        let at = self.candidates.partition_point(|c| c.start_line < start_line);
        self.candidates.get(at).is_some_and(|c| c.start_line == start_line)
    }

    /// The innermost candidate that reaches `line`, hides at least one line and is not folded
    /// yet. Candidates starting after `line` are cut off by a binary search; of the rest the
    /// last one reaching `line` is the innermost.
    pub(super) fn innermost_unfolded_candidate(&self, line: usize) -> Option<FoldRange> {
        let head = self.candidates.partition_point(|c| c.start_line <= line);
        self.candidates[..head]
            .iter()
            .rev()
            .find(|c| c.end_line >= line && c.end_line > c.start_line + 1 && !self.is_folded_at(c.start_line))
            .copied()
    }

    /// The innermost fold whose first line, last line or hidden body holds `line`.
    pub(super) fn innermost_fold_at(&self, line: usize) -> Option<FoldRange> {
        let head = self.folded.partition_point(|f| f.start_line <= line);
        self.folded[..head].iter().rev().find(|f| f.end_line >= line).copied()
    }

    /// Get all fold candidates
    #[inline]
    pub(super) fn fold_candidates(&self) -> &[FoldRange] {
        &self.candidates
    }

    /// Get all currently folded ranges
    #[inline]
    pub(super) fn folded_ranges(&self) -> &[FoldRange] {
        &self.folded
    }

    /// Counts every change to the folded list, so a reader can tell it is the same list.
    #[inline]
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// Clear all folds
    #[inline]
    pub(super) fn clear_folds(&mut self) {
        self.generation += 1;
        self.folded.clear();
    }

    /// Adjust folds and candidates after a text edit.
    ///
    /// - Folds/candidates overlapping the edited line range are removed
    /// - Folds/candidates after the edit are shifted by line_delta
    ///
    /// This avoids expensive full tree traversal on every keystroke.
    pub(super) fn adjust_folds_for_edit(
        &mut self,
        edit_start_line: usize,
        edit_end_line: usize,
        line_delta: isize,
    ) {
        // Adjust folded ranges
        if !self.folded.is_empty() {
            self.generation += 1;
            self.folded.retain(|fold| {
                !(fold.start_line <= edit_end_line && fold.end_line >= edit_start_line)
            });

            if line_delta != 0 {
                for fold in &mut self.folded {
                    if fold.start_line > edit_end_line {
                        fold.start_line = (fold.start_line as isize + line_delta).max(0) as usize;
                        fold.end_line = (fold.end_line as isize + line_delta).max(0) as usize;
                    }
                }
            }
        }

        // Adjust candidates the same way. The list is sorted by start line:
        // the span's own candidates are one run, the ones before it are kept
        // unless they enclose the edit, and only the tail is shifted.
        if !self.candidates.is_empty() {
            let lo = self
                .candidates
                .partition_point(|c| c.start_line < edit_start_line);
            let hi = lo
                + self.candidates[lo..].partition_point(|c| c.start_line <= edit_end_line);
            let mut kept = 0;
            for read in 0..lo {
                if self.candidates[read].end_line < edit_start_line {
                    self.candidates[kept] = self.candidates[read];
                    kept += 1;
                }
            }
            self.candidates.drain(kept..hi);

            if line_delta != 0 {
                for c in &mut self.candidates[kept..] {
                    c.start_line = (c.start_line as isize + line_delta).max(0) as usize;
                    c.end_line = (c.end_line as isize + line_delta).max(0) as usize;
                }
            }
        }

        self.needs_rebuild = true;
        self.bump_candidates();
    }

    /// Rebuild the fold mapping after wrap_map or fold state changes
    ///
    /// This is the core algorithm that projects wrap rows to display rows.
    pub(super) fn rebuild(&mut self, wrap_map: &WrapMap) {
        let wrap_row_count = wrap_map.wrap_row_count();

        // Performance optimization: skip rebuild if nothing changed
        if !self.needs_rebuild && wrap_row_count == self.cached_wrap_row_count {
            return;
        }

        self.cached_wrap_row_count = wrap_row_count;

        if self.folded.is_empty() {
            // Fast path: no folds, all wrap rows are visible
            self.set_hidden_rows(wrap_row_count, Vec::new());
            self.needs_rebuild = false;
            return;
        }

        // Build set of hidden wrap_row ranges from folded buffer lines
        let mut hidden_ranges = Vec::new();
        for fold in &self.folded {
            // Hide wrap rows from (start_line + 1) to (end_line - 1) (inclusive)
            // Both the first line and last line of the fold remain visible
            let hide_start_line = fold.start_line + 1;
            let hide_end_line = fold.end_line.saturating_sub(1);

            if hide_start_line > hide_end_line {
                continue; // No middle lines to hide (0 or 1 lines between start and end)
            }

            // Get wrap_row ranges for the hidden buffer lines
            let start_wrap_row = wrap_map.buffer_line_to_first_wrap_row(hide_start_line);
            let end_wrap_row = if hide_end_line + 1 < wrap_map.buffer_line_count() {
                wrap_map.buffer_line_to_first_wrap_row(hide_end_line + 1)
            } else {
                wrap_row_count
            };

            if start_wrap_row < end_wrap_row {
                hidden_ranges.push(start_wrap_row..end_wrap_row);
            }
        }

        self.set_hidden_rows(wrap_row_count, hidden_ranges);
        self.needs_rebuild = false;
    }

    /// Install the projection for `wrap_row_count` wrap rows with
    /// `hidden_ranges` hidden, merging overlapping and adjacent ranges.
    fn set_hidden_rows(&mut self, wrap_row_count: usize, mut hidden_ranges: Vec<Range<usize>>) {
        hidden_ranges.sort_by_key(|range| range.start);

        self.hidden.clear();
        self.total_hidden = 0;
        self.projected_wrap_row_count = wrap_row_count;
        for range in hidden_ranges {
            let range = range.start..range.end.min(wrap_row_count);
            if range.is_empty() {
                continue;
            }
            if let Some(last) = self.hidden.last_mut()
                && range.start <= last.wrap_rows.end
            {
                // Overlapping or adjacent, merge
                if range.end > last.wrap_rows.end {
                    self.total_hidden += range.end - last.wrap_rows.end;
                    last.wrap_rows.end = range.end;
                }
                continue;
            }
            self.total_hidden += range.len();
            self.hidden.push(HiddenRows {
                hidden_before: self.total_hidden - range.len(),
                wrap_rows: range,
            });
        }
    }
}

/// The most bytes [`whole_lines`] adds around an edit before it gives up, so
/// a minified one-line file never turns each key into a walk of the line.
const WHOLE_LINES_SLACK: usize = 16 * 1024;

/// The edited byte range widened to the whole lines it touches.
///
/// Candidates are replaced by line overlap, so the region they are extracted
/// from must cover the same lines; a byte range would miss a neighbour on the
/// same line and drop it. Past [`WHOLE_LINES_SLACK`] extra bytes the range is
/// returned unchanged.
pub(super) fn whole_lines(text: &ropey::Rope, bytes: Range<usize>) -> Range<usize> {
    let len = text.len();
    let start = bytes.start.min(len);
    let end = bytes.end.min(len);
    let first = text.byte_to_line_idx(start, ropey::LineType::LF);
    let last = text.byte_to_line_idx(end, ropey::LineType::LF);
    let from = text.line_to_byte_idx(first, ropey::LineType::LF);
    let to = if last + 1 < text.len_lines(ropey::LineType::LF) {
        text.line_to_byte_idx(last + 1, ropey::LineType::LF)
    } else {
        len
    };
    if (start - from) + (to - end) > WHOLE_LINES_SLACK {
        return bytes;
    }
    from..to
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brace pairs spanning at least three lines, outermost first on a shared
    /// start line, optionally only those overlapping `lines`.
    fn extract(text: &str, lines: Option<(usize, usize)>) -> Vec<FoldRange> {
        let mut stack = Vec::new();
        let mut found = Vec::new();
        for (row, line) in text.split('\n').enumerate() {
            for ch in line.chars() {
                match ch {
                    '{' => stack.push(row),
                    '}' => {
                        if let Some(start) = stack.pop() {
                            if row - start >= 2 {
                                found.push(FoldRange::new(start, row));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some((first, last)) = lines {
            found.retain(|c| c.start_line <= last && c.end_line >= first);
        }
        found.sort_by_key(|c| (c.start_line, std::cmp::Reverse(c.end_line)));
        found.dedup_by_key(|c| c.start_line);
        found
    }

    /// The retain, extend, sort and dedup this module did before.
    fn reference_adjust(
        candidates: &mut Vec<FoldRange>,
        start: usize,
        end: usize,
        delta: isize,
    ) {
        candidates.retain(|c| !(c.start_line <= end && c.end_line >= start));
        if delta != 0 {
            for c in candidates.iter_mut() {
                if c.start_line > end {
                    c.start_line = (c.start_line as isize + delta).max(0) as usize;
                    c.end_line = (c.end_line as isize + delta).max(0) as usize;
                }
            }
        }
    }

    fn reference_merge(
        candidates: &mut Vec<FoldRange>,
        start: usize,
        end: usize,
        new: Vec<FoldRange>,
    ) {
        candidates.retain(|c| c.start_line < start || c.start_line > end);
        candidates.extend(new);
        candidates.sort_by_key(|r| r.start_line);
        candidates.dedup_by_key(|r| r.start_line);
    }

    fn row_of(text: &str, offset: usize) -> usize {
        text.as_bytes()[..offset].iter().filter(|b| **b == b'\n').count()
    }

    fn rust_text(blocks: usize) -> String {
        "fn f() {\n    if x {\n        y;\n    }\n}\n".repeat(blocks)
    }

    /// Like the syntax tree walk: brace pairs of three or more lines whose
    /// bytes overlap `bytes`, outermost first on a shared start line.
    fn extract_in_bytes(text: &str, bytes: Range<usize>) -> Vec<FoldRange> {
        let mut stack = Vec::new();
        let mut found = Vec::new();
        let mut row = 0;
        for (at, ch) in text.char_indices() {
            match ch {
                '\n' => row += 1,
                '{' => stack.push((at, row)),
                '}' => {
                    if let Some((open, start)) = stack.pop() {
                        if row - start >= 2 && at + 1 > bytes.start && open < bytes.end {
                            found.push((FoldRange::new(start, row), open));
                        }
                    }
                }
                _ => {}
            }
        }
        found.sort_by_key(|(c, open)| (c.start_line, *open));
        found.dedup_by_key(|(c, _)| c.start_line);
        found.into_iter().map(|(c, _)| c).collect()
    }

    /// One incremental update: adjust, extract the edited region, merge.
    fn edit_and_update(
        map: &mut FoldMap,
        text: &mut String,
        at: usize,
        removed: usize,
        insert: &str,
        widen: bool,
    ) {
        let old_start = row_of(text, at);
        let old_end = row_of(text, at + removed);
        let delta = insert.matches('\n').count() as isize - (old_end - old_start) as isize;
        text.replace_range(at..at + removed, insert);
        let bytes = at..at + insert.len();
        let new_start = row_of(text, bytes.start);
        let new_end = row_of(text, bytes.end);
        let region = if widen {
            whole_lines(&ropey::Rope::from_str(text), bytes)
        } else {
            bytes
        };
        map.adjust_folds_for_edit(old_start, old_end, delta);
        map.merge_candidates_for_edit(new_start, new_end, extract_in_bytes(text, region));
    }

    #[test]
    fn same_line_neighbours_survive_an_edit_that_touches_one() {
        // Line 3 closes `a` and opens `b`.
        let source = "fn a() {\n x;\n y;\n} fn b() {\n x;\n y;\n}\n";
        let both = vec![FoldRange::new(0, 3), FoldRange::new(3, 6)];
        for (at, removed, insert) in [(source.find("} fn").unwrap(), 0, "z"), (source.find("b() {").unwrap() + 5, 0, " ")] {
            let mut text = source.to_string();
            let mut map = FoldMap::new();
            map.set_candidates(extract_in_bytes(&text, 0..usize::MAX));
            assert_eq!(map.fold_candidates(), &both[..]);

            // The byte range alone loses a candidate the edit never touched.
            let mut narrow = text.clone();
            let mut narrow_map = FoldMap::new();
            narrow_map.set_candidates(both.clone());
            edit_and_update(&mut narrow_map, &mut narrow, at, removed, insert, false);
            assert_ne!(narrow_map.fold_candidates(), &extract_in_bytes(&narrow, 0..usize::MAX)[..]);

            edit_and_update(&mut map, &mut text, at, removed, insert, true);
            assert_eq!(map.fold_candidates(), &extract_in_bytes(&text, 0..usize::MAX)[..]);
        }
    }

    #[test]
    fn whole_lines_widens_to_line_bounds_and_gives_up_on_a_huge_line() {
        let text = ropey::Rope::from_str("ab\ncd\nef");
        assert_eq!(whole_lines(&text, 4..4), 3..6);
        assert_eq!(whole_lines(&text, 1..4), 0..6);
        assert_eq!(whole_lines(&text, 7..8), 6..8);
        assert_eq!(whole_lines(&text, 8..8), 6..8);
        let long = ropey::Rope::from_str(&"x".repeat(5 * 1024 * 1024));
        assert_eq!(whole_lines(&long, 2_000_000..2_000_001), 2_000_000..2_000_001);
    }

    /// One edit at a time, like typing: returns the old and new line spans.
    fn random_edit(text: &mut String, seed: &mut u64) -> (usize, usize, isize, usize, usize) {
        let mut next = |n: usize| {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*seed >> 33) as usize) % n
        };
        const SNIPPETS: &[&str] = &["", "x", "\n", "\n\n", "{\n\n\n}", "fn g() {\n a;\n b;\n}\n"];
        let line_count = text.matches('\n').count() + 1;
        let row = next(line_count);
        let line_start = text.split('\n').take(row).map(|l| l.len() + 1).sum::<usize>();
        let line_len = text[line_start..].find('\n').unwrap_or(text.len() - line_start);
        let has_brace = text[line_start..line_start + line_len].contains(['{', '}']);
        let (range, snippet) = if has_brace {
            (line_start..line_start, SNIPPETS[next(SNIPPETS.len())])
        } else {
            let a = line_start + next(line_len + 1);
            let b = (a + next(line_len + 2)).min(text.len()).min(line_start + line_len + 1);
            (a..b, SNIPPETS[next(SNIPPETS.len())])
        };
        let old_start = row_of(text, range.start);
        let old_end = row_of(text, range.end);
        let delta = snippet.matches('\n').count() as isize - (old_end - old_start) as isize;
        text.replace_range(range.clone(), snippet);
        let new_start = row_of(text, range.start);
        let new_end = row_of(text, range.start + snippet.len());
        (old_start, old_end, delta, new_start, new_end)
    }

    #[test]
    fn single_edit_candidates_match_the_old_algorithm_and_a_fresh_extraction() {
        let mut text = rust_text(2_000);
        assert_eq!(text.matches('\n').count(), 10_000);
        let mut map = FoldMap::new();
        map.set_candidates(extract(&text, None));
        let mut reference = map.fold_candidates().to_vec();
        let mut seed = 7;
        for step in 0..400 {
            let (old_start, old_end, delta, new_start, new_end) = random_edit(&mut text, &mut seed);
            let fresh_span = extract(&text, Some((new_start, new_end)));

            map.adjust_folds_for_edit(old_start, old_end, delta);
            map.merge_candidates_for_edit(new_start, new_end, fresh_span.clone());
            reference_adjust(&mut reference, old_start, old_end, delta);
            reference_merge(&mut reference, new_start, new_end, fresh_span);

            assert_eq!(map.fold_candidates(), &reference[..], "step {step}");
            assert_eq!(map.fold_candidates(), &extract(&text, None)[..], "step {step}");
        }
    }

    #[test]
    fn merge_keeps_survivors_over_new_candidates_that_reach_past_the_span() {
        let mut map = FoldMap::new();
        map.set_candidates(vec![
            FoldRange::new(2, 4),
            FoldRange::new(10, 14),
            FoldRange::new(20, 25),
        ]);
        map.merge_candidates_for_edit(
            5,
            6,
            vec![
                FoldRange::new(20, 99),
                FoldRange::new(2, 40),
                FoldRange::new(5, 9),
                FoldRange::new(12, 15),
            ],
        );
        assert_eq!(
            map.fold_candidates(),
            &[
                FoldRange::new(2, 4),
                FoldRange::new(5, 9),
                FoldRange::new(10, 14),
                FoldRange::new(12, 15),
                FoldRange::new(20, 25),
            ]
        );
    }

    #[test]
    fn single_edit_cost_with_30k_candidates() {
        let candidates: Vec<FoldRange> = (0..30_000)
            .map(|row| FoldRange::new(row * 4, row * 4 + 3))
            .collect();
        let mut old = candidates.clone();
        let mut map = FoldMap::new();
        map.set_candidates(candidates);
        let edits = 2_000;
        let at = 60_000;
        let fresh = vec![FoldRange::new(at, at + 3)];

        let began = std::time::Instant::now();
        for _ in 0..edits {
            reference_adjust(&mut old, at, at, 0);
            reference_merge(&mut old, at, at, fresh.clone());
        }
        let old_time = began.elapsed();
        let began = std::time::Instant::now();
        for _ in 0..edits {
            map.adjust_folds_for_edit(at, at, 0);
            map.merge_candidates_for_edit(at, at, fresh.clone());
        }
        let new_time = began.elapsed();
        assert_eq!(map.fold_candidates(), &old[..]);
        println!(
            "30k candidates, per edit: old {:?}, new {:?}; old visits ~{} (retain, retain, extend, sort, dedup), new visits ~{} (enclosure scan of the head)",
            old_time / edits,
            new_time / edits,
            5 * 30_000,
            15_000
        );
    }

    #[test]
    fn replay_batch_discards_candidates_but_preserves_active_folds() {
        let mut map = FoldMap::new();
        map.set_candidates((0..20_000).map(|row| FoldRange::new(row * 3, row * 3 + 1)).collect());
        map.set_folded(59_997, true);
        map.clear_candidates();
        for _ in 0..20_000 {
            map.adjust_folds_for_edit(0, 0, 1);
            assert!(map.fold_candidates().is_empty());
        }
        assert_eq!(map.folded_ranges(), &[FoldRange::new(79_997, 79_998)]);
        map.set_candidates(vec![FoldRange::new(79_997, 79_998)]);
        assert!(map.is_folded_at(79_997));
        println!("20k replay fold candidate merges: old=20000, batch=0; full replacements=1");
    }

    /// The per-row tables the projection used to be stored as: display row to
    /// wrap row, and wrap row to display row.
    fn dense_projection(
        wrap_row_count: usize,
        hidden: &[Range<usize>],
    ) -> (Vec<usize>, Vec<Option<usize>>) {
        let mut visible = Vec::new();
        let mut display_rows = vec![None; wrap_row_count];
        for (wrap_row, display_row) in display_rows.iter_mut().enumerate() {
            if !hidden.iter().any(|range| range.contains(&wrap_row)) {
                *display_row = Some(visible.len());
                visible.push(wrap_row);
            }
        }
        (visible, display_rows)
    }

    #[test]
    fn hidden_runs_match_the_dense_projection() {
        let wrap_row_count = 10;
        let cases: &[&[Range<usize>]] = &[
            &[],
            &[0..3],
            &[2..5],
            &[7..10],
            &[2..4, 4..6],
            &[1..5, 3..8],
            &[6..9, 1..3],
            &[0..2, 3..4, 8..20],
            &[0..10],
        ];
        for &hidden in cases {
            let mut fold_map = FoldMap::new();
            fold_map.folded.push(FoldRange::new(0, 1));
            fold_map.set_hidden_rows(wrap_row_count, hidden.to_vec());
            let (visible, display_rows) = dense_projection(wrap_row_count, hidden);

            assert_eq!(fold_map.display_row_count(), visible.len(), "{hidden:?}");
            for wrap_row in 0..wrap_row_count + 2 {
                let display_row = display_rows.get(wrap_row).copied().flatten();
                assert_eq!(
                    fold_map.wrap_row_to_display_row(wrap_row),
                    display_row,
                    "{hidden:?}, wrap row {wrap_row}"
                );
                let nearest = match visible.binary_search(&wrap_row) {
                    Ok(index) => index,
                    Err(index) => index.saturating_sub(1),
                };
                assert_eq!(
                    fold_map.nearest_visible_display_row(wrap_row),
                    nearest,
                    "{hidden:?}, wrap row {wrap_row}"
                );
            }
            for display_row in 0..wrap_row_count + 2 {
                assert_eq!(
                    fold_map.display_row_to_wrap_row(display_row),
                    visible.get(display_row).copied(),
                    "{hidden:?}, display row {display_row}"
                );
            }
        }
    }

    #[test]
    fn fold_candidate_and_fold_lookups_are_binary_searches_over_30k() {
        let mut map = FoldMap::new();
        map.set_candidates((0..30_000).map(|row| FoldRange::new(row * 4, row * 4 + 3)).collect());
        for row in [0, 1, 14_999, 29_999] {
            assert!(map.is_fold_candidate(row * 4));
            assert!(!map.is_fold_candidate(row * 4 + 1));
            assert!(!map.is_fold_candidate(row * 4 + 3));
        }
        assert!(!map.is_fold_candidate(30_000 * 4));
        map.set_folded(8, true);
        map.set_folded(0, true);
        map.set_folded(40, true);
        map.set_folded(8, true);
        assert_eq!(map.folded_ranges(), &[FoldRange::new(0, 3), FoldRange::new(8, 11), FoldRange::new(40, 43)]);
        assert!(map.is_folded_at(8) && !map.is_folded_at(9));
        map.set_folded(8, false);
        assert_eq!(map.folded_ranges(), &[FoldRange::new(0, 3), FoldRange::new(40, 43)]);
        map.set_folded(5, true);
        assert_eq!(map.folded_ranges().len(), 2, "a line that is no candidate folds nothing");
    }

    #[test]
    fn fold_all_folds_every_candidate_that_hides_a_line_in_one_pass() {
        let mut map = FoldMap::new();
        let mut candidates: Vec<FoldRange> = (0..30_000).map(|row| FoldRange::new(row * 4 + 10, row * 4 + 13)).collect();
        // Nested inside the first; and a two-line range that hides nothing.
        candidates.push(FoldRange::new(11, 13));
        candidates.push(FoldRange::new(2, 3));
        map.set_candidates(candidates);
        let began = std::time::Instant::now();
        map.fold_all();
        let took = began.elapsed();
        assert_eq!(map.folded_ranges().len(), 30_000 + 1);
        assert!(map.folded_ranges().windows(2).all(|pair| pair[0].start_line < pair[1].start_line));
        assert!(!map.is_folded_at(2));
        println!("fold_all over 30k candidates: {took:?}");
        map.clear_folds();
        assert!(map.folded_ranges().is_empty());
    }

    #[test]
    fn caret_lookups_pick_the_innermost_range() {
        let mut map = FoldMap::new();
        map.set_candidates(vec![FoldRange::new(0, 20), FoldRange::new(2, 10), FoldRange::new(4, 8), FoldRange::new(30, 40)]);
        assert_eq!(map.innermost_unfolded_candidate(5), Some(FoldRange::new(4, 8)));
        assert_eq!(map.innermost_unfolded_candidate(9), Some(FoldRange::new(2, 10)));
        assert_eq!(map.innermost_unfolded_candidate(25), None);
        map.set_folded(4, true);
        assert_eq!(map.innermost_unfolded_candidate(5), Some(FoldRange::new(2, 10)));
        assert_eq!(map.innermost_fold_at(5), Some(FoldRange::new(4, 8)));
        assert_eq!(map.innermost_fold_at(8), Some(FoldRange::new(4, 8)));
        assert_eq!(map.innermost_fold_at(9), None);
    }

    #[test]
    fn candidates_generation_moves_with_candidates_only() {
        let mut map = FoldMap::new();
        let start = map.candidates_generation();
        map.set_candidates(vec![FoldRange::new(0, 20), FoldRange::new(2, 10)]);
        let set = map.candidates_generation();
        assert_ne!(set, start);
        // What every prepaint does (a font or layout set marks the projection dirty) and a fold toggle leave it alone.
        map.mark_dirty_with_wrap_count(30);
        map.set_folded(2, true);
        map.set_folded(2, false);
        assert_eq!(map.candidates_generation(), set);
        map.merge_candidates_for_edit(3, 4, vec![FoldRange::new(3, 8)]);
        assert_ne!(map.candidates_generation(), set);
    }
}
