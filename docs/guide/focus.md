---
title: Focus
feature: focus
type: guide
summary: View > Focus dims everything but the sentence, line or paragraph you are writing in.
covers:
  - src/focus.rs
  - src/marks.rs
  - src/workspace.rs
  - src/menu.rs
  - src/document.rs
  - vendor/gpui-base/src/input/base/element.rs
since: 2026-11-01
---

Focus keeps your eye on the writing. Pick **View > Focus** and choose what to light:

- **Off:** every line at full strength, as usual.
- **Sentence:** the sentence the caret is in. Everything else fades to about a third of its ink.
- **Line:** the line the caret is on.
- **Paragraph:** the block of lines around the caret, up to the blank lines on either side.

The check beside the name shows which one the window uses. The choice belongs to the window, so
each window can have its own, and it covers every tab in that window. It is not saved: a new
window, or the next time you open Finch, starts with Focus off.

## How it moves

Typing inside the lit sentence, line or paragraph leaves the light where it is. It moves only when
the caret crosses into another one, by an arrow key, a click or a new sentence, and it moves with a
short fade: the old text dims as the new text comes up, over about a tenth of a second. Then
nothing is animating, so a window at rest uses no frames.

Sentences follow the Mac's own rules for where a sentence ends, so an abbreviation such as "e.g."
or "Dr." does not split one. A sentence may run over several lines of a paragraph.

## In code

Source files have sentences only in comments, so **Sentence** acts as **Line** there. **Line** and
**Paragraph** work as they do in prose. A blank line is its own unit: with the caret on one, the
whole text fades until you type.

## What stays at full strength

Only the faded text changes. Syntax colors are kept (just paler), and the caret, the selection and
Find matches stay at full color even where they sit in faded text. With several carets, the
light follows the first one.

Focus only looks at the part of the file on screen and a screen above and below it, so a very
long file or a very long line costs no more than a short one.
