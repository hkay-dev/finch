#!/bin/bash
# Reruns the editor bench (gpui-editor#xs3g): scroll and resize frames for GPUI Kit's Editor on one file.
#
#   scripts/editor-bench.sh <file> [KEY=VAL ...]     e.g. scripts/editor-bench.sh /tmp/100k.rs EB_WRAP=0
#
# Builds examples/editor_bench.rs, wraps it in a signed LSUIElement bundle, launches it with `open -n -g -j` (no
# focus), waits for it to quit by itself and prints its `EB` lines. Needs nothing but cargo.
#
# The bench opens its window the way the app does (src/window_look.rs: display sync off, drawing while
# covered), so a background launch counts frames. The raw stderr log, with GPUI's own `frame duration`
# lines when ZED_MEASUREMENTS=1 is passed, stays at the path printed last as `log:`.
set -euo pipefail
file=${1:?usage: editor-bench.sh <file> [KEY=VAL ...]}
shift
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
nice -n 10 cargo build --release --example editor_bench # run it under the build wrapper
# An LSUIElement bundle at a fixed path, signed with the identity from scripts/sign-identity.sh, started with
# `open -n -g -j` so it never takes the front (gpui-editor#a6gn).
app="$root/target/bg/xsbench.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/examples/editor_bench "$app/Contents/MacOS/xsbench"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>xsbench</string>
<key>CFBundleIdentifier</key><string>dev.hkay.finch.xsbench.dev</string>
<key>CFBundleName</key><string>xsbench</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSUIElement</key><true/>
</dict></plist>
PLIST
source "$root/scripts/sign-identity.sh"
codesign --force -s "$SIGN_IDENTITY" --identifier dev.hkay.finch.xsbench.dev "$app" >/dev/null 2>&1 \
    || { echo "editor-bench: codesign with \"$SIGN_IDENTITY\" failed" >&2; exit 1; }
log=$(mktemp)
envs=(--env EB_SCROLL=1 --env EB_RESIZE=1)
for kv in "$@"; do envs+=(--env "$kv"); done
echo "load before: $(sysctl -n vm.loadavg)"
source "$root/scripts/bg-launch.sh"
bg_launch_safe || exit 3 # never over a full-screen app (finch#jxb8)
open -n -g -j "$app" --stderr "$log" "${envs[@]}" --args "$file"
sleep 2
pid=$(pgrep -f "$app/Contents/MacOS" | head -1)
for _ in $(seq 1 400); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
kill -0 "$pid" 2>/dev/null && kill "$pid"
grep -a '^EB' "$log"
echo "load after: $(sysctl -n vm.loadavg)"
echo "log: $log"
