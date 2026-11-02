#!/usr/bin/env bash
# Runs a vendored crate's own tests (they are not part of the app's `cargo test`).
#
#   scripts/vendor-test.sh <gpui-base|gpui-component> [cargo test args...]
#   scripts/vendor-test.sh gpui-base state::tests::some_test
#   scripts/vendor-test.sh gpui-component highlighter
#
# The crates are published as standalone packages, so their tests need what the
# app's root normally gives them: the root Cargo.lock beside the crate, an empty
# README.md at the repo root (gpui-base's text_view test include_str!s it), and
# `--config` patches that match the root [patch.crates-io] (without them the crate
# pulls the published gpui-pre* from crates.io instead of vendor/). Every stub is
# removed by a trap on exit, Ctrl-C and kill included, so `git status` stays clean.
# Runs through the build wrapper like every build; target dir target/vtest.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate=${1:-}
case "$crate" in
gpui-base | gpui-component) shift ;;
*)
    echo "usage: scripts/vendor-test.sh <gpui-base|gpui-component> [cargo test args...]" >&2
    exit 2
    ;;
esac

dir="$root/vendor/$crate"
stubs=()

# Creates a stub only when the file is absent, so a real file is never removed.
# stub <path> [copy-from-file]; with no source the file is empty. stub_text takes
# the file's text instead.
stub() {
    [[ -e "$1" ]] && return 0
    mkdir -p "$(dirname "$1")"
    if [[ -n ${2:-} ]]; then cp "$2" "$1"; else : >"$1"; fi
    stubs+=("$1")
}
stub_text() {
    [[ -e "$1" ]] && return 0
    mkdir -p "$(dirname "$1")"
    printf '%s\n' "$2" >"$1"
    stubs+=("$1")
}
# Removes the stubs, then any directory a stub made that is now empty.
cleanup() {
    local f
    for f in ${stubs[@]+"${stubs[@]}"}; do rm -f "$f"; done
    for f in ${stubs[@]+"${stubs[@]}"}; do rmdir -p "$(dirname "$f")" 2>/dev/null || true; done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

stub "$root/README.md"
stub "$dir/Cargo.lock" "$root/Cargo.lock"
if [[ $crate == gpui-component ]]; then
    # include_str!/include_bytes! paths in its tests that only exist in the upstream repo.
    stub_text "$root/examples/fixtures/test.md" "# Stub"
    stub_text "$root/vendor/assets/assets/icons/arrow-up.svg" '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M12 19V5"/></svg>'
    stub_text "$root/vendor/assets/assets/icons/search.svg" '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M11 3a8 8 0 100 16"/></svg>'
    # Its dock test compares against base's dock module; the real one is in gpui-base.
    stub "$root/vendor/base/src/dock/mod.rs" "$root/vendor/gpui-base/src/dock/mod.rs"
    # Smallest set the aurora gradient test accepts.
    stub_text "$root/themes/aurora.json" '{"name":"Aurora","themes":[{"name":"Aurora Light","mode":"light","colors":{"primary.background":"linear-gradient(135deg, #4F46E5, #06B6D4)","button.primary.background":"linear-gradient(135deg, #4F46E5, #06B6D4)"}}]}'
fi

# One --config per [patch.crates-io] line of the root Cargo.toml, paths made absolute.
patches=()
while IFS= read -r line; do
    name=${line%% *}
    rel=${line#*path = \"}
    rel=${rel%%\"*}
    patches+=(--config "patch.crates-io.$name.path=\"$root/$rel\"")
done < <(awk '/^\[patch\.crates-io\]/{on=1; next} /^\[/{on=0} on && /path = /' "$root/Cargo.toml")
[[ ${#patches[@]} -gt 0 ]] || { echo "vendor-test: no [patch.crates-io] paths in Cargo.toml" >&2; exit 1; }

# One command string for zsh -c; quote every argument into it. FINCH_BUILD_WRAPPER (say a build
# queue's `<tool> --`) runs in front of it when set.
cmd="cd $(printf %q "$dir") && CARGO_TARGET_DIR=$(printf %q "$root/target/vtest") nice -n 10 cargo test"
for a in "${patches[@]}" --features test-support "$@"; do cmd+=" $(printf %q "$a")"; done

# Not exec: the trap must run after the wrapper returns.
# shellcheck disable=SC2086 # the wrapper is a command line; split it into words.
${FINCH_BUILD_WRAPPER:-} zsh -c "$cmd"
