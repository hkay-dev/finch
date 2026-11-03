# gpui-component 0.8.1, patched for GPUI Editor

This is the crates.io `gpui-component` 0.8.1 source (GPUI Kit's styled
components, which hold the tree-sitter highlighter), used through
`[patch.crates-io]` in the app's `Cargo.toml`, the same way `vendor/gpui-base`
is. The crate's own `Cargo.lock`, `Cargo.toml.orig`, `.cargo-ok` and
`.cargo_vcs_info.json` were left out. Every change from the published crate is
listed here. On a GPUI Kit upgrade, copy the new crate in the same way and carry
each patch over, or drop it if upstream fixed it.

The highlighter runs on every edit, so every change gets a review.

## Compiled highlight queries shared per language (gpui-editor#zvb9)

`src/highlighter/highlighter.rs`, `src/highlighter/registry.rs`. Every new
document used to build its own `SyntaxHighlighter`, which compiled the combined
injections, locals and highlights query and the injections query again on the
main thread. `CompiledQueries` now holds the `Arc<Query>`, the injections query,
the pattern indices, `non_local_variable_patterns` and the special capture
indices, and `LanguageRegistry::compiled_queries` builds it once per language
name. `SyntaxHighlighter::query` is an `Option<Arc<Query>>`. `register` and
`register_parser_factory` drop the cache. A `Query` is immutable after build
(the locals patterns are disabled before it is stored), so documents and
threads share it.

## Injection layers re-parse in the background after a key (gpui-editor#ysd5)

`src/highlighter/highlighter.rs`, `src/highlighter/input_adapter.rs`. A key in a
file up to 256 KB ran `parse_injection_layers` (up to 512 layers; every Rust
macro token tree, every Markdown inline block) inside `SyntaxHighlighter::update`
on the main thread. `update_edits_deferring_injections` now parses the main tree
only: the layers were already shifted by `edit_injection_layers`, and
`injections_stale` is set. The adapter's `refresh_injections` waits
`PARSE_DEBOUNCE`, computes the layers on the background executor from a snapshot
(`pending_injection_job`) and applies them if the text is unchanged
(`apply_injection_layers`). Macro bodies and inline spans keep their old colours
for one debounce; a newly typed macro is coloured after it. Calls without edits
(a whole new text) still parse layers synchronously, so a file opens coloured.
Measured on a 210 KB macro-heavy Rust file, `fast` build, `update` call per key:
about 33 ms before, 2 to 3 ms after. A 200 KB Markdown file with light inline
use measured 2 to 5 ms both before and after (parse-bound there).

## Large files reuse their tree on the keystroke (finch#f4vx)

`src/highlighter/input_adapter.rs`. Single edits and batches with an existing
tree try the same 2 ms incremental main-tree parse as small files, still
deferring injections. A large file's first tree stays off-thread without a
debounce. On timeout the shifted tree remains visible and the existing
debounced background parse takes over.

A timeout above 256 KB disables further synchronous attempts for that document
until its text shrinks below 75% of the size that timed out, or back to 256 KB or less. Real concatenated
Rust source at 1 MB parsed keys in 0.29 to 1.03 ms; at 5 MB all 23 attempts
timed out at 2.18 to 2.65 ms before this backoff. Tree-sitter's 2 ms callback
budget is cooperative, not a hard wall-clock deadline, so an unproductive large
parse must not consume it on every key. Regression tests report per-key times
and timeout counts at both sizes and cover recovery after a substantial shrink.
With backoff, the final `fast` check measured 1 MB keys at 0.49 to 1.68 ms
(0/23 timeouts; old shift-only path 0.14 to 0.21 ms). At 5 MB just the first
key timed out at 2.24 ms; the next 22 cost 0.12 to 0.28 ms, comparable with
the old shift-only path's 0.12 to 0.21 ms. No extra work runs at rest.

## First parse starts without the debounce (gpui-editor#tx76)

`src/highlighter/input_adapter.rs`. `finish_update` waited `PARSE_DEBOUNCE`
before every background parse, including the first one of a highlighter with no
tree yet (a fresh `set_highlighter`, such as a big hidden tab shown again). When
the highlighter has no tree the background parse now starts at once; later edits
keep the debounce.

`src/input/search.rs`. The find panel gets a whole-word toggle next to the case toggle and
smartcase: until the case toggle is clicked, the query's own capital letters pick the case mode.

`src/input/search.rs`, `src/input/input.rs`. The panel hands the find bar `SearchSession::misses` (gpui-base) as `FindBar::misses`, which the app's find bar pulses its count for.

## Dead editors' find hosts pruned on every draw (gpui-editor#8pfq)

`src/input/overlay.rs`. `render_overlays` pruned hosts whose editor was gone only in the no-overlay branch, so closing the last tab with Find open kept its find panel, fields and matches until an editor drew without an overlay. Dead-owner hosts are now pruned at the top, before the `has_overlay` test.

## Syntax generation also counts injection layers (finch#r7qz)

`src/highlighter/input_adapter.rs`. `parses`, behind `syntax_generation`, now also advances when background injection layers are applied (macro bodies, Markdown inline spans recolour without a new tree), so the editor's reused visible styles (gpui-base `HighlightCache`) refresh. The bracket cache that reads the same number refreshes once more at that moment.

## Closing Find selects its current match (finch#7ttz)

`src/input/search.rs`. Returning focus to the editor captures the current match
before closing releases the matcher, then selects it. Hiding without returning
focus and searches without matches leave the editor selection unchanged.

## Enter and brackets read the highlighter's tree (finch#ffc6)

`src/highlighter/input_adapter.rs`, `src/input/syntax_context.rs`. `syntax_context_at` answers an insertion point from the highlighter's tree with `context_in_tree`, so the per-editor provider no longer holds a second parse. `tree_current` is false from a sync parse that ran out of budget (or was skipped) until the background tree lands; then, with no tree, or with a text of another length, it declines and the provider parses as before. A no-op update while stale is skipped so it cannot mark the stale tree current.

## Find field keeps a seeded multi-line query (finch#9pm1)

`src/input/search.rs`. `update_search_query` keeps the session's query, line breaks and all, while the one-line field still reads as that query with the breaks dropped.

## Highlighter batch takes the final text (finch#r0xq)

`highlighter/input_adapter.rs`. `update_batch` receives the edits and the text after the last, matching gpui-base's `InputHighlighter::update_batch`.

## Shift-Cmd-F with the replace row open goes to the replace field (finch#bfjq)

`src/input/search.rs`. `on_action_replace` with the row open moves focus from the find field to the replace field (from the replace field it closes the row), and `show_with_focus` does the same when the editor asks for replace again (`replace_row` remembers the row was open).

## Find bar Regex control (finch#38qa)

`src/input/input.rs`, `src/input/search.rs`. `FindControl::Regex`, `FindBar::regex` and `FindBar::bad_pattern`; the panel toggles `set_search_regex` on the editor and re-sends the query.

## Regex from the menu re-runs smartcase (finch#qkhv)

`src/input/search.rs`. `sync_session` sees the editor's `regex` differ from the panel's while the bar is open and smartcase still picks the case; `render` then defers one re-read (`update_search_query`) only when smartcase would pick a different case, since a re-read clears the anchor and the overlay would refocus the find field.

## A find option change keeps focus where it was (finch#zxjs)

`src/input/overlay.rs`. `sync` no longer counts a moved search anchor as a new open: Match case, Whole word and Regex re-anchor at the viewport, and only a new activation revision (Cmd-F, Shift-Cmd-F) or a mode change moves focus into the find bar.

## Edit > Find > Replace commands (finch#v2s2)

`src/input/search.rs`, `src/input/overlay.rs`, `src/input/input.rs`. `FindReplace` and `find_replace` run Replace, Replace and find or Replace all on the editor's open find bar from outside it, so a menu key never moves focus; they do nothing unless the replace row is open.

## Up and Down in the find field bring back earlier searches (finch#646m)

`src/input/search.rs`, `src/input/input.rs`. `FindHistory` (a global, 20 queries, newest first, none twice) fills when Return or Find next/previous (`remember_find_query`) searches or the bar closes; a capture handler on the panel takes Up and Down while the one-line find field has focus and gives the field and search the recalled query, Down past the newest the typed text. A multi-line query leaves both keys to the field.

## Return asks the parse about fences (finch#0nwa)

`src/highlighter/input_adapter.rs`. `TreeSitterInputHighlighter::in_fenced_code` implements the new `InputHighlighter` method from the Markdown tree (see vendor/gpui-base/PATCHED.md).

## Find in selection button (finch#dqvw)

`src/input/search.rs`, `src/input/input.rs`. `FindControl::InSelection` flips `set_search_in_selection` on the editor; `FindBar` carries `in_selection` and `scope_empty` (a caret alone took no scope) for the app's bar to draw.

## Find history skips huge queries (finch#6adz)

`src/input/search.rs`. `remember_query` also skips a query over 512 characters (a pasted file in one line), so `FindHistory` never holds a megabyte string for the session.

## unique_styles without a tree set (finch#ykse)

`src/highlighter/highlighter.rs`. `unique_styles` keeps the style ends in a sorted, deduplicated `Vec` (binary search) instead of a `BTreeSet`, filters empty ranges in place, and merges equal neighbours as it pushes, so a call builds one result vector instead of two. The output is the same, checked against a copy of the old function over the captures of a real Rust slice and random ranges (642 captures: 67.6 us before, 42.5 us after, release, input clone included).

## A tree behind the text still answers bracket and Return keys (finch#6vgd)

`src/highlighter/input_adapter.rs`. `syntax_context_at` and `in_fenced_code` also answer from a tree that isn't current (a timed-out parse on a big file, which waits for the background parse) when every edit since its parse went through `Tree::edit` (`edits_tracked`); an update with no edit leaves it unusable until a parse lands. Before, each bracket, quote or Return key in a 5 MB file re-parsed the text from scratch for 5 ms (`language.context_at`) and a Markdown list Return scanned every line from row 0. The answer is exact away from the edits and a guess near them, as the highlight colours are.

## The find panel's session copy follows a hidden editor's release (finch#hbm4)

`render_overlays` hands the released session to the panel of every other editor whose match list is empty while its host still marks the panel as holding one, so the panel's `SearchSession` clone stops pinning the dropped list (the host's `holds_matches` flag keeps the check to one bool per host per frame).

The find bar reads `shown_total` and `shown_current` of the matcher, so a released tab's count stays until its background scan lands; the sweep clones a session only for a host that takes it.

## Colouring an edit reuses its style list (finch#18pt)

`src/highlighter/highlighter.rs`. `styles` keeps its work buffers (the clipped style list, the interval list, the style ends, the active set) in a `RefCell` on the highlighter, cleared, and freed down to a small cap (4,096 entries) after a big call, so a call allocates only the vector it returns. The interval sort is an unstable sort with the style index as the last key (the same order the stable sort gave, without its buffer) and is skipped when the intervals are already sorted. Output unchanged. Release, highlighter.rs repeated to 100 KB, own cost of `styles` over `match_styles`: 1 line 3.5 us and 8 allocs before, 2.2 us and 2 allocs after; 50 lines 89 us and 12 allocs before, 34 us and 1 alloc after.

## A FILE completion row has two lines, and the provider's types are exported (finch#z5z0)

`src/input/popovers/completion_menu.rs`: an item of kind `FILE` draws its label with its `detail` under it (smaller, muted, one line each, `STACKED_ROW_HEIGHT` 44 px) instead of side by side in a 26 px row; every other kind is unchanged. `src/input/mod.rs` re-exports `Command`, `CompletionContext`, `CompletionItem`, `CompletionItemKind`, `CompletionResponse`, `CompletionTextEdit`, `LspRange` (`lsp_types::Range`) and `TextEdit`, the types a `CompletionProvider` is written in, so the app needs no `lsp-types` dependency of its own.

The same menu's `handle_action` no longer calls `cx.propagate()` for the keys it handles (Return, Escape, Up, Down): the propagate flag let the Return that took a row fall through to the input as text, so a newline went in beside the inserted text. It still propagates for any other action.

## The tree-sitter highlighter answers `inspect_current_tree` (finch#03xt)

`src/highlighter/input_adapter.rs`: `inspect_current_tree` hands the app the `tree_sitter::Tree` only when `tree_current` is true and the highlighter's text has the editor's length (a tree that is behind, or edited but not reparsed, answers false). Borrows the highlighter's `RefCell` for the call; no parse, no allocation.

## Markdown-only highlight roles (finch#ytpd)

`src/highlighter/registry.rs`: `SyntaxColors` gains `markup.heading`, `markup.heading.marker`, `markup.emphasis`, `markup.strong`, `markup.mark`, `markup.code.mark`, `markup.bracket`, `markup.paren` and `markup.link.uri` (fields and `style()` arms). The Markdown `highlights.scm` files capture headings, heading and emphasis marks, emphasis, link addresses and link brackets under these names instead of `title`, `punctuation.*`, `emphasis*` and `link_uri`, so the app styles Markdown's marks without moving a role other languages use. `markdown_inline` also captures `[`, `]` (`markup.bracket`) and `(`, `)`, `!` (`markup.paren`) of links and images, and the backtick marks (`markup.code.mark`), each with `highlight.allow-overlap` so the enclosing capture doesn't hide them and splits `link_uri` into `link_destination` (`markup.link.uri`) and `uri_autolink` (still `link_uri`). No per-frame cost: the roles resolve inside the cached highlight query. `wasm_stub.rs` is not changed (it never loads these queries).

`highlighter/input_adapter.rs` (finch#9s91): for the Diff language the fold candidates are one per file section (`diff_fold_ranges`, a text scan that counts the hunks' `@@ -a,b +c,d @@` lengths), from the file's header line to the next file's header (a fold keeps its last line shown), instead of the tree's nodes. Used by the background parse and by `fold_ranges` / `fold_ranges_for_edit`; other languages are unchanged.

## Open highlight roles and finer grammar captures (finch#38q6, finch#zqjz)

`src/highlighter/registry.rs`: `SyntaxColors` gains `extra`, a `#[serde(flatten)]` map of `ThemeStyle` by capture name, for roles that have no field (`variable.parameter`, `type.builtin`, `function.method`, `markup.quote`). `style()` looks a name up in `extra` first, then drops its last dotted part and tries again (`function.method.call` to `function.method`), then falls through to the fields as before; with an empty map nothing changes and nothing is looked up. The app builds the roles from a settings-chosen scheme, so a role needs no new field here.

`languages/rust/highlights.scm`: an enum variant's name in its definition is `@variant` (it was `@type`), and so is the CamelCase last part of a path in an expression or pattern (`Ordering::Less`, `Self::Open`; not an ALL_CAPS constant); the end of a `use` path stays `@type`. `languages/markdown/highlights.scm`: `block_quote_marker` is `@markup.quote` (it shared `@punctuation.special` with `block_continuation`). The app gives `variant` the type's color and `markup.quote` the quiet ink when colored roles are off, so nothing changes then.

`highlighter/highlighter.rs`, `highlighter/input_adapter.rs` (finch#3sgr, finch#tm6q, finch#a41a): `SyntaxHighlighter::bracket_depths` walks the main tree only where it meets the asked range (it jumps to the range with `goto_first_child_for_byte` at the root, and right after the opener inside a node with no syntax error that opens and closes with a matching one-byte pair and has no other anonymous child, so none of the children it skips is a bracket token; Java's `int[][]` and SQL's `[3][4]` are one node with four brackets, and a list with commas or semicolons as direct children is stepped too; elsewhere it looks at every child, so an opening bracket that is not a node's first child, as in `#[`, an index, a tuple pattern or C's `for (`, is counted) and reports each `()[]{}` token with the number of bracket pairs around it, so depth is right mid-file with no scan from the top; brackets in strings and comments are not grammar tokens and never appear; injection layers are not read; the adapter reports none for Markdown (prose and fences). Inside a pair the seek can return none when the child it descends into is hidden and holds no visible child ending after the range start (Go's statement list right before a block's `}`); it then steps every child. `styles_collecting` is `styles` plus the `variable` and `variable.parameter` captures in the range, from the same `match_styles_split` pass (which also reports where the injection layers' items start): a capture a later capture of another name overlaps, in the host list or the injection tail, is skipped (a function name in a macro body), and the name is hashed with FNV-1a, stable across runs. Both are exposed through `InputHighlighter::bracket_depths` and `styles_with_variables` in gpui-base; nothing runs unless the editor asks on a highlight cache refill.

## Root surface paints no ground (finch#x49c)

`src/root.rs`: `RootPlugin::style` no longer sets `.bg(cx.theme().tokens.background)` on the root surface (font family and text color stay). The window's own views paint the grounds (panes, tab bars, status line, preview, Settings form), so the system sidebar material behind the explorer column shows through (gpui-editor#yt30). The root has painted the kit's `tokens.background` since the first vendoring (b72ae0aa), so the material never showed (yt30-after.png is a white column).

## Spell check styles the editor tree (finch#yt1q)

`highlighter/highlighter.rs`: `SyntaxHighlighter::styles_for_tree(tree, text, range, theme)` adopts a tree another highlighter already parsed from `text`, builds the injection layers as `update` does, and styles with its own query and capture rules (same output as `update` then `styles`, minus the parse of the main text). The app reads the editor's current tree with `EditorState::with_current_syntax_tree` and passes it in, so the spell layer no longer parses the file on each pause. Callers must pass the parse of `text` in the highlighter's own language.

## A long file row keeps its extension (finch#11hx)

`src/input/popovers/completion_menu.rs`: a `FILE` item's name is drawn by `FileLabel`, an element that cuts the label to the width layout gives it (`cut_file_name`: the stem's start, an ellipsis, the stem's last two characters and the extension, measured with the text system) instead of `.truncate()`, which dropped the extension. The match highlights are remapped onto the cut text; the part of a range in the cut middle is dropped. Every other row keeps its end cut. The cut measures on each layout and paint of a file row that is too long (at most eight rows), by binary search over the head length.

## The completion menu uses the app's thin knob (finch#pbhs)

`src/list/list.rs`: `List::thin_scrollbar(true)` draws `thin_knob` instead of the kit's `Scrollbar`: a 7 pt pill 2 pt from the edge, at least 24 pt long, at 0.4 of the foreground, shown only while the rows move (a wheel, or a pick scrolled into view; a key that leaves the rows still shows nothing), held 0.5 s, then faded over 0.32 s on the one ease-out (`Easing::EaseOut`), the hold and fade stamped on the app's frame clock (`gpui_base::motion::frame_time`). The values mirror `SCROLL_KNOB`, `SCROLL_INSET`, `SCROLL_KNOB_MIN`, `KNOB_ALPHA`, `SCROLL_IDLE` and `SCROLL_FADE_OUT` in `src/tokens.rs`, which the vendored crate cannot import. The kit's scrollbar snapped on and off with a 2 s hold and a square 6 pt knob.

`src/input/popovers/completion_menu.rs` and `code_action_menu.rs`: the menus' lists set `thin_scrollbar(true)`.
## The Replace field keeps a history (finch#ymj1)

`src/input/search.rs`: `FindHistory` holds a second list, `replacements`, with the same 20-entry and 512-character limits and no multi-line entries as the find list (a `Field` picks the list). Replace, Replace and find and Replace all record the Replace field's text. Up and Down (`recall_entry`) step through the list of whichever field has focus, each with its own `Recall` position; typing in the Replace field resets its position.

## Replace all with no match pulses the count (finch#fbrj)

`src/input/search.rs`: `SearchPanel` keeps `replace_misses`, bumped once for each Replace all that replaced nothing; the bar gets `misses` as the editor's count plus this one, so the app's miss pulse (finch#fq9v) runs on it with no change to gpui-base.

## Use selection for replace (finch#47k7)

`src/input/input.rs`, `overlay.rs`, `search.rs`: `use_selection_for_replace(editor, cx)` fills the find panel's Replace field with the editor's selection and shows the replace row. The panel exists only while the bar is open, so with the bar closed the text waits in a `PendingReplacement` global that the host's next `sync` hands to the panel (`set_replacement`). A closed bar opens with `open_search_with(true, false, cx)`, so the find text is not replaced by the selection and the selection and carets are untouched.

## Cmd-F from the replace row goes back to plain Find (finch#gfhk)

`src/input/search.rs`: the panel handles `Search` itself (the find and replace fields are not searchable, so it reached nothing): with the replace row open it closes the row and the editor's session follows; the caret goes to the find field with its text selected, as on the Mac.

## A bad regex says why (finch#f3s5)

`src/input/input.rs`, `search.rs`: `FindBar::bad_pattern` is now `Option<&'static str>`, the matcher's `problem()`, instead of a bool.

## Two input tests follow the code again (finch#dm4d)

`src/input/overlay.rs`, `group/tests.rs`: tests only. The overlay test now draws once more after Find closes, because the host that drew the bar stays for the draw that sees it closed (the bar's exit). The textarea test sets Reduce Motion, because typing at the bottom glides the text up a line (gpui-editor#05q3) and the test judges where the caret ends.

## Recent Searches from the find field's magnifier (finch#pz5s)

`src/input/input.rs`, `search.rs`: `recent_find_queries(cx)` hands the app the find list (`FindHistory::queries`, newest first) that Up walks. `FindControl::Recent(n)` fills the find field with entry `n`, makes it the newest, resets Up's position and searches (`set_value` emits no change, so `update_search_query` runs by hand); `FindControl::ClearRecent` empties the find list only (the Replace list and the field's text stay). The app's magnifier menu builds from the first and sends the others through `FindBar::act`.

`search.rs` `recall_entry` (finch#pz5s): a panel's place in the shared find list is ignored when it is past the list's end (another panel cleared the list), so Up and Down start over instead of indexing past it.

## Ignore diacritics button and bar width (finch#8rmd)

`src/input/input.rs`, `search.rs`: `FindControl::IgnoreDiacritics` flips `set_search_ignore_diacritics`, so the app's find bar can draw and press an Ignore diacritics toggle; `FindBar::ignore_diacritics` hands the option's state to the bar. `FindBar::width` is the editor's `input_bounds()` width from the last layout (10,000 before the first), so the app's bar can drop option buttons that don't fit a narrow pane. One read per bar draw, no allocation.

## The find bar ends its own exit (finch#x0kj)

`src/input/input.rs`, `search.rs`: `FindBarRenderer::exit` and the panel's exit timer are gone. A closed bar is drawn (`FindBar::leaving`) until the app's bar calls `FindBar::gone` (set only while leaving, so an open bar allocates nothing for it); `SearchPanel::end_exit` then drops the kept matcher and the caret cover and notifies the editor. The app calls it from a next-frame callback before the frame that would draw the bar at 0, so that frame takes the bar away and shows what it covered, with no frame after it.
