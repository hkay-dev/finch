---
type: reference
summary: Local changes to gpui-base that must survive a dependency upgrade.
covers: [gpui-base]
---

# gpui-base 0.8.1, patched for GPUI Editor

This is the crates.io `gpui-base` 0.8.1 source (GPUI Kit's behavior layer,
which holds the `Editor` input, cursor blink and soft wrap), used through
`[patch.crates-io]` in the app's `Cargo.toml`, the same way
`vendor/gpui-pre-apple` is. The crate's own `Cargo.lock`, `Cargo.toml.orig`,
`.cargo-ok` and `.cargo_vcs_info.json` were left out. Every change from the
published crate is listed here. On a GPUI Kit upgrade, copy the new crate in
the same way and carry each patch over, or drop it if upstream fixed it.

Code here runs every frame, so every change gets a review.

Standalone vendored unit tests need the root `[patch.crates-io]` table supplied
through `cargo --config`; Cargo does not inherit that table from the app.
Without it, these tests resolve registry GPUI while app builds use local GPUI,
so passing tests would not exercise the same platform implementation.

## Batched history replay (finch#rap9)

`src/input/base/state.rs`: multiline undo and redo preserve sequential history
coordinates but send one highlighter batch and extract fold candidates once.
Forward multi-edit transactions also extract candidates once. Display-map
batches discard stale candidates up front and defer fold projection until the
final parse, while active folds still track each edit.
`src/input/base/element.rs`: wash capture stops after 256 changes and drops its
ranges; a replay already known to exceed that limit skips capture entirely.
Small paste/replay washes and reload washes keep their existing behavior.
This bound deliberately omits the wash on larger steps instead of spending
quadratic time shifting its extents.

## Single-edit fold candidates (finch#p7xf)

`src/input/editor/display_map/fold_map.rs`: `adjust_folds_for_edit` and
`merge_candidates_for_edit` find the edited span in the start-sorted candidate
list by binary search, splice the new candidates in, shift only the tail and
never sort the list. The one pass left reads the end line of candidates before
the edit, to drop those enclosing it. An existing candidate still wins over a
new one with the same start line.

## Fold candidates re-read whole lines (finch#j3x8)

`src/input/editor/display_map/fold_map.rs` (`whole_lines`), `display_map.rs`:
candidates are dropped by line, so an edit's candidates are read again from the
whole lines it touches, not its bytes, and a same-line neighbour no longer
vanishes; past 16 KB of widening (a minified line) the edit's own bytes are
used, as before.

## Frame-driven selection edge scrolling (gpui-editor#sxna)

`src/auto_scroll.rs`: replaces the fixed 16ms task with one next-frame callback
chain. Instant elapsed time scales pixels-per-second velocity, capped at 50ms
after a stalled frame. Speed is zero at the viewport edge, two viewports per
second at 60pt past it, and capped at six. Stop generations invalidate queued
callbacks so a quick release/restart cannot retain two drivers.
Editor mouse and touch drags scroll both axes with wrapping off; cursor-follow
does not compete with an active drag. Preview participants and synthetic-wheel
selection forward their owning window and retain vertical scrolling.
Headless preview projections still emit velocity but cannot schedule frames.
Wheel and trackpad event speed is unchanged.

## IME caret and converting clause (gpui-editor#qfpv)

`src/input/base/state.rs`, `src/input/base/element.rs`: the caret and reveal
position follow the selected range within marked text, not its end. A nonempty
selected clause gets a two-point underline while the rest stays one point,
including across syntax-highlight runs and masked text.
The macOS platform handler currently drops attributed marked-text clauses
when extracting its string, but forwards the relative UTF-16 selected range.
That range is the available converting-clause signal; no parallel IME state
or guessed segmentation is introduced.

## Multi-click drags keep words and paragraphs whole (gpui-editor#f9ta)

`src/input/base/state.rs`, `src/input/base/selection.rs`: a mouse press
records its click granularity. Double- and triple-click drags union the
pointer's whole word or logical line with the original anchor range, including
line breaks for triple-click. Both ordinary moves and autoscroll use this path.
Word boundaries reuse the Unicode rope selector without copying the document.
Shift-click still uses character extension after release.

## Double-click selects Unicode words (gpui-editor#19nj)

`src/text_boundary.rs`, `src/input/base/selection.rs`, `src/text/selection.rs`:
editor and preview selection share Unicode letters, numbers and combining
marks instead of a Latin/Cyrillic allowlist. Underscores still join words,
hyphens still split them, and emoji (including digit keycaps) stay whole.
Han ideographs remain individually selectable: macOS dictionary segmentation
is deliberately not used, so Chinese selection does not depend on a locale.
The rope path borrows chunks; the preview path borrows its string as a rope
slice. Neither copies the document. Preview clicks past the end still select
the last word or grapheme.

## Cursor movement and Delete keep graphemes intact (gpui-editor#cns9)

`src/text_boundary.rs`: `grapheme_boundary` uses Unicode's extended grapheme
cursor over borrowed rope chunks of the current logical line. No document or
line copy is needed, even when a cluster crosses chunks. CRLF is one cluster.
`src/input/base/state.rs`: cursor clipping, Left/Right, Shift selection,
forward Delete and hollow caret sizing all use those boundaries. Backspace
deletes emoji and CRLF intact, but removes only the last Unicode scalar from
other text clusters, including Indic conjuncts and decomposed Hangul, as on
macOS. Exact byte/UTF-16 source APIs are unchanged.
`src/input/base/state.rs`, `src/input/base/movement.rs`: forward word and
subword targets snap right to the next cursor boundary. A target inside a
grapheme therefore advances instead of snapping back and getting stuck.
`src/input/base/invisibles.rs`, `src/input/base/state.rs`: cursor stops match
the invisible-character cells used by shaping, including ZWNJ, Arabic
prepend controls and replaced variation selectors. The substitution is
unconditional; marker visibility does not change these boundaries.
Preserved joiners, emoji selectors and four-byte characters that shaping
does not replace still keep their Unicode graphemes.

## Cursor stops blinking when idle (gpui-editor#d16x)

`src/input/base/blink_cursor.rs`: the cursor blinks for 5 s after the last `start` or
`pause` (a time window since gpui-editor#8d7h, no longer 10 flips), then settles on a solid
visible cursor and schedules no more timers, so the window stops drawing frames.
Any later `start` or `pause` (focus, window activation, key, click, cursor move)
restarts the 5 s; `stop` still clears everything. Upstream blinks forever, and
each blink frame keeps Metal's first-render pool (about 178 MB) resident.
`src/otp_input.rs` shares the struct and gets the same rule. Finding:
`docs/research/szw6-empty-window-memory.md`.

## `TextViewState::measure_visible_only` (`src/text/state.rs`)

`TextViewState` builds its block list with `measure_all`, so the first layout
and every full re-parse lay out the whole document on the main thread. For a
long file that is hundreds of milliseconds (200 to 600 ms more on the first
frame for a 56 KB README). The new builder swaps in a list that measures only
the blocks in view; every other `TextViewState` keeps `measure_all`. The
markdown preview calls it right after `TextViewState::markdown`. The cost:
off-screen blocks count as zero height to the list until laid out, so GPUI's
own scrollbar handle would size the thumb from the measured part and clamp a
drag to it (gpui-editor#ndqk). A visible-only state therefore gives its
scrollbar `src/text/estimated_scroll.rs` instead: the track is the estimated
height of the whole document (each block weighs its source length; laid-out
blocks count their real height, the rest the pixels per byte learned from
them), a drag maps back to a block and an offset with `ListState::scroll_to`,
and the end of the track scrolls to the last block. The estimate holds still
during a drag. Nothing is measured up front. `measure_all` states keep the
plain `ListState` handle.

## One copy of the text, and layouts a hidden view lets go (finch#vvhs, finch#fhx4)

`src/text/state.rs`: `TextViewState` held the newest text as a `String` beside the
document's own `source`, a second copy of the file. It now holds a copy only while the
parse of the newest text is pending or failed (`text: Option<String>`, dropped by
`release_parsed_text` when a parse lands as the newest revision); `latest_text()` reads
whichever holds it. `src/text/inline.rs`: retained layouts carry the `TextViewState` they
were laid out for, and `TextViewState::release_layouts` drops them all. The table kept
one for every block ever painted until its state dropped, so a preview scrolled through a
long file held shaped lines for all of it. Finch calls it once the preview panel has
slid away, never per frame; the document, measured heights and scroll stay, so showing
again shapes the blocks in view during the first frame.

## Soft wrap near the viewport first (gpui-editor#8mgd)

Upstream `TextWrapper::update_all` wraps every buffer line in one call on the
main thread, about 16 us a line: a width change, font change or the first
layout of a 100k-line file stalled for seconds. Now a pass over more than 512
lines (`LAZY_WRAP_LINES`) is queued instead and run in 4 ms slices.

- `display_map/text_wrapper.rs`: `TextWrapper` keeps `pending` row ranges and a
  `focus_row`. `update_all` on a text whose tree already matches keeps the old
  wrap and queues all rows; a replacement of more than 512 lines (open, set
  value) fills the tree with one-row placeholders and queues the new rows.
  `step_wrap` wraps rows from the front of the queue until its budget is spent
  and splices them into the `SumTree` (`replace_rows`); `prioritize_focus` moves
  the range holding the viewport's top line to the front. An edit calls
  `adjust_pending`, which cuts the edited rows out of the queue and shifts the
  rest, so a pass never wraps a stale row; a new width, font, tab size or indent
  clears the queue and starts over. The per-line wrapping moved unchanged into
  `wrap_buffer_line`, and the closure that shapes text into `make_wrap_fn`, so
  a finished pass gives the same rows as upstream. Small texts still wrap at
  once.
- `display_map/wrap_map.rs`, `display_map.rs`: `wrap_pending`, `set_wrap_focus`,
  `step_wrap` (rebuilds the fold projection when rows changed) and
  `finish_wrap` (loops to the end).
- `base/element.rs` (`prepaint`): while a pass is pending, set the focus to the
  buffer line at the top of the viewport, run one 4 ms slice, keep that line in
  place by moving the scroll offset by the change in its display row, and
  `request_animation_frame` until the queue is empty. Display row lookups use the
  tree as it is, so they stay right while a pass runs; the row count and the
  scrollbar change as rows land.
- `base/state.rs` (`prewrap`): calls `finish_wrap`, so a caller that asks for a
  pre-wrap still gets every row.
- `base/element.rs` (`prepaint`): a pending scroll target (`deferred_scroll_offset`,
  from go-to-line or a search hit) is the anchor while it is set, and the row shift
  of a slice is applied to it, so the target keeps its place and its viewport wraps
  first.
- Memory: the slices also drop the text system's line-layout cache after each
  slice. In the one-go pass that cache lived for the whole pass (100k lines: 241 MB
  freed but still resident); `LineItem`s are unchanged, about 200 bytes a line.

## `TextViewState::scroll_source_to_top` (`src/text/state.rs`)

A new method for the markdown preview's scroll following (gpui-editor#1mqd).
Given a byte offset into the parsed source, it finds the last block whose span
starts at or before it (list item index equals block index in `render_root`)
and calls the list's `scroll_to` with that item and an `offset_in_item` of the
share of the block's bytes before the offset times the block's laid-out height.
The block is put at the top first, because `bounds_for_item` knows nothing of
items above the list top, then its height is read and the list scrolled again.
The height only counts when the block is measured; otherwise the block's top is
used and the method returns `Some(false)`, so the caller asks for another frame
and a far jump never overshoots an unmeasured block. It
reads the state and moves the list without a notify, and the list takes the new
top at its next layout. Nothing existing changes; the cost is one scan over the
blocks per call, and the preview calls it only when the editor's top moved.

## `TextViewState::source_at_top` (`src/text/state.rs`)

The inverse of `scroll_source_to_top`, for the editor following the preview
(gpui-editor#jgtk). It reads the list's logical top (item index equals block
index) and returns the top block's span start plus the share of its bytes that
`offset_in_item` has scrolled past, from the block's laid-out height. An
unmeasured block reads as its span start, a block without a span as the end of
the nearest spanned block above it, and a top past the last item as the last
block's end. It only reads; the preview calls it in frames where its own top
moved while it leads.

## `TextViewState::block_at_source` (`src/text/state.rs`)

For the preview's caret wash (finch#y1kh): the index of the top-level block (list
item index equals block index) whose source span holds a byte offset, by binary
search over the spans (a block without a span takes the next spanned block's
start), `None` when the offset sits between blocks or nothing is parsed. It only
reads: no notify, no allocation; the wash layer calls it once per frame it draws.

## `TextViewState::parsed_len` (`src/text/state.rs`)

For the caret wash (finch#y1kh): the byte length of the source the current blocks were
parsed from. The preview's text lags the editor's by up to a debounce, so the wash looks the
caret up shifted by the difference in lengths (`block_at_source`). Reads one field.

## Wheel scrolling on the display's clock (gpui-editor#ases)

Harrier's display-clock wheel driver (`harrier#84za`), ported as `src/smooth_wheel.rs`
(`pub mod smooth_wheel`). A mouse driver posts wheel events every 6 to 14 ms, unevenly,
and a scroller that jumps on each one moves half or double the last frame's distance.
Now the wheel moves a target, and every frame eases the shown offset toward it with a
16 ms time constant (Harrier's constants), rests on whole device pixels, and asks for no
frame once it has reached the target. `SmoothWheel` is state only (`nudge` from the wheel
handler, `step` from the frame's prepaint, which returns the offset to set); `driver` wraps
it for a plain `ScrollHandle`. Stretch past the ends is not in it: the editor and the text
view keep their own (`overscroll.rs`), fed by what the target could not take.

- Editor (`input/base/state.rs`, `element.rs`): `on_scroll_wheel` moves the target
  (`nudge_wheel`) for a vertical delta in a text that overflows; sideways motion, a text
  that fits and reduced motion keep the direct path. The trackpad's stretch still runs
  through `Overscroll::packet`, whose scroll closure now returns what the target took.
  `TextElement::prepaint` calls `step_wheel` before layout. Anything else that moves the
  offset (scrollbar drag, cursor reveal, find, soft-wrap's anchor shift, a pending
  `deferred_scroll_offset`) leaves it somewhere other than where the glide put it, and the
  next step drops the glide, so jumps stay jumps. New builder `smooth_wheel(bool)`, on by
  default and off in this crate's own tests (`cfg!(test)`), which read the offset right
  after an event.
- Text view (`scroll_bounce.rs`, `text/text_view.rs`): the markdown preview scrolls through
  `ScrollBounce` over a `gpui::list`, not the list's own wheel handler. `ScrollBounce` gets
  `smooth_wheel(bool)` (off by default); `TextView` turns it on for `scrollable(true)`. With it
  the wheel moves a target in `State::wheel` and `prepaint` steps it before the child lays
  out. No stretch is added to the list.
- Tree (`tree.rs`): `Tree::render` puts `smooth_wheel::driver` before the `uniform_list`,
  over the list's base `ScrollHandle`. It takes vertical wheel events in the capture phase,
  so a popover that occludes the list still gets its own. `scroll_to_item` (applied in the
  list's prepaint) drops the glide the same way.
- Limit: the tree's driver takes the capture phase, so a scroller nested inside the tree
  can't scroll.
- A finger on the trackpad skips the ease (gpui-editor#yk38): each scroller passes every
  wheel event's phase and arrival time (`Instant::now()`, not the frame clock) to
  `SmoothWheel::touch` (before the axis lock in `ScrollBounce`). From `Started` to `Ended`
  (or a touch that lifts without moving, which GPUI reports as a `Moved` of nothing) the
  glide stays alive with its fractional target instead of restarting from the painted
  pixel. A touch mid-glide catches it where it shows. Phaseless wheels still ease.
- The finger plays back on the frame clock (gpui-editor#dar7): showing each event's target
  on the next frame stepped one event's worth or two per frame whenever events and frames
  ran at different rates (the bench's 6 ms events on a 120 Hz screen: 81 of 234 steps
  outside half to double the median). `nudge` now adds each finger target to a `Path`
  (Harrier's `scroll_stream.rs`, harrier#51a7, with the lag cut to 8 to 32 ms and an 8 ms
  average for a finger) and `step` shows the finger where it was the longest recent event
  gap before the frame. The momentum straight after the lift goes on the same path; once
  it has played out, what's left eases.

## Link hover dim, external link arrow, per-block measure (gpui-editor#xegc)

- `src/text/inline.rs`: `InlineState` gets `hovered_link: Option<Range<usize>>`.
  - The mouse-move handler (still registered only for Inlines that hold links) writes it
    when the pointer enters, leaves or crosses a link, then notifies the view.
  - `paint` compares the hovered link it computes there with the stored one; when they
    differ it stores it and requests one animation frame, so a scroll that moves a link
    away from a still pointer undims it.
  - `Inline::request_layout` reads it. When set, `dim_link` clones the highlights and
    multiplies the color and underline color of the runs overlapping that link by 0.7.
    Otherwise there is no clone. Frames are requested only when the hovered link changes.
- `src/text/node.rs`: `set_leaf_key` builds its `InlineState` by assignment, because
  `InlineState` implements `Drop` and the new non-`Copy` field blocks `..Default::default()`.
- `src/text/markdown_ext.rs`: `EXTERNAL_LINK_ARROW` (a narrow no-break space and U+2197),
  `MarkdownExtensions::external_link_arrow()` and a crate-private reader. The flag is part
  of `has_same_parser_configuration`.
- `src/text/format/markdown.rs`: with the flag on, a `Node::Link` whose url is `http` or
  `https` and whose label has text gets `EXTERNAL_LINK_ARROW` appended to its own marked
  run. It stays a plain text run, so the paragraph keeps the cached layout path.
  Reference-style links and image-only labels get none.
- `src/text/state.rs`: `TextViewState::markdown_with(text, extensions, cx)`. `new` takes the
  extensions, so the first parse has them and a view with the same extensions doesn't re-parse.
- `src/text/style.rs`, `src/text/document.rs`: `TextViewStyle::with_measure` and `measure()`.
  `render_root` holds each block to the measure, centered, inside a full-width list item;
  the scroll container and scrollbar keep the full width. `PartialEq` includes the measure.

## Editor gutter, selection and current line to Mac values (gpui-editor#d83f)

New `src/input/base/system_look.rs`, used by `element.rs` and `state.rs`:

- Gutter: line numbers are 11 pt in the system face (`.SystemUIFont`) with tabular
  figures instead of the 14 pt code font. The gutter's width is the widest figures of that
  font (`"0"` repeated) plus the margin, a number is shaped bare and set flush right, and its
  y shifts so its baseline meets the code line's (half the gap between the two fonts'
  `ascent - descent`, the code side taken from the code font's metrics). Other lines' numbers
  are the text color at 0.4 ink (dark: `1 - (1 - a)^1.25`; light: text color 0.55 toward
  black, `1 - (1 - a)^1.85`), the current line's at full.
- Selection: the fill is `NSColor.controlAccentColor` (sRGB) at 0.4 dark or 0.3 light, read
  with a few AppKit getters only when a frame draws a selection fill, so a System Settings
  change shows at the next draw. A window that isn't key paints the selection in the text's own ink at
  0.07 dark or 0.098 light instead of nothing. Search matches and the hover highlight still
  paint only in the key window. Upstream paints `editor_style.selection` (the theme's token)
  and only while the window is active; that token is still the fallback off the Mac.
- Current-line band: 0.7 of its color when the window isn't key; it paints before the
  selection, so it stays under it.
- `InputState` repaints on window activation changes so all three follow the key state.

## A gliding caret with an eased blink (gpui-editor#8d7h)

`src/input/base/blink_cursor.rs`, `element.rs`, `state.rs`:

- Look: the caret is 1.5 pt wide (`CURSOR_WIDTH`) with rounded ends, hidden over a selection
  (`CursorRenderInfo::collapsed`) and drawn only in the key window of a focused editor.
  Upstream draws a 1 px square bar, also over a selection.
- Blink: `BlinkCursor` keeps AppKit's 0.53 s per half. A flip no longer toggles a bool the
  painter reads; it starts a `CARET_FADE` 0.14 s ease-out fade (bezier 0, 0, 0.58, 1) from the
  alpha the caret had, and the painter reads `BlinkCursor::alpha(now)`. Typing (`pause`) holds
  it solid for one half (was 0.3 s) with no fade in. The 5 s stop is a window since the last
  `start` or `pause`: a half that goes off is only scheduled when the on half after it also ends
  inside the window, so the last fade ends before 5 s and the caret is left solid. `visible()`
  still answers the target of the current half, which `src/otp_input.rs` (it shares the struct)
  reads, so the OTP field keeps working with the new timings (0.53 s halves, no fade drawn).
- Frames: `TextElement::paint` calls `request_animation_frame` only while a glide runs or a
  fade runs under a caret that is drawn (about 17 frames per flip at 120 Hz, then none until
  the next flip; none while every caret hides over a selection). `show_cursor` no longer reads
  the blink. When the window loses key the blink stops (`BlinkCursor::stop`), so a window
  that isn't key draws nothing; getting key back starts it again.
- Glide: `CaretGlide` (in `InputBaseState`) keeps the active caret's drawn spot in content space
  and eases it to each new one over `CARET_GLIDE` 0.09 s, carrying on from where it is when a
  key moves the target mid-glide. It also keeps the caret's text offset and the scroll offset.
  A target that moves while the offset stays is the layout moving under a still caret (panel
  resize, wrap, gutter width), so it shifts the glide's ends and starts none. A move to a new
  offset that also scrolls starts from where the caret stood on screen (old spot plus old
  scroll minus new scroll), not from off-screen. Wheel scrolling moves the text under the
  caret and never glides it. A blur clears it (paint clears it when the key window is
  lost; `on_blur` clears a document that isn't painted), so the next focus lands. A glide's x
  is not snapped to the device grid; a resting caret is.

## The caret as a layer of its own (gpui-editor#grwg)

`src/input/base/caret_layer.rs` (new), `element.rs`, `state.rs`, `blink_cursor.rs`, `mod.rs`:

- `InputBaseState::enable_caret_layer` (new, `#[doc(hidden)]`) returns a `CaretLayerHandle`:
  a cell shared with a view drawn over the input. From then on `TextElement::prepaint` records
  the carets (`CursorRenderInfo`, scroll, caret color, the content mask right of the gutter, and
  whether the input is focused and shows its caret) into the cell and `paint` draws none; the
  `CaretElement` the layer renders paints them with `paint_carets`, the loop that used to sit
  in `paint` (unchanged: glide through `CaretGlide`, `CARET_FADE` alpha, snap when resting,
  hidden over a selection). The glide state lives in the cell, and the layer asks the frames a
  glide or a fade needs.
- The `BlinkCursor` observer in `InputBaseState::new_in_mode` no longer notifies the input once
  a layer is enabled: a flip notifies the layer (`CaretLayerHandle::observe_blink`), so a blink
  never rebuilds the editor. Inputs that never enable a layer (the OTP field) paint and notify
  as before. The observer wakes the layer only when a flip could change a pixel
  (gpui-editor#5n6a): a shown bar and some caret collapsed. A selection at rest, a hollow box and
  an input drawn nowhere skip the wake, so the selection no longer presents a frame per flip for
  the 5 s blink window.
- A one-line field can take a layer too (the explorer's filter and rename fields,
  gpui-editor#em8z). It keeps the caret it painted itself: `record` gets no `hollow` and no
  spring for it (`springs` false: every move glides), and its `cell_width` stays 0.
  `CaretLayerHandle::hide` drops the record of an input that isn't drawn this frame (a list row
  scrolled or folded away), since it records nothing. The cover (`set_cover`, the find bar or
  the explorer's pinned rows) is tested where a caret is painted at rest, `x` as recorded and
  `y` plus the scroll; a bar caret used to be tested in content space.
- Same-frame exactness: the layer observes the input too (`CaretLayerHandle::observe`), so any
  notify of the input dirties the layer in the same frame, and the layer paints after the
  input's prepaint. A record that changed with no such notify (the pane resized) wakes the
  layer on the next frame (`record`, guarded by a pending flag so a frame already asked for is
  not asked twice). A blur forgets the glide (`forget`, from `on_blur`).
- `blink_cursor.rs` re-exports `Instant` (std or web_time) so `paint_carets` shares it.
- Hollow caret (gpui-editor#d72s): the record also holds `hollow` (the input is not showing its
  caret, the window is in front, the input is not disabled), and each `CursorRenderInfo` carries
  `cell_width`, the width of the character at the head of its selection (a space at a line
  end), measured only when `hollow`. The layer then paints `paint_hollow_carets` instead of the
  carets: a still 60% outline per selection head, both edges snapped to the device grid, no
  glide, no blink fade, no frame requests. An unfocused input's `BlinkCursor` is stopped, so no
  timer wakes it. Only a multi-line input records `hollow`.
- Bright line number (gpui-editor#dgar): with a band in the layer, the editor paints every gutter
  number dim except the current line's, which it skips; the `Band` carries that number
  (`Number`: text, gutter font, bright and dim colors, right edge, offset, line height). The
  layer paints it once, its color moving from dim to bright with how far the band's glide has
  come (`NumberFade`, read off the band's own position, so spring and glide both work). The
  lines going dark are a list painted bright with falling alpha over the editor's dim copies; a
  row change mid-glide keeps them partly faded, and a line lit again starts from what it showed.
  No clock or frame request of its own: the frames are the band's, and at rest the weight is 1
  and nothing is asked for. A blur or no band resets it; an input with no band, or whose handle
  has no bound layer (`CaretLayerHandle::bound`: an editor fading out after a tab switch, or
  held by a closing pane), keeps the editor's bright number.
- Dim in the pane without the keyboard (gpui-editor#q8k2): `DimFade` in `caret_layer.rs` eases a
  lit weight from the layer's own `Shown`: 0 on `Hollow` (unfocused, window in front), 1 on `Bar`
  and on `Nothing` (window not key keeps its look, at once). The band paints at `DIM_BAND` (0.5)
  to 1 of its color (`paint_band` takes the strength), and the bright number's weight and the
  fading-out numbers' alpha are multiplied by it, so at 0 it is the plain figure. Over 0.2 s
  going dim and 0.12 s coming back (`HOVER_OUT`, `HOVER_IN`, copied as `DIM_OUT`, `DIM_IN`); a
  frame is asked only while it runs.

## Rounded selection outlines (gpui-editor#wb5m)

`src/input/base/element.rs`: `layout_match_range` takes a `radius` (0.15 x line height for
selections, find matches, hover and fill decorations; zero for color swatches) and builds the
fill from `frame_outline_points` with `push_rounded_outline`: every corner a quadratic curve
clamped to half of each segment it joins (Zed's `HighlightedRange`). Rows that do not overlap
their neighbour become separate sub-paths in the same `Path`. Selections already painted as one
path each, so there is no new path pass. Stroked frames still use the sharp outline.

## Text readable over highlights (gpui-editor#q2nm)

`src/input/base/apca_contrast.rs` (new, Zed's `ui/src/utils/apca_contrast.rs`, tests left out):
`ensure_minimum_contrast` moves a color's lightness, then saturation, then to black or white,
until APCA Lc 45 (`MIN_HIGHLIGHT_CONTRAST`, Zed's default). `element.rs`:
`layout_background_segments` builds the segments `split_runs_by_bg_segments` recolors text
over: color swatches, then the selection, the current find match and the other matches in the
room the earlier ones leave, each as its fill blended over the editor background. The split
recolors with `readable_over` (a small thread-local cache) instead of the old black or white
flip, and rejoins neighbouring runs that came out the same. Nothing is built without a visible
selection or find.

## Hidden characters shown (gpui-editor#ytm8)

`src/input/base/invisibles.rs` (new, Zed's `display_map/invisibles.rs`). `layout_lines` scans
each row (one byte pass for plain rows); a row with invisible characters is shaped from
`stand_in` text of the same byte length (a space for a control character, a no-break space for
a two-byte one, U+2007 for a three-byte one) so a direction control cannot reorder what follows
and every index, column and caret step stays the buffer's. The runs get a background under each
(`highlight`), and `splice_symbols` swaps each control character's space glyph for its U+2400
symbol glyph. Four-byte invisibles and U+200D are left alone. `text_wrapper.rs` measures the
same stand-in text, so a row's wrap width matches what is drawn.

## Owner-drawn scrollbar for `TextView` (gpui-editor#djjb)

`src/text/text_view.rs`: `TextView::scrollbar(bool)`, default true, so a `scrollable` view skips
its own `Scrollbar` child when the owner turns it off. `src/text/state.rs`:
`TextViewState::scrollbar_handle()` returns the `EstimatedScroll` of a `measure_visible_only`
state (else the `ListState`) as an `Rc<dyn ScrollbarHandle>`, so the app's `src/scrollbar.rs` can
draw the Mac scrollbar (shown only after a hand scroll) over the same estimate. The kit bar shows
on any offset change, so it cannot tell a wheel from a follow. Default behavior is unchanged and
nothing runs per frame when the flag is on.

## Indent guides as 1-device-pixel quads (gpui-editor#4v5k)

`src/input/editor/indent.rs` and `src/input/base/element.rs`. Upstream strokes the guides into one `Path` painted with `paint_path` every frame, which opens the renderer's MSAA path pass. `layout_indent_guides` now returns `Bounds` runs, one per guide merged across consecutive lines, one device pixel wide with x rounded to the device grid, and paint draws them with `paint_quad`. Positions and color are upstream's.

## Text changed on disk applied as one edit (gpui-editor#dn40)

`src/input/base/state.rs`: new `InputBaseState::apply_external_text`. `external_text_diff` walks the rope's chunks against the new string from both ends (no copy) to find the shared prefix and suffix on char boundaries, and the method makes one silent replace over the span between them through the typing edit path, so wrapping and highlighting update incrementally instead of `set_value`'s full reset. It sends no Change event, clears undo history as `set_value` did, keeps the clamped selection and the scroll, and returns `false` when the text already matches. The app's `Document::reload` uses it.

## Width changes re-wrap only the lines that need it (gpui-editor#ngv1)

`src/input/editor/display_map/text_wrapper.rs`: a panel slide or split glide changes the editor
width every frame, and `set_wrap_width` used to wrap the whole file again. Now each `LineItem`
keeps `natural_width`, the shaped width of a line that fits one row. `measured_wrap_boundaries`
shapes the whole line first (lines up to 4096 bytes; longer ones take the old prefix probing) and
returns at once when it fits. `set_wrap_width` between two widths re-wraps only rows that wrapped,
have no stored width, or are wider than the new width (`rewrap_for_width`): up to
`LAZY_WRAP_LINES` rows at once, the rest in the focus-first time-sliced pass. A font, font size,
tab size, indent or inline element change still goes through `update_all`, and the rows it queues
mark `pending_foreign`, so a width change during that pass keeps them queued instead of trusting
their old widths. A text edit builds fresh items. Row breaks are unchanged. The crate's tests
need `--features test-support` and a `README.md` at the repo root (an `include_str!` in
`text_view.rs`).
## Re-parse only the blocks an edit touches (gpui-editor#t3g7)

`src/text/format/markdown.rs` (`parse_edit`), `src/text/state.rs` (`parse_content`),
`src/text/node.rs` (`BlockNode::shift_source`), `src/text/markdown_ext.rs` (`mdx_enabled`).
`set_text` with text that is not an append used to parse everything again. `parse_content` now
finds the common prefix and suffix of the old and new text, parses again the blocks the edit
touches plus the block on each side, keeps the earlier blocks, and clones the later ones with
every source position moved by the length change. The result equals a full parse. The
background parser and the synchronous (up to 4 KB) path both use it; appends are unchanged.
It falls back to a full parse for: fences, `$$`, tags, `]:` (link definitions, footnotes) or a
line of only dashes or equals signs in the old or new slice (and `]:` anywhere in the new
text); front matter at the start; MDX; blocks without spans; custom nodes in the moved
blocks; slice edges not on blank space; and a re-parsed slice whose block count, edges, or
neighbouring blocks (compared with the old ones, moved) differ, which covers splits, joins and
a neighbour that changes kind, such as a lazy list continuation.

## Far jumps center, undo reveals its change (gpui-editor#mgza)

`base/state.rs` (`scroll_to_with_padding`, `replay_history`): a target row more than a screen outside the view is centered vertically instead of pinned to an edge; nearer ones keep the least scroll. Undo and redo scroll the restored cursor into view the same way.
Undo and redo now ask the next layout to reveal the caret (`reveal_far_centered`, `center_if_far` in `base/element.rs`), so it is measured against the new text.

## Soft wrap skips shaping for plain ASCII lines in a monospace font (gpui-editor#mm2n)

`src/input/editor/display_map/text_wrapper.rs` (`ascii_cell_width`, `wrap_ascii_line`, `make_wrap_fn`).
A line of printable ASCII (no tabs, no other characters) in a font whose printable ASCII
characters all share one advance is as wide as its length times that advance, so
`measured_wrap_boundaries` runs with that product instead of shaping the line. Break choice is
the same code as before. Lines with anything else, fonts that are not monospace, and a wrap
width within 0.01 cell of a whole number of cells (shaper rounding could differ there) keep full
shaping. Checked against shaped wrapping on 405k lines (the repo's .rs and .md plus mixed prose
with combining marks, RTL, Thai, Devanagari, emoji and tabs) at four widths, with and without
indent: no differing break.

## Toggle comments on Cmd-/ (gpui-editor#21xg)

`src/input/base/comment.rs` (new), `state.rs` (`ToggleComments` action, `cmd-/` binding,
registration), `mod.rs`. Zed's `toggle_comments` on a per-language prefix table keyed by the
editor's language name: line comment prefix plus a space at the smallest indent of the non-blank
rows across all cursors, or removal when every non-blank row has it; html, css and markdown wrap
the rows in a block comment instead; plain text does nothing. One undo transaction, selections
map through the edits.

## Word moves walk the rope and stop at punctuation (gpui-editor#v6h4)

`src/input/base/movement.rs`, `state.rs`. `previous_start_of_word_at` and `next_end_of_word_at`
copied half the text into a `String` on every Option-arrow or Option-Delete and used UAX#29
boundaries. They now walk chars from the caret over the rope, with Zed's word, punctuation and
whitespace classes: `_`, `.`, `-` are stops, brackets stop one by one, whitespace and line
breaks are skipped. Word delete is cut short by `adjust_greedy_deletion` (a whitespace run of
two or more, or a bracket). New subword actions (`MoveToPreviousSubwordStart`,
`MoveToNextSubwordEnd` and the select pair) stop at `_`, `-` and camelCase changes.

## Find streams over the rope, Whole Word, smartcase (gpui-editor#czwc)

`src/input/editor/search.rs`, `src/input/base/state.rs`. The search matcher no longer keeps a
copy of the document (`SearchMatcher::update` takes the input's rope and streams its chunks into
Aho-Corasick; a stale flag set on every edit replaces the rope comparison). A case-insensitive
query with non-ASCII letters runs as an escaped `(?i)` regex line by line, so `é` matches `É`.
Added the Whole Word option (`set_search_options`, `SearchSession::whole_word`), smartcase
(`SearchSession::smart_case_insensitive`), Use Selection for Find (`use_selection_for_find`), and
sharing the query with the Mac find pasteboard when a search runs, Find opens and the input
gains focus.

## Duplicate line and move lines (gpui-editor#2c81)

New `src/input/base/line_ops.rs` (declared in `src/input/mod.rs`), with actions `DuplicateLine`, `MoveLineUp`
and `MoveLineDown` and macOS keys Cmd-Shift-D, Ctrl-Cmd-Up and Ctrl-Cmd-Down in `state.rs`. The plans
(`plan_duplicate`, `plan_move`) work on the rope and byte offsets, ported from Zed's `duplicate` and
`move_line_up/down`: rows touched by every selection merge into blocks, a block swaps with the neighbouring
line using the terminator between them (so CRLF and a last line without a newline survive), and the empty row
after a final newline is never a line. One press is one undo transaction.

## Delete line, join lines, transpose (gpui-editor#ksgp)

`src/input/base/line_ops.rs` and `src/input/base/state.rs`: actions `DeleteLine`, `JoinLines` and `Transpose`
with macOS keys Shift-Cmd-K, Ctrl-J and Ctrl-T, planned like duplicate and move (`plan_delete`, `plan_join`,
`plan_transpose`) and applied as one undo transaction. Delete line removes every touched row with its
terminator (a last row without one takes the terminator before it) and keeps the column on the row that takes
its place. Join lines joins each touched row with the next with one space and the next row's indent dropped; a
selection over several rows joins just those rows. Transpose swaps the characters around each bare cursor (the
two before it at a row end), as Cocoa's Control-T does, and also works in one-line fields: it is registered with
the editable actions, the other two with the multi-line ones.

## Pasted code takes the indentation where it lands (gpui-editor#bjt3)

`src/input/base/paste_indent.rs` (new), `src/input/base/state.rs` (`insert_clipboard`),
`src/input/editor/language_config.rs` (comment). In a code editor whose language is not text,
markdown, make, diff, Python or YAML, a multi-line paste shifts every line after the first by the caret line's indent (the
caret column when it sits inside the indent) minus the block's own indent (least indent of its
non-blank lines after the first). Blank lines become empty; tabs or spaces follow the editor's
tab setting; each cursor gets its own shift. Whole lines copied (first line starts with whitespace) pasted with only
whitespace before the caret use the least indent of all lines, first included, and the first
line keeps only its excess; with text before the caret the first line stays as copied. Plain text, markdown, Makefile, diff, Python and YAML (indentation is syntax there, finch#46kn) and single-line pastes are unchanged.

## Edit safety fixes (finch#vs9a, finch#qf1w, finch#335h, finch#j3ay, finch#ryk2, finch#tef5)

`src/input/base/state.rs`: `backspace_boundary` and `delete` clip to a character boundary without the fold clamp that caret movement uses, so one Backspace or Delete next to a fold removes one character and the fold opens through the edit (finch#335h). `replace_range` snaps `caret` to a character boundary (`clip_offset`, left) so a caret byte carried over from older text cannot land inside a character (finch#qf1w). `selected_texts` keeps empty selections as empty lines when there are several, so Copy and Cut give one line per caret; `insert_clipboard_text` splits the paste into lines only with several carets and strips the `\r` of each line (finch#j3ay). `src/input/editor/indent.rs` (`compute_block_indent`) leaves out the row a whole-line selection ends on at column 0 (finch#ryk2). `src/input/editor/search.rs` (`apply_occurrence_selections`) opens the folds that hide the new selections, for Cmd-D and Select all matches (finch#tef5).

## Expand and shrink the selection by syntax (gpui-editor#0nj0)

`src/input/base/syntax_select.rs` (new), `src/input/base/state.rs` (two actions, `ctrl-shift-up` and
`ctrl-shift-down`, one history field), `src/input/editor/highlighting.rs` (new
`InputHighlighter::syntax_ancestor`, default `None` = no tree), and in `gpui-component`
`highlighter/input_adapter.rs` (answers from the main tree) and `input/mod.rs` (re-export).
Expand grows each selection to the first named node strictly larger than it, the word first
inside a string or comment; with no tree it goes word, line, text. The history of earlier
selections is checked on each action against the document revision and the selections the last
action left, so no hook runs in the edit or selection paths.

## Matching brackets and Control-M (gpui-editor#exgc)

`src/input/base/brackets.rs` (new), `element.rs` (`layout_bracket_pairs`, painted with the
selections), `state.rs` (`MoveToEnclosingBracket`, Control-M, `bracket_cache`, cleared in
`set_highlighter`), `editor/highlighting.rs` (`InputHighlighter::syntax_contexts`, default
`false`), gpui-component `input_adapter.rs` and `input/syntax_context.rs` (`context_in_tree`).
The partner of the bracket next to a caret is found by a depth-counting byte scan that counts
only brackets the highlighter's own tree calls code, so strings and comments never mismatch and
a pair of any width matches (capped at 1 MiB from the bracket, not windowed like Zed's 16 KB
query). The brackets of each 16 KiB of scan are classified in one call and no new parse is made
(the provider path compared the whole rope per bracket and reparsed after every edit). Each
window is one walk of the tree over its range (`char_contexts_in_tree`, kinds looked up by id),
not a descent per bracket. Over 64 KiB (measured 2.7 ms for the outermost brace of dense JSON at
that size, 45 ms at 1 MB), or with no tree yet, every bracket counts as code. The pairs are found again only when the
document revision, text length, the selections (start, end, cursor) or the highlighter's parse generation
changed; a frame otherwise compares the key. The jump lands on the same side of the partner as the
caret was on the bracket, so a second Control-M comes back. Multi-line inputs only.

## Draw as key window (gpui-editor#2pej)

`src/input/base/system_look.rs` holds a process-wide `AtomicBool` with `set_draw_as_key` and
`window_key(window)` (`window.is_window_active()` or the flag), exported from `input`. The
caret, hollow caret, selection fills and current-line band in `element.rs` and the cursor's
visibility in `state.rs` read `window_key` instead of `is_window_active`. The app's debug
socket sets the flag (`draw-as-key`, background launches only); nothing else does. One atomic
load per read.

- `input/editor/search.rs`: `replace_all_search_matches` applies each match as a ranged edit in one undo step, maps the cursors through the edits and no longer scrolls to the top (gpui-editor#5n04).
- `input/base/transform.rs` (new), `line_ops.rs`, `state.rs`, `mod.rs`: the actions `MakeUpperCase`, `MakeLowerCase`, `Capitalize` and `SortLines` (Edit > Transformations). Case changes act on each selection, or the word at an empty caret; sort orders the touched lines by Unicode order. Each is one undo step. `line_ops.rs` splits `line_op` so `apply_line_plan` is shared and exposes its row helpers (gpui-editor#4vn9).

## Block images the app decodes itself (gpui-editor#j0tx)

`image_element` (`src/text/node.rs`) also takes `ImageSource::Render` and `ImageSource::Custom`, which the app's `image_source` returns for a `data:` image it decodes itself: `Custom` returning `None` while it decodes (the box is reserved from `ImageNode::header_size()`, nothing drawn), `Render` once done (frame box and fade-in as for `Image`). `header_size` reads a `data:` URL's picture header from the first 64 KiB of its payload once per node (`ImageNode::header`, a `OnceLock`).

## Local pictures the app decodes itself (gpui-editor#zc5p)

`text/node.rs`, `text/mod.rs`: a local file no longer loads through the asset cache (which kept it for the app's life and never saw an edit); the app's `image_source` sends it down the `Custom`/`Render` path above. While it decodes, the `Custom` loader calls `reserve_local_image(path, stamp, ..)`, which reads the file's header on a background thread (`HEADERS` is now keyed by path with a stamp, so a changed mtime reads again) and leaves the size for the `Custom` arm to take. `forget_local_image(path)` drops a path's header when the app frees its slot.

`retain_shown_images(live)` prunes `FIRST_SHOWN` (the fade-in's first-shown times) to the ids of the images the app still holds, so it stops gaining an entry per decode; the app calls it from both release calls (gpui-editor#ge7e). Path-keyed entries stay.

## Picture header sizes across formats (finch#7zkr)

`src/text/node.rs`: WebP, BMP, TIFF and ICO ask the decoder for dimensions
without decoding pixels, swapping sides for EXIF quarter turns. Local files
use a seeking reader on the background task so TIFF directories and WebP EXIF
can follow large pixel payloads. SVG parses at most 64 KiB through the existing
HTML parser, resolves missing and percent sides against the viewBox like usvg,
and includes GPUI's SVG rendering scale. Embedded header results are retained
in the node's `OnceLock`; local reads retain the stamp-keyed cache. PNG, GIF
and JPEG keep their short-header readers. Pixel decoding, image fade-in and the
app's animated-frame cap are unchanged. The direct `image` dependency enables
only codecs already built by GPUI.

## Task text follows its box (finch#gvjm)

`src/text/node.rs`: `render_list_item_row` interpolates foreground to muted
foreground using the checkbox's existing sampled fill. The text shares the
150 ms quick fade, reverses from its drawn colour and requests no extra frames
or animation state. At zero fill no colour is set, so unticked tasks retain
inherited ink, including a blockquote's grey. Ordinary list items are unchanged.


## A match keeps a comment grey (gpui-editor#rsd7)

`input/base/element.rs`, `apca_contrast.rs`: text over a highlight is asked for Lc 45, or the contrast it has over the editor background when that is lower, so quiet text (a comment) over a find wash stays its grey instead of turning white. `split_runs_by_bg_segments` and `readable_over` take the editor background; `apca_contrast` is `pub(super)`.

## A find miss is counted (gpui-editor#fq9v)

`input/editor/search.rs`: `SearchSession::misses` counts Find next or previous steps that find no match for a non-empty query (and notifies), so a view can answer a miss. Cmd-G goes to the editor state directly, never through the search panel, so the count has to live here.

## A scroll during a jump carries the jump's speed (gpui-editor#kejv)

`motion.rs` `RevealGlide` keeps a `speed` and the frame time of its last step; `retarget` rebases the glide from the offset the last step drew and the speed it had there (the same math as the app's `Glide::with_speed`), so a wheel notch or a new Find target mid-glide never steps. `input/base/state.rs` `aim_reveal_glide` retargets a running glide when the new target is within one screen; a farther one restarts with the one-screen cut. From rest the curve and length are unchanged.

## A paste or undo of two lines washes the changed rows (gpui-editor#4jk0)

`input/base/element.rs`, `state.rs`: `push_history` feeds a `WashCapture` while `insert_clipboard` or `replay_history` runs, noting each edit's new extent (earlier extents shift behind later edits). A paste that inserted a newline, or a replay where either side of a change held one, sets `wash` to those extents widened to whole rows. Prepaint steps it on the app's frame time (`WASH_LENGTH` 0.3 s, `blink_cursor::ease_out`, level 1 down to 0, the clock starting at the first frame that draws it; a wash no frame drew within `WASH_LENGTH` of the edit, as in a hidden tab, is dropped, not played late; rows end before a trailing newline so the next row isn't washed), asks for frames only while it runs, and paints it in the selection's fill at `WASH_INK` of its alpha, under the selection. The next edit (a changed `document_revision`) drops it; a second paste or undo replaces it from full. Typing, single-line pastes and Replace never set it.

## Copy lifts the selection fill once (gpui-editor#rrdj)

`input/base/element.rs`, `state.rs`: `copy` in a key window sets `lift` (the selections and `document_revision` it saw). Prepaint steps it on the same frame time as the wash (`step_lift`: `LIFT_LENGTH` 0.25 s, `blink_cursor::ease_out`, level 1 down to 0, the clock starting at the first frame that draws it) and asks for frames only while it runs; paint adds `LIFT_STEP` times the level to the selection fill's alpha, in a key window only. An edit or any change of the selections ends it; a second Copy starts it over. Copy with nothing selected copies nothing and lifts nothing; Cut lifts nothing. Paint only, no layout.

## A far jump washes the line it lands on (gpui-editor#mk6n)

`input/base/state.rs`: `aim_reveal_glide` sets `reveal_far` when it starts a glide (or restarts one with the one-screen cut) whose distance from the drawn offset is more than the viewport; a retarget within a screen keeps the flag, any other reveal clears it, and so does a wheel retarget. When `step_reveal` draws the glide's last frame, `land_far_glide` sets `wash` (the paste's wash, same painter and fade) to the caret's row, so the clock starts on the frame the view arrives and prepaint steps it in the same frame. A glide that is interrupted (the selection changes, a scrollbar drag sets the offset) never washes, and a row already inside a running edit's wash (`Wash::covers`) isn't washed again nor is that wash cut off. A short Find step, arrows, Page Down, the wheel and typing never set it; a new far jump mid-wash replaces it. A blank landing row has no text to wash and shows nothing.

## A file changed on disk washes the lines that changed (gpui-editor#9x51)

`input/base/state.rs`: `apply_external_text` runs its silent replace inside a `wash_capture` and ends it with `end_wash(WashKind::Reload)`, which washes whenever the capture has any rows (a paste needs an inserted newline, a replay a newline on either side), so a one-line change in place washes too. Identical text returns early and washes nothing, so our own save's reload is silent. `last_external_text` stamps every changed reload; a reload less than `EXTERNAL_WASH_QUIET` (2 s) after the previous one skips the capture, so a file a tool appends to every second washes once, not every time. Paint only: the text, selection and scroll are set exactly as before.

## A code block's edge, corner plate and label at rest (gpui-editor#7qdm)

`src/text/style.rs`, `node.rs` (`CodeBlock::render`): the block's edge is `style.hairline()` wide instead of 1 pt (a table's lines and a rule's thickness read it too since gpui-editor#ypzh), and the language name always shows, in `style.code_label()` (the muted foreground when unset), where it used to hide whenever the caller gave actions. The corner is one row (name, then the actions); the name and the actions each sit on their own plate filled `style.code_plate()` with `style.code_plate_radius()` corners, so the actions' plate covers the end of a first line only while they show. The plate is opaque, since a translucent card fill would stack on itself. Only the actions (and their plate) fade with the pointer. The app feeds the three from `src/preview/render.rs`'s prose style.

## A side gutter that follows the view's width (gpui-editor#2vrw)

`src/text/style.rs` `with_gutter(share, left_least, right_least, most)`, `document.rs` `render_root`: each list item becomes a row of a gutter, the block and a gutter, where a gutter is `share` of the row's width held between its least (the right one can be larger, for a scrollbar track) and `most`. The space is part of the scrolled item, so the wheel and a selection drag work over it (a padding on the view's parent did not: the wheel went nowhere over it), and it is read from the width the view is laid out at, so a panel sliding open lays the text out once. The app uses it for the preview's reading inset (24 pt at 420; at 120, 8 pt left and the 15 pt scrollbar track right).

## Table lines and rules draw at the style's hairline (gpui-editor#ypzh)

`src/text/node.rs` (`render_table`, `render_table_rows`, `table_row_corner_radii`, the rule and footnote rule): the frame, the column dividers, the row dividers and both rules use `style.hairline()` (default 1 pt, the app feeds `1 / scale_factor`), not `border_1`, `border_r_1`, `border_b_1` and `h(px(1.))`. The column layout reserves what a divider draws; the cached text measurements keep the old 1 pt (`CELL_BORDER_PX`), so a column keeps up to half a point of slack on a sharp screen. The row fills' inner radius subtracts the hairline too. The prose style cache keys on the hairline, so a window moved to another screen restyles once.

## Typing at the bottom glides the text up (gpui-editor#05q3)

`input/base/state.rs` `begin_typing_glide`, called in prepaint between `begin_undo_glide` and `step_reveal`: when the text changed since the last paint (`laid_out_revision`, set where `last_selected_range` is) and the active selection moved, it works out the offset the layout's own reveal would land on (`reveal_row` with `cursor_surrounding_padding`, judged from a running glide's end, clamped to the larger of the content height the edit leaves and last frame's `scroll_size`, which still has inline-completion rows; content that shrank stops the glide at the layout's clamp). A move down of at most one screen goes to `aim_reveal_glide` as an eased reveal, so a key or Return on the last lines glides on the reveal spring and a held Return retargets it with its speed. When the running glide's end already shows the caret, the glide carries on through the edit (`reveal_selection` is reset so the edit's new selection doesn't end it), so typing at a Find match lets that glide finish. Moves up, longer moves, pastes and replaces (`deferred_scroll_offset`), undo and auto-scroll land as before.

## `InputBaseState::tab_setting` (`src/input/editor/indent.rs`)

A read-only getter for the tab size the input indents with now (`mode.tab_size()`, which was
`pub(super)`). The app's status line shows the indentation from it, and it follows Convert
indentation to spaces or tabs, which change the setting live. Not hot: read on a caret move.

## A code block's language name shares the first line's baseline (gpui-editor#8zc1)

`src/text/node.rs` (`CodeBlock::render`): the corner plate hangs `CODE_CORNER_TOP` (13.5 pt) from the block's top instead of `CODE_CORNER_INSET` (6 pt), so the language name's baseline is the first code line's baseline, not 7.5 pt above it. The number is measured for the app's 13 pt code at 1.55 lines; a caller with another code size or line height moves it. A one-line block still holds the 22 pt plate (13.5 + 22 under a 48 pt block).

## Read-only input declines incoming Services (gpui-editor#cc6t)

`InputBaseState` implements `EntityInputHandler::accepts_text_input` with its existing
`is_editable` predicate, so native Services can distinguish selectable read-only text
from an editable input without changing selection or attempting an edit.

## Native Look Up reads its painted baseline (gpui-editor#86sp)

`InputBaseState::text_baseline_for_range` reads the cached visible logical and
wrapped rows, their byte offsets and shaped ascent/descent through a read-only
`InputLine::baseline_offset`. It reports window coordinates on demand, without
adding shaping, allocations or work to a frame.
`text_boundary::word_range_in_rope` is public so Look Up shares the Unicode word
and emoji rules used by editor and preview selection (gpui-editor#19nj).

## Plain Markdown runs do not reserve object payloads (gpui-editor#kkrb)

`src/text/node.rs`: `InlineNode` boxes its optional image and custom-node payloads.
A 5 MB Markdown preview's largest retained preview allocation was paragraph
children storage: ordinary text and spare vector slots each reserved both large
object payloads. Boxing only those payloads keeps ordinary runs compact without
changing the document tree, rendered text, source mapping or selection state.
`InlineNode::image` accepts an existing box too, so merging marked image runs
moves their allocation instead of freeing and reallocating it. Object clones
still own independent spans. A size regression test protects the plain-run cost.
The release footprint comparison and measurement caveats are in `docs/notes.md`.

## Callout colours from the host (gpui-editor#v4p3)

- `src/text/style.rs`, `src/text/node.rs`: `TextViewStyle::with_callout_colors([Hsla; 6])` (Note, Tip, Important, Warning, Caution, Harsha) and `callout_color(kind)`; `CalloutKind::color` stays as the fallback when the host sets none. The callout render reads the style instead of calling `kind.color(dark)`.

## Tests: external edit and spring-back (gpui-editor#hssz)

Tests only. `apply_external_text_edits_in_place` builds a textarea (a single-line input strips the newlines it feeds). The two `scroll_bounce` spring-back tests set the frame clock per drawn frame (`draw_frames`), since the springs step on `motion::frame_time` since gpui-editor#kn7q and the real clock does not move in a test.

## Preview pictures carry an element id (gpui-editor#9c3c)

`image_element` gives its `img` an id from the shown image (`ShownKey`), so `Img` keeps its frame index between draws and GIFs and animated WebPs play. The id is per image because HTML-block paragraphs share one id.

## Show invisibles marks and the gutter glide (gpui-editor#4c0a, gpui-editor#rnzw)

- Show invisibles (gpui-editor#4c0a): the space mark is a middle dot at the font size (was a half-size bullet) and the tab arrow starts a hair in from the tab, in `input/base/element.rs` and `input/editor/display_map/text_wrapper.rs`.
- Gutter width glide (gpui-editor#rnzw): when the line count gains or loses a digit the text is laid out at the new gutter width at once, and `LastLayout::line_number_width` (where the text, selections, carets and guides draw) springs to it on `motion::spring` (100 ms, critically damped, as the caret). `PrepaintState::gutter_width` holds the true width for the gutter, its numbers and fold icons, in `input/base/element.rs`.
- Gutter spring id (gpui-editor#rnzw): `motion::spring_state_id` and `spring_by_id` let the editor build its gutter glide key once (`gutter_spring_id` in `input/base/state.rs`) instead of allocating it every prepaint.
- Far-glide wash (gpui-editor#1g8n): `land_far_glide` in `input/base/state.rs` also treats the row just after an edit wash as covered (the wash stops before its last newline), so a paste ending in a newline is not washed a second time.

## Visible highlight styles reused between layouts (finch#r7qz)

`input/base/element.rs` (`HighlightCache`), `input/base/state.rs` (`highlight_cache`). `highlight_lines` kept the highlighter query for the visible lines and its group ranges, and asks again only when the visible buffer lines, `document_revision`, text length, the highlighter's `syntax_generation` or the theme resolver (held by `Arc`, compared with `ptr_eq`) differ. A highlighter that reports no generation is never cached. `set_highlighter`, `reset_highlighter` and `set_highlighter_factory` drop the entry. Semantic tokens, hover, decorations and diagnostics are still composed every layout from the cached base, so they need no stamp. The buffers are refilled in place.

`release_layout` (a hidden tab or a pane that faded out) drops the entry too, so a tab left behind holds no highlight styles; the return asks the highlighter once for its screen (finch#qnbk). Showing editors never call it, so scrolling still reuses the entry.

## Smart punctuation in Markdown prose (finch#nan6)

`src/text/format/markdown.rs`: `parse_inline_children` ends with `smarten_punctuation`, which turns straight quotes, `--`, `---` and `...` in the built inline runs into curly quotes, an en dash, an em dash and an ellipsis (a quote after an emphasis or link closes, since it reads the run before). Escaped and entity characters (`\"`, `&quot;`) stay straight, and a quote before a space or twin quote closes. Heading anchors slug the drawn dashes back to `--` and `---` (`src/preview/anchor.rs`). Code marks, autolink labels, objects, runs with no source mapping (inline HTML) and code blocks are skipped. Marks and `source_segments` are remapped; a replaced stretch is one non-linear segment covering all the source it stood for, the stretches between stay linear, so selection and reveal land on the right source. Unclaimed-math fallback text and MDX expressions are runs like any other and are not excluded.

## The undo cap counts each change's fixed cost (finch#6awj)

`input/base/undo_manager.rs`: `change_bytes` adds `size_of::<Change>()`, 16 bytes of allocator header per non-empty string and the token delta's box to each change's text, so 400k one-byte changes (a Replace all) read past the 32 MB cap and the oldest steps go. The newest step and the 1000-step bound are unchanged.

## `TextViewState::awaiting_first_parse` (finch#5e2f)

`src/text/state.rs`: true while a text too big to parse in the frame it was set has no parsed block and its background parse is still running (the held text exists, nothing failed, the text is not blank). The app's preview pane reads it each render to fade the preview in when the first parse lands; it is false for an edit of a text already parsed.

## Use Selection for Find takes the caret's word (finch#6j7k)

`input/editor/search.rs`, `input/base/selection.rs`. Cmd-E without a selection
uses the same Unicode word boundaries as double-click, but rejects whitespace
and punctuation runs and checks the preceding character at word ends. The same
caret-word helper drives occurrence selection. Neither selection nor the bar
changes; with no word, the retained query stays unchanged.

## Find matches become editable selections (finch#8ker)

`input/editor/search.rs`, `input/base/state.rs`. Select all search matches uses
the active search options, or selected text/the caret word with stored Match case
and Whole word options without a search. Starting from a caret word forces
whole-word matching without changing the toggle; a primary-range and document
revision marker preserves that origin across Cmd-D additions. Select next
occurrence adds unselected matches after the last caret, wrapping. Both close Find
and install unique
cursor IDs through `replace_all`, preserving one selection per adjacent match.
Select next match reveals the added selection's end, not the unchanged primary
caret, so offscreen additions are visible before typing.
Select all matches constructs at most the first 10,000 carets in document order;
the cap bounds normalization, rendering and the next edit's per-caret work.
Typing uses the existing multi-selection edit transaction and one Undo.
Next-occurrence lookup sorts the nonoverlapping selections once, then binary
searches them for each match instead of walking all carets per candidate.
Menu eligibility never scans the document: a selection or caret word enables
Select next match, and an exhausted action is a no-op. The observer compares only
the primary range, revision, search scan identity and session/editability flags,
without copying or walking the other carets.

## Enter and brackets read the highlighter's tree (finch#ffc6)

`input/base/mode.rs`, `input/editor/highlighting.rs`, `input/editor/language.rs`. `LayoutMode::syntax_context_at` asks `InputHighlighter::syntax_context_at` first (default `None`) and falls back to the syntax provider, whose cached parse is dropped through `SyntaxContextProvider::release_cache` once the highlighter answers.

## Occurrence selections follow smartcase (finch#ppvs)

`input/editor/search.rs`. Select all matches and Select next match with no search open take their case from the selected text (`occurrence_case_insensitive`), like Find, unless `SearchSession::case_chosen` is set: `mark_search_case_chosen` sets it, and the Match case button and Option-Cmd-C call it.

## Batched edits reach the highlighter as plain edits and the final text (finch#r0xq)

`input/base/state.rs`, `input/base/mode.rs`, `input/base/kind.rs`, `input/editor/mod.rs`, `input/editor/highlighting.rs`. `InputHighlighter::update_batch` takes `&[InputEdit]` and the text after the last edit, not one rope snapshot per edit; a 20k-match Replace all or undo of one held 20k diverging snapshots until the highlighter ran. The default `update_batch` hands the final text to `update` once with no edit.

## Window activation skips hidden editors (finch#ndft)

`input/base/state.rs`. The activation observer notifies only editors that have drawn and still hold their shaped lines (`is_showing`); a hidden tab (never drawn, or `release_layout`ed) reads the key state on its next draw. The blink start and stop still run for all.

## Wrap width getter (finch#5qnn)

`input/editor/display_map/{display_map,wrap_map,text_wrapper}.rs`. `DisplayMap::wrap_width()` reads the width the wrapper last wrapped at (read-only, no change to the wrap path), for the control socket's `tab-state`.

## The selection, band and find fills ease when the window gains or loses key (finch#qxv9)

`input/base/element.rs`, `motion.rs`. `key_level` keeps one `KeyEase` per window in `motion::with_window_slot` (one small value per window, dropped by `forget_frame_time` when the window closes), stepped in prepaint on the app's frame time: 1 key to 0 not key, `KEY_IN` 0.12 s up and `KEY_OUT` 0.2 s down (the app's `HOVER_IN` and `HOVER_OUT`) on `blink_cursor::ease_out`, turning around from the drawn level on a second flip. A window first seen starts at rest, so a launch doesn't fade. Paint mixes each fill between its key and not-key look in RGB weighted by alpha (`mix_fill`, so the faint grey doesn't drag the middle toward grey; `key_fill` for the selection and the paste wash ink, Find reads the system colours once per look and mixes both fields), and `band_color` takes the level for the band's 0.7 to 1 strength. Frames are asked for only while it eases. The background segments that recolor text for contrast (`layout_background_segments`) and the hover highlight still use the window's current key state, so text over a fill never reshapes per fade frame. The hollow caret rule is unchanged.

## The preview's menu knows the link under a right press (finch#txkm)

`text/state.rs`, `text/inline.rs`. An `Inline` with links records the link under a right mouse press on the view (`set_context_link`); the host's own right-press handler, higher up the same press, takes it once (`take_context_link`) and shows its menu. Left-button selection and link clicks ignore the right button, so they are unchanged. `selected_markdown` returns the exact source slice `selected_source_range` maps to (so typographic punctuation stays as typed), falling back to `SelectionFormat::Source` when it can't map (`None` for HTML); `selected_text_in` is now that function with the view's own format.

## Find can use regular expressions (finch#38qa)

`src/input/editor/search.rs`. `SearchSession::regex` (`set_search_regex`) makes `SearchMatcher::update_pattern` compile the query as a regex (`^`/`$` per line, CRLF aware, case and whole word still apply; empty matches are skipped). A pattern that fails to compile matches nothing and reads `SearchMatcher::invalid`. `replacement_for` expands `$1` and `${name}` for Replace and Replace all, and `map_through_replacements` now maps through each edit's own length. A query naming a line break (`\n`, `\r`) scans in blocks (the whole text in one buffer when the pattern has no length bound; see finch#2qjn) and always rescans in full after an edit.

## Find and Replace crash and data fixes (finch#2qjn, finch#bebx, finch#ys52, finch#fd67, finch#xq64, finch#bqb0)

`src/input/editor/search.rs`, `Cargo.toml` (new `regex-syntax` dependency, already in the lockfile, for a pattern's maximum match length), `input/base/state.rs` (`edit_blocked` is `pub(crate)`). `scan_blocks`: each block starts at the row where the last match ended (finch#2qjn, a start offset past the buffer panicked); `reach` comes from the pattern's longest match (`longest_match`), and a pattern with no bound, or one reaching past the text, scans one whole-text buffer (finch#bqb0); a match running off the buffer's end doubles the buffer instead of adding 256 rows, so the scan stays linear. After a single Replace the next scan makes the first match at or after the end of the inserted text current, wrapping to the first (`begin_replacement_at`, finch#bebx). A case-insensitive non-ASCII query builds the folding regex only up to 2048 bytes and when it builds; otherwise it uses the literal matcher with ASCII case folding (finch#ys52). `$` plus digits in a regex replacement is rewritten to `${digits}` before expanding, so `$1_old` keeps group 1 (finch#fd67). Replace and Replace all return at once when `edit_blocked` (Undo history scrubbed back) so they claim nothing and move no carets (finch#xq64).

## Return continues a Markdown list (finch#v82q)

`input/base/state.rs`. In an editor whose language is `markdown`, Return after a list item's marker (`- `, `* `, `+ `, `1. `, `1) `, with an optional `[ ] ` or `[x] `, under any indent and `> ` prefix) starts the next item, counting numbers up and unchecking task boxes; in the middle of the text it splits the item. Return on an item that holds only its marker deletes the marker and its indent (the `> ` stays) and adds no newline. Shift-Return, a caret before the item's text and any line inside a fenced block keep the plain newline. Fences are tracked by character and run length (opening after indent, `>` and a list marker; closing needs the same character, a run at least as long and nothing after), scanned once per Return up to the lowest caret, without allocating; `* * *`, `- - -` are rules, not items; two carets on one empty item each take the plain newline. Per caret in the multi-caret path, one undo step each. The Markdown check is the language name the app already sets, so no app change.

## Sort lines and the case changes lift the selection once (finch#y35y)

`input/base/transform.rs`. `lift_selection` sets the same one-shot `Lift` a Copy sets (finch#rrdj) over the selections a command leaves; `transform_selection` wraps `apply_line_plan` for Make Upper Case, Make Lower Case, Capitalize and Sort Lines and lifts only when the document revision moved, so a command that changed nothing, a caret alone or a window that isn't key sets none. The lift ends at its length or the next edit.

## Toggle comment, Indent and Outdent lift several rows once (finch#dasa)

`input/base/comment.rs`, `input/editor/indent.rs`. After the edit, Toggle Comment over two or more selected rows and a block Indent or Outdent that edits two or more rows call `lift_selection` (finch#y35y); one row, a plain Tab, a caret alone or a command that changed nothing set none.

## Replace without moving the view (finch#v2s2)

`src/input/editor/search.rs`. `replace_current_search_match` takes `jump`: the find bar's Replace button and Return in the Replace field still scroll to the next match, Edit > Find > Replace does not. The next match is the current one either way.

## Markdown wraps a selection in `*` `_` `` ` `` `~` (finch#2pct)

`input/base/state.rs`. `wrap_selections` also wraps in an editor whose language is `markdown` when the typed character is one of `MARKDOWN_EMPHASIS` (`*`, `_`, `` ` ``, `~`), the same way as brackets and quotes: each non-empty selection becomes `*text*` with the inner text kept selected, as one undo step. They are not auto-closing pairs (the pair lists are unchanged), so a caret without a selection types the character alone, and other languages replace the selection as before. Still gated by `auto_close`.

## Pasting a URL over a Markdown selection makes a link (finch#38f9)

`input/base/state.rs`. `insert_clipboard_text` asks `markdown_link_edits` first: in a `markdown` editor, when the trimmed clipboard is one `http://`, `https://` or `mailto:` address (`markdown_link_url`: text after the scheme, no whitespace, control characters or `<` `>`, balanced parentheses) and every selection is non-empty and on one line, each selection is replaced by `[selection](url)` in one edit batch, so undo gives the words back in one step. Anything else pastes as before.

## Return asks the parse about fences (finch#0nwa)

`input/editor/highlighting.rs`, `input/base/state.rs`; `gpui-component/src/highlighter/input_adapter.rs`. `InputHighlighter::in_fenced_code(text, offset) -> Option<bool>` answers from the highlighter's current tree (`None` when no tree describes `text`). The tree-sitter adapter walks up from the byte before `offset` looking for a `fenced_code_block`. `markdown_list_enter` uses it and keeps the line scan (`markdown_fenced_rows`) only as the fallback, so Return in a long Markdown list reads no lines once the file has parsed.

## The text view reports the link under the pointer (finch#wpcs)

`text/state.rs`, `text/inline.rs`. An `Inline` with links tells the view's state which link the pointer is on when that changes (mouse move, or a paint that finds a link slid under a still pointer): `report_hovered_link` keeps the url, who reported it and since when, and notifies the state only on a change; a "none" from anyone but the reporting `Inline` is ignored, since two paragraphs report in either order as the pointer crosses; new parsed content orphans the owner and the same link reported again by a new owner keeps its `since`. The host reads `TextViewState::hovered_link()` for its status pill. Nothing else changes: no frames at rest.

Amended (finch#wpcs review): the view's paint clears a `hover_seen` flag first; every `Inline` painted with the pointer on a link sets it; after the paint a hovered link nobody set it for is let go and the state notifies once (a scroll or parse that took the link away, a view that stopped painting). An `Inline` also lets go on `MouseExitEvent` (the pointer left the window).

## The preview reads its text in pieces, sums once and places on device pixels (finch#g0v8, finch#z9yw, finch#pt7s)

`text/state.rs`, `text/estimated_scroll.rs`. `TextViewState::set_text_pieces` is `set_text` for text in chunks (a rope's): it compares, appends or replaces from the pieces and writes the state's own buffer once, so the host no longer gathers a second string of the whole text per feed (960 KB per pause on a 1 MB file before, none now). The state counts the documents it has held (`blocks_version`), and `EstimatedScroll::set_weights` reads the block lengths once per document, not once per frame; `refresh` rebuilds the sums only when a weight, a height or the width changed (104 rebuilds over a 300 pt wheel on an 18,000-block file before, 3 now); scrolling over measured blocks costs the few blocks laid out. The state keeps the window's scale factor from its last layout, and `scroll_source_to_top` puts the list's top on a whole device pixel (offsets like 17.79 device px before, 18 now).

## Duplicate, Move, Join, Trim and Convert indentation wash their rows (finch#cdq5)

`input/base/state.rs` (`apply_washed_line_plan`, `WashKind::Command`), `line_ops.rs`, `transform.rs`, `element.rs` (`Wash::follow`). These commands run `apply_line_plan` with a `wash_capture` open and call `end_wash`, so the paste wash plays over the rows they left changed, once, within `WashCapture::MAX_EDITS`. A command that changed nothing washes nothing. A command that edits again while the running wash is under half of `WASH_LENGTH` (a held Move) retargets that wash to the new rows and keeps its clock; past half it starts a fresh one. Sort lines and the case changes still lift the selection instead, never both.

## Find in selection (finch#dqvw)

`input/editor/search.rs`, `input/base/element.rs`. `SearchMatcher` holds an optional `Scope` (sorted, apart ranges and the text they were placed in); `update_matches` moves it through the edit since the last scan (`common_ends`) and keeps only matches wholly inside it, so the count, stepping, ticks and Replace all read the limited list. `set_search_in_selection` takes the selections (a caret alone is an empty scope: nothing matches); `SearchSession::close` and `open_search` with a different selection lift it. `layout_background_segments` adds one faint layer (`SCOPE_FILL` of the match fill) under the selection and the matches, walking only the scope ranges on screen.

## Cmd-click opens a link in the editor (finch#9kz6)

`input/editor/lsp/definitions.rs`. `HoverDefinition` gets an optional `link` target that shares the symbol range, the `link_text` underline and the pointing hand with a Cmd-hovered definition. `link_in_line` finds a Markdown `[text](target)` (balanced parentheses, optional title, `<angle>` target) or a bare `http`, `https`, `mailto` or `file` address at a byte column of one line, and `handle_hover_definition` checks it before the definition provider, so it works with none installed. A left Cmd-click on one opens it: through `EditorState::set_link_opener` when the app set one (a global), else web addresses with `cx.open_url`. Lines over 20,000 bytes are not scanned. Upstream has no link handling in the editor.

## Edit > Markdown wraps through the public edit API (finch#mnw7)

`input/base/markdown_format.rs`, declared in `input/mod.rs` (the file that lists every `base/` module by `#[path]`). `plan` is plain text in, one replacement and a selection out (wrap, unwrap from outside or with the marks in the selection, empty pair, word at the caret); `apply_markdown_format` reads 4 KB either side of the selection, sets the selection to the replaced span, calls `replace` (one undo step) and sets the result selection. No change to `state.rs`.

## A font size change keeps the top line (finch#n21r)

`input/base/element.rs`. `hold_top_line_resized` runs `display_map.set_font` in `prepaint` when the line height differs from the last layout's: it notes the buffer line at the top and how far into its first row the view is, then sets the offset to that line's new row times the new line height plus the same share of a row, through the same glide and deferred-offset paths `hold_top_line` uses. A hidden tab does it on the frame it shows.

## The state's text is sized once and shared with the parser (finch#j45s)

`text/state.rs`. `TextViewState.text` and `UpdateOptions.pending_text` are `Arc<String>`: a full replacement writes its text into one buffer, reserved to the whole length first (`set_text_pieces` sums the pieces' lengths), and the parser's job shares that buffer instead of copying it. Feeding a 1.8 MB rope in 512 chunks: 5 reallocs over 64 KB moving 2.78 MB and two 1.8 MB buffers before, 0 reallocs and one buffer now. `push_str` and a merged append use `Arc::make_mut`, so an append while a big replacement is still parsing copies the text once.

## Every edit in a batch moves the search scope (finch#29ry)

`input/base/state.rs`, `input/editor/search.rs`. `replace_text_in_ranges` maps the search scope through each normalized edit as it applies it, including multi-caret typing, Indent and Toggle comment. Replace all uses that same path instead of mapping the scope a second time. The state test covers two separated scope ranges through these commands and Undo.

## Search takes the changed span from editing (finch#79vv)

`input/base/state.rs`, `input/editor/search.rs`. Batch edits, history replay, single-line edits and IME updates record their applied ranges. `SearchMatcher` composes them in current-text coordinates into one conservative old/new span for the incremental rescan, retaining `common_ends` only when no span was recorded. A mask rewrite forgets the scan. Temporary instrumentation on a 50 MB rope with 500,000 matches counted 97,661 chunk visits per key before and zero after; the counter was removed.

## Scope fill shares its last visible paths (finch#7vee)

`input/base/element.rs`, `input/base/state.rs`, `input/editor/search.rs`. One cache entry per editor holds the shared scope ranges, the last visible layout and its shared path list. A hit compares visible geometry and only the glyph positions at scope edges, catching font or token reshaping even when the total line width stays the same. Paint still resolves the current colours. Closing the scope or releasing a hidden editor's layout drops the entry. Temporary instrumentation counted 60 path-list builds over 60 caret-only redraws before and one after; the counter was removed. The regression checks shared paths across those frames, release/refill and scope-off cleanup.

## Whole-value updates preserve the search scope outside their changed span (finch#29ry)

`input/base/state.rs`, `input/editor/search.rs`. Whole-value replacement reuses `external_text_diff` after normalization, recording only the differing bytes instead of rewriting every scope range into the whole document. Identical values apply no edit. The regression covers unchanged values and a narrow insertion with a multibyte prefix through `set_value`, `replace_all` and accessibility Set value. The obsolete `scope_apply_all` helper is removed; the batch-scope test records each applied edit directly.

## Bracket scan over chunk slices (finch#y28p)

`input/base/brackets.rs`. `scan_window` walks the rope's chunk slices (`Rope::chunk`) for one window of `partner_of` and `enclosing_pair` instead of a per-byte `bytes.next()`/`prev()` iterator; the windows, the `MAX_SCAN` cap, the depth counting and the answers are the same, checked against a copy of the old scan in the tests. No `memchr`: it is not a direct dependency, and the slice loop already runs at about 1.4 GB/s in release (an unmatched `)` after 2 MiB: 0.77 ms against 1.52 ms per byte).

## Hidden tabs are pre-wrapped for a new text size (finch#26f3)

`input/base/element.rs`, `input/base/state.rs`. `prepaint` keeps the font and text size it set the wrap map to (`drawn_font`, compared before it is written, so no allocation per frame). `prelayout` now takes a `DrawnLook` (font, size, line height; `drawn_look` reads it off the showing editor): it applies it with `set_font`, through `hold_top_line_resized` when the line height differs, then wraps as before and records the line height in the layout, so the draw that shows the tab finds nothing to rewrap or re-hold. `drawn_size` is the cheap read the pane's probe compares.

## Auto-closed pairs are forgotten when typed over (finch#w1c4)

`input/editor/auto_close.rs`, `input/base/mode.rs`, `input/base/state.rs`. Skip-over of a generated closer now drops its pair; before, every `()` typed through stayed in the list forever, and the list was cloned per key and twice per undo step. The regression types 2,000 `(x)` runs and asserts fewer than 10 pairs remain. By construction the list is empty after each run instead of growing by one entry (two ranges, 32 bytes) per pair.

The list also keeps only the newest 64 pairs, since pairs the caret simply walks away from are never released. One existing test (`test_multiple_generated_pairs_retain_their_identity`) no longer types `*/` over the generated closer to set up its text; that pair is now released on skip-over, so it starts from the generated `/**/` directly. Its fixture text ends in a newline, which is why length checks see one extra byte.

## The longest line's width survives edits on other rows (finch#kwn8)

`input/base/element.rs`. With soft wrap off, `LongestLineKey` held `document_revision`, so every key missed, copied the longest line to a `String` and shaped it. It now holds a hash of the line's text instead (chunk-independent), next to row, length, font, size, wrap width and tab size. An edit on another row keeps the stored width; an edit on the longest line changes the hash and re-measures. By construction, 200 keys typed on another row shape the longest line 0 times instead of 200; 200 keys on the longest row still shape 200 times. The stored key carries the revision it was hashed at (`state.rs`, `longest_line_width`), so a frame with no edits compares integers and does not hash; an edit hashes the line once, without allocating. The counter test `typing_off_the_longest_row_keeps_its_width` pins 0 shapes over 200 keys on another row, 200 hashes (one per key) and 0 on repaints.

## Hidden editors let go of their Find matches (finch#hbm4)

`release_search_matches` and `restore_search_matches` on `InputBaseState`, and the matcher's `release`, `is_released`, `shown_total`, `shown_current`, `begin_scan` and `adopt_scan`: a tab that stops showing (its fade landed) drops the matcher's list (a 16-byte range per match; about 1.9 million matches, 30 MB, for "e" in a 20 MB English file) while the query, options, Find in selection's scope and current index stay; showing again rescans on a background thread, the bar keeps the old count and match until it lands, and an edit or reload of a hidden tab only marks the matches stale (no full scan).

## The Markdown fence scan reads a window, not the file (finch#170s)

`input/base/state.rs`. `markdown_fenced_rows`, the fallback of `markdown_list_enter` when the highlighter has no current parse (the first background parse of a big file, or a stale tree after an edit to a fence line), read every line from the top to the caret on each Return: 50,001 lines at row 50,000. It now takes the range of caret rows (`enter` computes the topmost and lowest in one loop, a single caret passes its own row twice) and reads from `FENCE_SCAN_WINDOW` (1,000) rows above the topmost to the lowest, taking the window's top as outside a fence, so a single caret reads at most 1,001 lines. The window starts at the topmost caret so that with several carets none loses its fence context (a first version ended at the lowest caret only, and a caret more than 1,000 rows above it continued a list inside a code block). Files under 1,001 rows, and every answer once a parse exists, are unchanged. The one wrong case is a fence that opened more than 1,000 rows above the topmost caret before a parse lands. `test_return_in_a_markdown_list_asks_the_parse_about_fences` counts the lines read; `test_return_scans_fences_above_every_caret` covers two carets 1,100 rows apart.

## Footnote size, pictures' ring, task box edge (finch#f894)

`text/node.rs`. Footnote rows set their text size on the row element (`FOOTNOTE_EM`, 13 of 15; the `with_text_style` around it never reached paragraphs, which shape at layout). Notes stand `FOOTNOTE_GAP` (4 pt) apart, the rule above the first takes `rule_top` and `rule_space` (the horizontal rule's own math, now one helper), and footnote numbers share one column width. Pictures draw a one-device-pixel inside ring in the theme border at `IMAGE_RADIUS`, an absolute overlay, so sizes and fades are as before. A task item's box is exactly the marker column wide, so its text starts on a bullet item's text edge.

## A press inside the selection can drag the selected text (finch#n8rz)

`input/base/state.rs` (`text_drag`, `pending_text_drag`, `on_text_drag`, and `on_mouse_down`, `on_mouse_up`, `on_drag_move`). With a handler set by the app, a plain single left press inside the only selection no longer starts selecting: it waits. Moving more than 2 points calls the handler with the selected text (it starts a GPUI drag through `App::start_active_drag`); releasing without moving places the caret where the press was, as before. A press outside the selection, or with a modifier, or a double or triple click, behaves as before; with no handler set nothing changes.

## An editor can move to another window (finch#0j6e)

`input/base/state.rs`. The key-state, focus and blur subscriptions of `InputBaseState::new_in_mode` live in the window the editor was built in, so they died with it when a tab was dragged out into another window. They are now their own list (`window_watch`, made by `watch_window`), and `rehome(window, cx)` drops the old ones, makes them again in the new window and points the touch handles' `auto_scroll_window` at it. Nothing changes for an editor that stays in its window.

## Typed-edit signal and replace_range (finch#3c3d)

`input/base/state.rs`. `typed_revision` is set where `on_text_typed` runs (a non-silent replace), and `edit_was_typed()` says the latest text change was typing, not Undo, Redo, a paste or a silent replace. `replace_range(range, text, caret, ..)` replaces a byte range as one atomic undo step and records the caret before and after, so Undo and Redo land the caret where it was. Both are new; nothing existing moved.

## A completion provider can skip inline requests and hear which row was taken (finch#z5z0)

`src/input/editor/lsp/completions.rs`, `overlay.rs`. `CompletionProvider` gets two default methods: `provides_inline_completion` (default `true`; `false` makes `schedule_inline_completion` return before it starts its debounce timer, so a provider that only answers on a trigger costs nothing per edit) and `completion_accepted(item, window, cx)`, called at the end of `insert_completion`, so a provider can do more than the text edit (Finch creates the note a `[[` link names). Providers that don't override them behave as before.

## Typing over an auto-closed closer closes the completion menu (finch#z5z0)

`src/input/base/state.rs`, the skip-over path of `replace_text_in_range`: it hides the context menu and clears the inline completion before moving the caret past the closer, so a menu opened for `[[query` can't take a row after `]]` was typed.

## Coming back after a while washes the caret line (finch#q9f6)

`src/input/base/element.rs`, `state.rs`: `Wash` carries its own `length` (default `WASH_LENGTH`) and an `ends_on_scroll` flag; `EditorState::wash_range(range, length, cx)` and `wash_caret_line(length, cx)` start a wash without an edit (kept off when another wash runs), and `on_scroll_wheel` drops a wash that has the flag. Paste, replay, reload, command and far-glide washes keep their length and don't end on scroll.

## Paste text without the clipboard (finch#nbcw)

`src/input/base/state.rs`: `EditorState::paste_text(text, window, cx)` runs `insert_clipboard` on a string, so text the app recognized from a photo goes in as one undo step with the paste wash and the pasteboard untouched. While an IME composition is open it is a plain `insert` after the composition instead, without the wash (finch#282d). Called on one user action only.

## Follow a growing file (finch#pebg)

`src/input/base/state.rs`: `EditorState::append_external_text(tail, window, cx)` puts bytes after the end with no diff, no wash and no `Change` event, and keeps the undo history (an append moves nothing before it). `tailing`, `following`, `follow_end` and `stop_following` carry the follow state: the first append with the caret on the last line starts it, the caret then rides the new end and the next layout glides to it (`begin_typing_glide`). `on_scroll_wheel` toward the start and `scroll_to_with_padding` (a caret move off the last line) let go; a caret move onto the very end follows again.

## Option-click removes a caret, Option-drag sweeps a column (finch#9acp)

`src/input/base/state.rs`, `cursor.rs`: Option-click on an added caret removes it (`Selections::remove_cursor_at`; the active caret and the last one stay); `build_columnar_selection` does nothing while the pointer is where the press was and no block has been built yet (`column_select_moved`), so an Option-click keeps the carets it added; `EditorState::cursor_count()` reads the caret count. `blink_cursor.rs`, `element.rs`: `CaretBirths` (a field of `CaretGlide`, sorted lookup) fades a newly added extra caret in over `CARET_FADE` inside `paint_carets`, only on a pass with more selections than the last (`CursorRenderInfo::total` counts all, in view or not), and asks for frames only while one fades.

## Increment and Decrement actions (finch#0b7x)

`src/input/base/transform.rs`, `state.rs`: new actions `Increment`, `Decrement`, `IncrementByTen`, `DecrementByTen` (Control-Option-Up/Down, with Shift for ten), handlers registered beside the other transformations, planner `plan_step` applied with `apply_washed_line_plan`. In `state.rs` only the action list, the key table (Control-Option-Up/Down were Windows-only Add cursor; Windows and Linux now use Shift-Option-Up/Down for it) and the handler registration changed.

## Compiler and stack-trace locations as Cmd-links (finch#r40n)

`src/input/editor/lsp/definitions.rs`: `location_in_line` finds `path:line[:col]`, `path(line,col)` and Python's `File "x.py", line 9` under the pointer, and `EditorState::set_location_handlers(cx, check, open)` lets the app say whether one leads to a file (asked once per token, after the mouse event, with the last refusal remembered) and open it. `link_at` now takes `cx` and returns a `Link` (target or location); without the handlers nothing changes. Hot path: runs on Cmd-hover only, never per frame.

## The highlighter's parse tree can be read (finch#03xt)

`src/input/editor/highlighting.rs`, `src/input/base/state.rs`: `InputHighlighter::inspect_current_tree(text, f)` (default: false) hands `f` the highlighter's tree as `&dyn Any` when it is current for `text`, and `EditorState::with_current_syntax_tree(f)` wraps it for the app, which downcasts to its parser's tree. Read only: it never parses, so the config checks (JSON, TOML, YAML error nodes, the path trail) cost no second parse.

## History jumps and the Undo history slider (finch#832h)

`src/input/base/undo_manager.rs`, `state.rs`: every undo step has a timestamp, and
`UndoManager::jump` moves any number of steps as one `Replay` (only those steps are cloned).
`replay_history` is split so `apply_replay` takes a replay: one `begin_edit_batch`, one highlighter
batch, one notify and one `Change` however many steps it holds. `history_jump`, `history_stops`,
`history_fold_redo` (moves the waiting redo steps back onto the undo stack under one new step
whose undo redoes them, so nothing is dropped) and `history_drop_redo` are the slider's API.
`HistoryGuard` (`set_history_guard`) lets a session refuse an edit that would drop waiting redo
steps (`edit_blocked`, at the four edit entry points and before `apply_indent`, `toggle_comments` and `apply_markdown_format` plan their selections, finch#vs9a) and turns an Undo while scrubbed back into a
jump to the newest text. Tests: `test_history_*` in `state.rs`.

## Reading column (finch#cbgq)

`set_reading_measure` and `set_reading_measure_wide` (`state.rs`) hold soft-wrapped text to a
number of characters (the advance of "0" in the editor font), centered right of the gutter. The one
wrap-width helper is `InputBaseState::text_column` (`element.rs`); `prelayout` and `prepaint` both
call it. The column's x offset is its own spring (`column_spring_id`, `GUTTER_GLIDE`) and is part
of `LastLayout::line_number_width`, the text origin every hit test, caret and selection already
reads; `LastLayout::column_offset` is the share of it the scrollbar leaves out. `prewrap` is
unchanged (the app never calls it). `set_reading_centered(false)` puts the column at the gutter
(offset 0) instead of centering it; Finch centers only in Minimal mode (finch#cbys).

## Typewriter scrolling (finch#y044)

`src/input/base/state.rs`, `element.rs`: `set_typewriter` holds the caret row of a key or typing move at a band 45% down the viewport (`typewriter_band`). Targets come from `reveal_target` (arrow keys, Page keys, Find, Go to line) and `begin_typing_glide` (edits) and ride the existing `RevealGlide`, so frames stop when it lands. `top_inset` lets the offset go positive by the band height while it is on, so the first lines can sit there: `reveal_row`, `clamp_vertical_scroll_offset` and `update_scroll_offset` take it as their ceiling (0 when off). A press or drag sets `pointer_touched`; `settle_pointer` (prepaint) records the caret the pointer left, and only a key that changes it re-centers. The smooth wheel (`smooth_wheel.rs`, `nudge` and `step`) takes the same top bound, so a wheel move from a lowered page eases from there (other scrollers pass 0). Tests: `typewriter_band_*`, `a_row_inside_the_top_inset_*`, `test_vertical_scroll_offset_*` in `element.rs`.

## Transient line-number hide (finch#m6nn)

`set_line_numbers_hidden` (`state.rs`) hides the gutter's numbers without touching the mode's
`line_number` flag, so `set_line_number` (the setting) and the hide never fight. `element.rs`
reads `shows_line_numbers` (mode flag and not hidden) where it read `mode.line_number()`.

## Text over a selection or find fill keeps full ink inside focus-dimmed text (finch#52yc)

`src/input/base/element.rs`: a run styled with `HighlightStyle::fade_out` (the app's focus dim) records its unfaded alpha (`faded`, passed through `layout_lines`), and `split_runs_by_bg_segments` gives `readable_over` that alpha for pieces over a selection or find fill inside a faded range, so matched text isn't left dim. Runs with no fade are untouched, translucent ones included. Per-line layout path, one binary search per recolored piece.

## Fold engine and keys (finch#15r4)

`fold_map.rs`: `is_fold_candidate`, `is_folded_at` and `set_folded` are binary searches (the lists are sorted by start line, one per line); `fold_all` folds every candidate that hides a line in one pass (no sort per push); `innermost_unfolded_candidate` and `innermost_fold_at` find the range around a line. `display_map.rs` wraps them. `state.rs`: `fold_at_caret`, `unfold_at_caret`, `fold_all`, `unfold_all` (each keeps the top visible line still through `hold_top_line_while`, a copy of `element.rs`'s `hold_top_line` that reads the line height from the last layout), `unfold_line` (shared with `unfold_at`); a search landing and Undo/Redo open the fold hiding their target.

## Reveal an offset without moving the caret (finch#cm4c)

`src/input/base/state.rs`: `reveal_offset(offset, cx)` is a public wrapper of `scroll_to` so the app can keep the word being read aloud in view (`src/speech.rs`) with the editor's own reveal; the caret and selection are untouched. It does nothing when the offset is already in view (so it never eats the user's wheel steps) and a far glide's arrival does not wash the caret's line (`reveal_far` cleared).

## Fold arrows, chip and glide (finch#15r4)

`src/input/base/element.rs` (`layout_fold_icons`, `paint_fold_icons`, `FoldMotion`), `state.rs`
(`fold_motion`): an open block's arrow fades with the pointer over the gutter (no longer shown on
the current line), a folded line ends in a `...` chip that opens it on click, and a block that
toggles draws under a ground-colored curtain for 0.2 s. Frames come from the element's own
`request_animation_frame` on the frame time while a fade or glide runs, none at rest.

`fold_map.rs`, `display_map.rs` (finch#15r4): `FoldMap::generation` (`DisplayMap::fold_generation`) is bumped wherever `folded` changes, so the fold gutter reads the list only when it moved.

## Change bars geometry (finch#d1c9)

`state.rs`: `gutter_rows` hands a layer drawn over the editor (`src/change_bars.rs`) the shown buffer lines' tops and heights from the last layout, the gutter's left edge, where the text starts and the line height. It reads the recorded layout and allocates nothing; `element.rs` is untouched.

## Pinned scope header reads (finch#zkqh)

`fold_map.rs` counts `candidates_generation` (the candidate list changed; bumped by every candidate mutator, never by a rebuild or a font set, which run each prepaint). With `fold_generation` it is the key the app's pinned scope header caches its scopes by. `display_map.rs` and `state.rs` expose read-only `fold_candidates`, `candidates_generation`, `fold_generations`, `display_row_count`, `buffer_line_at_display_row`, `display_rows_of_line` and `gutter_width`; `glide_to_line` puts the caret on a line and reveals it like a Find jump (surrounding-lines clearance, glide), for a click on a pinned line. Nothing here runs in the editor's own frame.

`input/base/state.rs`, `input/base/element.rs`, `input/editor/mod.rs` (finch#9s91): a patch (Diff language) look the host feeds with `set_patch_rows(revision, rows, digits, icon)`, one `(kind, old, new)` per buffer line. The gutter widens to two number columns while rows are set (`layout_line_numbers`); `prepaint` shapes the old-side numbers and prepaints the host's file icon on each visible `PATCH_FILE` row; `paint` fills added, removed, header and hunk rows across the whole row (a plain quad before the text, and again over the gutter's ground), so no text run changes. Rows paint only while their revision is the text's own. The caret layer's current-line figure is skipped in patch mode (the editor paints both columns). `handle_patch_click` runs before `handle_click_hover_definition` on Cmd-click and hands the buffer line to the host's `set_patch_open`. Other editors take none of these paths.

## IME composition and app edits (finch#dgc2, finch#282d)

`undo_manager.rs`: `commit_transaction` returns whether it recorded an undo step. `state.rs`: `unmark_text` and the empty-text cancel in `replace_and_mark_text_in_range` send `InputEvent::Change` when it did, so a composition that ends by changing the document marks the tab edited. An edit outside the marked range keeps the mark and shifts it (`adjust_marked_range`, run per edit in `replace_text_in_ranges` and in the single-edit path); only an edit that touches it ends the composition, and `replace_text_in_range` ends the transaction only then. `insert` and `replace` move a caret inside the mark to its end and name the selection, so they land after the composition; `paste_text` is a plain `insert` during one. `apply_external_text` drops the mark before a reload, and a preedit whose transaction was closed opens a new one.

## Rainbow brackets and semantic variable colors (finch#3sgr, finch#tm6q)

`input/base/state.rs`: `set_rainbow_brackets(Option<Rc<[Hsla]>>, cx)` and `set_semantic_palette(Option<Rc<[Hsla]>>, cx)` (`None` is off; an equal palette does nothing; a change drops the highlight cache once and notifies). `input/editor/highlighting.rs`: two `InputHighlighter` methods with no-op defaults, `bracket_depths(range, out) -> bool` (code brackets `()[]{}` as `(offset, depth)`) and `styles_with_variables(range, resolver, variables)` (`styles` plus the variable and parameter identifiers as `(range, stable name hash)` from the same query pass), `palette_overlay` (`palette[depth % len]`, `palette[hash % len]`) and `apply_overlay`, which sets the palette color on the highlighter's runs explicitly, splitting them at the overlay's edges (the palette must beat the theme's color; `combine_highlights` folds overlapping styles in hash order and would not). `input/base/element.rs` (`highlight_lines`): the overlay is applied inside the cache refill, next to the query, so a frame served from the cache does nothing new and only the visible line groups are asked. Both palettes off adds no overlay, so the runs are unchanged. The tree-sitter side is in gpui-component's PATCHED.md.

## Find halo (finch#5ncd)

`input/base/element.rs`: `Halo` and `step_halo` (a ring that rises over 0.12 s and settles over 0.22 s on `ease_out`, clock from the first frame that draws it, dropped on an edit). It is painted as one rounded quad per row under the current match's path. `input/base/state.rs`: `halo` field and `start_search_halo`, which hands the level drawn last to the new halo so a second jump rises from there. `input/editor/search.rs`: `next_search_match` and `previous_search_match` start it; the live search as you type, Replace and Select all matches never call them.

## A pick washes where it lands, Escape glides back (finch#yj50, finch#vt49)

`input/base/state.rs`: `land_far_glide` now shares `wash_caret_row_once` with three public methods for the app. `wash_landing(cx)` washes the caret's row once after a jump the app commits: with a glide running it sets `reveal_far` so the arrival washes, otherwise it washes at once (a near jump, which `aim_reveal_glide` never marks). `forgo_landing_wash()` clears `reveal_far` for a live preview or a restore. `glide_scroll_offset(offset, cx)` sets the deferred offset and aims the reveal glide at its y (a retarget keeps the drawn offset and speed), with no wash, so Escape in Go to symbol glides a near scroll back instead of snapping through `set_scroll_offset`. The far restore is unchanged.

## Fold glide turns around (finch#dqc2)

`input/base/element.rs`: `FoldGlide` carries `from`, how much of the block its curtain covered when it began, and `covered(now)` eases that to 0 on `ease_out`. A fold toggled again while it glides (same line, opposite way) starts from the cover drawn instead of restarting at full, so the curtain never jumps. The curtain is read from `covered` in `layout_fold_icons`.

## Jumps unfold through the fold glide (finch#048c)

`input/base/element.rs`: `FoldMotion::toggled` reads one or more folds fewer as an opening when the folds left are a subset of those seen and each one that opened nests in the one opened before it (a chain, outermost first). A jump into code under nested folds (`unfold_line` opens every fold above the line in one generation) glides as that outer fold, over all its rows, as a click does. Fold-all, unfold-all and folds shifted by an edit still don't glide.

## A followed log's first appended lines wash after a quiet spell (finch#3ckk)

`input/base/state.rs`: `append_external_text` ran no wash at all, so a line landing after a pause arrived with no cue. It now shares `apply_external_text`'s quiet clock (`last_external_text`, `EXTERNAL_WASH_QUIET`, 2 s): an append that comes at least that long after the last external change records its rows in `wash_capture` and ends with `end_wash(Reload)`, one wash over the new rows. Every append restarts the clock, so a stream of lines never washes.

## Insert line below and above bind in the code editor (finch#4tke)

`input/base/state.rs`: `LINE_EDIT_CONTEXT_PREDICATE` is `Input && (line_edit || mode == editor)`. The code editor's element sets the key context `Input mode=editor`, which replaces `line_edit`, so Cmd-Return and Shift-Cmd-Return (InsertLineBelow, InsertLineAbove) never matched there; Cmd-Return fell through to plain Return and Shift-Cmd-Return had no binding. Single-line fields (`Input` alone) are unchanged.

## Strikethrough in Edit > Markdown (finch#x52h)

`input/base/markdown_format.rs`: `MarkdownFormat::Strike` wraps in `~~` like Bold: `mark` gives `~~`, and `wrapped` takes the pair off when two or more `~` stand either side of the span or are the span's own first and last two characters (so a selection that holds the marks unwraps, and an empty pair goes). It stacks with the other marks and runs through the same one-undo-step `apply_markdown_format`. Bold, Italic, Code and Link are untouched.

## A note link made from the list washes in once (finch#0ghf)

`src/input/editor/lsp/completions.rs`: `CompletionProvider::washes_accepted_text` (default `false`). `src/input/editor/lsp/overlay.rs`: `insert_completion`, after the replace, calls `wash_range` over the inserted text (0.3 s) when the provider says so, before `completion_accepted`. The `[[` provider answers `true`, so the link that swapped in for `[[query]]` is washed once; no other provider's rows wash. The wash ends on typing, an edit or a scroll like any other, and `wash_range` keeps a wash that is already running.

## Find scrolls sideways to a match on the rows' spring (finch#6p4j)

`input/base/state.rs`, `input/base/element.rs`: a search reveal now eases x on a second `RevealGlide` (`reveal_glide_x`, drawn as `glide_x` over the layout's x, cleared after paint) stepped with the rows' glide in `step_reveal`, same spring and same cut (one width short for a start farther than the view is wide), retargeted from the drawn x and speed by a second find. Before, x was set at once for a row already laid out, and a match on a row not yet laid out (a far row) never moved x at all, since its position was unknown. `reveal_x` is the old x rule factored out; for such a row `reveal_x_pending` holds the offset until a frame of the y glide shows the row, then the x glide starts. Arrow-key reveals still land at once; wrap on has no x to move.

## Undo history counts without the step times (finch#8rg8)

`input/base/state.rs`: `EditorState::history_counts()` returns `(applied, waiting)` straight from the undo manager's two lengths (O(1), no allocation). `history_stops()` builds a `Vec<Instant>` of every step on each call, so a caller that only counts or tests for emptiness paid one allocation per step per call. The app's count-only callers (the document's change handler, Edit > Undo history's grey, the colour picker's session, the slider's Keep and its drop prompt) use `history_counts()` or `history_waiting()`; the slider's own list still uses `history_stops()`.

## A wide preview table shows a knob and edge fades (finch#68ct)

`src/scrollable_mask.rs`: `horizontal_scroll_area` takes the table's ground, its header row's ground and height (set by `render_table_rows` from the header row's laid-out cells, so the top band fades into the header fill) and ink, and its `ScrollableMask` paints a `ScrollDecor` over the viewport: left and right edge fades (`ground`, 24 pt, strength by how far the table still scrolls that way, inside the frame border, under the rounded-corner cover) and a thin bottom knob that eases up when a horizontal wheel step moves the mask (`KnobClock`, on the frame clock `motion::frame_time`, ease `Easing::EaseOut`), holds, then fades out on smoothstep. The hold asks for no frames: one timer wake at its end, then frames only for the fade. Values copy the app's `src/scrollbar.rs` tokens (`SCROLL_KNOB`, `SCROLL_INSET`, `SCROLL_KNOB_MIN`, `KNOB_ALPHA`, `HOVER_IN`, `SCROLL_IDLE`, `SCROLL_FADE_OUT`, `LIST_EDGE_FADE`), each named at its constant, since the text code can't reach app tokens. No hover or drag on the knob. `src/text/node.rs` `render_table` passes the table background and foreground. Vertical wheel still bubbles to the preview.

## Find on a selection with Regex on finds that text (finch#bqz5)

`input/editor/search.rs`: `SearchSession::seeded` gives the query a selection seeds. Under the Regex option it is `regex::escape(selection)`, so Cmd-F and Cmd-E find exactly what was selected (`a.b` no longer matches `axb`, `(x)` no longer fails to compile). The find pasteboard still gets the selection as selected; `set_find_query` takes the text to share beside the query. The escaped query goes through `update_pattern` like any regex, so a selection past the regex size limit reads as an invalid pattern, never a panic. Another app's find text and a retained query are not escaped.

## A regex Replace reads \n, \t and \\ (finch#zj2a)

`input/editor/search.rs`: `SearchMatcher::unescaped` runs first in `replacement_for` and `replacements_for` when the Regex option is on: `\n`, `\t` and `\\` become a newline, a tab and one backslash, left to right (`\\n` is a backslash then `n`), and any other backslash stays. It runs before `$1` and `${name}` expand, so text a group brings in is never read as an escape. A plain Replace is literal as before.

## Typing in the find field narrows the matches instead of scanning the file (finch#jwy0)

`input/editor/search.rs`: `SearchMatcher` keeps `found` (the query, case mode and text its matches answer) while they hold every occurrence of a plain literal: no regex, no whole word, no Find in selection, and a query no occurrence of which overlaps another (`borderless`). A new query that extends it, in the same case mode, on the same text (`common_ends` compares chunks by pointer), goes through `narrow`: each old match that still matches at its start with the longer length is kept, reading one chunk per cluster of matches. Both queries borderless means every occurrence of the longer one starts at an old match, so the pick is what a scan finds. A regex, whole word, a case flip (smartcase), Find in selection, an overlapping query such as `retur`, or any edit scans in full as before; finch#fnmr's rescan near an edit is untouched. 100k lines, typing `buffer`: 6 full scans and 25248 chunk visits before, 1 scan and 12985 visits after.

## Find picks the current match by binary search (finch#76p1)

`input/editor/search.rs`: `update_cursor_by_offset` (the current match a new query starts from) walked every match from the first; it now `partition_point`s the sorted, non-overlapping ranges for the first one that ends at or after the offset, the last one when none does, the same pick as the walk.

## Typing in Find scrolls to the first match (finch#7p67)

`input/editor/search.rs`: `set_search_options`, when the query or an option changed and the bar is open, aims the search reveal (`scroll_to_with_padding` with surrounding lines, the one Find next uses, so a jump over a screen glides) at the current match the count names. The caret and selection stay where they are; Return steps from that match and Escape selects it where the view already is. With the bar closed (the Match case and Whole word menu keys) nothing scrolls.

## open_search_with (finch#47k7)

`input/editor/search.rs`: `open_search_with(replace_mode, from_selection, cx)` is `open_search` that reads the selection as empty when `from_selection` is false (no query from it, no scope change, selection and cursors untouched). `open_search` calls it with `true`.

## The find pasteboard keeps the selection as selected (finch#bqz5, review fix)

`input/editor/search.rs`: the find bar echoes a seeded selection back through `set_search_options` escaped under Regex; that echo no longer writes the escaped text over the pasteboard's raw selection (it is recognised as `regex::escape` of the text last shared). `narrow` also trims its result with `shrink_to_fit`, like `find_all`.

## Typing's reveal skips a match in view (finch#7p67, review fix)

`input/base/state.rs`: `offset_shown(offset)` is true when the offset's row is wholly in view (judged from a running glide's target, edge padding not counted) and not hidden in a fold. `input/editor/search.rs`: `reveal_search_match` (called from `set_search_options`, `set_search_regex` and `set_search_in_selection`, open bar only) reveals the current match only when it is not shown, and lets go of a followed file's end unless the match is on the last line. The pasteboard echo check `is_escaped` compares without allocating.

`input/base/state.rs` (finch#7p67, recheck): `reveal_target` takes `hold`, whether the typewriter holds the row at its band. The reveals pass `typewriter_driven()` as before; `offset_shown` passes `false`, so with typewriter on a match in view still reads as shown.

## Skip and select next occurrence (finch#11ej)

`input/editor/search.rs`: `skip_next_occurrence` and `can_skip_next_occurrence`. Cmd-D's `select_next_occurrence` and the skip share `add_occurrence`; the skip finds the next occurrence with the newest pick still selected (so the search starts after it), then drops that pick (the last selection) before adding the new one. With only a caret it acts as Cmd-D, and with nothing left to add it changes nothing.

## A bad regex says why (finch#f3s5)

`input/editor/search.rs`: `SearchMatcher` keeps what is wrong with a pattern that does not compile as a short plain phrase (`problem()`, from `regex_syntax`'s parse error kind; `invalid()` still reads true). The find bar shows it as the count's tip.

## A decoding picture shows the plate fill (finch#1ex6)

`text/node.rs`: `image_element` gives the image frame the plate fill (`PLATE_FILL`, at the image radius) while the picture is still decoding, and once ready the fill is `plate_alpha(fade)`, `PLATE_FILL * (1 - fade_in)`, so it leaves with the 150 ms fade. No animation of its own; the fade already asks for frames.

## TextView on_copied (finch#3z0g)

`text/text_view.rs`: `TextView::on_copied(handler)` runs after Cmd-C wrote a non-empty selection to the pasteboard (an empty selection still propagates, no call), so the host can show a sign of the copy.

## A sideways scrollbar drag ends Find's wait for a far row's x (finch#7316)

`input/base/state.rs`: `reveal_x_seen` holds the x the pending reveal last saw shown (or drew, while an x glide runs). `step_reveal` ends `reveal_x_pending` when the handle's x differs from it by more than the rest distance, as the wheel already does, so a drag during the wait holds after the row shows.

## Markdown splice keeps going past link definitions (finch#xrry)

`src/text/format/markdown.rs`: `parse_edit` no longer returns `None` for a whole text that holds `]:` anywhere. `decides_references` falls back only when a slice holds `]:` or `[^`, or the document has definitions and a slice holds `[` (a possible reference use). The neighbour equality checks are unchanged.

## Markdown splice goes through beside fences, rules, tags and front matter (finch#agjr)

`src/text/format/markdown.rs`: the constructs that reach past a block (`may_reach_past_its_block`) and the reference test are looked for only in the lines of the blocks the edit touches, in both texts, not in the neighbour blocks parsed again as a check, which must still come out equal. A slice that starts on `---` or `+++` below the document start still falls back, since the slice parse would read it as front matter.

## A drag right after Find's jump also ends the wait (finch#ekg0)

`input/base/state.rs`: `scroll_to_with_padding` seeds `reveal_x_seen` with the handle's x when a far-row wait begins, so a sideways scrollbar drag before the first `step_reveal` (when nothing had been seen yet) ends the wait too.

The same drag also sets the jump's `deferred_scroll_offset.x` to the dragged x in `step_reveal`, so the next layout does not snap back to the pre-jump x (finch#ekg0).

## Markdown splice keeps going in a file with footnotes (finch#jcd0)

`src/text/format/markdown.rs`: the notes section (`BlockNode::Footnote`, no span) used to make `parse_edit` return `None` for any document with a footnote definition. `parse_edit` now splits the old blocks into those with a source and the trailing notes, splices the former, drops the slice's own notes section, and keeps the old notes with their first-reference position and blocks moved by the edit's length change (`moved_note`; a position the edit covers parses in full). A footnote label (`[^`) in the lines the edit touches still parses in full, since numbers and notes could change; so does a neighbour holding a footnote reference, whose number the slice cannot know, as it parses to something other than the old block.

## Markdown splice resolves reference links beside the edit (finch#efq2)

`src/text/format/markdown.rs`: a slice holding a `[` is parsed with the document's other top-level link definitions after it (`definitions_outside`), and the blocks they make are dropped, so a neighbour or touched paragraph with `[text][label]` resolves as in a full parse. `decides_references` no longer falls back on a `[` alone, only on `]:` or `[^` in the touched lines. The definitions are handed over only when every `]:` of the document sits in a top-level definition block (a footnote definition only at its label), and no label repeats; otherwise the parse is in full. Cost: a document with thousands of definitions parses them with each slice that holds a `[`.

## Reverse lines (finch#qgsx)

`input/base/transform.rs`, `state.rs`: `ReverseLines` action and `plan_reverse_lines`. Sort lines' body became `plan_reorder_lines(rope, selections, reorder)`, which both use: the touched lines' text is permuted and the line endings stay where they are.

## Remove duplicate lines (finch#34vh)

`input/base/transform.rs`, `state.rs`: `RemoveDuplicateLines` action and `plan_remove_duplicate_lines`, on a shared `plan_remove_rows(rope, selections, pick)` that deletes whole lines (touched lines, or every line with no selection; a deleted last line without a terminator takes the one before it). Each run of touched lines is checked on its own, so a copy outside a selection is not a repeat of one inside it.

## Sort lines descending and ignoring case (finch#smg4)

`input/base/transform.rs`, `state.rs`: `plan_sort_lines` takes a `SortMode` (`Ascending`, `Descending`, `IgnoringCase`); `SortLinesDescending` and `SortLinesIgnoringCase` actions. Ignoring case sorts by the lower-cased text with a stable sort, so lines that differ only in case keep their order and a second sort changes nothing.

## Remove blank lines (finch#gvey)

`input/base/transform.rs`, `state.rs`: `RemoveBlankLines` action and `plan_remove_blank_lines`, on `plan_remove_rows`: a line goes when every character is whitespace (an empty line too).

## Right-click the gutter (finch#27h6)

`input/base/state.rs`: `on_gutter_menu` sets a host handler (`gutter_menu_handler`). `input/base/element.rs`: at the end of the paint, a bubble-phase right-press listener over a prepainted hitbox (`gutter_menu_hitbox`, so a float above the editor keeps its clicks) on the gutter (`gutter_menu_bounds`: the fold gutter's bounds, at least `GUTTER_MENU_EDGE` wide so a gutter with nothing left still takes it) calls `prevent_default` and defers the handler with the window point, so the text's own press (caret move, context menu) is skipped. Only for code editors (line numbers shown or hidden) and only when a handler is set.

## Cut and Copy with no selection take the whole line (finch#q3bc)

`input/base/state.rs`: `caret_line` gives, in a multi-line unmasked input with one empty selection, the caret's line (`TextSelector::line_range`, newline included); the last line gets a `\n` on the clipboard and Cut takes the newline (and `\r`) before it. `copy` and `cut` put it on the clipboard with `WHOLE_LINE` metadata (no lift) before their `is_copyable` check; Cut removes it as one atomic edit. `insert_clipboard_text` pastes text carrying that metadata and ending in `\n` at the start of the caret's line when there is one empty selection, moving the caret by its length. Several carets or any selection paste as before. The context menu's Cut and Copy stay grey with no selection.

## Hard wrap (finch#88n1)

`input/base/transform.rs`, `state.rs`: `HardWrap` action and `plan_hard_wrap`, which refills the paragraphs among the touched lines (or the caret's paragraph) to `HARD_WRAP_COLUMN` (80). A paragraph is a run of non-blank lines with the same indent and mark (`wrap_prefix`: `//`, `///`, `//!`, `--`, a run of `#` or `>`); every new line repeats the first line's prefix and the line break of the paragraph's first line.

## Hidden tabs skip a re-wrap they don't need (finch#8dev)

`input/base/element.rs`: `InputBaseState::prelayout` returns early when the wrap width, font, size and line height are unchanged and no rows are pending.

## Re-wrap finds stale rows through the tree (finch#gbyt)

`input/editor/display_map/text_wrapper.rs`: `LineSummary` gains `max_need` (4 bytes per line), and `rewrap_for_width` finds stale rows with a filter cursor over it instead of iterating every line; the unused `LineItem::fits` is gone.

## Convert case, Surround selection and Remove surrounding (finch#07nz, finch#gx14)

`input/base/transform.rs`, `state.rs`: Convert case actions (`ConvertToTitleCase`, `ConvertToSnakeCase`, `ConvertToCamelCase`, `ConvertToPascalCase`, `ConvertToKebabCase`) share `plan_case` through a word splitter that keeps combining marks with their letter and puts the caret on a character boundary. `SurroundWith{Parentheses,Brackets,Braces,DoubleQuotes,SingleQuotes,Backticks}` and `RemoveSurrounding` use `plan_surround` and `plan_unsurround` through `transform_selection`, so each is one undo step with the selection lifted. `input/base/brackets.rs`: `surrounding_pair` and `InputBaseState::surrounding_bracket_pair` take the selection's own first and last characters when `partner_of` pairs them, else `enclosing_pair` (strings and comments skipped, 1 MB scan cap).

## Preview mark fill, heading code size, card insets (finch#8fvc, finch#8r7z, finch#c38t)

`text/style.rs`, `text/node.rs`, `text/html.rs`, `text/markdown.rs`: `TextViewStyle::with_mark_fill` and `with_inline_code_scale`; `TextMark::themed_highlight` and `highlight_themed` (a `<mark>` with no colour of its own takes the style's fill); `default_mark_color` is `pub(crate)`; the missing-picture plate takes `CARD_INSET` and a hairline edge.

## Reading column centres on a window resize (finch#4sya)

`input/base/element.rs`, `layout.rs`: `LastLayout` gains `viewport_width`; the reading column's centring spring adopts its target on a frame where the window's width changed since the last layout (slides and setting changes still glide), and `drawn_column()` returns the last drawn text start, column offset and wrap width for the rects series.

## Callout and link underline values from the style (finch#m8jk)

`text/style.rs`, `text/node.rs`: `TextViewStyle::with_callout_fill(dark, light)`, `with_callout_bar(width, alpha)`, `with_callout_radius` and `with_link_underline(thickness, alpha)` with their accessors; the callout block and `link_underline` read them instead of literals, with defaults equal to the old literals (0.09/0.13, 2 pt at 0.85, 6 pt, 1 pt at 0.4).

## Re-indent from the syntax tree (finch#5sxy)

`input/base/reindent.rs`, `state.rs`: `Reindent` action (Control-I on macOS) and `plan_reindent`, applied through `transform_selection`. Each touched row sits one indent under the row that opened its innermost bracket (brackets from `InputHighlighter::bracket_depths`, a closer kept only when its depth matches the last open opener, so a `${`'s `}` is dropped); rows starting with closers take that opener row's indent; a row whose first character sits in a node that starts on an earlier row inside the same bracket (`syntax_ancestor`, below the root) keeps its offset under that row. Rows starting inside a string open across the line break, or inside a comment begun earlier, are left alone. `paste_indent.rs`: `paste_reindents` is `pub(super)` and gates it too; `transform.rs`: `transform_selection` is `pub(super)`.

## A save's tidy-up washes the rows it changed (finch#9caw)

`input/base/save_normalize.rs`, `state.rs`: `normalize_for_save` runs its plan through `apply_washed_line_plan_in` (the command wash, finch#cdq5) and returns the rows it changed (`changed_rows`, rows of the text after the edits); a save that changes nothing washes nothing and returns none. The bytes written are unchanged.

## Use these and a version restore replace in place (finch#et1k)

`input/base/state.rs`: `replace_all_in_place(text, window, cx)` replaces only the span that differs (through `external_text_diff`, as `replace_text` did), as one undo step, keeps the caret's line and column (clamped to the new text, counted on the rope slice with no line copy) and washes the changed rows. Extra cursors are dropped as `reset_selection` did. The app's Compare Use these and Browse versions restore call it.

## A code block's label keeps clear of its first line (finch#ejre, finch#tjt4)

`text/node.rs` (`CodeBlock::render`, `code_label_room`, `CODE_ACTIONS_SIZE`): a block with a language label pads its right side by the label plate's room (corner inset, the measured language name through the line layout cache, two label pads, the corner gap) plus the caller's actions square and gap when `code_block_actions` is set, at least `CARD_INSET`, so a wrapped line ends before the plate. The plate's place and baseline are unchanged. The table width tests expect the 28 px cell padding (finch#821c).

## Find can ignore diacritics (finch#11qg)

`src/input/editor/search.rs`: `SearchSession::{set_,}ignore_diacritics` and `InputBaseState::set_search_ignore_diacritics`. With it on, `SearchMatcher` rebuilds a plain query (up to `FOLDED_LETTERS`, 400 letters; the regex build fails past about 500) as a regex over each letter's accented forms (table `ACCENTED`, Latin, Greek and Cyrillic) followed by `(?-i:\p{Mn})*`, kept out of case folding since U+0345 folds to iota. A `Fold` (accent-free key plus an anchored regex) lets `narrow` keep a growing query's earlier match starts, reading a window that ends before the first character past the key's letters that isn't a mark. Regex mode ignores the option. `is_word_char` counts combining marks, so whole word agrees with `\b` on decomposed text.

## A reload's span for the preview wash (finch#re8g)

`input/base/state.rs`: `EditorState::external_wash_span()` is the new-text byte span of the last `apply_external_text` or `append_external_text` that washed, `None` otherwise (field `external_wash`). `text/state.rs`: `TextViewState::blocks_in_source(range)` returns the list items whose source spans meet a byte range, and `parse_settled()` says the latest text is parsed.

## Syntax colours ease in after a language change (finch#py25)

`src/input/base/state.rs` (`syntax_mix`, `fade_syntax`; `set_highlighter` disarms) and `src/input/base/element.rs` (`SyntaxMix`, `highlight_lines`, prepaint): the app arms a per-editor mix after a save or rename really changes the language. While armed, the copy of the cached highlight styles drawn each frame has its colours mixed (`mix_fill`) from the plain text colour toward the highlighter's by the eased level; the cache itself keeps the true colours, so it needs no key for the mix and a frame with no fade only does the clone. The clock starts on the first frame the highlighter has colours (the parse lands after the swap), runs the app's length on its ease, then disarms; frames are requested only while it runs. A first parse that colours nothing on screen (plain Markdown paragraphs) disarms it, so it can't fire later while typing. An empty text or Reduce Motion disarms it, so the colours cut in. Once the fade ends the styles are identical to before.

## Wash probe for the rects harness (finch#kk38)

`src/input/base/element.rs`, `src/input/mod.rs`: `set_wash_probe(fn(&Window, &[Bounds<Pixels>], f32))` stores a callback in a `WASH_PROBE` `OnceLock`; the line-wash paint calls it with each wash path's bounds and the drawn alpha, so `debug rects` can list `wash:<n>` (save, reload and restore washes) with their fade. The app sets it only when `GPUI_EDITOR_RECTS=1`; otherwise a painting wash costs one `OnceLock::get` and no allocation. App-only harness, no behaviour change.
