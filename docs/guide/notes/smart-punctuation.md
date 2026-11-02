---
title: Smart punctuation
feature: smart-punctuation
covers:
  - src/smart_punct.rs
  - src/document.rs
  - src/menu.rs
since: 2026-11-01
---

# Smart punctuation

In a Markdown or plain text file, Finch tidies punctuation as you type, the moment the key lands:

- A straight quote (`"` or `'`) becomes a curly one. It opens after a space, an opening bracket or a dash, and at the start of a line; anywhere else it closes. An apostrophe inside a word (it's, don't) is a closing quote.
- Three dots (`...`) become an ellipsis (…).
- Two dashes (`--`) become an en dash (–).

Typing `"Hi" -- it's...` gives “Hi” – it’s…

## Undo gives back what you typed

The change is its own step in Undo. Press Cmd-Z right after it and the straight quote, the three dots or the two dashes come back exactly as you typed them; press it again and the typed text itself goes.

## What it leaves alone

- Code spans (between backticks), fenced and indented code blocks (a line indented four spaces or a tab after a blank line, outside a list item) and front matter at the top of a Markdown file.
- Link targets, web addresses, HTML tags and link reference lines.
- A line of only dashes, so `---` stays a Markdown rule, and table lines like `|---|---|`. Two dashes typed in front of a third dash are left too.
- Pasting, Undo, Redo and files that change on disk: only a key you type makes the change.
- Code and other files (a `.rs` file, a config file): never touched. Plain text means an Untitled page or a `.txt` file. An Untitled page that reads as Markdown (a `#` heading or `---` on its first line, or a Markdown template) follows the Markdown rules.

A third dash typed after an en dash that was just made stays a plain dash, so a dash in the middle of a line can need Cmd-Z after the second one.

## Turn it off

View > Smart punctuation has a check mark while it is on. Click it to turn it off for every file; the choice is kept the next time you open Finch. In the settings file it is `smart_punctuation` under `[editor]`.
