# gpui-pre 0.3.10, patched for GPUI Editor

This is the crates.io `gpui-pre` 0.3.10 source, used through `[patch.crates-io]` in the app's `Cargo.toml`. Every change from the published crate is listed here. `.cargo-ok` and `.cargo_vcs_info.json` were left out.

## The last glyph uses its midpoint for cursor hits (gpui-editor#ysk2)

`src/text_system/line_layout.rs`: `closest_index_for_x` compares the final
glyph's origin and the line width just like the interior glyphs. Upstream
only did this for a one-byte line, so clicking the left half of the last
glyph on a longer line put the caret after it. Clicks, drags and vertical
cursor alignment now share the same midpoint rule.

## The frame callback lets an idle platform stop its frame source (gpui-editor#f5eq)

Ported from Harrier (harrier#n1r5). `src/window.rs` (`IDLE_TICKS_BEFORE_STOP`, the end of the
`on_request_frame` callback) and `src/platform.rs` (`PlatformWindow::stop_idle_frame_source`,
default no-op). The callback counts consecutive ticks where the window isn't dirty, has no
next-frame callbacks, owes no present (`needs_present`, high-rate input) and has no deferred forced
render; after 4 (gpui-editor#26dx; it was 16, about 16 empty main-thread wakeups after every
blink fade, and the waker's kick already covers a request made a tick late) it calls `stop_idle_frame_source`, and any tick with demand resets the count. Only
macOS implements it (it stops the display link; its `frame_waker` restarts it on invalidate or
`on_next_frame`). Nothing is skipped or capped: a window with demand never counts as idle.

## A window move or pointer enter and leave doesn't refresh (gpui-editor#40zs)

Ported from Harrier (harrier#qa7c). `src/window.rs`: `Window::bounds_changed`,
`Window::display_changed` and the `on_hover_status_change` callback. Upstream calls `refresh()` on
every platform move or resize, so a title bar drag redrew every cached view at the pointer's rate.
`bounds_changed` now refreshes only when the scale factor or content size changed; the stored
values and `mouse_position` still update and the bounds observers still fire on every call, so
`Window::bounds()` sees the new origin. A display change refreshes in `display_changed` (0.3.10
tracks `display_id` there, not in `bounds_changed`). Pointer enter and leave still set
`Window::hovered` but no longer refresh: on macOS `is_window_hovered` answers from the active
state, and the app reads no window-hover state. The activation callback keeps its own `refresh()`;
the `bounds_changed` call just before it no longer adds a duplicate, so that one refresh is now the only one.

## Backports from Zed (gpui-editor#7rjr)

- Element arena in an `Rc` (Zed #66293, `2529740`): `src/app.rs` (`element_arena: Rc<RefCell<Arena>>`) and `src/window.rs` (`ArenaClearNeeded::clear`). Draws reach the arena through a raw pointer while a `&mut App` is live, so it must not sit inline in `App`.
- Lone modifier keystrokes flush (Zed #55800, `bc7041c`): `src/platform/keystroke.rs` (`Keystroke::is_modifier_key`) and `src/window.rs` (a single pending modifier keystroke now gets the pending-input timeout), so a lone Shift from an input-method switch doesn't stay pending. Upstream's tests were not ported.

## System quit can await unsaved changes (gpui-editor#bezt)

`Application::on_should_quit` and the platform hook let macOS defer system termination while the
app asks about unsaved files. `App::quit` confirms it; `App::cancel_quit` cancels it, including when
a save fails. Other platforms retain their existing termination behavior.

## Native Services can query editability (gpui-editor#cc6t)

`PlatformInputHandler::can_replace_text` queries the input's existing `accepts_text_input`
through its window context, without requiring a live App/window borrow in the native callback.

## Definition cards have a window entry point (gpui-editor#86sp)

`Window::show_definition` forwards lookup text, its painted baseline, and the editor's
font family and size to a macOS-only `PlatformWindow` method. The test backend ignores
the native display request.

## Animated WebP decodes to frames (gpui-editor#9c3c)

`Image::to_image_data` decodes a WebP with `has_animation()` through `AnimationDecoder::into_frames`, swapped to BGRA like a GIF; a still WebP keeps `decode_static_image`. The preview's picture (`vendor/gpui-base/src/text/node.rs`) now carries an element id, so `Img` keeps its frame state between draws: before, a GIF showed only its first frame too.

Decoded GIF and animated WebP frames stop at 64 MiB of full-canvas BGRA (`MAX_ANIMATION_BYTES`), at least one frame.

## Test launches can force the window active (gpui-editor#hdjq)

`Window::set_force_window_active(bool)` sets a process-wide atomic; only `Img` reads it (`Window::force_window_active()`, ORed with `is_window_active` at its two reads), so GIF and animated WebP frames step in a background window. `is_window_active` stays the real state, so activation observers are unaffected. Only the app's control socket (`draw-as-key on`, background launches only) sets it.

## Destructive prompt buttons (finch#tyfe)

`PromptButton::Destructive(label)` (`PromptButton::destructive`, `is_destructive`) marks an answer that throws work away. Only the macOS prompt reads it (see `vendor/gpui-pre-macos/PATCHED.md`); the in-window fallback prompt shows the label like any other button.

## `App::start_active_drag` (finch#n8rz)

`src/app.rs`. Starts a drag from code that is not an element's `on_drag` (the editor dragging its selected text). It does nothing when a drag is already running.

## Window pictures without screen capture (finch#7nbd)

`src/platform.rs` `PlatformWindow::render_to_image` and `src/window.rs` `Window::render_to_image` are no longer gated on `test-support`, so the app's `debug snapshot` verb can render a window's last scene to an `RgbaImage`. Cold code: nothing calls it per frame.
