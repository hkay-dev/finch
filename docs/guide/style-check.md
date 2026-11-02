---
title: Style Check
feature: style-check
type: guide
summary: View > Style Check puts a faint underline under filler words from your own list and under sentences that run too long.
covers:
  - src/style_check.rs
  - src/marks.rs
  - src/settings.rs
  - src/menu.rs
since: 2026-11-01
---

Style Check is a quiet second read of your writing. Pick **View > Style Check** and Finch
underlines, very faintly, two things in Markdown and plain text files:

- **Words from your list**, such as "very", "really" or "just".
- **Sentences over a length**, 30 words at first.

The underline is the faintest ink the editor has, so it never shouts. Code files are never
marked. A word inside a long sentence has no mark of its own, because the sentence is already
underlined along its whole length.

The check is off until you turn it on. The choice is remembered for the next start, the same in
every window (`style_check` under `[editor]` in the settings file).

## Why a mark is there

Point at a mark and wait a moment. A small note says why: "On your list" for a listed word, or
"Sentence is 42 words" for a long sentence.

## While you type

The word at the caret has no mark, so a word you are in the middle of writing never flickers. The
mark comes back when the caret leaves it. Marks update about a third of a second after you stop
typing, and only for the part of the file on screen and a screen above and below it.

## Your word list

The first time you turn the check on, Finch makes a plain text file next to the settings file,
`style-words.txt`, with a starting list: delve, crucial, pivotal, tapestry, robust, seamless,
realm, useful, worth, honestly, leverage, utilize, very, really, just, actually and basically.

Settings > Writing > Edit word list… opens it as a tab (and makes it from the starting list first if it isn't
there yet; an existing file is never overwritten). The button is dimmed while the check is off.
Edit it in any editor. One word or phrase per line, case doesn't matter, and a line starting
with `#` is a note. A phrase such as `in order to` matches only when its words follow each other.
Only whole words match, so "very" leaves "every" alone. Saving the file changes the marks in
open windows within a moment; there is nothing to restart. If the file is gone or unreadable,
the starting list is used.

## Sentence length

`style_sentence_words` under `[editor]` in the settings file is how many words a sentence may
have before it is marked (30 when the line is missing). Sentences are split by the same rules
Focus uses, so "e.g." and "Dr." do not end one. Fenced code blocks in Markdown are skipped.
