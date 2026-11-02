---
type: guide
summary: Edit > Undo history scrubs the shown file back through this session's edits with a slider.
covers: [src/undo_slider.rs, src/workspace.rs, src/document.rs, vendor/gpui-base/src/input/base/undo_manager.rs, vendor/gpui-base/src/input/base/state.rs]
---

# Undo history

Edit > Undo history… opens a slim slider at the foot of the shown editor. Each stop is
one undo step, so a typing run is one stop; the label says how long ago the stop was made ("3 min
ago (edit 4 of 9)"), "Before the first edit" at the left end and "Latest edit" at the right. A small
tick marks where the text stood when the slider opened.

- **Drag** the knob (or press anywhere on the track): the text steps back or forward to that stop. A
  jump of any length is one replay: one batch, one highlighter update, one `Change` event. The changed
  lines wash as they do for Undo (the editor skips the wash past 256 changes in one jump).
- **Let go**: the text stays there. The slider stays open, so the knob can be moved again.
- **Keys**: the slider has the keyboard when it opens. Left and Right move one stop, Option-Left and
  Option-Right a tenth of the history (at least one stop), Home and End go to the ends, Return keeps
  the text like Done, Escape restores. Each key settles like letting go. The editor has its keys
  back when the slider closes; while the slider holds the keyboard, typing does nothing (click the
  text to type, which asks as below when scrubbed back).
- **Escape**: the text goes back to where it was when the slider opened, and the slider closes.
- **Done**, or Edit > Undo history… again: closes and keeps the text. The newer steps that were
  scrubbed past are folded under one new step, so a single Undo brings back the newest text and
  nothing is dropped. Undo while the slider is open and scrubbed back with the knob also brings
  the newest text back; after that, Undo steps one edit at a time as usual.
- **Typing while scrubbed back** asks first ("Drop the later edits?"): it would drop them. Drop edits
  does that and goes on editing; Cancel leaves everything as it was; the keystroke that asked is not typed. Typing at the newest
  stop needs no question. Text the app adds itself (Services, pasted image links, camera text) is
  never refused: it folds the later steps the way Done does and goes in.

## What it does not do

- **Drafts** (Untitled pages) are not written per step: the draft is written once when the slider
  closes. Text typed at the newest stop is written at once, so quitting with the slider open
  keeps it, never an older scrubbed text.
- **Line endings and Compare's Use these** close the slider first (keeping the text, like Done),
  then apply, so they always land.
- **Deleted or changed files**: letting go at the saved text keeps the dot while the file is gone
  or changed on disk.
- **Unsaved dot**: the document remembers the text it was opened, reloaded or last saved with
  (`Document.saved_text`). Letting go at that text clears the dot; any other text sets it.
- **A reload from disk** (the file changed outside, or Revert) ends the scrub first, keeping the
  text, then replaces the text and the history as it always did.
- **Menu**: the app menu has no Option alternate (`MenuItem` carries none), so Undo history… is a
  plain item under Redo.
- **Caps** are unchanged: 1000 steps and 32 MB. Folding copies the scrubbed-past steps once (the
  copies count toward the 32 MB), so old steps can go sooner than they would.
- No frames are drawn at rest; the slider has no animation.
- **Grey**: Undo history… is grey while the shown tab has no step to go back to (a fresh Untitled
  page, a file not yet edited) or its editor takes no edits, and it beeps if a key or the control
  socket asks anyway. One rule (`can_scrub`, workspace.rs) serves the item and the handler. The bar
  reads it only when it refreshes (a tab change, or the document's undo history going from empty
  to not empty or back, which a restored unsaved draft needs); nothing runs per
  frame. A file locked on disk still takes edits, so it keeps the item once it has some.

## Where it lives

`UndoManager` (vendor `undo_manager.rs`) stamps each step and replays any number of steps as one
`Replay` (`jump`); `fold_redo` makes the folded step. `EditorState::history_jump`,
`history_stops`, `history_fold_redo`, `history_drop_redo` and `set_history_guard` (state.rs) are the
API; the guard refuses an edit that would drop waiting steps and reports an Undo or Redo. The slider
(`src/undo_slider.rs`) is a view beside the editor pane, drawn by the workspace, that never observes
the editor. Check it with `debug action undo-history` and `debug undo-history [to <n>|escape|close]`.
