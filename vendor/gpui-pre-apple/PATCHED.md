# gpui-pre-apple 0.3.10, patched for GPUI Editor

This is the crates.io `gpui-pre-apple` 0.3.10 source (GPUI's Metal renderer
and macOS text system), used through `[patch.crates-io]` in the app's
`Cargo.toml`, the same way Harrier vendors its GPUI crates. The crate's own
`Cargo.lock`, `Cargo.toml.orig` and `.cargo-ok` were left out. `vendor/gpui`
inside it is upstream's: `build.rs` reads it to make the shader header. Every
change from the published crate is listed here. On a GPUI upgrade, copy the
new crate in the same way and carry each patch over, or drop it if upstream
fixed it.

## Text knobs the app sets (gpui-editor#1sp6)

`src/text_tuning.rs` (new) and `src/gpui_apple.rs` (`pub mod text_tuning`).
Relaxed atomics the app sets at launch: the thickening level, and the gamma
and contrast of the glyph shader's correction. Until the app sets them, text
draws as upstream does.

## Thickening that ignores the text color (gpui-editor#1sp6)

`src/text_system.rs`, `glyph_dilation_for_color`. Upstream picks the Core
Graphics font smoothing level (0 to 4) from the text color's brightness, so
white text thickens the most and black text not at all, and turns it off for
the whole app when its `AppleFontSmoothing` preference is 0. When the app sets
a level with `text_tuning::set_thicken`, every color gets that level and the
preference is ignored. The level was already part of each glyph's cache key
(`RenderGlyphParams::dilation`), so nothing new runs per frame.

## No thickening for the system font (gpui-editor#hzdn)

`src/text_system.rs`, `rasterize_glyph`. A glyph whose font's family name starts with a dot (the system UI font, `.AppleSystemUIFont`) is drawn without Core Graphics smoothing, whatever `params.dilation` says, so only code and other named fonts thicken. `glyph_dilation_for_color` only gets the color, so the check sits where the font is known; it runs on an atlas miss, not per frame, and `dilation` stays in the cache key unchanged.

## Gamma and contrast for glyphs (gpui-editor#1sp6)

Upstream's Mac renderer only multiplies the text color by glyph coverage. The Windows and Linux renderers also correct coverage with gamma and contrast; this ports the grayscale path of the Linux (wgpu) one (`apply_contrast_and_gamma_correction` in gpui-pre-wgpu `shaders.wgsl`, from Microsoft Terminal's `dwrite.hlsl`). Monochrome SVG icons use the same pipeline, so they're corrected too, as in upstream's wgpu renderer.

- `src/shaders.metal`: new `GlyphGammaParams` struct and `glyph_correct_coverage`; `monochrome_sprite_fragment` takes the params at `SpriteInputIndex_GammaParams` and corrects `sample.a` only when `enabled != 0`. With gamma 1.0 and contrast 0.0 `enabled` is 0, so the output is upstream's exactly. Polychrome (emoji) sprites are untouched.
- `src/metal_renderer.rs`: `SpriteInputIndex::GammaParams = 5`, a matching `#[repr(C)] GlyphGammaParams`, and a `glyph_gamma_cache` on `MetalRenderer` holding the last gamma and its `gpui::get_gamma_correction_ratios` result. `draw_monochrome_sprites` reads `text_tuning::gamma()` and `contrast()` (two relaxed loads; a negative contrast counts as 0, as wgpu clamps it, since it would divide by zero), recomputes ratios only when gamma changed, and sends the 32 bytes with `set_fragment_bytes`.

## Weights (gpui-editor#1sp6)

`src/text_system.rs`, two changes to `font_id` and `id_for_native_font`:

- **Face match rounds to hundreds** (ported from Harrier, harrier#wf5d). Core Text reports continuous weights, so font-kit matched a 500 request to Regular on the system font. `round_weight` rounds both the request and each face's weight before `find_best_match`; only the face match uses it.
- **Exact weight on variable faces.** When the matched face has a `wght` axis (CTFont variation axes), `varied_weight_font` makes a copy with `CTFontDescriptorCreateCopyWithVariation` + `CTFontCreateCopyWithAttributes` at the requested weight (clamped to the axis), pushes it as its own `FontId`, and `font_selections` caches it. Copies are cached by base `FontId` and weight (`varied_by_base`), since each base face carries its `FontKey`'s features and fallbacks. A copy shares its base face's PostScript name, so `varied_font_ids`, keyed by (PostScript name, wght in thousandths), maps a laid-out run back: `id_for_native_font` checks it first (read from `CTFontCopyVariation`) so layout lands on the varied font, and `font_ids_by_postscript_name` keeps the base entry. `is_emoji` is unaffected. Static faces still snap to the nearest face.

Measured (14 pt layout of "Hamburgefonstiv"; ink = summed 8-bit alpha of 'o' at 56 pt, scale 2):

| Request | Google Sans Code NF (static) | System font (SF, variable) width / ink |
| --- | --- | --- |
| 300 | Light | 109.974 / 313170 |
| 400 | Regular | 111.057 / 376393 |
| 450 | Medium | 112.553 / 408818 |
| 500 | Medium | 114.051 / 440848 |
| 550 | SemiBold | 115.548 / 472478 |
| 600 | SemiBold | 117.045 / 503670 |
| 700 | Bold | 120.039 / 569018 |

Without the rounding, 400, 450 and 500 on the system font all matched `.SFNS-Regular`. `SF Mono` and `Google Sans Flex` are not found by family name on this Mac (`no font found`), so they were not measured.

## Font list without the full descriptor scan (gpui-editor#2f25)

`src/text_system.rs`, `all_font_names`. Upstream builds a descriptor for every installed font (1,431 here) just to read family names, and callers such as GPUI Kit's theme init only check that a family is installed. The patch returns `CTFontManagerCopyAvailableFontFamilyNames()` plus the in-memory families, sorted and deduped. This drops the `lenient_font_attributes` module, which only the scan used.

## Shaders always precompiled (gpui-editor#trz5)

`build.rs` and `src/metal_renderer.rs`. GPUI Kit forces the `runtime_shaders` feature, so upstream compiled the stitched shader source with `newLibraryWithSource` for every renderer (about 120 ms on a fresh bundle copy). The patch ignores the feature: `build.rs` always runs `xcrun metal` and `metallib` (header from cbindgen as before), and the renderer always calls `new_library_with_data` on the metallib. The stitching step is removed; the `runtime_shaders` feature stays declared and does nothing.

## Each sized font made once (gpui-editor#5t0p)

`src/text_system.rs`. Upstream calls `clone_with_font_size` for every run of every shaped line in `layout_line` and for every glyph in `rasterize_glyph`. A `sized_fonts` map on `AppleTextSystemState` (a `parking_lot::Mutex`, since rasterizing takes `&self`), keyed by `FontId` and the size's bits, makes each one once through `sized_font`. The alternating `next_up` size `layout_line` uses to break ligatures between runs gets its own key. `fonts` is append-only, so entries never go stale and the map is bounded by faces times sizes.

## Path textures made on demand (gpui-editor#4v5k)

`src/metal_renderer.rs`. Upstream keeps a full-size path intermediate texture and its 4x MSAA texture per window and remakes both on every resize, whether or not anything draws a path (about 10 MB at 1800x1400, 59 MB at 5K). `render_frame` now calls `ensure_path_intermediate_textures` only when the scene has paths, `update_drawable_size` remakes them only when they exist, and after `PATH_TEXTURE_IDLE_FRAMES` (120) frames in a row without paths (`frames_without_paths`) both are dropped. The headless render functions no longer make them up front.

## GPU memory line and idle trim hook (gpui-editor#navf, gpui-editor#vta3, finch#5xpt)

`src/metal_renderer.rs`: with `GPUI_EDITOR_GPU_STATS=1` (read once, `LazyLock`) a frame that ends 10 s
after the last line prints atlas, path intermediate, path MSAA and instance buffer pool bytes to stderr
(`MetalAtlas::allocated_bytes` in `src/metal_atlas.rs`), plus a line when the pool shrinks. `idle_mark` and
`trim_unused_since` let `gpui-pre-macos` drop the path textures of a window that went idle, and shrink the
shared instance buffer pool back to 2 MB buffers when a dense frame grew it (doubling, never shrinking
upstream) and no window took a buffer since the mark. Small text with reading width off and two panes
grew it to 4 MB, on a 1728 or 2560 pt wide window.

## First run keeps its exact size (gpui-editor#7rjr)

`src/text_system.rs`, `layout_line`. Ported from Zed #65728 (`44ec569`), merged with the `sized_font` cache above: `break_ligature` starts false so the first run is laid out at the exact size (SF tracks by exact point size, so the `next_up` nudge made text a hair wider or narrower than AppKit), and an empty run no longer flips it. Upstream's tests were not ported.

## Present inside an explicit transaction (finch#c69w)

`src/metal_renderer.rs`, `draw`. Upstream's normal path calls `presentDrawable:`, so `-[CAMetalDrawable present]` runs on Metal's scheduling thread. Now and then, for a window that just closed, that present opens an implicit Core Animation transaction that the thread never commits ("deleted thread with uncommitted CATransaction" in the system log); the transaction keeps the `CAMetalLayer` and its drawables, about 30 MB of IOSurfaces, until the app quits. The renderer now adds a scheduled handler that wraps `present` in `[CATransaction begin]` and `commit`, holding its own retain on the drawable (`next_drawable` returns an autoreleased ref). The main thread still never waits; the presents-with-transaction path is unchanged.

## Window pictures without screen capture (finch#7nbd)

`src/metal_renderer.rs`: `render_to_image`, `render_scene_to_image` and `read_texture_to_image` are no longer gated on `test-support`, and `render_to_image` renders the scene into its own offscreen texture at the layer's drawable size instead of taking and reading a drawable, so it needs no drawable and no screen. `configure_layer` no longer calls `set_framebuffer_only(false)` under `test-support` (an upstream line), since nothing reads a drawable's texture now. The per-frame draw path is unchanged.
