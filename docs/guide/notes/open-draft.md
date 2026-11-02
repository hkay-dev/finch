---
title: Open an old draft
feature: open-draft
covers:
  - src/draft_list.rs
  - src/drafts.rs
since: 2026-11-01
---

Every Untitled page is kept as a scrap draft, so nothing you typed is lost, even after you close its tab. Open draft… brings one of those back.

## Open a draft

1. Choose File > Open draft…, press Option-Cmd-O, or click Open draft… on an empty window.
2. Pick a draft. Up and Down move the pick, or click a row.
3. Press Return. The draft opens as an Untitled page in a new tab, with its text, and keeps saving to the same draft. If its tab is already open, Finch shows that tab instead.

Press Escape to close the list without opening anything.

## What a row shows

The list looks like Open quickly and sits in the same place. Newest drafts are first. Each row shows:

- the draft's title, in the editor's font: named the same way as its tab (a leading `#` left out, front matter skipped in favour of its `title:` or the first line after it, a `#!` line as written), or "Empty draft" when there is no text;
- when it last changed: "Today 14:05", "Yesterday 09:30", or the date, with the year when it isn't this one;
- how many words it has. A very long draft counts only its start, and shows a plus ("900+ words").

## Find a draft

Type to narrow the list. A draft stays when its text holds every word you typed, in any order, ignoring capitals. Only the start of each draft is searched.

## Throw a draft away

Press Cmd-Delete to move the picked draft to the Trash. Its row slides away, and Finder's Put Back can return it. A draft that is open in a tab, in any window, is never moved: its row says it is open instead. Close the tab first.

Finch reads the drafts folder when you open the list, never when it starts.
