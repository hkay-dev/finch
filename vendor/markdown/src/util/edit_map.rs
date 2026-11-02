//! Deal with several changes in events, batching them together.
//!
//! Preferably, changes should be kept to a minimum.
//! Sometimes, it’s needed to change the list of events, because parsing can be
//! messy, and it helps to expose a cleaner interface of events to the compiler
//! and other users.
//! It can also help to merge many adjacent similar events.
//! And, in other cases, it’s needed to parse subcontent: pass some events
//! through another tokenizer and inject the result.

use crate::event::Event;
use alloc::{
    collections::{btree_map::Entry, BTreeMap},
    vec::Vec,
};

/// Shift `previous` and `next` links according to `jumps`.
///
/// This fixes links in case there are events removed or added between them.
fn shift_links(events: &mut [Event], jumps: &[(usize, usize, usize)]) {
    let mut jump_index = 0;
    let mut index = 0;
    let mut add = 0;
    let mut rm = 0;

    while index < events.len() {
        let rm_curr = rm;

        while jump_index < jumps.len() && jumps[jump_index].0 <= index {
            add = jumps[jump_index].2;
            rm = jumps[jump_index].1;
            jump_index += 1;
        }

        // Ignore items that will be removed.
        if rm > rm_curr {
            index += rm - rm_curr;
        } else {
            if let Some(link) = &events[index].link {
                if let Some(next) = link.next() {
                    events[next]
                        .link
                        .as_mut()
                        .unwrap()
                        .set_previous(Some(index + add - rm));

                    while jump_index < jumps.len() && jumps[jump_index].0 <= next {
                        add = jumps[jump_index].2;
                        rm = jumps[jump_index].1;
                        jump_index += 1;
                    }

                    events[index]
                        .link
                        .as_mut()
                        .unwrap()
                        .set_next(Some(next + add - rm));
                    index = next;
                    continue;
                }
            }

            index += 1;
        }
    }
}

/// Tracks a bunch of edits.
#[derive(Debug)]
pub struct EditMap {
    /// Changes keyed by event position, so a new edit does not scan every earlier edit.
    map: BTreeMap<usize, (usize, Vec<Event>)>,
}

impl EditMap {
    /// Create a new edit map.
    pub fn new() -> EditMap {
        EditMap {
            map: BTreeMap::new(),
        }
    }
    /// Create an edit: a remove and/or add at a certain place.
    pub fn add(&mut self, index: usize, remove: usize, add: Vec<Event>) {
        add_impl(self, index, remove, add, false);
    }
    /// Create an edit: but insert `add` before existing additions.
    pub fn add_before(&mut self, index: usize, remove: usize, add: Vec<Event>) {
        add_impl(self, index, remove, add, true);
    }
    /// Done, change the events.
    pub fn consume(&mut self, events: &mut Vec<Event>) {
        if self.map.is_empty() {
            return;
        }

        // Calculate jumps: where items in the current list move to.
        let mut jumps = Vec::with_capacity(self.map.len());
        let mut add_acc = 0;
        let mut remove_acc = 0;
        for (at, (remove, add)) in &self.map {
            remove_acc += remove;
            add_acc += add.len();
            jumps.push((*at, remove_acc, add_acc));
        }

        shift_links(events, &jumps);

        // Move the untouched stretches between edits inside the one vector, so
        // a large document never holds a second copy of its events while the
        // edits apply. Stretches that move left go first, front to back, then
        // those that move right, back to front: a stretch never lands on one
        // that has not moved yet.
        let old_len = events.len();
        let new_len = old_len + add_acc - remove_acc;
        if new_len > old_len {
            let filler = events
                .first()
                .copied()
                .or_else(|| self.map.values().find_map(|(_, add)| add.first().copied()))
                .expect("growing needs an event to fill with");
            events.resize(new_len, filler);
        }

        // (source start, source end, destination, where this edit's additions go)
        let mut plan = Vec::with_capacity(self.map.len());
        let mut add_before = 0;
        let mut remove_before = 0;
        let mut edits = self.map.iter().peekable();
        while let Some((at, (remove, add))) = edits.next() {
            let start = at + remove;
            let end = edits.peek().map_or(old_len, |(next, _)| **next);
            plan.push((
                start,
                end,
                start + add_before + add.len() - remove_before - remove,
                at + add_before - remove_before,
            ));
            add_before += add.len();
            remove_before += remove;
        }
        for &(start, end, dest, _) in &plan {
            if dest < start {
                events.copy_within(start..end, dest);
            }
        }
        for &(start, end, dest, _) in plan.iter().rev() {
            if dest > start {
                events.copy_within(start..end, dest);
            }
        }
        for ((_, (_, add)), &(_, _, _, add_at)) in self.map.iter().zip(&plan) {
            events[add_at..add_at + add.len()].copy_from_slice(add);
        }
        events.truncate(new_len);

        self.map.clear();
    }
}

/// Create an edit.
fn add_impl(edit_map: &mut EditMap, at: usize, remove: usize, mut add: Vec<Event>, before: bool) {
    if remove == 0 && add.is_empty() {
        return;
    }

    match edit_map.map.entry(at) {
        Entry::Occupied(mut entry) => {
            let (removed, added) = entry.get_mut();
            *removed += remove;
            if before {
                add.append(added);
                *added = add;
            } else {
                added.append(&mut add);
            }
        }
        Entry::Vacant(entry) => {
            entry.insert((remove, add));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EditMap;
    use crate::event::{Event, Kind, Name, Point};
    use alloc::{vec, vec::Vec};

    fn event(index: usize) -> Event {
        Event {
            kind: Kind::Enter,
            name: Name::Data,
            point: Point {
                line: 1,
                column: index + 1,
                index,
                vs: 0,
            }
            .into(),
            link: None,
        }
    }

    #[test]
    fn merges_edits_in_call_order_and_consumes_in_position_order() {
        let mut events: Vec<_> = (0..8).map(event).collect();
        let mut edits = EditMap::new();
        edits.add(5, 1, vec![event(50)]);
        edits.add(1, 1, vec![event(10)]);
        edits.add(5, 1, vec![event(51)]);
        edits.add_before(5, 0, vec![event(49)]);
        edits.add_before(5, 0, vec![event(48)]);
        edits.add(3, 0, vec![event(30)]);
        edits.add(2, 0, vec![]);
        edits.consume(&mut events);
        assert_eq!(
            events.iter().map(|event| event.point.index).collect::<Vec<_>>(),
            vec![0, 10, 2, 30, 3, 4, 48, 49, 50, 51, 7]
        );
        assert!(edits.map.is_empty());

        edits.add(0, 1, vec![event(99)]);
        edits.consume(&mut events);
        assert_eq!(events[0].point.index, 99);
        assert_eq!(events.len(), 11);
        edits.consume(&mut events);
        assert_eq!(events.len(), 11);
    }

    #[test]
    fn matches_a_plain_rebuild_when_stretches_move_both_ways() {
        // Each edit is (at, remove, how many events to add); positions are
        // ascending and removals never reach the next edit.
        let cases: [&[(usize, usize, usize)]; 6] = [
            &[(1, 3, 0), (6, 0, 4), (9, 2, 0), (12, 0, 1)],
            &[(0, 2, 5), (4, 1, 0), (5, 0, 0), (10, 4, 1)],
            &[(2, 0, 3), (3, 2, 0), (7, 0, 6), (8, 1, 0), (14, 0, 2)],
            &[(0, 16, 0)],
            &[(16, 0, 3)],
            &[(3, 5, 1), (8, 5, 1), (13, 3, 9)],
        ];
        for case in cases {
            let original: Vec<_> = (0..16).map(event).collect();
            let mut expected = Vec::new();
            let mut edits = EditMap::new();
            let mut from = 0;
            let mut fresh = 100;
            for &(at, remove, add) in case {
                expected.extend_from_slice(&original[from..at]);
                let added: Vec<_> = (fresh..fresh + add).map(event).collect();
                fresh += add;
                expected.extend_from_slice(&added);
                edits.add(at, remove, added);
                from = at + remove;
            }
            expected.extend_from_slice(&original[from..]);
            let mut events = original.clone();
            edits.consume(&mut events);
            assert_eq!(
                events.iter().map(|event| event.point.index).collect::<Vec<_>>(),
                expected.iter().map(|event| event.point.index).collect::<Vec<_>>(),
                "{case:?}"
            );
        }
    }
}
