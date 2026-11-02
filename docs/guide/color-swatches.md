---
type: guide
summary: A small round swatch beside each color written in code; a click opens the Mac color panel and the text follows the pick.
covers: [src/swatches.rs, src/marks.rs, src/panes.rs]
---

# Color swatches

In code files, a small round swatch shows the color beside each color written as text:

- hex: `#FF9F0A`, `#fff`, `#RGBA`, `#RRGGBBAA`, and `0x423E39` (six digits)
- `rgb(...)` and `rgba(...)`, with commas or spaces and a `/` before the alpha, channels as numbers or percentages
- `hsl(...)` and `hsla(...)`, with the hue as a number or in `deg`

The swatch takes no room: the text, the caret and the selection are where they were. It sits in the
space before the color when there is one, and under the first letter when there isn't (`("#fff")`).
A color with alpha shows over a faint grey, so it reads as see-through.

**Click a swatch** to open the Mac color panel on that color. Moving through the panel rewrites the
color in the text, in the way it was written: `#fff` stays three digits while the pick fits in three
(and widens to six when it doesn't), `rgb(10, 20, 30)` keeps its commas, a lower-case hex stays lower
case, a `0x` number stays a `0x` number. The whole pick, however long you drag, is **one Undo**: press
Cmd-Z in the editor and the color is back as it was. Editing the text in between ends the pick, and the
panel's later colors are ignored until you click a swatch again.

Over a swatch the pointer is a pointing hand and the swatch's rim fades up brighter (the same fade
as a row's hover); pressing shows at once, the color drawing in a little from the rim. The fade is
the only thing that asks for frames: at rest a swatch repaints nothing. The hand's area is the chip
and two points around it, and sits where the chip was drawn the frame before while the editor scrolls.

A swatch for a color you type or paste fades in over a moment. One already in the file when it opens,
one that scrolls into view, one on a tab you switch back to, and one that only changes color as you
keep typing it appear at once. When an edit before a fading swatch moves it (a character typed earlier
on the line), the fade carries on at the swatch's new place, and so does a hover fade.

## What it leaves alone

- **Markdown and plain text** show no swatches: `#123` there is more often an issue number than a color.
- **Short all-digit hex** (`#123`, `#1234`) counts only in CSS, HTML, Svelte, Astro, EJS and ERB, or when
  every digit is the same (`#000`). In other languages it is read as a number.
- **Names that run on** (`#fade-in`, `a#fff`, `&#123;`, `0x423E39_u32`) and `rgb(var(--x))` are not colors.
- **A file still loading** is read-only until its text arrives: its swatches show, but clicking one does nothing, and a swatch there neither lights nor shows the pointing hand. Once it has loaded it behaves as any file.
- A window past 512 KB of text (a minified one-line file) is not scanned.

## How it works

The swatch layer is one of the marks on the shared driver (`src/marks.rs`). It scans only the text on
screen and a screen each way, only when the text changes or the screen leaves that window, so a scroll
step inside the window scans and sets nothing. At most 500 swatches are kept. A view of its own over
the editor (drawn by the pane) paints the chips from where the editor says each color starts, so the
editor's text runs are never split or recolored and the language server's document colors are not used.
