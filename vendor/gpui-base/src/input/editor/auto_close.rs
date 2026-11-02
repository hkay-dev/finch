use std::ops::Range;

/// Delimiters inserted by the editor, retained across edits and history replay.
#[derive(Debug, Clone, Default)]
pub(crate) struct AutoClosedPairs {
    pairs: Vec<(Range<usize>, Range<usize>)>,
}

const MAX_PAIRS: usize = 64;

impl AutoClosedPairs {
    pub(crate) fn empty() -> &'static Self {
        static EMPTY: AutoClosedPairs = AutoClosedPairs { pairs: Vec::new() };
        &EMPTY
    }

    pub(crate) fn record(&mut self, open: Range<usize>, close: Range<usize>) {
        // Pairs the caret walks away from are never released; keep the newest.
        if self.pairs.len() == MAX_PAIRS {
            self.pairs.remove(0);
        }
        self.pairs.push((open, close));
    }

    /// Forgets pairs whose closer ends at `end`: the cursor just typed over it.
    pub(crate) fn release_closer_ending_at(&mut self, end: usize) {
        self.pairs.retain(|(_, close)| close.end != end);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pairs.len()
    }

    pub(crate) fn contains(&self, open: Range<usize>, close: Range<usize>) -> bool {
        self.pairs
            .iter()
            .any(|(begin, end)| *begin == open && *end == close)
    }

    pub(crate) fn contains_closer(&self, cursor: usize, index: usize, len: usize) -> bool {
        self.pairs
            .iter()
            .any(|(_, close)| close.start + index == cursor && close.len() == len)
    }

    pub(crate) fn adjust(&mut self, edit: &Range<usize>, new_len: usize) {
        let delta = new_len as isize - edit.len() as isize;
        let shift = |range: &mut Range<usize>| {
            range.start = range.start.saturating_add_signed(delta);
            range.end = range.end.saturating_add_signed(delta);
        };
        self.pairs.retain_mut(|(open, close)| {
            if edit.end <= open.start {
                shift(open);
                shift(close);
            } else if edit.start >= open.end && edit.end <= close.start {
                shift(close);
            } else if edit.start < close.end {
                return false;
            }
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_keeps_the_newest_pairs() {
        let mut pairs = AutoClosedPairs::default();
        for index in 0..200 {
            pairs.record(index * 2..index * 2 + 1, index * 2 + 1..index * 2 + 2);
        }
        assert_eq!(pairs.len(), MAX_PAIRS);
        assert!(pairs.contains(398..399, 399..400));
        assert!(!pairs.contains(0..1, 1..2));
    }
}
