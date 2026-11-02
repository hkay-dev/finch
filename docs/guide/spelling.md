---
title: Spell check
feature: spelling
type: guide
summary: View > Check spelling underlines misspelled words in prose, comments and strings, never names in code.
covers:
  - src/spell.rs
  - src/marks.rs
  - src/menu.rs
  - src/panes.rs
  - src/settings.rs
---

# Spell check

Misspelled words get a red wavy underline, using the Mac's own spell checker, so it follows the
language you set on the Mac and the words you taught it.

## Where it looks

- **Markdown and plain text:** every word, except what sits in code fences, `inline code`, link
  targets, tags and `&entities;`.
- **Code files:** only the words inside comments and strings. Names in code are never looked at.
- Anything that doesn't read as a plain word is left alone: a chunk with digits, an underscore, a
  slash or a dot in it (`fn_nmae`, paths, addresses, `e.g.`), camelCase and ALL CAPS.
- The word you are typing is skipped until you move on, so it doesn't flash red at every letter.
- It checks the screen and about a screen each way, a moment (about a third of a second) after you
  stop typing. A line over 10,000 bytes is skipped, and a file over 4 MB is not checked (code over
  1 MB isn't either, since its comments and strings come from the syntax colors).

## Fixing a word

Right-click an underlined word. At the top of the menu are the Mac's guesses (picking one replaces
the word, and one Undo brings it back), then **Learn spelling** (the Mac remembers it, in every app)
and **Ignore spelling** (until you quit Finch). The usual items stay below.

## Turning it off

View > Check spelling toggles it for every window. On at first; the choice is remembered as
`check_spelling` under `[editor]` in the settings file.
