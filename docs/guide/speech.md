---
title: Read aloud
feature: speech
type: guide
summary: Edit > Speech > Start speaking reads the selection, or from the caret on, in the Mac's voice, lighting each word as it is said.
covers:
  - src/speech.rs
  - src/marks.rs
  - src/focus.rs
  - src/menu.rs
  - src/panes.rs
  - vendor/gpui-base/src/input/base/state.rs
since: 2026-11-01
---

Finch can read your text to you. Choose **Edit > Speech > Start speaking**:

- With text selected, it reads the selection.
- With only a caret, it reads from the caret to the end of the file.

The voice is the Mac's own system voice. Each word gets a soft wash of your accent color as it is
said, and the view scrolls along so the word stays on screen. Your caret and selection stay where
they were the whole time.

## Stopping

It stops by itself at the end. To stop sooner, choose **Edit > Speech > Stop speaking**, press
**Escape**, click in the text, or change the text. The wash goes away at once. Stop speaking is grey
when nothing is being read. Escape only does this when something is being read; otherwise it does
what it always did.

## With Focus

With **View > Focus** on, the lit sentence, line or paragraph follows the voice instead of the
caret, so the light moves down the page as it reads. When the reading ends, it goes back to the
caret. See [Focus](focus.md).

## Good to know

- Nothing about speech is loaded until the first time you start it.
- Reading a Markdown note reads the text as written, marks included.
- Once the reading ends, nothing keeps animating.
