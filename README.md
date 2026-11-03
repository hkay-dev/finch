<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/readme-header-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/readme-header-light.png">
    <img alt="Finch, a small, fast text editor for the Mac" src="docs/assets/readme-header-light.png">
  </picture>
</p>

Finch is a small, fast text editor for the Mac, written in Rust on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui). It opens files in tabs with a folder tree on the left and a live Markdown preview on the right.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/readme-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/readme-light.png">
  <img alt="Finch with a folder tree, a Markdown note and its live preview" src="docs/assets/readme-dark.png">
</picture>

Finch started in October 2026, for my own daily writing.

## Why Finch

I wanted an editor that wasn't Electron, was light, and wasn't a full IDE, because I don't use one anymore. I wanted a Markdown preview beside the text, the folder tree in view, and writing tools like Typewriter and Focus mode when I want them. That's why Finch was born.

It's an opinionated editor. It's GPU accelerated and written in Rust on GPUI. It uses about 80 MB of memory on first launch, and the app is about 67 MB on disk. Smooth scrolling is one of its goals, and there's more work to come on it.

Finch is for writing, whether that's jotting down a quick note or editing a Markdown file, and that's all it should do. It reads and edits code files too, with code colors and a few conveniences like bracket folding, for when you need to open one and change it. It isn't meant to be a coding environment, but you're welcome to use it as one.

### What it'll never have

- IDE features. Don't go looking for linting or tab to autocomplete.
- A terminal. There are plenty of good ones already.
- A plugin system. It's kept light and simple on purpose.
- A Git client.
- An agent chat mode.

## What it does

- **Writing:** a live Markdown preview (tables, callouts, math, Mermaid), Typewriter and Focus modes, spelling and a style check.
- **Files:** a folder tree, Open quickly, tabs and split panes.
- **Code:** code colors for 34 languages, Find in folder, multiple cursors and folding.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/readme-code-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/assets/readme-code-light.png">
  <img alt="A Rust file in Finch with its code colors" src="docs/assets/readme-code-dark.png">
</picture>

## Speed

Finch draws its window on the GPU, like Zed. A release build on an M-series Mac draws its first frame 0.35 to 0.6 seconds after launch, whether the file has 2 lines or 50,000. A million-line file scrolls as smoothly as a short one, and long lines wrap a piece at a time.

## Build and install

You need:

- A Mac with Apple Silicon running macOS 14 (Sonoma) or later. Intel Macs aren't supported.
- Rust (edition 2024; Finch is built with a current stable toolchain).
- Xcode with the Metal toolchain, since the GPU shaders are compiled at build time. If `xcrun -f metal` fails, run `xcodebuild -downloadComponent MetalToolchain`.

Run it from the source folder:

```sh
cargo run --release -- path/to/file.md
```

Or make an app bundle you can put in /Applications:

```sh
scripts/bundle.sh
```

It builds a release and writes `target/release/Finch.app`. The bundle is signed with the identity in `FINCH_SIGN_IDENTITY`, or the first line of `~/.config/finch/sign-identity`, and ad hoc when neither is set. An ad hoc build runs on the Mac that built it.

To make a disk image for a release download:

```sh
scripts/dmg.sh
```

It writes `target/release/Finch-<version>.dmg` (about 13 MB), with the app beside a shortcut to Applications. The app in it is signed ad hoc unless `FINCH_SIGN_IDENTITY` names a certificate, and it isn't notarized, so on another Mac the first open is blocked: open it from System Settings > Privacy & Security > Open Anyway.

There are no prebuilt downloads yet.

## Settings

Every setting in Finch is in `~/Library/Application Support/Finch/settings.toml`. The Settings window (Cmd-,) edits the same file.

The default editor font is [Google Sans Code NF](https://github.com/wylu1037/google-sans-code-nerd-font) at 14 points. Without it, code draws in the Mac's monospaced font, and any installed monospaced font can be picked in Settings.

## Layout

- `src/`: the app.
- `vendor/`: GPUI, GPUI Kit, markdown-rs and two small crates, vendored with local changes. Each folder's `PATCHED.md` lists what changed and why.
- `docs/guide/`: the user guide, bundled into the app.
- `docs/design/system.md`: every size, color and motion length the views use, generated from `src/tokens.rs` by `scripts/tokens.sh`.
- `scripts/`: building, bundling, and launching test windows in the background.

Tests run with `cargo test`. Comments and commit messages cite tickets like `finch#abcd`. They point to the project's private tracker.

## License

Finch is licensed under the [Apache License 2.0](LICENSE). The vendored code has its own licenses (Apache 2.0 and MIT); see [NOTICE](NOTICE).
