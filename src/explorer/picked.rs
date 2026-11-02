//! Several rows picked at once, as Finder picks them: Command-click adds a row or takes it out,
//! Shift-click picks the run of rows from the last one clicked.
//!
//! The tree keeps its one selected row, where the selection's shape glides and the keys move
//! from. The picked rows hold only while that row is the one the last pick left selected, so a
//! plain click, an arrow key or a reveal that moves it lets them all go (`Explorer` checks after
//! every change to the tree).

use std::{collections::HashSet, rc::Rc};

use gpui_kit::SharedString;

#[derive(Clone, Default)]
pub struct Picked {
    /// Every picked row's id, the selected row's too. Empty with one row selected, or none.
    pub rows: Rc<HashSet<SharedString>>,
    /// The tree's selected row when they were picked.
    primary: Option<SharedString>,
    /// Where a Shift-click's run starts: the row Command-clicked last, else the selected row.
    anchor: Option<SharedString>,
}

impl Picked {
    /// Whether these picks still hold with `selected` the tree's selected row.
    pub fn holds(&self, selected: Option<&SharedString>) -> bool {
        !self.rows.is_empty() && self.primary.as_ref() == selected
    }

    /// The picks after a Command-click (`run` false) or a Shift-click on row `ix`, and the row
    /// the tree selects. `selected` is the tree's selected row, `id_at` a pickable row's id (none
    /// for a placeholder or past the end) and `index_of` a row's index.
    pub fn pick(
        &self,
        selected: Option<usize>,
        ix: usize,
        run: bool,
        id_at: impl Fn(usize) -> Option<SharedString>,
        index_of: impl Fn(&SharedString) -> Option<usize>,
    ) -> (Picked, Option<usize>) {
        let Some(id) = id_at(ix) else { return (self.clone(), selected) };
        let selected_id = selected.and_then(&id_at);
        let holds = self.holds(selected_id.as_ref());
        let (mut rows, primary, anchor) = if run {
            let anchor = if holds { self.anchor.clone() } else { selected_id.clone() };
            let from = anchor.as_ref().and_then(&index_of).unwrap_or(ix);
            let rows: HashSet<SharedString> = (from.min(ix)..=from.max(ix)).filter_map(&id_at).collect();
            (rows, Some(id.clone()), anchor.or(Some(id)))
        } else {
            let mut rows: HashSet<SharedString> = if holds { (*self.rows).clone() } else { selected_id.iter().cloned().collect() };
            if rows.remove(&id) {
                // The selected row stays selected; when it was the one taken out, the nearest
                // picked row above it takes over (below, when none is above).
                let primary = selected_id.filter(|selected| rows.contains(selected)).or_else(|| {
                    let mut left: Vec<(usize, &SharedString)> = rows.iter().filter_map(|row| Some((index_of(row)?, row))).collect();
                    left.sort_unstable_by_key(|(at, _)| *at);
                    let above = left.iter().rev().find(|(at, _)| *at < ix);
                    above.or(left.first()).map(|(_, row)| (*row).clone())
                });
                (rows, primary.clone(), primary)
            } else {
                rows.insert(id.clone());
                (rows, Some(id.clone()), Some(id))
            }
        };
        let select = primary.as_ref().and_then(&index_of);
        if rows.len() < 2 {
            rows.clear();
        }
        (Picked { rows: Rc::new(rows), primary, anchor }, select)
    }

    /// The picks after Command-A: every row in `ids` (the pickable rows in order). The selected
    /// row stays the one the keys move from, else the first row takes over; the run's anchor is
    /// that row. Returns the picks and that row's id (none when there are no rows).
    pub fn all(&self, selected: Option<&SharedString>, ids: Vec<SharedString>) -> (Picked, Option<SharedString>) {
        let primary = selected.filter(|selected| ids.contains(selected)).or_else(|| ids.first()).cloned();
        let mut rows: HashSet<SharedString> = ids.into_iter().collect();
        if rows.len() < 2 {
            rows.clear();
        }
        (Picked { rows: Rc::new(rows), primary: primary.clone(), anchor: primary.clone() }, primary)
    }

    /// The picks after Shift-Up (`down` false) or Shift-Down with row `selected` the far end of
    /// the run: the far end moves one pickable row, the anchor stays, so the run grows away from
    /// the anchor and shrinks back past it. Returns the picks and the row the tree selects, or
    /// none when there is no row to move to.
    pub fn extend(
        &self,
        selected: Option<usize>,
        down: bool,
        rows: usize,
        id_at: impl Fn(usize) -> Option<SharedString>,
        index_of: impl Fn(&SharedString) -> Option<usize>,
    ) -> Option<(Picked, usize)> {
        let ix = match (selected, down) {
            (Some(from), true) => (from + 1..rows).find(|ix| id_at(*ix).is_some())?,
            (Some(from), false) => (0..from).rev().find(|ix| id_at(*ix).is_some())?,
            (None, true) => (0..rows).find(|ix| id_at(*ix).is_some())?,
            (None, false) => (0..rows).rev().find(|ix| id_at(*ix).is_some())?,
        };
        let (picked, _) = self.pick(selected, ix, true, &id_at, index_of);
        Some((picked, ix))
    }

    /// The picks with the right-clicked row `id` (one of them) selected, so its menu acts on all.
    pub fn select(&self, id: SharedString) -> Picked {
        Picked { primary: Some(id), ..self.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [&str; 5] = ["a", "b", "c", "d", "e"];

    fn click(picked: &Picked, selected: Option<usize>, ix: usize, run: bool) -> (Picked, Option<usize>) {
        picked.pick(selected, ix, run, |ix| ROWS.get(ix).map(|row| SharedString::from(*row)), |id| ROWS.iter().position(|row| *row == id.as_ref()))
    }

    fn names(picked: &Picked) -> Vec<&str> {
        let mut rows: Vec<&str> = picked.rows.iter().map(|row| row.as_ref()).collect();
        rows.sort_unstable();
        rows
    }

    #[test]
    fn command_click_adds_and_takes_out() {
        let (picked, selected) = click(&Picked::default(), Some(0), 2, false);
        assert_eq!((names(&picked), selected), (vec!["a", "c"], Some(2)));
        let (picked, selected) = click(&picked, selected, 4, false);
        assert_eq!((names(&picked), selected), (vec!["a", "c", "e"], Some(4)));
        // Taking out the selected row selects the picked row above it.
        let (picked, selected) = click(&picked, selected, 4, false);
        assert_eq!((names(&picked), selected), (vec!["a", "c"], Some(2)));
        // Down to one row: a plain selection, nothing picked.
        let (picked, selected) = click(&picked, selected, 0, false);
        assert_eq!((names(&picked), selected), (vec![], Some(2)));
        // The last row out leaves nothing selected.
        let (picked, selected) = click(&picked, selected, 2, false);
        assert_eq!((names(&picked), selected), (vec![], None));
    }

    #[test]
    fn shift_click_runs_from_the_last_command_click() {
        let (picked, selected) = click(&Picked::default(), Some(1), 3, true);
        assert_eq!((names(&picked), selected), (vec!["b", "c", "d"], Some(3)));
        // A second Shift-click runs from the same row, the other way.
        let (picked, selected) = click(&picked, selected, 0, true);
        assert_eq!((names(&picked), selected), (vec!["a", "b"], Some(0)));
        let (picked, selected) = click(&picked, selected, 4, false);
        let (picked, selected) = click(&picked, selected, 2, true);
        assert_eq!((names(&picked), selected), (vec!["c", "d", "e"], Some(2)));
    }

    #[test]
    fn picks_let_go_once_the_selection_moves() {
        let (picked, selected) = click(&Picked::default(), Some(0), 2, false);
        assert!(picked.holds(Some(&"c".into())));
        assert!(!picked.holds(Some(&"d".into())));
        // An arrow key moved the selection to d: a Command-click starts from d alone.
        let (picked, _) = click(&picked, selected.map(|_| 3), 4, false);
        assert_eq!(names(&picked), vec!["d", "e"]);
    }

    fn step(picked: &Picked, selected: Option<usize>, down: bool) -> Option<(Picked, usize)> {
        picked.extend(selected, down, ROWS.len(), |ix| ROWS.get(ix).map(|row| SharedString::from(*row)), |id| ROWS.iter().position(|row| *row == id.as_ref()))
    }

    #[test]
    fn shift_arrows_grow_and_shrink_the_run_past_the_anchor() {
        // From b: Shift-Down twice picks b..d with the far end on d.
        let (picked, at) = step(&Picked::default(), Some(1), true).unwrap();
        assert_eq!((names(&picked), at), (vec!["b", "c"], 2));
        let (picked, at) = step(&picked, Some(at), true).unwrap();
        assert_eq!((names(&picked), at), (vec!["b", "c", "d"], 3));
        // Shift-Up shrinks it from the far end, down to the anchor alone.
        let (picked, at) = step(&picked, Some(at), false).unwrap();
        assert_eq!((names(&picked), at), (vec!["b", "c"], 2));
        let (picked, at) = step(&picked, Some(at), false).unwrap();
        assert_eq!((names(&picked), at), (vec![], 1));
        // And on past the anchor the other way: a..b.
        let (picked, at) = step(&picked, Some(at), false).unwrap();
        assert_eq!((names(&picked), at), (vec!["a", "b"], 0));
        // The first row has nothing above it.
        assert!(step(&picked, Some(at), false).is_none());
        // Back down and past the anchor again.
        let (picked, at) = step(&picked, Some(at), true).unwrap();
        assert_eq!((names(&picked), at), (vec![], 1));
        let (picked, at) = step(&picked, Some(at), true).unwrap();
        assert_eq!((names(&picked), at), (vec!["b", "c"], 2));
    }

    #[test]
    fn shift_arrows_stop_at_the_ends_and_skip_rows_that_cannot_be_picked() {
        let (picked, at) = step(&Picked::default(), Some(3), true).unwrap();
        assert_eq!((names(&picked), at), (vec!["d", "e"], 4));
        assert!(step(&picked, Some(at), true).is_none());
        // A row that is not pickable (a placeholder) is stepped over.
        let id_at = |ix: usize| ROWS.get(ix).filter(|_| ix != 2).map(|row| SharedString::from(*row));
        let index_of = |id: &SharedString| ROWS.iter().position(|row| *row == id.as_ref());
        let (picked, at) = Picked::default().extend(Some(1), true, ROWS.len(), id_at, index_of).unwrap();
        assert_eq!((names(&picked), at), (vec!["b", "d"], 3));
    }

    #[test]
    fn command_a_picks_every_row() {
        let ids: Vec<SharedString> = ROWS.iter().map(|row| SharedString::from(*row)).collect();
        let selected = SharedString::from("c");
        let (picked, primary) = Picked::default().all(Some(&selected), ids.clone());
        assert_eq!((names(&picked), primary), (vec!["a", "b", "c", "d", "e"], Some(selected.clone())));
        assert!(picked.holds(Some(&selected)));
        // Shift-Down from there runs from the selected row, the anchor.
        let (picked, at) = step(&picked, Some(2), true).unwrap();
        assert_eq!((names(&picked), at), (vec!["c", "d"], 3));
        // Nothing selected: the first row takes over. One row alone picks nothing.
        let (picked, primary) = Picked::default().all(None, ids);
        assert_eq!((names(&picked).len(), primary), (5, Some("a".into())));
        let (picked, primary) = Picked::default().all(None, vec!["a".into()]);
        assert_eq!((names(&picked), primary), (vec![], Some("a".into())));
    }
}
