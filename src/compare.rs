//! Line diff and merge model for comparing unsaved edits ("Yours") with the
//! text now on disk ("Disk"). Pure data: the side-by-side view draws `rows()`
//! and applies `merged()`.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use similar::{Algorithm, DiffOp};

/// How long the line diff may run before we give up and call the whole file one change.
const DIFF_DEADLINE: Duration = Duration::from_millis(200);

/// Which text a change is taken from when merging. `Disk` is also the side an old version of the
/// file stands on when the Compare view shows one (`CompareView::old_version`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Yours,
    Disk,
}

/// One differing run: line ranges (0-based, end-exclusive) in each text.
/// Either range may be empty (a pure insert or delete).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub yours: Range<usize>,
    pub disk: Range<usize>,
}

/// One aligned row of the side-by-side view. A side with no line is blank filler. Three `u32`
/// (12 bytes, with `NONE` standing for "no line") rather than three `Option<usize>` (48): a big
/// file has a row per line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    yours: u32,
    disk: u32,
    change: u32,
}

impl Row {
    const NONE: u32 = u32::MAX;

    fn new(yours: Option<usize>, disk: Option<usize>, change: Option<usize>) -> Self {
        let put = |ix: Option<usize>| ix.map_or(Self::NONE, |ix| u32::try_from(ix).ok().filter(|&ix| ix != Self::NONE).expect("a file has fewer than 2^32 lines"));
        Self { yours: put(yours), disk: put(disk), change: put(change) }
    }

    fn get(ix: u32) -> Option<usize> {
        (ix != Self::NONE).then_some(ix as usize)
    }

    /// The line of `Comparison::yours` on this row, `None` for filler.
    pub fn yours(&self) -> Option<usize> {
        Self::get(self.yours)
    }

    /// The line of `Comparison::disk` on this row, `None` for filler.
    pub fn disk(&self) -> Option<usize> {
        Self::get(self.disk)
    }

    /// Index into `Comparison::changes`, or `None` for an unchanged row.
    pub fn change(&self) -> Option<usize> {
        Self::get(self.change)
    }
}

#[derive(Clone, Debug)]
pub struct Comparison {
    /// Lines of the buffer, line endings stripped. Shared so the view draws them without copying.
    pub yours: Vec<Arc<str>>,
    /// Lines of the disk text, line endings stripped.
    pub disk: Vec<Arc<str>>,
    pub changes: Vec<Change>,
    /// The aligned rows of the side-by-side view, built with the comparison (off the main thread).
    pub rows: Vec<Row>,
    /// Whether each line of `yours` / `disk` ended in a CR, which a merge puts back so a file
    /// with mixed line endings keeps them.
    yours_cr: Vec<bool>,
    disk_cr: Vec<bool>,
}

/// The lines of `text` as slices of it, with CRLF and LF treated alike (the CR is cut off by
/// slicing, nothing is copied). A trailing newline does not start an extra empty line, so a
/// missing final newline is not a difference. One allocation: the vector, sized up front.
pub fn line_slices(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut lines = Vec::with_capacity(body.bytes().filter(|&byte| byte == b'\n').count() + 1);
    lines.extend(body.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)));
    lines
}

/// `line_slices` into shared lines, copying each line once.
fn shared_lines(text: &str) -> (Vec<Arc<str>>, Vec<bool>) {
    if text.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n')
        .map(|l| match l.strip_suffix('\r') {
            Some(stripped) => (Arc::from(stripped), true),
            None => (Arc::from(l), false),
        })
        .unzip()
}

impl Comparison {
    pub fn new(yours: &str, disk: &str) -> Self {
        let (yours, yours_cr) = shared_lines(yours);
        let (mut disk, disk_cr) = shared_lines(disk);
        let changes = diff(&yours, &disk);
        let rows = align(&yours, &mut disk, &changes);
        Self { yours, disk, changes, rows, yours_cr, disk_cr }
    }

    /// The merged text: unchanged lines, and for each change (in order) the
    /// lines of the picked side. `picks` holds one entry per change; missing
    /// entries count as `Yours`. Lines are joined with "\n" (a line that ended
    /// in CRLF keeps its CR) and there is no trailing newline: the caller
    /// re-applies the document's final newline.
    #[cfg(test)]
    pub fn merged(&self, picks: &[Side]) -> String {
        self.merge(picks).0
    }

    /// `merged` and how many lines it holds.
    pub fn merge(&self, picks: &[Side]) -> (String, usize) {
        let mut out = String::new();
        let mut count = 0;
        let mut push = |line: &str, cr: bool| {
            if count > 0 {
                out.push('\n');
            }
            out.push_str(line);
            if cr {
                out.push('\r');
            }
            count += 1;
        };
        let mut at = 0; // cursor in yours
        for (i, change) in self.changes.iter().enumerate() {
            for y in at..change.yours.start {
                push(&self.yours[y], self.yours_cr[y]);
            }
            match picks.get(i).copied().unwrap_or(Side::Yours) {
                Side::Yours => change.yours.clone().for_each(|y| push(&self.yours[y], self.yours_cr[y])),
                Side::Disk => change.disk.clone().for_each(|d| push(&self.disk[d], self.disk_cr[d])),
            }
            at = change.yours.end;
        }
        for y in at..self.yours.len() {
            push(&self.yours[y], self.yours_cr[y]);
        }
        (out, count)
    }
}

/// Aligned rows for two columns. Unchanged lines pair up; a change takes
/// as many rows as its longer side, with filler on the shorter one. Each unchanged disk line is
/// pointed at its twin in `yours`, so the text of a line the two sides share is held once.
fn align(yours: &[Arc<str>], disk: &mut [Arc<str>], changes: &[Change]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(yours.len().max(disk.len()));
    let (mut y, mut d) = (0, 0);
    for (i, change) in changes.iter().enumerate() {
        while y < change.yours.start {
            disk[d] = yours[y].clone();
            rows.push(Row::new(Some(y), Some(d), None));
            y += 1;
            d += 1;
        }
        let (yl, dl) = (change.yours.len(), change.disk.len());
        for k in 0..yl.max(dl) {
            rows.push(Row::new((k < yl).then(|| change.yours.start + k), (k < dl).then(|| change.disk.start + k), Some(i)));
        }
        y = change.yours.end;
        d = change.disk.end;
    }
    while y < yours.len() {
        disk[d] = yours[y].clone();
        rows.push(Row::new(Some(y), Some(d), None));
        y += 1;
        d += 1;
    }
    rows
}

fn diff(yours: &[Arc<str>], disk: &[Arc<str>]) -> Vec<Change> {
    let a: Vec<&str> = yours.iter().map(|line| &**line).collect();
    let b: Vec<&str> = disk.iter().map(|line| &**line).collect();
    // The diff may be partial: show everything as one change.
    line_changes(&a, &b).unwrap_or_else(|| if a == b { Vec::new() } else { vec![Change { yours: 0..a.len(), disk: 0..b.len() }] })
}

/// The differing runs between two texts' lines, within `DIFF_DEADLINE`; `None` when the diff ran out of time
/// (the change bars show nothing then; `Comparison` calls it one change instead).
pub fn line_changes(a: &[&str], b: &[&str]) -> Option<Vec<Change>> {
    let deadline = Instant::now() + DIFF_DEADLINE;
    let ops = similar::capture_diff_deadline(
        Algorithm::Myers,
        a,
        0..a.len(),
        b,
        0..b.len(),
        Some(deadline),
    );
    if Instant::now() >= deadline {
        return None;
    }

    let mut changes: Vec<Change> = Vec::new();
    for op in ops {
        let (yours, disk) = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete { old_index, old_len, new_index } => {
                (old_index..old_index + old_len, new_index..new_index)
            }
            DiffOp::Insert { old_index, new_index, new_len } => {
                (old_index..old_index, new_index..new_index + new_len)
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                (old_index..old_index + old_len, new_index..new_index + new_len)
            }
        };
        match changes.last_mut() {
            Some(last) if last.yours.end == yours.start && last.disk.end == disk.start => {
                last.yours.end = yours.end;
                last.disk.end = disk.end;
            }
            _ => changes.push(Change { yours, disk }),
        }
    }
    Some(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(c: &Comparison, side: Side) -> String {
        c.merged(&vec![side; c.changes.len()])
    }

    #[test]
    fn identical_has_no_changes() {
        let c = Comparison::new("a\nb\nc\n", "a\nb\nc\n");
        assert!(c.changes.is_empty());
        assert_eq!(c.rows.len(), 3);
        assert!(c.rows.iter().all(|r| r.change().is_none()));
    }

    #[test]
    fn one_edited_line() {
        let c = Comparison::new("a\nb\nc", "a\nB\nc");
        assert_eq!(c.changes, vec![Change { yours: 1..2, disk: 1..2 }]);
        assert_eq!(c.merged(&[Side::Disk]), "a\nB\nc");
        assert_eq!(c.merged(&[Side::Yours]), "a\nb\nc");
    }

    #[test]
    fn insert_at_start_and_end() {
        let c = Comparison::new("b", "a\nb\nc");
        assert_eq!(
            c.changes,
            vec![
                Change { yours: 0..0, disk: 0..1 },
                Change { yours: 1..1, disk: 2..3 },
            ]
        );
        assert_eq!(c.merged(&[Side::Disk, Side::Yours]), "a\nb");
        assert_eq!(c.merged(&[Side::Yours, Side::Disk]), "b\nc");
        let rows = c.rows;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], Row::new(None, Some(0), Some(0)));
        assert_eq!(rows[1], Row::new(Some(0), Some(1), None));
        assert_eq!(rows[2], Row::new(None, Some(2), Some(1)));
    }

    #[test]
    fn delete() {
        let c = Comparison::new("a\nb\nc", "a\nc");
        assert_eq!(c.changes, vec![Change { yours: 1..2, disk: 1..1 }]);
        assert_eq!(all(&c, Side::Disk), "a\nc");
        assert_eq!(all(&c, Side::Yours), "a\nb\nc");
    }

    #[test]
    fn crlf_equals_lf() {
        let c = Comparison::new("a\r\nb\r\n", "a\nb\n");
        assert!(c.changes.is_empty());
        let c = Comparison::new("a\r\nb\r\n", "a\nX\n");
        assert_eq!(c.changes.len(), 1);
        // The unchanged line keeps its CRLF; the disk's own line has none.
        assert_eq!(all(&c, Side::Disk), "a\r\nX");
    }

    #[test]
    fn missing_final_newline() {
        let c = Comparison::new("a\nb", "a\nb\n");
        assert!(c.changes.is_empty());
        let c = Comparison::new("a\nb", "a\nc\n");
        assert_eq!(c.changes, vec![Change { yours: 1..2, disk: 1..2 }]);
        assert_eq!(all(&c, Side::Disk), "a\nc");
    }

    #[test]
    fn empty_texts() {
        assert!(Comparison::new("", "").changes.is_empty());
        let c = Comparison::new("", "a\nb");
        assert_eq!(c.changes, vec![Change { yours: 0..0, disk: 0..2 }]);
        assert_eq!(all(&c, Side::Disk), "a\nb");
        assert_eq!(all(&c, Side::Yours), "");
    }

    #[test]
    fn merged_all_and_mixed() {
        let yours = "1\nyours2\n3\nyours4\n5";
        let disk = "1\ndisk2\n3\ndisk4\n5";
        let c = Comparison::new(yours, disk);
        assert_eq!(c.changes.len(), 2);
        assert_eq!(all(&c, Side::Yours), yours);
        assert_eq!(all(&c, Side::Disk), disk);
        assert_eq!(c.merged(&[Side::Disk, Side::Yours]), "1\ndisk2\n3\nyours4\n5");
        assert_eq!(c.merged(&[Side::Yours, Side::Disk]), "1\nyours2\n3\ndisk4\n5");
    }

    #[test]
    fn rows_align_with_filler() {
        let c = Comparison::new("a\nx\ny\nz\nb", "a\nq\nb");
        let rows = c.rows;
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[1], Row::new(Some(1), Some(1), Some(0)));
        assert_eq!(rows[2], Row::new(Some(2), None, Some(0)));
        assert_eq!(rows[4], Row::new(Some(4), Some(2), None));
        assert_eq!((rows[2].yours(), rows[2].disk(), rows[2].change()), (Some(2), None, Some(0)));
    }

    #[test]
    fn a_row_is_twelve_bytes() {
        assert_eq!(std::mem::size_of::<Row>(), 12);
    }

    #[test]
    fn unchanged_disk_lines_are_the_same_allocation_as_yours() {
        let c = Comparison::new("a\nx\nc\nd", "a\ny\nc\nd");
        for (y, d) in [(0, 0), (2, 2), (3, 3)] {
            assert!(Arc::ptr_eq(&c.yours[y], &c.disk[d]), "line {y}");
        }
        assert!(!Arc::ptr_eq(&c.yours[1], &c.disk[1]));
        assert_eq!(&*c.disk[1], "y");
    }

    #[test]
    fn big_files_finish() {
        let a: String = (0..200_000).map(|i| format!("a{i}\n")).collect();
        let b: String = (0..200_000).map(|i| format!("b{i}\n")).collect();
        let start = Instant::now();
        let c = Comparison::new(&a, &b);
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(all(&c, Side::Disk), b.trim_end());
        assert_eq!(all(&c, Side::Yours), a.trim_end());
    }
}
