---
title: Services and Shortcuts
feature: services
covers:
  - src/services.rs
  - src/control.rs
  - src/main.rs
  - scripts/ship.sh
since: 2026-11-01
---

Send text to Finch from any app without switching to it. Select some text in Safari, Notes, Mail or anywhere else, and pick one of two items from the app's Services menu (the app's name menu, then Services, or the right-click menu):

- **Append to Finch Note** adds the text to a note, under a line with the date and time (`2026-11-01 14:05`), after a blank line.
- **New Finch Note** makes a new scrap draft holding the text. It is saved in `~/Documents/Scrap Drafts/` like any other draft.

Finch doesn't come forward and the app you were in keeps the keyboard. A note that isn't open opens as a tab in a Finch window behind, so you find it there next time you switch over. With no normal window open (Finch is resident with every window closed, or only the scratch window is open), the appended note stays on disk and no tab opens. A note that is already open in a tab gets the text at the end of that tab, left unsaved like any edit you type (a scrap draft saves itself as usual); the tab stays where it was and your cursor doesn't move.

## Which note an append goes to

1. The note you name, when you use the command line below with `--note`.
2. Otherwise the last note an append went to. Finch keeps its path in a file called `service-note` in its folder (`~/Library/Application Support/Finch/`). To make appends go to a note of your choice, put that note's full path in the file.
3. Otherwise, and when that note is gone, a new scrap draft, which then becomes the note appends go to.

A note that isn't plain UTF-8 text is never changed: the append fails. Started from the Services menu, a failed append shows nothing, so use the command line, which says what went wrong, when you need to know.

## From the command line and Shortcuts

`finch --append` adds the text piped into it, as Append to Finch Note does:

```
pbpaste | finch --append
echo "call the dentist" | finch --append --note ~/Notes/inbox.md
```

`finch --new-note` makes a new scrap draft from the piped text. Both print the note's path and exit 0, or say what went wrong and exit 1. They take no path other than `--note`, and they never start Finch or bring it forward: a running Finch shows the note as a tab (with no normal window open, the note is just written and no tab opens), and with none running the file is just written. If a running Finch doesn't answer within five seconds, the command says the result is unknown and exits 1 without touching the file: check the note before running it again.

In the Shortcuts app, add a **Run Shell Script** action, set Pass Input to **to stdin**, and use `/Applications/Finch.app/Contents/MacOS/finch --append` (or `--new-note`) as the script. Feed it any action that makes text, such as Get Text from Input or Get Clipboard.

## If the items are missing

The two items appear after the installed Finch has run once, which registers them with macOS. If they still don't show, open System Settings, Keyboard, Keyboard Shortcuts, Services, and tick them under Text. A copy of Finch run from a build folder never adds them.
