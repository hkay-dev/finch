---
title: Other uses of a name
feature: occurrence-marks
type: guide
summary: Rest the caret on a name and its other copies on screen get a faint wash; Control-Cmd-G selects them all.
covers:
  - src/occurrences.rs
  - src/marks.rs
since: 2026-11-01
---

Put the caret on a name and leave it for a moment (about half a second). Every other copy of that
whole word in the text on screen gets a very faint wash, so you can see where it is used without
searching. Nothing moves and nothing is selected. It is on in every file, and there is no setting.

- **What counts as a name:** the word the caret is in or just after, or a selection that is exactly
  one word. A longer or partial selection marks nothing. The word must be 2 to 64 characters, and
  only whole words match: `count` does not mark `counter` or `count_all`.
- **Case:** like Cmd-D, a word with a capital matches case, and a lowercase word matches any case.
- **Typing:** the marks go the moment you type or move the caret to another word, and come back
  after the pause. Moving the caret inside the same word, or scrolling, leaves them as they are.
- **Find:** while the find bar is open nothing is marked, and Find's own matches, counts and menu
  items are untouched.
- **Many cursors:** with more than one caret nothing is marked.

Only the text on screen and about a screen above and below it is looked at, so a long file costs
no more than a short one, and the scroll bar shows no ticks for these marks.

## Select them all

**Edit > Find > Select all matches** (Control-Cmd-G) with the caret on a name selects every whole-word
copy in the whole file as multiple selections, so one edit renames them all. It is the same item
Find uses; with no find open it takes the word at the caret.
