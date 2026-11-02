---
title: Linking to another note
feature: note-links
covers:
  - src/note_links.rs
  - src/note_links/
  - src/explorer/filter.rs
  - src/document.rs
  - vendor/gpui-component/src/input/popovers/completion_menu.rs
  - vendor/gpui-base/src/input/editor/lsp/completions.rs
since: 2026-11-01
---

In a Markdown file, type `[[` and a list of your notes opens at the caret. Pick
one and it becomes a link to that note.

## Link to a note

1. Type `[[`. The list shows the Markdown files under the folder the window's
   tree starts from, each with its file name on top and its first heading under
   it. The shortest names come first.
2. Type a few letters of the name. The list narrows the way Open quickly does:
   names that start with your letters come first, then names that hold them,
   then names that have them spread out.
3. Use Up and Down to move, then Return (or a click) to take one. What you typed,
   `[[` and the letters, is replaced by a link such as
   `[Meeting notes](../meetings/2026-11.md)`. The words are the note's first
   heading, or its file name without the extension when it has none. The path
   starts from the file you are writing in, and spaces in it are written as `%20`
   so the preview and a click on the link find the file.

Return puts the whole link in as one edit, so one Undo takes it back to what you
typed. The new link washes in once, a soft glow that fades in a third of a second,
so you can see what swapped in.

Escape closes the list and leaves `[[` and your letters as typed. So does moving
away, or typing `]`, `[` or `|`: a `|` starts a name of your own for the link,
which you type by hand.

## A note that doesn't exist yet

When no note matches what you typed, the list has one row: Create “name.md”.
Taking it writes the link and makes an empty note with that name in the same
folder as the file you are in. If the name is taken by a file the list doesn't
show (a hidden one, say), the new note gets the next free name and the link
points at that one. A name with a `/` or a `:` in it gets no row.

## Where it works

- Only in Markdown files, by their name, and only in a file that has been saved:
  an Untitled page has no folder to write a path from.
- Not in a code span (between backticks) and not in a fenced code block. A `[[`
  there stays plain text.
- The file you are in is never listed.

## Behind the list

Nothing is read until the first `[[` in a window. That one reads the file names
under the tree once, in the background, and the next ones use the same list. The
list is forgotten after about a minute and a half without a `[[` (much sooner on
a huge folder), when the window's folder changes, and when a Markdown file or
folder is added, removed or renamed. The next `[[` reads it again. Headings are
read from the first few KB of each file shown in the list, and remembered until
the file changes.
