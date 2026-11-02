# Pele

Obsidian's Mermaid renderer, version 1.13.0, MIT (see LICENSE).
Source: https://github.com/obsidianmd/pele (npm package `pele`).

`pele.global.js` is the package's `dist/pele.global.js`, unmodified. It is not a Cargo crate:
`src/preview/pele.rs` embeds it with `include_str!` and runs it in JavaScriptCore.

To update: `npm pack pele@<version>`, unpack, copy `package/dist/pele.global.js` and
`package/LICENSE` here, change the version above, then run `cargo test pele`.
