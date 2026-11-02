---
title: Scratch window
feature: scratch-window
covers:
  - src/hotkey.rs
  - src/scratch.rs
  - src/settings.rs
  - src/workspace.rs
since: 2026-11-01
---

A quick place to type from any app. Press Control-Option-Space and a small Untitled window slides down from the top of the screen the pointer is on, ready to type. Press the same keys again, or press Escape, and it slides away and the app you were in comes back to the front.

## Use it

1. In any app, press Control-Option-Space. The scratch window slides down at the top middle of the screen the pointer is on.
2. Type. Everything you type is kept as a scrap draft, like any Untitled page (see Save in the main guide), and `Open draft…` brings it back later.
3. Press Control-Option-Space again, or Escape (or Cmd-W), to slide it away. Your text is written to its draft first. If that write fails, Finch asks before closing, as it does for any Untitled page.

Only one scratch window exists at a time. It has no folder tree, and it never changes the size or place Finch remembers for your windows. It stays put when you click another app; press the shortcut to send it away.

Cmd-Q with the scratch window open works as it does for any window: unsaved drafts are written, and the app quits.

The File menu has the same switch: Show scratch window slides it down (the title reads Hide scratch window while it is open), so it opens even with the shortcut turned off or taken by another app. The item shows no key, because the shortcut is a system-wide one the menu can't show without binding it a second time.

## Change or turn off the shortcut

In `settings.toml`, the `[scratch]` section:

```toml
[scratch]
shortcut = "ctrl-alt-space"   # "off" turns it off
resident = false
```

`shortcut` is words joined by `-`: any of `ctrl`, `alt`, `cmd`, `shift` and one key (a letter, a digit, `space`, `return`, `tab`, `f1` to `f12`, or `minus`, `equal`, `comma`, `period`, `slash`, `semicolon`, `quote`, `backslash`, `grave`, `lbracket`, `rbracket`). It needs `ctrl`, `alt` or `cmd`, so a bare key is never taken. `"off"` or an empty string turns the shortcut off. A change in the file takes effect at once. If another app already holds the keys, or the words don't make a shortcut, Finch says so in its log and has no shortcut until you change it.

The shortcut needs no privacy permission.

## Keep Finch running with no window

With `resident = true`, closing the last window leaves Finch running (its Dock icon stays), so the shortcut still opens the scratch window. Clicking the Dock icon opens a new window. With `resident = false` (the usual), closing the last window quits, as always. Cmd-Q always quits, and waits for drafts to finish writing.

With `resident = false` the shortcut works only while Finch is running, so it has no window to show if you have quit.
