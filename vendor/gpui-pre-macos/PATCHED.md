# gpui-pre-macos 0.3.10, patched for GPUI Editor

This is the crates.io `gpui-pre-macos` 0.3.10 source, used through `[patch.crates-io]` in the app's `Cargo.toml`. Every change from the published crate is listed here. `.cargo-ok` and `.cargo_vcs_info.json` were left out.

## An idle window stops its display link (gpui-editor#f5eq)

Ported from Harrier (harrier#n1r5). `src/window.rs`: `MacWindowState::frame_source_idle`,
`start_display_link`, `MacWindow::stop_idle_frame_source` and `frame_waker`; `src/display_link.rs`:
`WindowFrameSource::kick`.

Upstream keeps the `CVDisplayLink` running while the window is visible, so an unchanged window woke
the main run loop on every refresh just to find nothing to draw. The frame callback now asks the
window to stop the link after 4 ticks with nothing dirty, presented or scheduled
(`stop_idle_frame_source`, which sets `frame_source_idle`), and the `frame_waker` the invalidator
already calls on every new demand (a view going dirty, `on_next_frame`) starts it again while that
flag is set. Occlusion, screen change and activation keep calling `start_display_link`, which clears
the flag.

The waker also kicks the window's dispatch source (`merge_data(1)`) right after restarting the
link: a restarted `CVDisplayLink` delivers its first tick up to a refresh later (Harrier measured
9.2 ms p50 at 120 Hz), the kicked request runs the same `step` on the main queue at once.

## An idle window gives back its path textures and a grown instance buffer pool (gpui-editor#vta3, finch#5xpt)

`src/window.rs` `stop_idle_frame_source` reads the renderer's `idle_mark` (none when there is nothing to
give back) and schedules a trim 5 s later (`IDLE_TRIM_GRACE`, on the window's foreground executor, holding
a weak ref) that calls the renderer's `trim_unused_since` with it. A window whose link stopped draws no
frames, so the renderer's paintless-frame drop never ran for it.

## Display wake and session switch restart the display links (gpui-editor#phnb)

`src/platform.rs` also observes `NSWorkspaceScreensDidWakeNotification` and
`NSWorkspaceSessionDidBecomeActiveNotification` (`onDisplayWake:`); with `NSWorkspaceDidWakeNotification`
each calls `window::restart_display_links`, which stops every running window's link, starts them all
again and kicks one frame. A window idled by `stop_idle_frame_source` is left alone, so
`frame_source_idle` stays set and the frame waker still starts its link on the next request. Only the
real wake notification reaches the app's wake callback. `WindowFrameSource::is_running` is new.

## Alert prompt: a Cancel first keeps Return (gpui-editor#s3np)

`prompt` in `src/window.rs` binds Escape to a Cancel button only when it is not the first button, so a
Cancel at index 0 keeps NSAlert's Return key equivalent. The initial focus is not moved onto a later
button when the first one is Cancel. Prompts with Cancel last behave as before.

## A test launch has its own clipboard (gpui-editor#csy2)

`src/platform.rs` builds `general_pasteboard` from `Pasteboard::unique()` (a uniquely named
NSPasteboard, `src/pasteboard.rs`, no longer test-only) when `GPUI_EDITOR_BACKGROUND` is set, so copy,
cut and paste work inside the launch and never touch the Mac's clipboard. Without the flag it is
`Pasteboard::general()` as before.

## Dock Quit and logout await unsaved changes (gpui-editor#bezt)

`applicationShouldTerminate:` consults the app's should-quit hook and returns `NSTerminateLater`
while its save question runs. A confirmed `Platform::quit` replies YES to the pending request
instead of asking again; `cancel_quit` replies NO. Without a pending request, confirmed quit
still calls `terminate:` asynchronously so shutdown never re-enters a borrowed App.
The app flushes Untitled drafts before deciding whether a question is needed, so a failed last
draft write cannot silently permit termination. A real launch activates before asking, revealing
the question even when the app was hidden; background test launches never activate.

## Menu refresh retains at most two bars of actions (gpui-editor#cq14)

`set_menus` recycles the previous bar's vector instead of appending actions forever. The Dock
uses its own vector. Tags restart at zero in each menu tree; dispatch and validation first walk
to the item's owning root menu, then select that root's action list. Keeping the previous tree
and actions lets an already open menu finish after a rebuild. Items older than that are ignored,
never interpreted against a new list. Native menus and their actions are bounded to two bar
generations plus the Dock, and action vector capacity is reused.

## Selected text participates in Services (gpui-editor#cc6t)

The platform registers modern and legacy plain-string Services types once at launch.
`GPUIView` validates both directions of a request, writes a nonempty selection to the
service's supplied pasteboard and inserts returned text through the existing input handler.
Returned text uses the explicit active selection range, not the typing path that can
fan out across multiple cursors or wrap the selection with brackets.
Read-only input can send but cannot receive. The callbacks take and restore the handler
through `with_input_handler`, just like IME; no frame or key-dispatch work is added.

## Definition cards use AppKit (gpui-editor#86sp)

`MacWindow::show_definition` defers the definition-card call outside the GPUI update,
converts the editor's top-left baseline to NSView coordinates and sends the full
lookup text as an attributed string with `NSFontAttributeName` matching the editor's
family and size, falling back to the system monospace font at that size if the
requested family cannot resolve. Only an explicit Look Up request reaches it;
there is no frame, key-dispatch or marked-text change.

## Copied files reach the pasteboard (finch#qzyg)

`src/pasteboard.rs` `Pasteboard::write` ignored a lone `ClipboardEntry::ExternalPaths`; it now calls
`write_paths`, which declares the filenames list (`NSFilenamesPboardType`), `public.file-url` (the first
file, percent-encoded) and plain text (the paths, one a line), the way Finder writes a copy. `read`
already took the filenames type, so a copy in the tree pastes in Finder and a Finder copy pastes in
the tree. It runs on a copy key, never per frame.

## Destructive alert buttons (finch#tyfe)

`MacWindow::prompt` calls `setHasDestructiveAction(true)` on the `NSButton` of each `PromptButton::Destructive` answer, so AppKit draws Revert, Discard and Overwrite as destructive. Button order, tags, the Escape key equivalent and the Return default are unchanged. Runs once per alert, never per frame.

## Import from iPhone or iPad (finch#nbcw)

`src/window.rs`: `validRequestorForSendType:returnType:` also answers the view for an image return type (PDF, JPEG, PNG, TIFF, with their older names) and no send type, while the input handler can replace text, so AppKit enables Continuity Camera's items. `readSelectionFromPasteboard:` with no text but one of those types posts `GPUIPasteboardImageNotification` (object: the view, userInfo `pasteboard`) and returns YES; the app reads the data. `src/platform.rs` and `gpui-pre` `SystemMenuType::ImportFromDevice`: a menu item carrying `NSMenuItemImportFromDeviceIdentifier`, which AppKit fills in. Runs when the user picks an item or the menu validates, never per frame.

## Window pictures without screen capture (finch#7nbd)

`src/window.rs`: `MacWindow::render_to_image` (and its `anyhow::Result` and `RgbaImage` imports) are no longer gated on `test-support`, for the app's `debug snapshot` verb. Runs only when the verb is called, never per frame.
