---
title: Settings
feature: settings-window
type: guide
summary: Finch > Settings (Cmd-comma) opens one Settings window with General, Editor, Writing, Code colors and Text panes; every control writes the settings file, and a hand edit of the file shows in the window.
covers:
  - src/settings_window.rs
  - src/settings_controls.rs
  - src/settings_panes.rs
  - src/settings.rs
  - src/tokens.rs
  - src/app.rs
  - src/control.rs
since: 2026-11-02
---

# Settings

Finch > Settings (Cmd-comma) opens one Settings window, or brings it forward. It looks like a Mac
settings window: a sidebar of panes on the left, grouped rounded sections on the right, with a title,
switches, pop-up buttons, sliders, steppers and text fields. The window has a fixed width and any
height, and scrolls when a pane is tall. It opens on the pane you used last and where you left it (position and height) while Finch runs, or centered when that screen is gone.

- **General:** appearance (System, Light, Dark), Minimal mode, the Folder tree and Markdown preview switches (the same as Cmd-B and Shift-Cmd-V, so the View menu's Show or Hide items follow), show hidden files, the scratch window
  shortcut, and keeping Finch running with no window. The Open Settings File button opens the file as a tab,
  as Settings used to.
- **Editor:** font family (the installed fixed-width fonts; Default means none), size, the Markdown and code
  typefaces, wrap long lines, reading width, line numbers, show invisibles, smart punctuation, typewriter,
  trim trailing whitespace and final newline. A family named in the settings file that this Mac doesn't have
  draws as the system's fixed-width font; the file keeps the name, so the font comes back once it's installed
  and Finch restarts.
- **Writing:** styled Markdown, spelling, style check with its Edit word list… button (opens the word list as a tab) and its long sentence length (both dimmed while style check is
  off), and the parts of speech to highlight.
- **Code colors:** the scheme, a preview of it in the current appearance (picking a scheme eases the preview to its colors; the other controls change it at once), Vivid, Colored roles, Rainbow
  brackets, Semantic colors, and your own colors for single roles in the current appearance (a swatch, the role,
  a hex field and a remove button; Add a color offers the common roles; Reset takes them all off).
- **Text:** weight, thicken, gamma and contrast, which apply at once, and a Reset button for all four.

Every control writes the settings file through the same path as the View menu, so the editor, the View menu
checks and other windows follow at once, and a change to the file by hand shows in the window. The color
tables `[code_colors.dark]` and `[code_colors.light]` are written key by key, so your comments and other keys
stay (see [Code colors](code-colors.md)). A color field only writes once its text is a hex color; the scratch
shortcut field only when you press Return or leave the field with text that parses, and shows red while it doesn't. While nobody is typing in it, the field also shows the shortcut in glyphs at its right edge, like ⌃⌥Space.
Esc in the shortcut field or a color field puts the saved text back and returns the keys to the pane list, so Up and Down move panes again.

The window doesn't keep Finch open: when the last editor window closes, Settings closes with it and Finch quits (unless it is set to keep running with no window). Sizes, radii, colors and durations are the `SETTINGS_*`
tokens in `src/tokens.rs`.

## Search

The sidebar starts with a Search field (Cmd-F in the Settings window puts the keys in it). Typing narrows the sidebar to the panes
that hold a matching row, and each pane to its matching rows. A row matches when every word you typed sits in its label or its
note, in any case and anywhere in a word. The matched letters in the label turn semibold at full ink while the rest of the label dims,
as the folder tree's filter marks names. A section with no matching row leaves the page. Choosing a pane while searching shows that
pane's matching rows; when the pane you chose has no match, the page shows the first pane that has one, and the pane you chose
comes back when the search is cleared. Up and Down move among the panes the search leaves. When no pane matches, the page shows the
empty state, No Results for what you typed, with a Clear search button. Esc or the clear button empties the field and every pane and row returns.
`debug settings-search <text>` types into the field and `debug settings-search --escape` presses Esc, for a background launch.

