---
type: guide
summary: What Finch keeps of each saved file, for how long, and how to browse an old version and put it back.
covers:
  - src/versions.rs
  - src/versions_list.rs
  - src/compare_view.rs
  - src/document.rs
  - src/workspace.rs
---

# Versions

Every time you save a file, Finch keeps a copy of the text it wrote, and of the text the save is about to replace. You can look at an old copy next to what is in the tab and take it back.

## What is kept

- A copy from each successful save of a file with a name: Save, Save as, Save all, plus the text each save replaces. An Untitled page has no file and keeps none (its draft is its safety net).
- A save whose text matches the newest copy adds nothing. A file over 2 MB is never copied.
- Copies live in Finch's Application Support folder, in `Versions`, one folder per file. The folder is named for the file's full path, so a renamed or moved file starts a new history, and a file replaced by another editor or by git keeps its own.
- Copying a save's text runs in the background after the save has landed. A copy is also made of the text a save is about to replace, before the file is written, so a cut-short write can't leave the old text nowhere (a first save can wait a moment for it). A copy that can't be written is only logged: it never fails a save.

## How long

- Everything from the last hour.
- Then one per hour for a day.
- Then one per day for 30 days. Older copies go.
- Each file holds at most 20 MB of copies and all files together 200 MB; the oldest go first when over.

## Browse and put one back

1. Open the file and choose File > Browse versions... (greyed on a page with no file). A list shows the copies, newest first, each with its time and size. Two copies saved in the same minute add their seconds ("Today 14:32:05") so they read apart; the rest show the minute only.
2. Up and Down move, Home and End jump to the newest and the oldest copy, Page Up and Page Down move a screenful, and the pick stays in view. Return or a click picks, Escape closes.
3. The Compare view opens over the tab: your text on the left, the old copy on the right, titled by the copy's time, scrolled so the first change shows with a few lines above it. Each side has a narrow column of line numbers in the editor gutter's quiet ink, flush right and as wide as the biggest number, blank on the filler rows. Option-Cmd-Down and Option-Cmd-Up (or View > Next change and Previous change) step to the next or previous change, and the header counts along ("2 of 5 changes"). Each change takes your side until you pick the other. "Take all from <time>" in the header takes the old copy for every change at once, and "Take all yours" takes yours for every change again. Right-clicking a change opens a menu: Copy puts that side's lines of the change on the clipboard (greyed when that side has none there), and Take yours and Take <time> pick a side for that change (the side already taken is greyed). Hovering a button for a moment tells what it does; Cancel's tip ends with ⎋ and Use these's with ↩, the keys that run them. The other buttons have no keys.
4. Use these puts the merged text in the tab as one Undo step. Only the lines that differ change: your caret stays on its line and column (or the last line, if the text got shorter), the view stays where it was, and the changed lines glow once. The tab shows its unsaved dot and the file on disk is untouched: save when you are happy, or press Undo once to get your text back. It never counts as the file's saved time and never clears a changed-on-disk warning.
5. Cancel (or Escape) closes the view and changes nothing.

If you type in the tab while the old copy is being read, the view is made from the text as it is by then.
