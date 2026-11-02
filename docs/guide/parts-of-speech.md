---
title: Parts of speech
feature: parts-of-speech
type: guide
summary: View > Highlight tints the nouns, verbs, adjectives, adverbs or conjunctions of a Markdown or text file.
covers:
  - src/parts_of_speech.rs
  - src/marks.rs
  - src/menu.rs
  - src/settings.rs
  - src/tokens.rs
---

# Parts of speech

View > Highlight tints one kind of word so the shape of your sentences shows while you
write. It has five items: Nouns, Verbs, Adjectives, Adverbs and Conjunctions. Each is a check mark
you turn on or off on its own, and any mix works. Each kind has its own soft color: blue for nouns,
green for verbs, amber for adjectives, rose for adverbs and violet for conjunctions, a little
deeper in a light theme so they read on the white page.

It works in Markdown and plain text files only. Code files are never touched, and in Markdown the
words inside fenced code blocks, inline `code` and link addresses are left alone.

The choice is remembered, the same in every window, and is saved in the settings file as
`highlight`, a list such as `["adverbs", "verbs"]`. It also works with View > Focus: a word
outside the focus is dimmed and keeps its tint, dimmed.

## While you type

The tint follows the text a moment after you stop typing (about a third of a second), not on each
key. The word you are typing is left uncolored until you move on, so a half-typed word never flashes
the wrong color. Only the text on screen and a screen each way is looked at, and at most 3,000 words
are tinted at once, so a very long file costs no more than a short one.

The Mac's own language tools (`NLTagger`) decide which words are which, so a word can be tinted
wrongly when its use is unclear ("light" can be a noun, a verb or an adjective). The tools load the
first time you turn a kind on; until then nothing about them is loaded.
