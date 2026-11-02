---
title: Draft titles
feature: draft-titles
covers:
  - src/document.rs
  - src/tab_bar.rs
  - src/pane.rs
since: 2026-11-01
---

An Untitled page names its tab after its first line, so a page of notes is easy to find among several.

## The tab name

Type a first line and pause for half a second: the tab's name fades from "Untitled 2" to that line. A leading `#` and the spaces after it are left out, so `# Grocery ideas` shows as "Grocery ideas". A line longer than about 40 characters is cut with an ellipsis. The name is drawn a little quieter than a file's name, so you can tell a page that isn't saved yet from a file.

Blank lines at the top are skipped. A page that starts with front matter (a block between two `---` lines, as in the Markdown note template) is named by its `title:` line, or by the first line after the block when there is no title. A line that starts with `#!`, like a shell script's, is not a heading: the tab shows it as written.

The window title and the tab's tip show the same name. While there is no line to name it by, the tab keeps its number (Untitled, Untitled 2). Typing never waits for the name; it changes only after you stop. Change the line again and the name fades to the new one.

A page that comes back after you quit shows its first line from the start.

## Saving

Save offers the title as the file name, made safe for a file name (slashes and colons become spaces). A page that reads as Markdown (a `#` heading, front matter, or a page from a Markdown template) saves as `Grocery ideas.md`; any other page saves as `Grocery ideas.txt`. With nothing to name it by, the name is still Untitled.txt, or Untitled.md for a Markdown page.

A script whose first line is a `#!` line of a known interpreter (bash, sh, zsh, fish, python, node, deno, bun, ruby, perl, php or lua, also through `env`) is offered `Untitled.sh`, `Untitled.py`, `Untitled.js` and so on by language instead of its first line.

The page's draft in Scrap Drafts keeps its own name; only the tab and the Save panel use the line.
