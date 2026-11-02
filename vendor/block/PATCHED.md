---
type: implementation
summary: Clear block 0.1.6's future-incompat and deprecation warnings without changing behavior.
---

# Local patch to block 0.1.6

Upstream: <https://github.com/SSheldon/rust-block>, crates.io `block` 0.1.6,
MIT license, unmaintained. `metal` 0.33 depends on it. The root manifest selects
this copy with `[patch.crates-io]`.

## Warnings (finch#y9fd)

- `enum Class { }` became `#[repr(C)] struct Class { _private: [u8; 0] }`. The
  extern static `_NSConcreteStackBlock: Class` had an uninhabited type, which
  rustc flags as a future hard error (rust-lang/rust#74840). Only its address
  is ever taken, so a zero-sized opaque struct keeps the same code.
- Every bare `extern` became `extern "C"` (the same ABI, now spelled out).
- The manifest drops the `objc_test_utils` dev-dependency, whose `test_utils`
  folder isn't in the published crate, and its `exclude` list.
