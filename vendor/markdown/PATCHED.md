---
type: implementation
summary: Reduce Markdown parser event storage and edit lookup costs without changing parsed output.
---

# Local patch to markdown 1.0.0

Upstream: <https://github.com/wooorm/markdown-rs>, crates.io `markdown` 1.0.0,
MIT license. The preceding vendor commit contains its published source unchanged.
The root manifest selects this copy with `[patch.crates-io]`.

## Event edit lookup (gpui-editor#3171)

`util/edit_map.rs` used a vector and scanned every earlier edit when adding an
edit at an event position. GFM table resolution produces many edits. With a
large document this search dominated parsing even though rendering was already
virtualized: a five-second sample attributed 3,529 top-of-stack samples to
`edit_map::add_impl`, under `gfm_table::resolve` / `flush_cell`. The UI thread
mostly waited for events. Temporary stage timings on the 5 MB fixture were
28.240 s for `to_mdast` and 28.527 s through preview node construction.

Use a `BTreeMap` keyed by event position, making lookup logarithmic. Keep the
original before/after insertion ordering within each position, accumulated
removal counts, ascending jump calculation, descending event splicing, and
link-shifting logic. No parsing options, syntax rules, preview styling, scroll
state, typing debounce, hidden-panel behavior, or image handling changed.
The regression test covers unsorted positions, repeated edits at one position,
`add_before`, removal accumulation, empty edits, and map reuse after consumption.

## Visible-preview measurements

arm64 Mac, background launch, dark theme, explorer hidden, 900 x 700 pt window.
Fixtures concatenate the project's real Markdown documentation and README files,
then repeat and cut at newline boundaries: 1,048,522 / 2,621,230 / 5,242,799 bytes.
Each run starts with the preview hidden, then enables it. Time is measured from
the toggle request until a captured frame contains preview text, with roughly
0.4 to 1.0 s capture/poll resolution. Footprints are `footprint -p` historical
`phys_footprint_peak`, sampled during parsing (and after first draw for fixed).
The Mac was shared with other work, so these are individual runs, not medians.

| Profile and state | Size | First draw, before | First draw, fixed | Peak MB, before | Peak MB, fixed |
| --- | --- | ---: | ---: | ---: | ---: |
| fast, visible | 1 MB | 1.794 s | 0.509 s | 372 | 357 |
| fast, visible | 2.5 MB | 5.757 s | 1.040 s | 556 | 531 |
| fast, visible | 5 MB | 34.542 s | 2.037 s | 891 | 830 |

The untouched release build, measured once before the fix, took 1.773 / 6.632 /
26.368 s with peak footprints of 369 / 565 / 910 MB at the same three sizes.
There is no fixed-release measurement. The fast result changes roughly
quadratic growth into roughly linear growth; it does not eliminate the parser's
substantial linear memory use.

## Output checks

- The 1 MB fixture's complete GFM-plus-math mdast debug output has identical
  SHA-256 before and after:
  `1fca6926fa1bd0737970a3573347dae60f962d0cadfec23d972c257e9426fe0e`.
- `docs/assets/3171-before.png` and `3171-after.png` show the same 5 MB preview
  at its top scroll position. The preview region has zero differing pixels.
  The only whole-window difference is the editor's blinking caret.
- The targeted `util::edit_map::tests` regression passes. It was compiled with
  `rustc --test` against the app's existing `unicode-id` artifact, avoiding
  unrelated upstream benchmark/dev dependencies. The app's fast build passes;
  pre-existing image-match, unused-import, and impl-trait warnings remain.

## Parse memory (finch#g34j)

A 5 MB document peaked near 810 MB in the app while parsing, of which the
parser's own transient copies were the largest part. Three changes, none of
which alters the event list or the mdast (debug-output hash identical on the
5 MB fixture and on `docs/guide/index.md`):

- `EditMap::consume` rebuilt the event vector from `split_off` pieces, holding
  two copies of a multi-million-event list. It now moves the stretches between
  edits inside the one vector (left movers front to back, then right movers
  back to front). `Event` and its parts derive `Copy` for `copy_within`.
- `divide_events` peeled the child's slices with `split_off` but kept the
  child's full capacity until the tokenizer dropped. It shrinks the buffer
  when more than half is unused (geometric, so copying stays linear).
- `Link::previous`/`next` were `Option<usize>` (16 bytes each, `Event` 80
  bytes); they are `Option<NonZeroU32>` holding index plus one, read and
  written through `Link::previous()`, `next()`, `set_previous()`, `set_next()`
  (`Event` is 48 bytes). A document over 4 billion events panics in `pack`.

Standalone `to_mdast` on the 5 MB fixture (repeated project docs, 5.2 MB),
fast-profile deps, `MallocSpaceEfficient=1` as the app runs, `/usr/bin/time -l`:
peak footprint 330 MB to 167 MB, max RSS 667 MB to 300 MB, 0.85 to 1.06 s
down to about 0.6 s. With the first two changes only: 267 MB. In the app (fast
build, preview toggled on that file) the first two changes read 815 and 811 MB
peak before and 758 MB after; the third change was not launched in the app.
`util::edit_map::tests` (including a plain-rebuild comparison) pass.

## Compact event coordinates (finch#b6zb)

Events retain `EventPoint`, with `u32` line and column coordinates.
Byte offsets and virtual steps remain `usize`, avoiding casts on every slice
lookup and keeping byte-offset range unchanged. Tokenizer cursors remain
native-width: narrowing happens only when retaining an event, and widening
only when starting a child tokenizer or exposing a public unist position.
On arm64, an event occupies 40 bytes instead of 48, a sixth less storage in
every event vector and edit buffer, without a side table or allocation.

Before tokenizing, the parser rejects input over `(u32::MAX - 1) / TAB_SIZE`
bytes with a `document-too-large` Message ("document is too large to parse").
Even an input entirely made of tabs cannot overflow the compact visual column
under that bound. Packing uses a debug assertion as an internal invariant,
not a release-mode panic on user input. Tab expansion, BOM/CRLF handling,
byte-based Unicode coordinates, virtual spaces and public `unist::Point`
fields otherwise keep their existing semantics. The cursor's arithmetic and
source offsets are not narrowed.

`examples/parse_memory.rs` is a standalone parse-only measurement harness:
compile it against the parser rlib, then run it under `/usr/bin/time -l` with
`MallocSpaceEfficient=1`. Its `--dump` option emits the complete GFM-plus-math
mdast debug output for byte comparisons; leave it off for memory measurements.

### Parse-only evidence

One run per side on arm64, optimized standalone parser, `MallocSpaceEfficient=1`,
`/usr/bin/time -l`, under Quartermaster quiet holds. State: parsing only, no app,
rendering or mdast serialization during the measured run. The 5,242,878-byte
fixture repeats `docs/working-here.md`, two newlines, this patch note at
`a415de0d`, and two newlines, cuts at 5,242,880 bytes, then drops the last
partial line. This is a different document from finch#g34j's 167 MB fixture.

| Parse-only state | Before | Compact events |
| --- | ---: | ---: |
| Peak memory footprint, bytes | 106,037,776 | 89,080,288 |
| Maximum resident set size, bytes | 175,144,960 | 147,406,848 |
| Time inside `to_mdast`, seconds | 0.263358 | 0.245171 |

Peak footprint falls 16.0%; this single pair shows no parse-time regression.
The complete GFM-plus-math mdast debug output is byte-identical (`cmp`) for
the 5 MB fixture, the build guide, and a BOM/Unicode/CRLF/tab fixture exercising
tables, task lists, footnotes, math, emphasis, strikethrough and autolinks.
The 5 MB mdast SHA-256 is
`1da2bcd5e7f6d44c96427d158674ada3a1b462bf185a90396a321e2578c096f7`.
The app's fast build passes, as do the four `event::tests`, two
`util::edit_map::tests`, and two `unist::tests`. The event tests pin the arm64
layout and cover coordinate limits, round trips, tabs and UTF-8 shifts.
