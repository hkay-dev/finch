# Finch

A native Mac text editor in Rust on GPUI. See README.md for building and layout.

- Build: `cargo build --profile fast` (binary `target/fast/finch`); `--release` only for launch time and memory numbers.
- Tests: `cargo test --profile fast <filter>`. Vendored crates: `scripts/vendor-test.sh <crate> [cargo test args]`, never plain `cargo test` inside `vendor/`.
- Launch test windows only with `scripts/bg-launch.sh <binary> -- <absolute path>`, with `GPUI_EDITOR_CONTROL`, `GPUI_EDITOR_SETTINGS`, `GPUI_EDITOR_DRAFTS` and `GPUI_EDITOR_VERSIONS` pointing at a scratch folder, so a test never touches a running Finch or its files. `<binary> debug <verb>` drives a running launch; `src/control.rs` lists the verbs.
- Saving files and `vendor/` (on every frame) are the hot paths. A change under `vendor/<crate>` adds an entry to that crate's `PATCHED.md`.
- Sizes, colors and motion lengths are named constants in `src/tokens.rs`; run `scripts/tokens.sh` after changing one.
- The user guide in `docs/guide/` ships inside the app; update the page a change makes untrue.
