---
title: Templates with a date and tab stops
feature: template-tab-stops
covers:
  - src/tab_stops.rs
  - src/templates.rs
since: 2026-11-01
---

File > New from template starts a new Untitled page from a template. A template
can leave spots for you to fill in, and put today's date in for you.

## What a template can hold

Write these words between double braces in a template, the built-in ones or your
own files in the templates folder:

- `{{date}}` becomes today's date, like 2026-11-01, wherever it stands. It can
  appear as many times as you like.
- `{{title}}` marks a spot to fill in. The first one is where the caret starts.
- `{{cursor}}` marks where you want to end up. It comes after every `{{title}}`
  spot.

The markers themselves never show in the page. Any other text in braces is left
exactly as you wrote it, and a template with none of these opens as it always
did.

## Your own templates

Every file in the templates folder is listed under the built-in ones, by name without its extension (`letter.md` shows as `letter`). When two files would share a name, like `a.md` and `a.txt`, those two keep their extension. Only the first 50 files, by name, are listed; the rest stay in the folder but not in the menu.

A page from a Markdown template (the Markdown note, the Daily note, the README, or your own file named `.md`) is Markdown before you save it: it gets Markdown colors, styled marks and the preview at once, and Save offers a `.md` name. It stays Markdown whatever its first line says until you delete all its text or quit; a page that comes back after you quit goes by its first line, like any Untitled page. Note links and pasted pictures still wait until it's saved, since they need a folder.

## Hopping between spots

1. Pick a template from File > New from template. The caret lands on the first
   `{{title}}` spot (or on `{{cursor}}` when there is no title).
2. Type what belongs there.
3. Press Tab. The caret glides to the next spot: any further `{{title}}` spots
   in the order they stand, then `{{cursor}}`. The line it lands on lights up
   for a moment, the way it does after Go to line.

What you type moves the later spots along with it, so a long title doesn't throw
them off. Press Escape, click in the text, or press Tab once more after the last
spot to stop hopping; Tab then types a tab as usual. Other pages are never
touched: Tab works as usual in every one of them.

## Daily note

The built-in Daily note puts today's date in a heading, leaves a blank line and
puts the caret below it, ready for the day's notes.
