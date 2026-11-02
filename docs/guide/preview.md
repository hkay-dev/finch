---
title: Markdown preview
feature: preview
covers:
  - src/preview/
  - src/export_html.rs
since: 2026-10-30
---

The preview shows a Markdown file as formatted text, in a panel to the right
of the editor. It follows what you type, so you can write on the left and read
the result on the right.

## Show and hide it

Press Cmd-Shift-V to show the preview, and again to hide it. The small
side-panel button in the top right corner of the window does the same: there is one, it stays in the same place whether the preview is open or shut, and it belongs to the pane you are in. It rests
in grey and brightens when you point at it; while the preview is open it stays
lit, with a soft square behind it like the find bar's Match case button when
it is on, and the square fades in and out with the panel's slide. Rest the
pointer on it for half a
second and it says "Show preview ⇧⌘V", or "Hide preview ⇧⌘V" while the preview
shows. Finch remembers the choice for the next start. The preview is off
until you first turn it on.

Drag the divider between the editor and the preview to change how wide the
preview is. The width is remembered too. The editor never gets narrower than
360 pt: in a small window the preview, then the folder tree, draw narrower than
you set them, and your saved widths come back when the window grows.

## Which files get it

Files ending in `.md`, `.mdx`, `.mdwn`, `.mdc`, `.markdown` or `.MD`. The preview button only shows for those
files. Switch to another kind of file and the panel goes away; switch back to
a Markdown file and its preview comes back. Saving an Untitled page with a
`.md` name gives it a preview right away. An Untitled page has one before it's
saved when its first line is a `#` heading or a `---` front matter line, or when
it came from a Markdown template (the Markdown note, Daily note, README, or one
of your own named `.md`).

## Ready at once

The Markdown tab you're on, and the tab you were on just before it, are
prepared in the background, whether or not the preview is showing. Press
Cmd-Shift-V, or switch between the two, and the formatted text is already
there, with nothing to wait for. A Markdown tab you haven't visited for a while lets go of its formatted
copy to save memory (your text is never touched) and prepares it again when you
come back.

When you type, the preview catches up about a tenth of a second after you
stop, and at least four times a second while you keep typing. The work happens
behind the scenes, so the editor keeps up with your typing, even in a file of
a megabyte or more.

A Markdown file with nothing in it, or only blank lines, shows "Nothing to preview" in the panel; type a character and the preview takes its place.

## Scrolling

The editor and the preview scroll together. As you scroll the editor, the
preview follows: the part of the text at the top of the editor stays at the top
of the preview. Scroll the preview and the editor follows it the same way, so
the source of what you're reading stays in view for editing. The pane you last
scrolled or clicked leads and the other follows, so the two never pull against
each other. Moving through the editor with the keyboard or Find makes the editor
lead again. Both move with the same wheel or trackpad scroll, with no lag. The
mouse wheel glides in the editor, the preview and the folder tree alike: each
turn eases the text toward where it is going at your display's own pace, so it
moves evenly instead of in uneven jumps, and it comes to rest by itself. Two
fingers dragging on the trackpad move the text with them, with no ease, in even
steps on every frame, and the coast after they lift carries on at the trackpad's
own pace before it settles. Typing
never scrolls the editor to follow the preview. While the preview is hidden,
nothing follows; show it again and it lines up with the editor at once.

While the preview follows the editor, the block that holds your caret gets a faint
wash, the same one a heading link leaves when it lands. It slides from block to
block as the caret moves, and fades out about two seconds after the caret stops.
A paragraph inside a list washes the whole list. With Reduce Motion on it jumps
between blocks instead of sliding. Nothing is washed while the preview leads, or
when the caret sits on a blank line between blocks.

When the file changes on disk and Finch reloads it (another program saved it), the
blocks that hold the changed text wash once as the new preview lands, the same wash
the editor gives the changed lines. Saving from Finch, typing, and a second change
within two seconds of the last one wash nothing.

A slim rounded scrollbar shows at the preview's right edge only while you scroll
it yourself (wheel, trackpad or a drag of the knob). It holds half a second,
fades out and then draws nothing; following the editor never shows it. The knob
widens when the pointer is over it. When there is more text below, the bottom
edge fades into the page, and the top edge too once you have scrolled down.

## What it shows

- headings, paragraphs, lists, quotes and tables
- task lists, with their boxes ticked or empty. The boxes sit in the column a
  bullet would, so task and bullet text start on one edge; a ticked box fills
  with your Mac's accent colour, easing in and out as it flips. Click a box and it ticks or
  unticks in the file (`[ ]` becomes `[x]` and back), as an edit you can undo
  with Cmd-Z; the file shows as unsaved until you save it, and the editor takes
  the focus; the line washes in the editor once, as a paste's does, when it is in view. The pointer turns to a hand over a box.
- code blocks, colored by the language named after the opening fence
- images, found relative to the Markdown file's folder. A wide image shrinks to fit
  the column and a small one stays its own size; both have rounded corners and fade
  in. A picture you change on disk shows its new version. A missing image, one from the web, or one too big to show (over 16384 pixels a side, or 256 MB decoded) shows its description on a plate. A diagram that takes over 10 seconds to draw shows an error instead
- links: a web link opens in your browser, and a link to another Markdown file
  (any name the editor treats as Markdown, such as `.md`, `.mdx`, `.mdwn`, `.mdc`, `.markdown`
  or `.MD`) opens that file in the editor. A link to a heading, such as `[jump](#install)`,
  scrolls the preview to that heading (and the editor with it), and
  `[setup](other.md#setup)` opens that file and scrolls smoothly to its heading. A heading's
  link name is its text in lower case, with spaces turned to dashes and
  punctuation dropped (`## What's new?` is `#whats-new`); a second heading with
  the same text is `#whats-new-1`, a third `#whats-new-2`. A link to a heading
  that isn't there opens the file and stays at the top; in the same file it
  stays put. Either way, and for a link to a file that doesn't exist, the Mac
  beeps.
  The block you land on, a heading or a footnote, washes once as the preview
  arrives, so you can see where you are; click the link again and it washes again.
- callouts: a quote whose first line is `[!NOTE]`, `[!TIP]`, `[!IMPORTANT]`,
  `[!WARNING]`, `[!CAUTION]` or `[!HARSHA]` (in any case) shows as a tinted box
  with an icon and the kind's name on top. Words after the marker on that line
  (`> [!WARNING] Hot surface`) become the title instead. The marker line itself
  is not shown. Any other quote stays a plain quote.
- footnotes: a reference such as `[^1]` shows as a small raised number, numbered
  in the order the references appear, and click it to scroll to its note. The
  notes are gathered at the end of the preview under a thin line, however far
  apart you wrote them, each with its number and a small arrow that scrolls
  back to the reference. A note nobody refers to is numbered after the others.

## Diagrams and math

- A code block fenced as `mermaid` draws as a diagram: flowcharts, sequence
  diagrams and the other Mermaid types. It is laid out for the width of the
  preview column and takes the preview's colours, light or dark, so it follows
  the theme. Drag the divider and the diagram redraws once the width stops
  moving, never on every step of the drag.
- Math in `$$ ... $$` lines draws as a centred block, and `$...$` inside a line
  draws as part of the text. Dollar signs in ordinary prose stay as typed:
  `spent $5 and $10` shows exactly that. A formula opens right after its `$`,
  closes right before its last one, and isn't followed by a digit.
- A block with a mistake in it shows as a plain code block with the first line
  of the error under it in the muted colour, never a blank. A formula in a line
  that can't draw stays as its `$...$` text. Copy and Copy as Markdown give the
  source either way, and Copy as Rich Text keeps it as a code block.
- Nothing loads for a file without a diagram or a formula. The first diagram
  starts the Mac's own JavaScript engine for a minute or so. A diagram's box is
  held open while it draws, so the text below moves once when the first picture
  lands, and the picture fades in as it does; a redraw for a new width or an edit keeps the old picture until the new one is ready.

Diagrams are drawn by [Pele](https://github.com/obsidianmd/pele), Obsidian's
Mermaid renderer (MIT), and math by `latex-rust`.

A file that starts with front matter (a block between two `---` lines at the
very top, as on this page) shows it in the editor but not in the preview, which
starts at the text after it.

## Selecting and copying

Drag across the text to select it, double-click to select a word and
triple-click to select a paragraph. Cmd-C copies what you selected as plain
text, and Cmd-A selects the whole preview.

Right-click the preview for a menu: Copy and Copy as Markdown (both grey until
something is selected), and Select all. Copy as Markdown puts the source text
on the clipboard, with its `**bold**`, `#` and link marks, where Copy gives the
text as you read it. Right-click a link and Open link and Copy link address come
first. Only a left click opens a link: a right or middle click leaves it alone.

Copy as Rich Text, also in that menu, copies the selection (the whole document
when nothing is selected) as styled content: paste it into Mail, Pages or Notes
and the headings, bold, italics, links, lists, tables and code keep their
formatting, in light colors with no page background. A text field gets the
Markdown source. Cmd-C stays plain text.

Every copy from the preview, Cmd-C and each menu copy, shows a check mark in the
corner of the preview for a moment. A Copy with nothing selected stays quiet.

## How it looks

The text is set for reading. Body text is 15 pt with generous line spacing, and
a line holds to about 70 characters, centered when the panel is wider than
that. The panel starts 420 pt wide; drag the divider and your width is kept. In
a narrow window the preview gets narrower so the editor keeps at least 360 pt.
The space beside the text is 24 pt at the starting width and shrinks with the
panel to 8 pt at the narrowest (120 pt) on the left, and never less than the
scrollbar's track on the right, so a squeezed preview still keeps a 73 pt column
to read.

Prose gets typographic punctuation: straight quotes become curly (“like this”, it’s), `--` becomes an en dash, `---` an em dash and `...` an ellipsis. Code, code blocks, link addresses and the file itself keep what you typed; copying from the preview copies the curly text.

- Headings are 26, 22, 17 and 15 pt in semibold, with room above them.
- Quotes and list markers are in the softer grey, and a quote has a thin bar on
  its left.
- One rhythm runs through lists and cards. A second paragraph or a code block
  inside a list item sits a paragraph gap below the text above it, and a nested
  list a little below its item. A numbered list keeps one column for its
  numbers, as wide as its last number, so items 10 and up start their text on
  the same edge as 1 to 9. Code blocks, tables, quotes and callouts all start
  their text 14 pt in from the card's edge.
- Code is a little smaller than the text, in the editor's code font and weight
  (change the font in settings and the preview follows), on a faint card with
  rounded corners and a hairline edge, as a table has; short code inside a line
  has its own faint chip. A block's language name sits quietly in its top right
  corner at all times, on a small plate in the card's own fill. Point at the block and a copy button, on a plate of its own, fades in beside
  the name; rest on it and it says "Copy". Click it to copy the code, and its
  clipboard fades into a check for a moment (it says "Copied" if you point at it
  again then), on that block only.
- Tables have hairline borders, one screen pixel wide, like the card edge
  and the rules, so on a sharp screen they are finer than a point, and their numbers line up in columns. A name
  or code in a table stays whole, and long text wraps; a table too wide even
  so scrolls sideways. A table sits on the same card as a code block, in light
  and dark, with its header row a step darker. While you scroll a wide table
  sideways a thin bar fades in along its bottom and out again, and its sides
  fade where more of the table hides past them.
- Links are blue and dim gently while you point at them, easing back when you
  move away. A web link also ends
  in a small up-right arrow, to tell you it leaves Finch. Rest the pointer on a link for half a
  second and a small pill fades in at the preview's bottom-left corner, saying where it goes: the
  address of a web link, the file name of a link to a local file (with its `#heading` when it has
  one), or `#heading` for a link inside the file. A long one loses its middle. It fades out when you
  move away, and it never takes a click.
- Callouts are a soft box in the kind's own colour (blue for a note, green for a
  tip, purple for important, orange for a warning, red for a caution, teal for
  Harsha), with a stronger bar on the left edge. The icon and the title above the
  text are in that colour, the text itself in the normal ink.
- Footnote numbers are blue, 11 pt and sit above the line; the notes at the end
  are 13 pt, a step under the body, about 4 pt apart, under a thin rule with the same room
  above and below it as a Markdown rule. Pictures get a hairline ring just inside their edge.

## Export as HTML

File, Export as HTML… saves the Markdown file in front as one web page. A Save panel
opens in the file's folder with `name.html` filled in; the Replace question for a
name that is taken is the panel's own. The page is built from the text in the
editor right now, unsaved changes included, and the Markdown file itself is never
changed. It shows what the preview shows (headings, lists, tables, task lists, code
blocks, footnotes) in the same type, spacing and colours, light or dark to match the
reader's Mac, with the file's name as the page title.

A picture on your Mac, found relative to the file's folder or by its full path, is
put inside the page, so the single `.html` file can be moved or sent on its own; one
bigger than 5 MB, or a missing one, keeps its address and shows broken wherever the
page moves. Pictures from the web stay as web addresses. The item is grey for any
other kind of file and with no tab open. If the page can't be written, an alert says why.
