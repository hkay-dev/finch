---
title: Finch
feature: editor-window
covers:
  - src/main.rs
  - src/app.rs
  - src/window_look.rs
  - src/workspace.rs
  - src/pane.rs
  - src/tab_bar.rs
  - src/snippet.rs
  - src/native_menu.rs
  - src/find_in_folder.rs
  - src/folder_search.rs
  - src/document.rs
  - src/compare.rs
  - src/compare_view.rs
  - src/drafts.rs
  - src/language.rs
  - src/settings.rs
  - src/menu.rs
  - src/config_check.rs
  - src/config_path.rs
  - src/start.rs
  - src/templates.rs
since: 2026-10-30
---

Finch is a small text editor for the Mac. It opens files in tabs in a
window, with line numbers down the left side. Code is colored by language: C,
C++, C#, CMake, CSS, Diff, Elixir, Go, GraphQL, HTML, Java, JavaScript, JSON,
Kotlin, Lua, Makefiles, Markdown, PHP, Protobuf, Python, Ruby, Rust, Scala,
shell scripts, Svelte, Swift, TOML, TSX, TypeScript, YAML, Zig and the Astro,
EJS and ERB templates. The file name picks the language: its ending (`.rs`,
`.go`, `.py`, `.cpp`, `.yml` and the like) or a whole name such as `Makefile`,
`CMakeLists.txt`, `Gemfile`, `.zshrc` or `.bash_profile`; the longest match wins.
A file whose name says nothing is looked at once when it opens: a first line like
`#!/usr/bin/env python3` or `#!/bin/bash` picks the language, and so does an
Emacs line (`-*- mode: ruby -*-`) or a Vim line (`vim: set ft=lua:`) in the
first or last few lines. The name always wins over the first line, and that over
a modeline. Any other file, such as `.txt` or an SQL file, shows as plain text.
Saving an Untitled page with a new ending, or renaming a file to one, eases the new
colors in over a fraction of a second (a plain save, opening a file and typing don't;
with Reduce Motion on they cut in).
An Untitled page
itself is plain text, except one whose first line is a `#` heading or a `---`
front matter line, or that came from a Markdown template: that page is Markdown
from the start, with its colors and preview, and goes back to plain text if you
change that first line.

Line numbers are small and quiet: the current line's number is at full
strength and the others are dim; when the line highlight glides to a new line, the
bright number fades from the old line's to the new one's with it. Selected text takes your Mac's accent color
from System Settings and follows it when you change it. Find matches are a soft
yellow wash, and the current match is orange, drawn over the rest; the marks on
the scroll bar use the same colours, with the current match's mark in orange,
and start below the find bar. While the window isn't in front, the
selection and the find matches (the current one too) turn grey instead of
disappearing, and the band under the current line dims with it. Selections and
find matches have softly rounded corners that follow where each line ends, as
on the Mac. Colored code keeps its color under a selection or a find match, and
is only lightened or darkened as far as it takes to stay easy to read there.

The text cursor is a thin rounded bar that glides to each new spot, as you
move with the arrow keys or type, and fades softly in and out as it blinks. It
stays solid while you type, hides while text is selected, and shows only in the
window that's in front. After about five seconds without a key it stops
blinking and stays solid. In the window's other pane, or while the folder tree
or find bar has the keys, the text cursor shows as a still, faint outline around
the character it's on (at the end of the selection, if text is selected), so you
can see where you left it.

When you type or press Return on the last lines of the view, the text glides up
to make room instead of jumping a line, and holding Return scrolls it in one
smooth run.

Left and Right step over a whole visible character, including an accented
letter, a flag or a joined family emoji. Shift selects the same whole units.
Forward Delete removes the whole character. Backspace keeps emoji whole,
but removes one Unicode code point from other text: an accent, the last
part of an Indic conjunct, or one part of a decomposed Hangul syllable.
A Windows-style CRLF line break is one step.

Double-click selects a whole word in Greek, Hebrew, Korean, Japanese kana
and other scripts, keeping accents attached. Underscores join words;
hyphens split them. Emoji select as one visible character. Chinese
ideographs select one at a time, without dictionary-based word breaks.
Dragging after a double-click extends by whole words, keeping the first word
selected even when you reverse direction. Triple-click selects the whole
paragraph, including its line break; dragging then extends by whole paragraphs.
Dragging outside the editor scrolls farther as you move farther past the edge,
and stops when you release. With soft wrap off, this also works sideways.

While composing with an input method, the caret follows the input method's
selected position inside the underlined text. The clause currently being
converted has a thicker underline; the rest of the composition stays thin.

Characters you can't see are shown, so they can't hide in your code. Control
characters (a bell, an escape) appear as their small symbol glyphs, and
zero-width characters and text-direction controls (such as the zero-width space
and right-to-left override) appear as a highlighted blank cell; the text after
a direction control is not reordered. Standalone controls are one cursor
step; a joiner inside an emoji stays with that emoji. Tabs, plain spaces and
line breaks look as before.

The usual Mac text keys work. Control-K deletes to the end of the line (at the
end of a line it deletes the line break), Control-D deletes the character after
the cursor, and Control-H the one before it. Option-Up moves to the start of the
paragraph, or the previous paragraph's start if you're already there;
Option-Down moves to the end of the paragraph, or the next one's end. Add Shift
to select instead. A paragraph is a run of text between line breaks. Control-F,
Control-B, Control-N and Control-P move the cursor right, left, down and up one
step, like the arrow keys; add Shift to select.

Option-Left and Option-Right move by word, and Option-Delete and Option-Backspace
delete by word; a word stops at every `_`, `.`, `-` and bracket, and spaces and
line breaks are skipped over. A word delete that starts after two or more
spaces removes just those spaces. Control-Option-Left and Control-Option-Right
move by part of a name instead, stopping at camelCase humps and at `_` and `-`;
add Shift to select.

The window has no title bar. The three window buttons sit at the top left, over
the folder tree, and the text starts just below the top edge. The folder tree
uses the Mac's sidebar look, so your desktop shows faintly through it while the
window is in front, and a thin line divides it from the text. Drag the top edge
of the window, above the text, to move it; double-click there to zoom it the way
your Mac is set to. The window's title is still the file's name, in Mission
Control and the Window menu.

## Open a file

The folder tree is on the left of the window, and where it starts depends on
what you give Finch:

- A folder: the tree starts at that folder and the page is empty.
- A file: the page opens the file, and the tree starts at the nearest folder
  above it that holds a `.git` (the top of its project), or at the file's own
  folder if there's none. A file that doesn't exist yet opens as an empty
  page, and saving creates it.
- Nothing, from a terminal: the tree starts at the top of the project you're
  in (the nearest folder with a `.git`), or at the folder itself, with an
  empty Untitled page.
- Nothing, from the Dock or Finder: the tree starts at the folder and file you
  had open last time, or at your home folder if that folder is gone.

Pipes and devices are not files Finch can open. Images and other binary
files are refused after a small sample, without loading the whole file.
For a text file larger than 256 MiB, Finch asks before reading it:
opening it may take a while and use a lot of memory. Open is the default (Return
opens it); Cancel or Esc closes the tab. Its named tab stays in place while the question is
open, and a requested line is shown after the file loads. Reloading or
reverting a file already open does not ask for its size again.

Click a file in the tree and it opens in a tab, or, when it's already open,
its tab comes to the front. Switching tabs, or clicking another tab, never
loses anything: each tab keeps its own unsaved changes, Undo history and
scroll place, and the tree selects the file of the tab in front. An Untitled
page is a tab like any other, so leaving it asks nothing; a blank Untitled
page in front is replaced by the next file you open. While an Untitled page is
empty it shows a faint “Start typing…” where the text will go; it fades out with
your first character and back in if you delete everything (an empty file with a
name never shows it).

You can also hand Finch a folder or file from outside:

- In Finder, right-click it and choose Open With, Finch.
- Drop it on the Finch icon in the Dock.
- Drop files on the window to open them as tabs; a drop of only folders opens nothing and beeps.
- In a terminal, run `open -a "Finch" <path>`, or `finch <path>`.
- Right-click the Finch icon in the Dock and pick one of the recent files
  or folders it lists (the same list as File > Open recent).
- Choose File > New window (Shift-Cmd-N), or right-click the Finch icon in the Dock and choose New window: a window
  with an empty Untitled page, and the tree at the folder you had open last
  time (or your home folder).

A file from outside opens as a tab, or its tab comes to the front when it's
already open, and the window comes forward. It goes to the window whose folder
tree holds the file (the one rooted deepest, if several do), otherwise to the
window in front. With Finch already running, `finch <file>` in a
terminal hands the file to it and returns when it answers. If the app is busy,
the command waits up to five seconds, then reports “Finch isn't responding.”
and exits with an error, never starting a second copy after handing it the file.
With none listening, it starts as above. A busy app still counts as listening: the command
keeps trying the socket a little longer, and a launch that finds another Finch behind the socket
hands it the files and stops rather than run a second copy. A bare `finch` in a
terminal, with the app running, opens its folder in a new window there.
Add a line after the name, `finch src/main.rs:42`, or a
line and column, `src/main.rs:42:7` (both counted from 1), and the cursor lands
there with that spot in view, also when the file is already open in a tab. A
file really named `main.rs:42` opens as that file. A folder from outside, or a file
you haven't saved yet, opens its own window, 22 points down and to the right of
the window in front, so the windows you already have stay as they are. File > Open (Cmd-O)
opens each file you choose as a tab in the window in front, and each folder in
a new window. Finch quits when you close the last window; closing one of
several leaves the rest open. Finch never makes itself the default app
for any file type, so Open With is the way in from Finder.

To use Finch as the editor of another command (git commit, say), put
`--wait` (or `-w`) before the name: `finch --wait <file>`, or
`EDITOR="finch --wait"`. The command then waits until you close that
file's tab, and ends with 0 when you do (a folder waits for its window to
close). With none running, it starts Finch first, which keeps running
after the tab is closed. A line after the name works here too. With several
files, it waits until every one of them has closed.

Give it several files, `finch a.rs b.rs:12 c.rs`, and each opens as a tab
in the order you typed them, the last one in front. With Finch running,
they all go to it, and with none running the first opens the window and the
others follow as tabs in it. A refused file does not stop later files from
opening. The handover returns an error only if none opened, with the first
file's name and the reason it was refused.

A lone dash takes text from another command: `echo hi | finch -`, or `git diff | finch -`.
It opens as a new Untitled page holding that text, in the running app or in a new
window when none runs (it fills the window's empty page instead of adding a tab). The page
is one you typed into: it's saved as a draft and closing it asks what to do with the text.
Text that is too big (over 256 MB) or isn't text says so and opens nothing. In a
terminal with nothing piped in, `finch -` says "nothing was piped in" and ends with 1 at
once rather than waiting. Give `-` once; `--wait` works with it too.

When a file can't be opened, the command says why in the app's words, like the alert would
(`finch: You don’t have permission to open “notes.txt” in this folder…`), and ends with 1.

`finch --help` (or `-h`) prints a short list of what the command takes
and `finch --version` (or `-V`) prints the version; both end with 0 and
open nothing. A word that starts with a dash and isn't one of the options
(`finch --bogus`) prints "unknown option" and a hint to run `--help`,
and ends with 2. To open a file whose name starts with a dash, put `--` before
it: `finch -- -notes.txt`.

## Open a file by name

Press Shift-Cmd-O (File > Open quickly…) to open a small field in the middle of
the text, just under the tabs. Type part of a file's name and the files under
the folder tree's top folder narrow as you type, the same way the explorer's
filter matches: the letters you type must appear in the name in that order,
capital and small letters count the same, and the matched letters show bold.
Up to ten files show under the field, each with its icon and the folder it's in;
a folder too long for the row loses its middle and keeps its top folder and its
last ones (`archive/…/2025-final/appendix-notes`).
A name that starts with what you typed comes first, then one that holds it at
the start of a word, then anywhere, then names with the letters spread out;
among those, shorter names and files nearer the top come first. Folders don't
show, and neither do the files the tree hides.

When the window has room to the right of the field, a quiet card beside the list
shows the first twelve lines of the highlighted file, in your editor's font, and
cross-fades to the next file as you move the highlight. It reads only the start
of the file, so a big file shows at once; a file you have open shows the text in
its tab, edits and all, once that tab has been shown. Files that aren't text
show no card, and neither does a narrow window, which keeps the list as it is.

Up Arrow and Down Arrow move the highlight, and Return opens the highlighted
file as a tab (or brings its tab to the front when it's already open). Clicking
a file opens it too. When nothing matches, the list says "No files match", and
Return beeps. Escape closes the field, and so does a click anywhere else. The
folder tree stays as it was, with its open folders and filter untouched.

Open quickly lists up to 200,000 files and folders. In a bigger tree (a window
with no folder starts at your home folder, with Library in it) it stops there
and the last row of the list says "Showing the first 200,000 files. Open a
smaller folder for the rest." Files past the stop aren't listed, and the
explorer's filter shows the same note under its matches.

## Tabs

The tabs sit along the top of the window, right of the folder tree, on the
line of the window buttons, with a thin line under them. Each tab is as wide as
what it shows, up to 200 points: the file's icon at the left (the folder tree's icons; an
Untitled page has the plain text one), then the file's name. While the file has unsaved changes a dot
follows the name;
a file that can't be written shows a lock there instead, and with unsaved
changes the lock carries a small dot at its corner. The tab grows a little to
make room for the dot and gives the room back after a save. The tab in front
stands on a small raised plate, lit along its top in a dark theme and white on the
grey tab band in a light one; the other tabs are plain. Point at a tab and its
icon fades into a close button, which
takes a click a little outside the small x too. Rest the pointer on any tab and
its tip gives the file's full path (with ~ for your home folder; an Untitled
page gives its title), "Read-only" on its own line for a locked file, and
"Changed on disk" or "Deleted from disk" when that has happened. Untitled
pages are numbered in the order you made them (Untitled, Untitled 2, Untitled
3) until they have a first line; then the tab takes that line instead (see
Draft titles). A number stays with its page while it is open, and a new page takes the
lowest number nobody holds. Files with the same name show the folder they are
in after the name, a step quieter (mod.rs src), and the folder is cut first when
the tab is short. The tabs, the close buttons, the
preview button, the explorer button and the folder tree's header buttons all light up and darken
with a short fade as the pointer comes and goes, and a little more while pressed. A name too long for its tab shortens
in the middle and keeps its extension ("workspa....rs"). When the tabs don't
fit, the bar scrolls, and an end with more tabs past it fades out instead of
cutting a tab off; a very narrow pane shrinks a tab to fit, down to about 116 points. The plate under the tab in front is
raised more while its pane has the keyboard in the front window, and flatter when it doesn't.
When a save lands, the dot shrinks away in a moment and the tab closes up its room, and the title brightens once; Save all ripples that across the tabs, left to right. A save that fails swells the dot once in the warning color instead. A save that is still writing a quarter of a second after it started (a big file, a slow disk) makes the dot breathe, dimming softly and back about once every second, and the shrink starts from however bright the dot is when the save lands; a quicker save shows nothing extra.
Hold Cmd and click a tab to see where its file lives, as the title of a Finder or TextEdit window does: a menu opens under the tab with the file, then each folder above it up to the disk, each with its Finder icon. Pick one and Finder opens it with the next one down the path selected (the file itself is shown selected in its folder). The tab's right-click menu has the same as Show path. A page with no file, such as an Untitled one, has no path menu and the Cmd-click just selects it.
Rest the pointer on a tab for half a second and its tip opens as a small card under the tab: the tip's lines (path, Pinned, Unsaved changes and so on), then the five lines of the file around its caret, with the caret's line tinted, in the editor's font. It is the same for the tab in front. Move to the next tab and the card glides across to it instead of closing and opening; leave the tabs, press a tab or start a menu and it fades. A file you haven't looked at yet in this window shows only the tip, since its text isn't read until you do. The card doesn't mark changed lines.
Press the file icon at a tab's left (the spot where the close x shows under the pointer) and drag: a small card with the file's icon and name follows the pointer, and letting go on the Desktop, in a Finder folder or in a Mail message copies the file there, as the proxy icon in an old title bar did. A plain click on the same spot still closes the tab, and dragging the rest of the tab still reorders it. It is the file as it is on disk that goes, not unsaved changes; save first to send those. An Untitled page has no file, so its icon drags like the rest of the tab.
While the tabs don't fit, a small chevron sits at the right end of the bar. Click it
for a list of every tab in the pane, in the bar's order. Each line has the file's
icon, or a page icon for an Untitled tab. Two tabs with the same file name show
their folder after it, like `index.md (guide)`. Unsaved tabs end with a dot, a
pinned tab ends with "Pinned", and a check marks the one in front. A very long
name loses its middle and keeps its start and extension. Pick one and it comes
forward and scrolls into view. While a tab you can't see has unsaved edits or a
changed-on-disk warning, the chevron shows a small dot at its top right. The
chevron goes away when all the tabs fit again.
In full screen there are no window buttons, so the tabs start by the explorer button at the left edge
and the window's empty toolbar is hidden; the band is yours again, and comes
back when you leave full screen.

- Cmd-T or Cmd-N opens a new Untitled tab. Cmd-N no longer opens a window.
- File > New from template opens a new Untitled page that already holds a
  starting text: a Markdown note with front matter, a Daily note (today's date
  as a heading), a README, or a shell script with its `#!` line. The page in
  front takes it when it's a blank Untitled page; otherwise it opens in a new
  tab. A template can hold `{{date}}`, `{{title}}` and `{{cursor}}`: the date is
  filled in and Tab hops between the other spots (see Templates with a date and
  tab stops). Your own templates are the files in the
  `templates` folder next to the settings file
  (`~/Library/Application Support/Finch/templates/`); they're listed by file
  name below the built-in ones, and the last item, Open templates folder, makes
  that folder if it's missing and shows it in Finder. A file you add shows up
  the next time a Finch window comes to the front. Hidden files are skipped,
  and a file over 1 MB or one that isn't text is refused with a note that says
  why. The page is a plain Untitled page like any other: it colors once you
  save it with a name.
- Cmd-W closes the tab in front. If it has unsaved changes, it asks Save,
  Don’t Save or Cancel for that one file. Closing the last tab leaves an empty
  page with the folder tree, and Cmd-W on an empty page closes the window.
  Clicking a tab's close button, or middle-clicking a tab, closes it too. The
  tab that shows next is the one you used just before, not the neighbour.
- Cmd-Shift-T (File menu, Reopen closed tab) brings back the tab you closed
  last, in the pane and slot it was in, with the caret where it was. Press it
  again for the one before; the last 20 closed files are kept. A file that is
  gone or already open is skipped, and an Untitled page isn't kept. A pinned
  tab comes back pinned, in its old place among the pins. With
  nothing to bring back (or every closed file gone) the menu item is grey and the key beeps.
- Right-click a tab for Close tab, Close other tabs, Close tabs to the left and Close tabs to the right (the
  last three grey when they would close nothing, as in the File menu). On the tab in front
  the menu also shows Cmd-W, Shift-Option-Cmd-R (Show in Finder) and Shift-Option-Cmd-C (Copy path);
  the other tabs' menus show no keys, since the keys act on the tab in front. When
  two or more of the tabs being closed have unsaved changes, one question
  covers them all: Save all, Discard all or Cancel. Save all saves each file
  and closes the ones that saved (a tab whose save failed stays open); Cancel
  closes nothing. With one unsaved tab the question is the usual Save, Don’t
  Save or Cancel, and Untitled pages never ask.
- Right-click a tab and choose Pin tab (Unpin tab on a pinned one). A pinned
  tab moves to the end of the pins at the left of the bar and stays left of
  every other tab: new tabs open after the pins, and dragging a tab along the
  bar keeps pinned tabs among the pins and the others after them. A pinned tab
  shows a pin after its name (with unsaved changes the dot
  shows there instead) and has no close button. When the tabs don't
  fit, the pinned tabs stay at the left of the bar and only the others scroll,
  under a fade at the pins' right edge; showing a tab scrolls it clear of the
  pins. Close other tabs,
  Close tabs to the left and Close tabs to the right leave pinned tabs open; Cmd-W and middle-click still
  close one. A pinned preview tab becomes a normal tab. Pins come back when
  you start again on the same folder.
- Window > Move tab earlier and Move tab later move the tab in front one place along the bar, within its own group: a pinned tab stays among the pins and the others stay after them. The tabs glide as after a drop, and each item is grey when the tab is already at that end of its group.
- Below Pin tab, the tab's menu has Move tab to left, right, upper and lower pane
  (each grey when no pane lies that way; see [Panes](panes.md)), Move tab to new window
  (grey while the window holds only that tab), Split right and Split down (grey while the pane
  holds only that tab; they split for the tab you clicked), then Show in
  Finder, which selects the file in
  a Finder window, Reveal in explorer, which shows the explorer if it is hidden and selects the file in its tree,
  and Copy path, which puts the file's full path on the
  clipboard. File > Show in Finder (Shift-Option-Cmd-R) and Copy path
  (Shift-Option-Cmd-C) do the same for the tab in front. An Untitled page has no file yet, so all of these are greyed out.
- The tabs come back when you start Finch again on the same folder, in
  the same order with the same tab in front, including the Untitled pages
  (their text comes back from the drafts, see Save; a draft that isn't valid
  UTF-8 is read as any file is when no byte is lost, and one that isn't text, or
  would lose bytes, is kept in the drafts folder, and the window says so once with a Show in Finder button). A file that no longer exists
  is skipped when it is the tab in front; any other remembered tab is only checked when the window comes forward or you open it, and a tab whose file is gone is closed then. With several windows open, the folder and tabs of the window
  that changed last are the ones kept, and only the first window of a start
  brings them back.
- Shift-Cmd-[ and Shift-Cmd-] show the previous and the next tab by position.
  Cmd-1 to Cmd-8 show that tab, and Cmd-9 the last.
- Ctrl-Tab and Ctrl-Shift-Tab go by use: while Control is held, each Tab press
  goes one further back (Shift: forward) in the tabs you used last, so
  Ctrl-Tab twice lands on the tab you used two back. Letting go of Control
  settles on the tab shown and makes it the most recent. A quick Ctrl-Tab
  swaps between the two latest tabs.
- Drag a tab sideways to move it; it lifts off the bar as you pick it up as a
  lighter card with a faint rim, follows the pointer, and the others make room.
  Let go in the bar and it glides into its new place.
- The window's title follows the tab in front. The red close button's dot shows
  while any tab in the window has unsaved edits, whichever tab is in front. The
  window's close button and Shift-Cmd-W still close the window.

## Show and hide the folder tree

Press Cmd-B to hide the folder tree or show it again. It slides shut and open
(the text beside it follows its edge, and so does the sidebar look behind it),
and its rows fade in once it is partly open. Pressing again halfway turns it
around from where it is. Finch remembers the choice for the next start.

A small side-panel button right of the window's three buttons does the same as Cmd-B. It sits in the same place whether the tree shows or not, rests in grey, brightens when you point at it, and says "Hide explorer ⌘B" or "Show explorer ⌘B" after half a second. With the tree hidden, the first tab starts right of it. In full screen, with no window buttons, it moves to the left edge.

## Preview Markdown

Press Cmd-Shift-V on a Markdown file to see it formatted in a panel on the
right; it slides in, and pressing it again slides it out. See [Markdown preview](preview.md).

Hold Cmd over a link in the editor (a web address, a mail address or a Markdown link) and it underlines; Cmd-click opens it in the browser, or opens a linked file in a new tab.

The same works on an error's location in compiler output or a stack trace, such as `src/main.rs:42:7`, `--> src/main.rs:42`, `File "run.py", line 9`, `at foo (/abs/x.js:10:5)` or `Program.cs(42,7)`. Hold Cmd over it and it underlines, but only when that file exists: Finch looks from the folder of the window's file tree first, then the folder of the open file (the folder you ran it from, for text piped in). Cmd-click opens the file in a tab with the caret on that line and column, and the line washes briefly. Run `cargo build 2>&1 | finch -` and Cmd-click the error lines one after another to walk through them.

## Outline ticks in long notes

In a Markdown file with three or more headings that is longer than the window, a thin column of ticks fades in at the right edge when you scroll or point there, with the scrollbar's own fade, just left of its track. There is one tick for each heading, and the higher the heading's level the longer its tick. The tick of the section the top of the view is in stays brighter than the rest as you scroll.

Slide the pointer along the column and a small pill beside it names the heading nearest the pointer. Click to go there: the caret moves to the heading and the view glides to it, as it does for Go to a line. Headings are read from `#` lines and from lines underlined with `===` or `---`; lines inside code blocks and the front matter at the top don't count. A heading list is read a moment after you stop typing, and not at all for a file over 4 MB. Other kinds of file show nothing new.

## Lists in Markdown

In a Markdown file, Return after a list item starts the next one: `-`, `*` and
`+` repeat, numbers count up, and a task box starts unchecked. Return in the
middle of an item splits it into two items. Return on an item that holds only
its marker ends the list. Shift-Return, and any line inside a code fence, give
a plain new line.

Select text and type `*`, `_`, `` ` `` or `~` to wrap it, and the text stays selected. Paste a
web or mail address over selected words on one line to make a link; Cmd-Z gives the words back.

## Wrap long lines

Long lines wrap to the window's width, and the wrapped part starts at the left
edge of the text, with no extra indent. In a long file the window stays usable
while that happens: the lines near what you're looking at wrap first, the rest
follow over the next few seconds, and the scrollbar settles as they do. A file
with more than 50,000 lines opens with wrap off, because wrapping all of it
takes that much longer. In the View menu, Soft wrap loses its check mark. Choose
Soft wrap to wrap the lines anyway, and again to stop wrapping; Option-Z wraps
them too, at the window's width. It works on any file and lasts until you open
another one.

With Reading width on, wrapped text stops at the reading column. Press Option-Z
to let the file in front wrap at the window's width instead, and press it again
to go back to the reading column. View > Wrap at window width has a check mark
while the file wraps to the window. Each file keeps its own choice until you
open it again.

A code file over 4 MB opens without colours, to save memory: the colouring of a
huge file holds hundreds of megabytes. The status line still names its language
and a Markdown file still has its preview. Rename or Save As re-checks the size.

## Show spaces and tabs

Choose View > Show invisibles (Option-Cmd-I) to draw a faint dot for every space
and an arrow for every tab, in the text of every tab in every window. Choose it
again to hide them. The check mark in the menu says which way it is, and
Finch remembers the choice for the next start (`show_invisibles` in the
settings file). Editing that line in the settings file by hand does the same at
once, with no restart. Line ends aren't marked.

## Show line numbers

Choose View > Show line numbers to hide the numbers down the left side of the
text, in every tab in every window, and choose it again to bring them back. The
check mark in the menu says which way it is. The line at the top of the window
stays where it is while the text moves over to fill the gap (or give way to the
numbers), even when long lines wrap. Finch remembers the choice for the next
start (`line_numbers` in the settings file, on when the line is missing), and
editing that line by hand does the same at once. There's no key for it. You can
also right-click anywhere in the left column (the numbers, the fold arrows, the
change bars) for a one-row menu, Show line numbers, checked while they show;
with the numbers hidden, the narrow strip that's left still takes the
right-click, so you can bring them back. A right-click in the text keeps the
usual text menu.

## Fold code

In a code file, a function, a block or any other part that runs three lines or
more can be folded down to its first line, so the rest of the file is easier to
see. With the caret in it, press Option-Cmd-Left (View > Fold) to fold the
innermost part around the caret, and Option-Cmd-Right (View > Unfold) to open
the one the caret is on again. Press Option-Cmd-Left again to fold the part
around it. The part's last line (the closing brace) stays shown. View > Fold
all folds every part in the file in one go and View > Unfold all opens them all;
they leave the line at the top of the window where it is.

The caret never sits on a hidden line: folding around it moves it to the end of
the first line. Go to line, Go to symbol, Find (a match inside a fold), Cmd-D and Undo or Redo open
the fold that hides where they land. Backspace and Delete next to a fold remove one character, never the hidden lines. The keys and the menu items work with line
numbers off and in minimal mode too. Folds aren't saved: a file opens with
everything unfolded. Option-Cmd-Left closes the folder tree's folders instead
while the tree has the keyboard.

Move the pointer over the numbers and an arrow appears beside each line that
opens a block (a function, a class, a list). Click it to fold the block into its
first line, and click it again to open it. The arrow fades away when the pointer
leaves, except on a folded line, where it stays. A folded line ends in a small
rounded `...` chip, and a click on the chip opens the block too. Blocks glide
shut and open.

## Changes since the last commit

In a file that git tracks, a thin bar beside the line numbers marks what differs
from the last commit: green for lines that are new, amber for lines that
changed, and a small red notch where lines were taken out. The colors are the
ones the folder tree's file icons wear for the same states. Click a bar and a small card
opens with the old lines. Revert puts them back in place of the change, as one
step that Undo takes back, and Copy old text puts them on the clipboard. Click
the bar again, click anywhere else or scroll to close the card. View > Next
change and View > Previous change move the caret to the first line of the next
or previous change the way Go to line does, and beep when there is none.

The bars show up when the file opens and settle about half a second after you
stop typing, so they stay still while you type. They follow the file when it
changes on disk, and they read the commit again after you commit or switch
branches. A file git doesn't know (new, ignored, or renamed and not yet
committed), one that isn't text, and one over 4 MB get no bars, and a file so
different from the commit that the comparison takes too long shows none rather
than marking everything. With line numbers off the bars stay at the text's left
edge. There's no key for the menu items.

## Reading width

Long lines are tiring to read, so Finch stops the text at a comfortable width.
The text still starts at the left edge, right after the line numbers, so
turning Soft wrap on never moves it right. In a Markdown or plain text file the
column is about 70 characters wide; in any other file it only starts once the
pane is wider than about 1,400 points, and then it is about 100 characters. The
current-line band still runs the full width. The width comes from the editor
font's own character width, so a different font gets its own column. The text
doesn't re-wrap while the pane stays wider than the column. A pane narrower than
the column looks the same as before, and so does a file with Soft wrap off
(including files over 50,000 lines). Minimal mode centers the column.

Choose View > Reading width to turn it off in every tab in every window, and
choose it again to bring it back. The check mark in the menu says which way it
is. Finch remembers the choice for the next start (`reading_width` in the
settings file, on when the line is missing), and editing that line by hand does
the same at once. There's no key for it.

## Text style

Choose View > Text style, then Mono, Serif or Sans, to change the letters the
file in front is drawn in. Mono is how Finch has always looked: your editor
font (Google Sans Code NF unless you set `font_family`). Serif is New York and
Sans is SF Pro, the Mac's own reading fonts, drawn at the regular weight with
the rows a little taller (1.65 times the text size, against 1.5 for Mono). The
check mark says which one the front file uses.

The choice is kept for the kind of file: one style for prose (Markdown and plain
text) and one for everything else, so notes can read in serif while code stays
Mono. It applies to every tab of that kind in every window, and the text fades
out and back in over about a quarter of a second as it changes, wrapping to the
new letters once. Finch remembers the choice (`prose_style` and `code_style`
under `[editor]`, both `"mono"` when missing), written once per pick, and
editing them by hand does the same at once. The text size still follows Bigger
and Smaller. There's no key for it.

## Scroll past the end

The last line of a file doesn't stop at the bottom edge: scroll down and the text
keeps going until the last line sits in the middle of the window, with empty space
under it. Typing at the end of a file never has to happen on the bottom edge. The
scroll bar's knob counts that space, so dragging it to the bottom puts the last line
mid-window. A file shorter than half the window doesn't scroll at all. There's
nothing to turn on.

## Typewriter scrolling

Choose View > Typewriter scrolling to keep the line you're typing at one height,
a little above the middle of the window, so your eye never chases the caret.
Typing, Return and the arrow, Page Up and Page Down keys (and Find and Go to line)
move the page under the line instead of moving the line, with the same
easing as the glide at the end of a file. The first lines of the file can sit at
that height too, and, with Scroll past the end, so can the last. A click in the
text or the wheel never moves the page by itself; the next key you press puts
the caret's line back at the height. The page stops moving as soon as it lands.
The check mark in the menu says which way it is, and Finch remembers the choice
for the next start (`typewriter` in the settings file, off when the line is
missing). Editing that line by hand does the same at once. There's no key for it.

## Pinned scope lines

Deep inside a long function, table or block of YAML, the line that opens it stays pinned
under the tab bar in a quieter ink, so you always know where you are. Blocks inside it
pin under it, up to three levels, outermost first. As you scroll back up past a block's
first line, or down past its last, its pinned line slides away. Click a pinned line to
glide the caret to that line in the text. It works wherever the editor can fold code
(Rust, Python, JSON, YAML and TOML), with nothing to turn on. A short window pins one line
or none, so the lines never crowd the one you're typing on, and a file over 4 MB pins nothing.
Find's bar stays above the pinned lines.

## Minimal mode

Choose View > Minimal mode (Control-Cmd-Return) to see nothing but the text. The
tab bar, the folder tree, the status line, the preview and the window's three
buttons all fade away together in one short ease, and the text keeps its place
(the top margin eases down to a small one, so nothing jumps). The line numbers
and the scrollbar go too, and the text stays in its centered reading column
even when Reading width is off. It works the same in full screen, where the
window is the page edge to edge.

Move the pointer to the very top edge of the window to bring the tabs and the
window buttons back; they stay while the pointer is in that band and fade again
when it leaves. Press Control-Cmd-Return again, or Escape twice within half a
second, to leave (an Escape that closes the find bar or a popup doesn't count).
Shift-Cmd-V still shows the preview while you are in the mode, and Cmd-B leaves
the mode and shows the folder tree.

Minimal mode belongs to its window, so splits follow it together. Finch
remembers it for the next start (`minimal` under `[window]` in the settings
file, off when the line is missing). Turning it on or off never changes your
folder tree, preview or line number choices: they are back as you left them.

## Find text

Press Cmd-F (Edit > Find > Find…) to open the find bar. It floats over the top
right of the text, just under the tabs, and rises into place; the text
underneath stays where it is. Type what you're looking for and press Return to
jump to the next match (Shift-Return goes back). The view glides there over a
fifth of a second (a match more than a screen away first cuts most of the way);
arrow keys still move it at once, typing at the match lets the glide finish, and
a wheel turn the other way stops the glide. The bar counts the matches as "3 of 12" (with a comma in big counts, "1,234 of 12,345"), and its buttons
(hover one for its name) step to the previous or next match and close the bar.
Escape closes it too and leaves the current match selected in the text, ready to
edit. With no match, your selection stays where it was. When nothing matches, the
previous and next buttons (and Replace and Replace all) ease to a dim look and stop responding.
Close the bar and reopen it right away and it turns around from where it is,
instead of starting over. When the window isn't the front one, the find field
softens to its resting shade.

With several lines selected, Cmd-F, Cmd-E and Shift-Cmd-F all search that text, line breaks
included; the find field is one line, so it shows the text with the breaks left out until you edit it.

The replace row (the arrows button beside the close button, whose tip shows its key, Shift-Cmd-F) has a Replace field.
With the row open, Shift-Cmd-F moves to the Replace field instead of closing the row; pressed again there, it closes the row (so does the button). One Cmd-F closes it from either field, and the button's tip shows that key.
Press Return in it to replace the current match and move on to the next one,
or click Replace; Replace all swaps every match at once, and the count then
reads "Replaced 4" until you change the search or the text. When Find next
(Return or the arrows) runs past the last match and comes round to the first, the
next button lights up for a moment so you notice the wrap; Find previous
wrapping the other way lights the previous button. A plain step doesn't. Return
and the arrows pulse for any count; Cmd-G does with three or more matches.
When a search finds nothing, Return or Cmd-G brightens "No results" for a moment so
you can tell the press landed; typing in the field doesn't. The count fades from
"12 of 40" to "No results" or "Replaced 12" and back when its kind changes, and
stepping through matches changes the numbers at once.

Cmd-F with the replace row open (from either field) goes back to plain Find: the row closes and the caret lands in the find field with its text selected.
Edit > Find > Skip and select next occurrence (Cmd-K Cmd-D) drops the newest Cmd-D pick and adds the occurrence after it, so an unwanted match is passed over and the earlier picks stay.

Matching ignores case until you type a capital letter: `cafe` finds `Cafe`, but
`Cafe` finds only `Cafe`. The case button in the find field overrides that and
stays as you set it. Accented letters match across case too, so `café` finds
`CAFÉ` (letters that expand, such as `ß` and `SS`, are not treated as equal).
The whole-word button next to it skips matches inside longer words: `foo`
passes over `foobar` and `foo_bar`.

Edit > Find > Ignore diacritics (off until you turn it on, with a check mark while it is on) lets a letter find
the same letter with any accent: `cafe` finds `café` and `resume` finds `résumé`, whether the accent is one
character or a letter plus a combining mark, and a replaced match takes the accent with it. The accents in your
query are ignored too, so `café` finds `cafe`. It covers Latin, Greek and Cyrillic letters, and a
regular expression ignores it. It has no key. The last option button in the find bar, after Find in selection (an "æ", tooltip
"Ignore diacritics") turns it on and off and stays in step with the menu item; like Whole word it stays on for the
next search in that file. When the editor is too narrow for the whole bar, it drops option buttons from the right
(Ignore diacritics, then Find in selection, then Regular expression, Whole word and Match case) so Next, Replace and Close
stay inside it. An option that is on is never dropped; the next one that is off goes instead. Every option stays in Edit > Find.

The curly-braces button reads the query as a regular expression: `\d+` finds each
run of digits, `^TODO` finds lines that start with it, and `\n` matches a line
break. Match case and Whole word still apply. In Replace, `$1` and `${name}`
stand for the match's groups (`$$` is a dollar sign), so `(\w+)@(\w+)` with
`$2:$1` swaps the parts. Replace all is one Undo. A pattern that isn't valid
shows "Bad pattern" and matches nothing; rest the pointer on the count and a tip says what is wrong, such as "Unclosed bracket" or "Nothing to repeat". A match that is empty, such as `x*`
on a line without `x`, isn't shown.

All three options have keys and menu items: Option-Cmd-C (Edit > Find > Match case),
Option-Cmd-W (Whole word) and Option-Cmd-R (Regular expression), each with a check mark while it is on. Rest the
pointer on a button to see its key. With the find bar closed, Whole word and Regular expression carry
to the next search; Match case is picked again from the query's capitals when
Find opens.

Switching an option from the menu or its key while you're typing in the text leaves the
caret where it is; only Cmd-F and Shift-Cmd-F move into the find bar.

The framed-text button next to the braces (Option-Cmd-L, Edit > Find > Find in selection, grey while
the bar is closed) limits the search to what is selected when you turn it on, and the text
that limit covers gets a faint fill. The count, Find next and previous, the ticks in the
scroll bar and Replace all (one Undo) leave out everything outside it. Typing or replacing
inside the selection keeps the limit with the text; turning the button off brings back the
whole file. Closing the bar ends it, and so does Cmd-F with a different selection. With only a
caret there is nothing to limit to, so the count reads "No selection".
Each selected range keeps its own limit when you type with several carets, indent or toggle comments.

The Find submenu's Replace, Replace and find and Replace all work on the open replace row
from wherever the caret is, without moving it. Replace (Cmd-=) swaps the current match and
stays in the view; Replace and find (Option-Cmd-=) does the same and moves on to the next
match, as the Replace button does (its tip says so); Replace all (Option-Cmd-Return) is
the Replace all button. Rest the pointer on either button to see its key.

In the find field, Up brings back the last search, and again the one before it, up to the
last 20 you ran this session, newest first and none twice. Down steps back and, past the
newest, gives you what you had typed. A search joins the list when you press Return or Cmd-G
with it, or close the bar on it. A query with several lines keeps Up and Down as caret moves.
The Replace field keeps its own list the same way: Up brings back the last replacement and
Down steps forward again. Replace, Replace and find and Replace all each add what the field held.
The magnifier in the find field opens a Recent Searches menu, as a Mac search field does: the same list Up walks, newest first, under a grey "Recent Searches" title. Pick one to put it in the field and search for it; Clear Recent Searches empties the list (grey when it is empty) and leaves the field as it is.

While Find is open, the editor's scroll bar marks every stretch of the file that
has matches, and the line your cursor is on. The marks stay visible when the
scroll bar itself has faded.

A click in a scroll bar's track pages the view toward the click, or jumps to the spot clicked if System Settings says so; Option-click does the other.

Cmd-E (Edit > Use selection for find) makes the selected text the find text,
or the word at the caret when nothing is selected, without moving the caret or
opening the find bar; Cmd-F then starts with it. The find text is shared
with other Mac apps through the system find pasteboard: what you search for here
is what Find shows in TextEdit, Safari and the like, and a search made there is
what Find shows here the next time you open it or come back to the text.

Option-Cmd-E (Edit > Find > Use selection for replace) puts the selected text in the Replace
field and shows the replace row, opening the find bar if it is closed. The find text stays as it
was; a selection of several lines goes in joined onto one line, as a paste into the field would.

With Regex on, selecting text and pressing Cmd-F or Cmd-E looks for exactly that text; the field shows it with special characters escaped.
With Regex on, a replacement can use `\n` for a new line, `\t` for a tab and `\\` for a backslash, alongside `$1` groups; without Regex it is inserted exactly as typed.
Typing in the find bar scrolls to the first match, when it isn't already in view, without moving the caret.

Shift-Option-Cmd-F (Edit > Find > Find in folder…) opens a float over the top of the active pane, like Open
Quickly, that lists every match of its text in the text files under the folder tree's root. The field starts with
the selection (a one-line one), or else the find bar's text, and searches at once; each key after that searches
again once you pause, and drops the search before it. A row shows the file's name with its folder dimmer, the
line number, and that line's text with the match bold; the list shows seven rows and scrolls. Matching follows
the in-file Find's three options (Match case, Whole word, Regular expression), shown as the find bar's buttons
beside the field; a click flips the same option the find bar and Edit > Find hold. It reads the files the tree
shows: `.gitignore` and `.ignore` rules apply, `.git` and dot files are left out unless Show hidden files is on,
and binary files and files with a line over a megabyte are skipped. A match lives on one line, so a pattern with
`\n` finds nothing here. Files come in the tree's name order (folders first, each in Finder's name order, whatever View > Sort by picks), and a file's
matches in line order. It stops at 500 results and says so in a dim line under the list. With nothing typed the
float shows nothing; "No results" when nothing matches; the regular expression's own message when it doesn't
compile; and "Find in folder needs an open folder" in a window with no folder. Up and Down move the pick, and
Return or a click opens the file in the active pane (or brings its tab forward), selects the match and gives it
Find's halo; Escape closes the float and leaves everything as it was.

Cmd-G (Edit > Find > Find next) selects the next match of the find text and
Shift-Cmd-G (Find previous) the one before it, wrapping round at the ends of the
file. They work with the find bar closed too, so you can press Cmd-E on a word
and then Cmd-G to walk through the file; with no find text yet they do nothing.
With the bar closed, each step also shows a small float in the editor's top right
corner, like Go to line's: "3 of 12" and, when the step came round past the end
(or the start) of the file, an arrow and "Wrapped". It fades in, stays about a
second after the last step (another step updates it in place and starts the second
over) and fades out; opening the find bar hides it at once.
Each Find next or Find previous step (Return, Cmd-G, the chevrons) gives the match it
lands on a brief halo, a lit ring that grows a little and settles in a third of a
second; typing in the find field, Replace and Select all matches show none.
Cmd-J (Jump to selection) scrolls back to the selected text after you've
scrolled away.

Control-Cmd-G (Edit > Find > Select all matches) closes Find and selects every
match for editing at once, keeping the search's case and whole-word options.
If there are more than 10,000 matches, it selects the first 10,000 in file order.
With no search open, it uses the selection or the word at the caret, keeping the
stored Whole word option. Case follows Find's smartcase: selecting "foo" takes Foo
too, selecting "Foo" takes only Foo, unless you've set Match case yourself.
Starting from a caret word always
matches whole words. Type to replace all those selections; one Undo restores them.

Cmd-D (Edit > Find > Select next match) selects the word at the caret first.
With text selected, each press adds the next occurrence after the last caret,
using those same options and case rule. Starting from a caret word keeps whole-word
matching for the sequence. It wraps round and skips text already selected,
closes the find bar
and leaves the editor ready to type. The menu item is dim when there is neither
a selection nor a word at the caret. With all occurrences selected, it does nothing.

## Several cursors with the mouse

Option-click in the text to add a cursor where you clicked. Option-click on a
cursor you added takes it away; the last cursor stays. Hold Option and drag to
sweep a column: every line between where you pressed and where the pointer is
gets its own cursor, or its own selection when the drag is wider than nothing,
all over the same columns. A line shorter than the column gets its cursor at its
end. Whatever you type, paste or delete happens at every cursor at once, as one
undo step. Escape goes back to one cursor. A cursor you add fades in with the same
soft ease as the blink; moving or typing doesn't fade them again. The pointer
shows a crosshair while Option is held over the text, and Option keeps the link
and definition hover from showing. Option-Command-Up and Option-Command-Down still add a
cursor above or below from the keyboard.

## Go to a line

Press Cmd-L (Edit > Go to line…) to open a small field in the middle of the
text, just under the tabs. Type a line number, like `120`, or a line and a
column, like `120:8` (both count from 1), and press Return: the cursor moves
there, the same way a jump from the command line does, and the field closes.
A line past the end of the file is refused once: the Mac beeps, the field tints
and shakes as below, and a red line under it says how many there are (`Only 400
lines. Return again goes to the end.`). The field stays open; type a line that
exists and press Return to jump, or press Return again to land on the last line
(that landing isn't washed, because nothing was found). Escape closes the field
and leaves the cursor where it was; a click anywhere else closes it too, and the
keyboard stays where you clicked (in the filter or the find bar, say). If what
you typed isn't a number (or a line and a column), the Mac beeps, the field tints
red for a moment and gives a short shake, like the Mac's login field, and it stays
open so you can fix it. The same shake answers other refusals: Open quickly's
Return with nothing to open and a refused rename shake their field, and a tab
command that can't act (Reopen closed tab with nothing to reopen, Close other
tabs, Pin tab, a split or a move with nowhere to go) shakes the tab bar. Like the find bar's
fields, it lights up and shows a ring while it has the keyboard, and softens when
the window isn't in front. When the jump glides more than a screen, the line it
lands on is washed once as the view arrives, the way a paste is; a short jump
doesn't wash.

Coming back to Finch after three minutes or more in another app washes the line
the caret is on once, slowly, in about half a second, so you find your place.
A quick switch doesn't, scrolling or typing ends it, and Reduce Motion skips it.

## Go to a symbol

Press Control-Cmd-J (Edit > Go to symbol…) to find a part of the open file by
name. A field opens under the tabs like Open quickly, with a list of what the
file holds, each with a small icon for its kind and its line number: functions,
and types (struct, enum, trait, impl, class) in code; headings in Markdown (the
`#` kind and the kind underlined with `===` or `---`, but not a `#` line inside
a code fence); tables in TOML (`[a.b]` and `[[x]]`); and the keys at the left
edge of a YAML file. The list opens on the part the cursor is in. Type to narrow
it: names are matched and ordered the way Open quickly does it, with the matched
letters in bold.

The text behind the field follows the pick. Press Up and Down and the cursor
goes to that item's line (washing nothing), and the view glides there when it is
a screen or more away. Return keeps the cursor on that line, washes it once as a
far jump's landing is washed (on arrival when the view glides), and closes the field; Escape puts the cursor back and
glides the scroll back to where it was (a far preview lands the view by the
cursor's own glide instead); a click elsewhere closes it and
leaves the text where it is. Up and Down leave a fold shut; Return opens the fold the line is in. The list is
read from the syntax the editor already keeps, when you open the field, so
nothing runs until then. A file over 4 MB, or a language with no syntax support,
lists nothing.

## Writing goal

Set a number of words to write in a Markdown or plain-text file and watch the
status line's top edge fill. Put `goal: 500` in the file's front matter (the
`---` block at the very top), or choose Format > Writing goal… and type a number;
Return sets it, an empty field clears it, Escape and a click elsewhere leave it
as it was, and a number that isn't a whole number of words (or is 0) makes the
Mac beep and the field shake. A goal from the menu belongs to the open file for
this session only: it isn't saved in the settings and it isn't written into the
file, and it wins over the front matter's while it's set. Closing the tab ends it.

With a goal, a one-pixel line along the bottom of the text, just above the status
line, fills in the accent color from the left as the word count climbs. When you
reach the goal it glows once, for under a second, and turns green; nothing else
moves or pops up. A file that already has the words when you open it, or a goal
you set below the count, is simply green. Setting a goal from the menu grows the line in from the left, and clearing it fades the line out where it stands; opening a file that has a goal draws the line at once. The line has no effect on a code file,
which has no word count.

With a goal, resting the pointer on the word count in the status line says how far along you are (`412 of 500 words, 82%`, or `Goal reached`), and its pop-up gets a Goal row with the same words.

## Step back through your edits

Edit > Undo history… shows a slider over the text that steps the file back through every edit since it opened: see [Undo history](undo-history.md).

## Right-click menu in the text

Right-click in the text for the Mac's own menu: Cut, Copy and Paste (grey when
there is no selection, or the file can't change), Select all, Toggle comment,
Use selection for find, and Reveal current file, which opens the folders down to
the file in the tree. Each shows its key, like the explorer's menu does. Copy
(here or Cmd-C) brightens the selection once as it takes the text, then it fades
back; Cut doesn't, since the text leaving is the sign. With nothing selected,
Cmd-C copies the whole line the caret is on and Cmd-X cuts it, and Cmd-V puts a
line taken that way back above the caret's line, wherever the caret sits in it.

Look up “word” (Control-Cmd-D) opens the Mac's definition card for the selected
text, or the word at or immediately before the caret when nothing is selected.
It appears only for text containing a letter or number. Long selections are
shortened in the menu title, not in the card's lookup text.

## Insert text from an iPhone photo or scan

Edit > Import from iPhone or iPad > Take Photo or Scan Documents (the Mac's own
items, shown when an iPhone or iPad is nearby and the file can change) puts the
text in a photo or scan at the caret. The text is read on the Mac, a moment
after the picture arrives, and goes in as one edit, brightened once like a
paste; one Undo takes it out. Pages of a scan are separated by a blank line. A
picture with no readable text inserts nothing and the Mac beeps. Your clipboard
isn't touched.

## Config files

In a JSON, TOML or YAML file, a small amber note fades in at the left of the status line when
the file stops parsing: "Line 12: expected a comma". It shows about a third of a
second after you stop typing, never while you are in the middle of a word, and
never stops you saving. A faint wavy line marks the spot in the text, and
clicking the note puts the caret there. It goes away as soon as the file parses
again. Comments in JSON are fine, and so are trailing commas in `.jsonc` files
and in `tsconfig.json`. A YAML file with several documents (`---`) is fine too.

Edit > Transformations > Format JSON re-indents a JSON file in one Undo step,
with the file's own indentation (two spaces if it has none) and its comments
kept. When the file doesn't parse it does nothing; the amber note says where it
stops.

The status line also shows where the caret is in a JSON, TOML or YAML file, as a
quiet trail after the position, led by a small outline symbol (three indented lines) so it reads as a place and not a stray word: `server > ports > 0`, with a key or an array position at each
step. It follows the caret as you move and type, and while the file is briefly
not parsing it keeps the last place it knew. Click it to copy the path
(`server.ports[0]`); Option-click copies a jq filter (`.server.ports[0]`). The
trail says "Copied" in its own place for a second, then fades back. Keys
with spaces or other odd characters are quoted. In TOML it reads tables, dotted
keys and `[[array of tables]]` entries; in YAML, block and flow forms alike.

## Save

Press Cmd-S to save the file in front. While a tab has changes that aren't
saved yet, it shows a dot where its close button is, the window's close button
shows a dot while the tab in front has them, and the file shows a dot after its
name in the folder tree. If the save fails, a message says why and offers
Save as… to choose another name or folder. Return opens Save as; Escape or
Cancel leaves your text unsaved. If that Save as fails, its message ends at
OK without asking again, and your text stays unsaved.

Saving keeps the same file, including its hard links and Finder tags. It
tries to reserve room before making a file longer, so a full disk usually
stops the save before the file is touched. A file that shares space with a
copy or a Time Machine snapshot can still be left partly overwritten.
The file is shortened only after the new text has been written. A force
quit or a disk error during the overwrite can also leave old and new text
mixed; this is not an atomic replacement.

A big file saves while you keep typing and scrolling. If you type during the
save, those new changes stay unsaved (the dot stays) and the next Cmd-S saves
them. Closing the tab or quitting waits for the save to finish first, so the
file on disk is never left half written, and if something is still unsaved
after the wait, you are asked as usual.

Quitting from the Dock or while logging out asks about unsaved files just as
Cmd-Q does. Save (Save all for several files) saves before quitting, Don’t Save
(Discard all for several files) quits without those changes, and Cancel keeps every tab open and cancels the logout request.

Two settings tidy a file as it is saved, both off until you turn them on in
Settings (Cmd-,) > Editor or in the settings file: `trim_trailing_whitespace` removes the spaces and
tabs at the end of every line (in a Markdown file, two or more spaces after text
stay, since they make a line break), and `insert_final_newline` makes the file end
with a newline (one is added only when it is missing, in the file's own line
ending). They apply to Save, Save as, Save all and a save from the close
question, never to the Untitled drafts Finch writes by itself. The
tidy-up is one change in the text, so Cmd-Z undoes it, and the caret and the
view stay where they were. The lines it changed glow once, like a paste, and a
save that changes nothing doesn't. Editing the settings file takes effect at the next
save.

An Untitled page saves itself. With its first character, it gets a file in
`~/Documents/Scrap Drafts/`, named for when you started it (`Untitled
2026-10-30 14.05.txt`, with ` 2`, ` 3` added if that name is taken). Your edits
are written half a second after you stop typing (and at least every 5 seconds while you
keep typing), and at once when you close the tab or the window, quit, or switch to another app, so a crash leaves the
last full copy. An empty page leaves no file, and a draft you delete all the
text from is deleted from disk until you type again. Because it is always on
disk, an Untitled page shows no unsaved dot and closing it asks nothing. The
first time Finch writes there, macOS may ask whether it can use your
Documents folder, with the reason “Finch keeps your Untitled pages in
Documents so you never lose them.”; say yes, or drafts can't be saved. Cmd-S on an Untitled page
asks where to save it, as Save as does; once it is saved there, the draft file
is deleted and the tab follows the new file.

Text from another app can be sent to a note without switching to Finch: see [Services and Shortcuts](services.md).

Each save keeps a copy of the file, so you can look at an earlier one and take it back: see [Old versions](versions.md).

Choose File, Save as… (Shift-Cmd-S) to save the file under another name or
in another folder. The window then shows the new file, and the old one stays on
disk as it was, and the new file joins File, Open recent. On an Untitled page
the save panel opens in the folder your tree shows; on a file it opens in that
file's own folder. If you save over a file that another tab (in this window or
another) has open, that tab closes, since one file is one tab. When it has
unsaved changes of its own, it comes forward and asks first: Discard and close
closes it and discards them, Cancel keeps both tabs.

File, Save all (Option-Cmd-S) saves every file in the window that has unsaved
changes, one after another. A file that changed or was deleted outside the
editor asks its usual question first, with its tab in front. It stops at the
first file that doesn't save.

File, Revert to saved throws away the changes in the file in front and puts back
what is on disk. It asks once (“Revert to the saved version of ‘name’?”,
“Your changes will be lost.”) and Cancel is the button Return presses. It stays
grey for an Untitled page and for a file with no unsaved changes. The caret
stays on its line and the view stays where it was, but undo can't bring the
changes back.

File, Print… (Cmd-P) prints the file in front as plain text: black on white whatever the
theme, in your editor font and size, wrapped to the page, with no line numbers, colours or
highlighting. A Markdown file prints its source, not the preview. It opens the Mac's print
sheet on the window, where Save as PDF is in the PDF menu. It stays grey with no tab open.

File, Rename… opens a small field over the editor with the file's name in it and
the name without its extension selected. Return renames the file in its folder
and Escape leaves it as it was. A name that is taken, empty or has a slash or
colon shows the same reason the folder tree gives, with its warning sign, under the
field, and keeps it open; the field's red tint and the reason ease in together and
ease out again when you edit the name. File, Duplicate makes `name copy.ext` next to the file (`name copy 2.ext`
when that is taken) and opens the copy as the tab beside it; a tab with unsaved
changes copies them too, in the file's encoding and line endings, and the original
stays unsaved. File, Move to…
asks for a folder and moves the file into it; a folder that already has a file
of that name refuses. After a rename or a move the tab stays open and follows the
file, with your unsaved changes and undo history, and Open recent follows too. All three
work on the file in front whether or not the folder tree shows it, and stay grey
for an Untitled page.

## Files in other encodings

Files saved as UTF-16 (with or without a byte order mark), Latin-1 and other
Western text, or Japanese and other non-Unicode encodings such as Shift-JIS
open as readable text. A file that is valid UTF-8 is always taken as UTF-8;
anything else is guessed from its bytes. A file that is not text at all, such
as an image or a program, doesn't open: a message says it isn’t a text file.

Save writes the file back in the encoding it was opened in, with its byte order
mark if it had one, and Save as keeps that encoding too. A new Untitled page is
UTF-8. A file whose line breaks were all Windows-style (CRLF) when it opened
saves every line break as CRLF, including the ones Return and paste added; any
other file keeps its line endings as they are, a file with both kinds of break
(Mixed) included.

To change them on purpose, choose Format > Line endings > LF or CRLF, or click
the line endings on the right of the status line (LF, CRLF or Mixed) and pick
one. Every line break in the file becomes that kind as one Undo step, the tab
shows unsaved, and nothing on disk changes until you Save. From then on the file
saves with that kind, including the breaks Return and paste add. Picking the
kind the file already has does nothing, and Undo takes the whole change back.

If a file opens with broken bytes that show as “�”, Save asks before replacing
the original bytes, even if you edited those marks away. Cancel leaves the
file on disk unchanged. A guess that can save every original byte back as it
was needs no warning. The guess discards encodings that need replacement, so
this question normally appears for a file identified by its byte order mark
or after Reopen with encoding.

If you type a character the file's encoding can't hold (a Japanese character in
a Latin-1 file, say), Save asks “Some characters can’t be saved in” that
encoding. Choose Save as UTF-8 to keep every character: the file is saved as
UTF-8 from then on. Choose Cancel and the file on disk stays as it was.

If the guess was wrong and the text looks like gibberish, choose File > Reopen
with encoding and pick the right one (Unicode (UTF-8), Western (Windows Latin 1),
Cyrillic (Windows), Japanese (Shift JIS) and others; Western (Windows Latin 1)
also covers ISO Latin 1). The file is read from disk again that way, and Save
writes that encoding from then on. With unsaved changes it asks first (“Reopen
‘name’ as …?”, “Your changes will be lost.”) and Cancel is the button Return
presses. If some bytes have no character in the encoding you picked, they show as
“�”; Save then asks before writing, so the original bytes are never replaced
without your say-so.

To save a file in another encoding on purpose, choose File > Convert to and pick
one, or click the encoding on the right of the status line, which opens both
lists (Reopen with encoding, for a file on disk, and Convert to) as two submenus.
Each list holds the common encodings and ends in a More encodings submenu with
the rest (the other Latin, Cyrillic, Greek, Arabic, Hebrew, Turkish, Baltic,
Vietnamese and Thai ones, KOI8, DOS Russian, ISO 2022-JP and GB 18030); the
file's own encoding is checked wherever it sits.
Reopen reads the file again; Convert to leaves the text as it is, marks the tab
unsaved, and nothing on disk changes until you Save, which then writes the new
encoding. Converting to UTF-16 (either order) from any other encoding always
writes a byte order mark, because UTF-16 without one doesn't read back reliably;
between the two UTF-16s and into UTF-8 the file's own mark stays or stays absent,
and every other encoding has none. If a character doesn't fit the encoding you
converted to (a Japanese character in Western (Windows Latin 1)), Save asks the
same question as above before it writes anything. Revert to saved and Discard
edits read the file as it is on disk and so undo a Convert to that isn't saved
yet. Picking the encoding the page already has does nothing.

## Read a patch

A `.diff` or `.patch` file opens as a review. Each file's header (`diff --git`
and the `---` and `+++` lines under it) is a bar across the window with the
file's icon at its left; added lines are washed green and removed lines red
across the whole row; and the gutter shows two columns of line numbers, the
old file's and the new file's, worked out from the `@@ -12,5 +12,7 @@` lines
(so a removed line has only an old number and an added line only a new one).
The extra column appears only in patches; every other file keeps its single
column.

Cmd-click a line to open the real file at that line: the new file's line for an
added or unchanged line, the old file's for a removed one. The file is looked
for in the folder tree's root first, then next to the patch, with or without the
first folder of the name (`b/src/main.rs` or `src/main.rs`). Nothing opens when
the file isn't there.

A file's hunks fold under its header: click the arrow in the gutter on a bar to
fold the whole file, as in [Fold code](#fold-code).

`git diff | finch -` opens the piped text as a patch too, and so does a page
with no name whose first line reads like one (`diff --git`, `Index:` or a
`git format-patch` header). The numbers and washes are worked out a moment
after you stop typing, in the background, so they trail an edit briefly; a patch
over 4 MB keeps the plain look.

## When a file changes or goes away outside the editor

A tab with no unsaved changes shows the new text as soon as something else
writes the file, and closes by itself when something else deletes or moves the
file. The editor looks again each time the window comes to the front, so this
also covers files outside the folder tree. The lines that changed wash in a soft
tint that fades in about a third of a second, the way a paste's do; a file that
changes again within two seconds, or text that comes back the same, washes
nothing. Revert to saved washes the lines it brings back.

A tab that does have unsaved changes keeps them. Once the file has changed on
disk under them, the tab shows a warning sign in the amber warning color, in
the place of the unsaved dot, and pointing at the tab says "Changed on disk".
This includes an older copy restored from a backup. Cmd-S on it brings that tab
forward and asks Cancel, Compare…, Save as…, Discard edits or Overwrite. Return presses Cancel, which does
nothing. Save as… keeps both versions: it asks where to save your text and leaves the other
version where it is. Overwrite saves your text over the other version, and Discard edits drops
your changes and shows the file as it is on disk now. The same question comes from Save
all and from closing the tab with Save.

Compare… opens a view over that tab's text with your text on the left (Yours) and the file as it is on
disk now on the right (Disk), in the editor's font, lines that match side by side. Each place where the
two differ is tinted green on your side and red on the disk side, and the shorter side gets blank rows
so the two stay level. Each side has a narrow column of line numbers (the gutter's quiet ink, flush right, as wide as the biggest number, blank on the filler rows). The view opens scrolled to the first difference, with a few lines above it.
A small Yours or Disk chip on the first row of each difference, or a click on
either side of it, picks the side to keep (Yours at first); the side you didn't pick dims, and comes up under the pointer. A right-click on a difference opens a menu: Copy puts that side's lines of it on the clipboard (greyed when that side has none there), and Take yours and Take Disk pick a side for that difference (the side already taken is greyed). The header names
the file (a long name is cut in the middle, keeping its extension, and keeps a readable start in a narrow window), counts the differences and has Take all yours and Take all from Disk, which pick one side for every
difference at once (no keys). When the window is narrow the count drops to "2 of 5", then the buttons read "All yours" and "All from Disk", then the count goes, and last "All from Disk" is cut with an ellipsis. Option-Cmd-Down and Option-Cmd-Up (also View > Next change and Previous change) scroll the next or previous
difference to the top with a few lines above it, and the header says which you are on, like "2 of 5 changes" (it follows the wheel too); at the last or first one they beep. Cancel (Escape) closes the view and changes nothing. Save as…
saves all of your text to a new file and closes the view. Use these (Return) puts your picks together in the
tab as one change you can undo (only the lines that differ change, your caret stays on its line and
they glow once), leaves the tab unsaved, and counts the file as seen: the next Cmd-S saves
without asking again. Compare never writes to the file itself.

If the file was deleted instead, the tab keeps its text, shows the same sign
(the tip says "Deleted from disk") and its name is struck through, and Cmd-S asks Save, Discard and close or Cancel.
Save writes the file again with your text, or opens Save as if its folder is
gone too. Discard and close closes the tab and discards your changes, and Cancel keeps the tab open.
A page you emptied of all its text closes
along with its deleted file, since nothing is lost.

A file you can't write to, one with no write permission, shows a small lock
in its tab, where the unsaved dot would be; pointing at the tab turns it into
the close button. Cmd-S on it says the file is read-only and asks Cancel or Save as…
(Return presses Cancel), so you can keep your changes in a copy, instead of failing with “Couldn’t save” and the file’s name.
When the file is yours (a Finder lock or no write permission, on a disk you can write to), the question
also offers Unlock: it takes the lock off, lets you write to the file, and saves your text
at once, and the lock leaves the tab. A file that isn't yours keeps just Cancel and Save as….
The lock also goes away when the file becomes writable and the window comes to the
front again.

A log that another program keeps writing scrolls along as lines arrive when the caret sits at its end: see [Follow a log](log-follow.md).

View > Focus dims everything but the sentence, line or paragraph at the caret: see [Focus](focus.md).

Rest the caret on a name and its other copies on screen get a faint wash: see [Other uses of a name](occurrences.md).

A small round swatch shows beside each color written in code (`#FF9F0A`, `rgb(...)`, `hsl(...)`); click it to pick another: see [Color swatches](color-swatches.md).

View > Highlight tints the nouns, verbs, adjectives, adverbs or conjunctions in Markdown and text: see [Parts of speech](parts-of-speech.md).

View > Code colors picks one of 25 color schemes for code (Xcode, GitHub, VS Code, JetBrains, Solarized, Gruvbox, Catppuccin, Rosé Pine, Tokyo Night, Dracula and more) and switches Vivid, Colored roles, Rainbow brackets and Semantic colors: see [Code colors](code-colors.md).

Finch > Settings (Cmd-comma) opens one window with every setting in five panes: General, Editor, Writing, Code colors and Text. See [Settings](settings.md).

Edit > Speech > Start speaking reads the selection, or from the caret on, aloud with each word lit: see [Read aloud](speech.md).
View > Check spelling underlines misspelled words in prose, comments and strings: see [Spell check](spelling.md).
View > Style Check underlines words from your own list and sentences that run too long; Settings > Writing > Edit word list… opens the list: see [Style check](style-check.md).

## Nudge a number or flip a word

With the cursor on a number, or right after it, press Control-Option-Up to add
one and Control-Option-Down to take one away; add Shift to step by ten. (Edit >
Transformations > Increment and Decrement do the same by one.) The number keeps
its width and leading zeros (`007` becomes `008`, `009` becomes `010`), its sign
follows the result (`-1`, `0`, `1`), hex keeps its digit count and the case of its
letters (`0x0f` becomes `0x10`), and a decimal steps its whole part and keeps
its decimals (`1.50` becomes `2.50`). On `true`/`false`, `yes`/`no` or `on`/`off`
the same keys flip the word and keep its case (`True` becomes `False`). Every
cursor steps at once, as one Undo, and the changed line glows softly like a paste.
With nothing under the cursor, nothing happens. Add cursor above and below are
Option-Cmd-Up and Option-Cmd-Down.

## Quit

Press Cmd-Q or close the window. If any file has unsaved changes, Finch
asks first and names those files, every unsaved tab of every window. Save all
saves them and quits or closes only if every save worked, Don’t Save quits or
closes and drops the changes (with several files it says Discard all), and Cancel keeps the window open. Closing a window
asks only about that window's tabs, with the same Save all, Discard all or Cancel when several have unsaved changes. Untitled pages are never named and never
asked about: they are already saved as scrap drafts (see Save), and the text
is written to its draft file before the window closes or the app quits.

## Menu bar

The menu bar along the top of the screen has Finch, File, Edit, Format, View,
Window and Help menus, and an item that has a shortcut shows it. Rows with a clear symbol (Cut, Copy,
Paste, Show in Finder, Pin tab and the like) show the same SF Symbol as the
right-click menus do for that action.

On other keyboard layouts (German, French and the like) a shortcut keeps the
place macOS gives it on that layout, so Cmd-{ and Cmd-} or Cmd-? sit where the
menu shows them, and switching layouts moves them with no restart.

- **Finch:** About Finch, Settings… (Cmd-, opens the Settings window, or brings it forward; its General pane has
  an Open settings file button for the file as a tab), Services, Hide Finch (Cmd-H),
  Hide others (Option-Cmd-H), Show all and Quit Finch (Cmd-Q).
  Services sends the selected text to a Mac service; a service that returns text
  replaces the selection, and Undo restores it. Available services depend on
  the apps installed on your Mac.
- **File:** New tab (Cmd-N, or Cmd-T: an Untitled page in a new tab), New from template (a new Untitled page that starts with a Markdown note, a Daily note, a README, a shell script or one of your own templates, then Open templates folder), New window (Shift-Cmd-N: a window with an empty Untitled page), New file in explorer and New folder in explorer (slide the explorer in if it is hidden and open the name for a new file or folder in the showing file's folder, or the tree's folder; Return makes it; grey in the scratch window; see Make, rename, copy and trash files), Open…
  (Cmd-O, pick files or folders; a file opens as a tab in the window in front, a folder in its own window), Open quickly… (Shift-Cmd-O, find a file under the folder tree by name; see Open a file by name), Open draft… (Option-Cmd-O, a list of your old scrap drafts, newest first; see Open an old draft), Open recent (the files and folders you opened that still exist, newest first, as many as System Settings, Desktop & Dock, Recent documents allows, and none when that is None,
  then Clear menu; grey while there are none; a pick opens like Open… does; two
  files with the same name show their folder after it, as “README.md (notes)”),
  Save (Cmd-S), Save as… (Shift-Cmd-S), Save all (Option-Cmd-S), Revert to saved
  (grey until the file in front has unsaved changes), Reopen with encoding (a
  list of encodings with a check by the file's own; grey for an Untitled page),
  Convert to (the same list, a check by the page's own, grey only with no tab: an
  Untitled page converts too; see Files in other encodings),
  Print… (Cmd-P: the file in front as plain text, grey with no tab),
  Export as HTML… (the Markdown file in front as one web page, see the Markdown preview page; grey for any other file or no tab),
  Rename…, Duplicate and Move to… (the file in front; grey for an Untitled page, see Save),
  Show in Finder (Shift-Option-Cmd-R) and Copy path (Shift-Option-Cmd-C), also on the file in front and grey for an Untitled page,
  Close tab (Cmd-W), Close other tabs, Close tabs to the left and Close tabs to the right (grey when
  they would close nothing: a lone tab, the last tab, or the rest all pinned),
  Reopen closed tab (Shift-Cmd-T, grey when no closed file is left to bring
  back) and Close window (Shift-Cmd-W, which asks about unsaved
  files the way the close button does).
- **Edit:** Undo, Redo, Cut, Copy, Paste, Move item here (Paste's Option alternate, Option-Cmd-V: moves files copied in Finder into the explorer's folder instead of copying them; see Make, rename, copy and trash files), Select all, Select line
  (Control-Cmd-L), Select inside brackets, Expand selection
  (Ctrl-Shift-Up), Shrink selection (Ctrl-Shift-Down) and a Find submenu (Find…,
  Find next, Find previous, Use selection for find, Use selection for replace, Jump to selection, Select all
  matches (Control-Cmd-G; see [Other uses of a name](occurrences.md))), with the
  usual Cmd shortcuts. Thin lines group them: undo and redo, clipboard,
  selection, lines, comment and indent, cursors, Transformations, then Find and
  jumping around. They act on the text you're editing and go grey when
  nothing can take them. Expand selection grows each selection by syntax: the
  word (inside a string or comment), then the expression, argument list,
  block, function and file. Shrink selection steps back the same way; moving
  the cursor or typing starts the steps over. In a plain text file, Expand
  goes word, line, then the whole text.
  Pasting several lines of code into a code file moves them to the
  indentation where the cursor is, so a block copied from deeper in another
  file lines up; text, markdown, Makefile, patch, Python and YAML files paste exactly what you copied.
  Re-indent (Control-I, in Edit after Outdent) sets the indent of every line
  the selection touches, or the cursor's line, from the code's structure: a
  line inside `(`, `[` or `{` sits one step deeper than the line that opened
  them, and a line that starts with a closing bracket lines up with that line.
  Brackets in strings and comments don't count. A line that carries on the
  one above, like a method chain, a `where` clause or JSX inside a tag, keeps
  how far it sat under that line; blank lines lose their spaces, and lines
  inside a multi-line string or block comment stay as they are. It's one Undo
  step, and it does nothing in the files that paste exactly as copied.
  A paste of two or more lines, and an Undo or Redo that changes two or more lines,
  washes the rows it changed in a soft tint that fades out in about a third of a second;
  typing, a one-line paste and Replace in find never do.
  The editor reads each file's indent when it opens (spaces or tabs, from its first
  1,000 lines) and uses it for guide lines, Tab and Shift-Tab; a file with no
  indent to learn from, and a new Untitled page, gets 4 spaces. A tab draws 4 columns wide.
  With the caret next to a bracket, `(`, `[` or `{`, that bracket and its
  partner light up, ignoring brackets inside strings and comments. Jump to
  matching bracket (Control-M) moves the caret to the partner, and pressing
  it again brings you back.
  Select inside brackets selects the text between the pair around the cursor
  (again from a selection, the pair's own brackets too, then the pair around
  that), ignoring brackets inside strings and comments.
  A find hit or an undo far from the view lands with its line in the middle;
  a nearby one scrolls only as far as it must.
  Duplicate line (Shift-Cmd-D) copies the line the cursor is on below itself,
  or, with text selected, copies just the selection right after it. Move line up and Move line down (Control-Cmd-Up and Control-Cmd-Down) swap every line
  the cursors touch with the line above or below, and do nothing at the first
  or last line. Each press is one Undo step.
  Delete line (Shift-Cmd-K) removes every line the cursor or selection touches,
  and the cursor keeps its column on the next line. Join lines (Control-J) joins
  the cursor's line with the one below, with one space and the lower line's indent
  dropped; a selection over several lines joins just those lines. Transpose
  (Control-T) swaps the two characters around each cursor, or the last two at a
  line's end, in the explorer's name fields too. Each is one Undo step.
  Open line (Control-O) puts a line break at each cursor and leaves the cursor
  in front of it, so the rest of the line drops to a new row below; one Undo
  step.
  Insert line below (Cmd-Return) and Insert line above (Shift-Cmd-Return) open
  a new line next to each cursor's line, with the same indent Return would give
  and the cursor on it, wherever the cursor sat in its line; one Undo step for
  all cursors. In a find or go-to field, Cmd-Return still acts as Return.
  Select line (Control-Cmd-L) selects the whole line the cursor is on, break
  included, at every cursor; press it again and the next line joins the
  selection, as often as you like.
  Typing a bracket or quote (`(`, `[`, `{`, `"` or `'`) over selected text in
  a code file wraps each selection in the pair and keeps the text selected,
  so typing again wraps it a second time; with nothing selected the closing
  character is added as before, plain text files don't wrap, and Undo takes
  the wrap back in one step.
  Edit > Surround selection has Parentheses, Square brackets, Curly braces,
  Double quotes, Single quotes and Backticks (no keys), which work in any
  file, plain text included: each selection is wrapped in the pair and stays
  selected, and a cursor with nothing selected gets the empty pair with the
  cursor between. Edit > Remove surrounding drops the nearest pair around each
  selection (or the selection's own first and last character when they are a
  pair): brackets must balance, so `(a]` is left alone, and quotes pair up
  on their own line, ignoring an apostrophe inside a word and a quote behind a
  backslash. A selection with no pair is left as it is. Each is one Undo step.
  The Transformations submenu has Make upper case, Make lower case and
  Capitalize, which change each selection (with nothing selected, the word at
  the cursor) and keep it selected, Sort lines, which puts every line the
  selection touches in order by letter code, so capitals come before lower
  case, Sort lines descending, which does the same from the last to the
  first, Sort lines ignoring case, which orders by the letters alone and
  leaves lines that differ only in case in the order they had, Reverse
  lines, which turns the lines the selection touches upside
  down (the line endings stay put), Remove duplicate lines, which keeps the
  first of each repeated line among the lines the selection touches (or the
  whole file when nothing is selected) and deletes the later copies, Remove blank
  lines, which deletes the empty and whitespace-only lines among those (or in
  the whole file when nothing is selected), Remove trailing spaces, which strips the spaces and tabs at the end
  of every line the selection touches, or of every line when nothing is selected,
  and leaves line endings and untouched lines alone, Hard wrap, which refills
  the paragraphs among the lines the selection touches (with nothing selected,
  the paragraph at the cursor) so no line runs past 80 columns, keeping each
  paragraph's indent and its comment or quote mark (`//`, `///`, `#`, `--`,
  `>`) on every new line, with blank lines and blank comment lines left as
  breaks between paragraphs, and Convert indentation to
  spaces or to tabs, which rewrites only the spaces and tabs each of those lines
  starts with (a tab counts as the file's tab width, so a tab followed by spaces
  converts by how far it reaches; spaces after the first character of code stay).
  Converting also sets the file's indent style, so the next Tab key types what
  you converted to. Each is one Undo step, and a
  selection that is already in that case or order, or a file with nothing
  trailing, changes nothing.
  Transformations > Convert case has Title Case, snake_case, camelCase,
  PascalCase and kebab-case (no keys). They act on each selection, or the word
  at the cursor when nothing is selected, keep it selected, and are one Undo
  step. Words split at spaces, `_`, `-` and case changes (`fooBar` is foo and
  bar, `HTTPServer` is http and server, `v2Beta` is v2 and beta). Each line of a
  selection converts on its own, keeping its indentation and any `_` at its
  ends, and punctuation between words goes in the four code styles. Title
  Case spaces the words, capitalizes each and keeps small words (of, the, to)
  lower in the middle of a line.
  The Markdown submenu (grey unless the file in front is Markdown) has Bold
  (Shift-Cmd-B), Italic (Cmd-I), Strikethrough (Shift-Cmd-X, wraps in `~~`),
  Inline code (Shift-Cmd-C) and Link (Option-Cmd-K). Each wraps the selection, or the word at the caret, in its
  marks and keeps it selected; with the caret between words it puts in the empty
  pair with the caret inside. Run it again on text already wrapped and it takes
  the marks off. Spaces at the edges of a selection stay outside the marks.
  Link makes `[text]()` with the caret between the parentheses, and takes an
  existing link off, keeping its text. Each is one Undo step. Cmd-B stays Hide
  explorer, which is why Bold takes Shift.
  Edit also lists Indent (Cmd-]), Outdent (Cmd-[), Re-indent (Control-I), Add cursor above
  (Option-Cmd-Up) and Add cursor below (Option-Cmd-Down), Jump to matching
  bracket (Control-M), Go to line… (Cmd-L) and Go to symbol… (Control-Cmd-J),
  and the Find submenu also has Find and replace… (Shift-Cmd-F), Replace (Cmd-=),
  Replace and find (Option-Cmd-=), Replace all (Option-Cmd-Return; the three are grey unless
  the find bar's replace row is open), Match case
  (Option-Cmd-C), Whole word (Option-Cmd-W), Regular expression
  (Option-Cmd-R) and Ignore diacritics (no key). The Mac adds Writing Tools,
  AutoFill, Start Dictation… and Emoji & Symbols at the end.
- **Toggle comment (Cmd-/):** comments the lines you have selected, on every
  cursor, with the file's own comment mark (`//`, `#` or `--`) at the shallowest
  indent. If every line is already commented it takes the marks off instead.
  Blank lines are left alone. HTML, Markdown and CSS wrap the lines in a block
  comment; plain text has none, so the Mac beeps and nothing changes. One Undo puts it back.
- **Format:** Line endings, a submenu with LF and CRLF (see Files in other
  encodings). A check marks the file's own; a file with both kinds of break has
  neither checked, and the status line says Mixed. The file's own row is grey,
  and the submenu is grey with no tab. Writing goal… sets a word count to reach
  (see Writing goal).
- **View:** Hide explorer or Show explorer (Cmd-B), Hide preview or Show
  preview (Shift-Cmd-V), Split right (Cmd-\) and Split down (Shift-Cmd-\, which
  the menu shows as Cmd-|), Soft wrap, which has a check mark
  while the file in front wraps its lines, Wrap at window width (Option-Z,
  checked while the file in front wraps past the reading column), Show invisibles (Option-Cmd-I, with a
  check mark while they show), Show line numbers, Reading width (checked by default; see Reading width), Smart punctuation (checked while
  typing in Markdown and text files makes curly quotes, an ellipsis and en dashes; see Smart punctuation), Typewriter scrolling (checked while the line you type stays at one height; see Typewriter scrolling), Styled Markdown (checked while Markdown files show semibold headings and faded marks; see [Styled Markdown](notes/styled-markdown.md)), Code colors (a submenu of the 25 color schemes with a check on the current one, then Vivid, Colored roles, Rainbow brackets and Semantic colors as checked switches; see [Code colors](code-colors.md)), Style Check (checked while filler words and long sentences get a faint mark; see [Style Check](style-check.md)), then Fold (Option-Cmd-Left), Unfold (Option-Cmd-Right), Fold all and Unfold all (see Fold code), then Next change and Previous change (see Changes since the last commit), then Bigger (Cmd-+, which is Shift-Cmd-=), Smaller (Cmd--) and Actual size (Cmd-0) for the text size, Text style (Mono, Serif or Sans, the one in use checked; see Text style), Enclosing folder (Cmd-Up, while the folder tree has the keyboard; in the text Cmd-Up still goes to the top of the file), Reveal current file
  (Shift-Cmd-J), Show hidden files (Shift-Cmd-Period, with a check mark while
  they show), Show ignored files (with a check mark while files your `.gitignore` rules out show, dimmed; see [Explorer](explorer.md)), Sort by (Name, Kind, Date modified or Size, with a check on the window's order; see [Explorer](explorer.md)), Collapse all folders (Option-Cmd-Left while the folder tree has the keyboard; in the text that key folds), Focus explorer (Shift-Cmd-E) and Filter explorer (Option-Cmd-F). The window title is only the file's
  name; the check mark is where you see that a big file opened with wrap off.
  Enter full screen (Control-Cmd-F) fills the screen with the front window;
  the item becomes Exit full screen, with the same key to return.
  Minimal mode (Control-Cmd-Return, with a check mark while on; see Minimal mode)
  hides everything but the text in the front window.
- **Window:** Minimize (Cmd-M), Zoom, Show previous tab (Shift-Cmd-[), Show next tab (Shift-Cmd-]), Show first tab (Cmd-1) and Show last tab (Cmd-9), which go grey with fewer than two tabs, Pin tab (Unpin tab on a pinned one), Move tab earlier and Move tab later (grey at the end of the tab's group), Move tab to left, right, upper or lower pane, Move tab to new window (grey while the window holds one tab in one pane) and Focus pane left, right, up or down (grey when no pane lies that way; the focus items keep their Cmd-K arrow keys, and Cmd-K then an arrow with Cmd held works too, which a menu can't show), Focus next pane and Focus previous pane (through the panes in reading order, grey with one pane), Even out panes (every divider to even halves, grey with one pane), Merge panes (every pane's tabs into the active pane, grey with one pane), Merge all windows (every other window's tabs into the front window, grey with one window), and Bring all
  to front.
- **Help:** Finch Help (Cmd-?) opens this guide in a new tab with the preview showing. The pages are read-only: Save asks where to save a copy and never writes the guide itself. The Help menu is the system's, so it also has the Search field.

## Settings

On the first normal launch after the rename, Finch moves the old
`~/Library/Application Support/GPUI Editor` folder to `Finch` if the Finch
folder does not exist yet. The whole folder moves together, with its bytes
unchanged. If both folders exist, Finch uses its own and leaves the old one
alone. If the move fails, it logs the error and uses the old folder for that
run, so saved work is not replaced with an empty store. Untitled pages already
in `~/Documents/Scrap Drafts` stay there.
If the old app is still running, Finch uses the old folder for that run and
waits to move it until a normal launch after the old app quits. Background
checks and debug commands never move it.

Finch keeps its settings in a plain text file at
`~/Library/Application Support/Finch/settings.toml`. It reads the file
when it starts and again when another program changes it, and a missing file or a missing line just means the usual
setting. The first time you choose Settings… (Cmd-,) and the file is missing or empty, it opens with every setting listed as a commented line, its usual value and its range beside it: take the `#` off a line to change it. A file with anything in it is never rewritten for this. The file holds:

- `[window]` with `width`, `height`, `x`, `y`, `display`, `state` and `theme`:
  the window's size, place and colors. Resize, move or zoom a window and it's
  saved; the window you start with comes back there the next time: at the same
  place and size, on the same display, and in full screen if it was in full
  screen (`state` is `"windowed"`, `"maximized"` or `"fullscreen"`; `width`
  and `height` are the size to come back to when it leaves full screen). If
  that display is no longer connected, or the saved place is off it, the
  window opens centered at the saved size instead (900 by 700 at first, and never
  smaller than 480 by 200). A saved window bigger than its display, or
  hanging off it, comes back shrunk and moved to fit below the menu bar and
  above the Dock.
  Windows opened later, from the Dock or from other apps, open 22 points down
  and to the right of the window in front (the last one opened when none is
  in front), on that window's screen, and start again at the screen's top-left
  corner when that would run past the menu bar or Dock area. `theme` is `"system"` (the default),
  `"light"` or `"dark"`. `system` follows the Mac's light or dark mode while
  the app is open: when the Mac switches, every window fades to the other
  colors over a third of a second. `light` and `dark` keep the colors you
  chose whatever the Mac does. The colors are the Mac's standard ones: a white
  page in the light and the Mac's dark grey in the dark, with the folder tree on
  the Mac's own sidebar material. The text you read in the light (code and the
  preview's body) is a soft neutral dark grey, not black.
- `[editor]` with `font_family`, `font_size`, `soft_wrap`, `reading_width`, `show_invisibles`, `line_numbers`, `styled_markdown`,
  `smart_punctuation`, `trim_trailing_whitespace` and `insert_final_newline`:
  the text's font, its size, whether long lines wrap (on by default, but off
  for a file over 50,000 lines), whether spaces and tabs show as marks (off
  by default; see Show invisibles), whether the line numbers show in the gutter (on by default; see Show line numbers), whether Markdown shows semibold headings and faded marks (`styled_markdown`, off by default; see [Styled Markdown](notes/styled-markdown.md)), whether the style check marks filler words and long sentences (`style_check`, off by default, and `style_sentence_words`, 30; see [Style Check](style-check.md)) and whether a save removes trailing spaces
  and ends the file with a newline (both off by default; see Save). The font
  starts as Google Sans Code NF at size 14 (`font_size` is 6 to 96, in points). Leave a font line out to keep that,
  or set your own. View > Bigger and Smaller change `font_size` for every tab in every window, one point
  at a time up to 24 and two at a time above, and are grey at 96 and at 6; Actual size sets it back to 14.
  The keys are Cmd-+ (Shift-Cmd-=), Cmd-- and Cmd-0, as in Xcode and TextEdit. Pressing one again within
  half a second shares the write, and a `font_size` typed by hand in the file applies at once. A tab
  keeps the line at the top of its window when the text resizes. Pinching on a trackpad over the text
  does the same, one size at a time through the same sizes, with a light tick at each size and a firmer
  one on 14; the file is written once when your fingers lift. A pinch over the preview, the folder
  tree or the tab bar does nothing. `prose_style` and
  `code_style` are `"mono"`, `"serif"` or `"sans"` (both mono by default; see Text style).
- `[code_colors]` with `scheme`, `vivid`, `colored_roles`, `rainbow_brackets` and `semantic`, and the optional tables `[code_colors.dark]` and `[code_colors.light]` of role colors: how code is colored (View > Code colors; see [Code colors](code-colors.md)).
- `[text]` with `weight`, `thicken`, `gamma` and `contrast`: how the text is
  drawn. `weight` is how bold code text is, from 100 (thin) to 900 (black);
  it starts at 500 (Medium). The window's own labels (folder tree, tabs,
  preview text) use the Mac's regular weight and are never thickened.
  `thicken` fattens each code letter a little for a fuller, smoother look: 0
  turns it off, 1 to 4 make it stronger, and anything above 4 counts as 4
  (it starts at 4). `gamma` (0.2 to 3.0; 1.0, which is off)
  and `contrast` (0.0 to 1.0; 0.0, which is off) fine-tune how soft the letter edges are.
  Changes in Settings > Text apply at once, and so does a `weight`, `thicken`, `gamma` or `contrast` typed by hand in the file.
  A number outside
  its range is used as the nearest end of the range (a font size of 0 is 6, a weight of 0 is
  100) and the file keeps what you typed.
- `[layout]` with `left_panel_width` and `right_panel_width`: how wide the side
  panels are. `left_panel_width` is the folder tree's width; it's saved when
  you drag the divider. `right_panel_width` is the preview's width; it starts
  at 420, and a width you saved stays as it is. A narrow window draws the side
  panels narrower so the editor keeps at least 360 pt (the preview gives way
  first, then the folder tree); the saved widths come back when the window
  grows, and the window can't be made narrower than 480 pt.
- `[explorer]` with `visible`, `show_hidden`, `show_ignored`, `sort` and `last_root`: whether the
  folder tree shows (on by default), whether dot files show in it (on by
  default), whether what `.gitignore` rules out shows in it, dimmed (off by default),
  how a new window's tree sorts each folder (`name`, `kind`, `date` or
  `size`; the last View > Sort by pick), and the folder you had open last time, which a start from the Dock
  or Finder opens again. An older file's `last_file` (the file open last time)
  is still read, but the app never writes it. The app also keeps `open_files`,
  `active_tab`, `pinned_tabs`, `active_pane` and `[explorer.panes]` here, your
  tabs and how the panes were split, and brings them back; the first settings
  file lists them too, but you'd rarely change them by hand. A window's `x`,
  `y` and `display` have no usual value: they only come back together.
- `[preview]` with `visible`: whether the Markdown preview shows (off by
  default).
- `[scratch]` with `shortcut` and `resident`: the global shortcut that opens the
  scratch window (`"ctrl-alt-space"` at first, `"off"` turns it off) and whether
  Finch keeps running when its last window closes (off by default). See
  `notes/scratch-window.md`.

If the file has a mistake and can't be read, Finch starts with the usual
settings, shows an alert, "Your settings file has a mistake", that says which line is wrong and
why in plain words ("Line 4: font_size must be a number."), and leaves the file alone so you can fix it. Until it's fixed, window size, tabs and widths
aren't saved. The alert shows once for the same text. Its Show settings file button opens the file
(or brings its tab forward); OK just closes it. Fix the file and save it, and the settings and the
saving come back.

If the file can't be opened at all (its permissions forbid it, say), the alert is "Your settings
file can't be read", and says what to change. If Finch can't write the file (the folder is read-only
or the disk is full), "Your settings couldn't be saved" says why, once for the same problem; Finch
keeps your changes while it runs, and the next write that works brings the file up to date.

You can edit the file in another editor while Finch is open (a file that
is a link to somewhere else, like one managed by chezmoi, works too). A
change you save there shows within a moment, and what Finch saves later,
such as a new window size, keeps your edit. If the edit leaves the file
unreadable, the same alert shows (saying the settings in use stay), nothing is saved over the file, and the settings in use stay as they were until you fix it.
