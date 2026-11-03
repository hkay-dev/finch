#!/usr/bin/env bash
# Makes target/release/Finch-<version>.dmg for a release download: the app from scripts/bundle.sh
# beside a shortcut to /Applications, in a compressed (LZMA) read-only image that macOS 10.15 and
# later open. It signs ad hoc unless FINCH_SIGN_IDENTITY names a certificate, since a personal
# certificate means nothing on another Mac. The build is for every Apple Silicon Mac (the repo's
# .cargo/config.toml), so leave CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS unset. Prints the
# image's path on stdout.
#
#   scripts/dmg.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
root=$PWD
target="${CARGO_TARGET_DIR:-$root/target}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
[[ -n $version ]] || { echo "dmg: no version in Cargo.toml" >&2; exit 1; }

app="$(FINCH_SIGN_IDENTITY="${FINCH_SIGN_IDENTITY:--}" scripts/bundle.sh)"
dmg="$target/release/Finch-$version.dmg"

stage="$(mktemp -d -t finch-dmg)"
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/Finch.app"
ln -s /Applications "$stage/Applications"

rm -f "$dmg"
if diskutil image create from --help >/dev/null 2>&1; then
    # Newer macOS (diskutil has `image`): an APFS image straight from the folder, never mounted.
    diskutil image create from --format ULMO --volumeName "Finch $version" "$stage" "$dmg" >/dev/null
else
    # Older macOS: hdiutil mounts a scratch volume while it copies. Its APFS size guess runs out
    # of room for the app, so the image is HFS+.
    hdiutil create -quiet -volname "Finch $version" -srcfolder "$stage" -fs HFS+ -format ULMO -ov "$dmg"
fi
hdiutil verify -quiet "$dmg"
echo "dmg: $(du -h "$dmg" | cut -f1) at $dmg" >&2
echo "$dmg"
