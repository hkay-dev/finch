#!/usr/bin/env bash
# A pane dragged to its least (finch#jmzs): splits a window, drags the divider so one pane is at
# PANE_MIN, then prints that pane's rects and captures the window. One background launch with its
# own socket, settings and drafts under /tmp; never takes focus.
#
#   scripts/look/narrow-pane.sh [binary] [outdir] [left|right] [tabs]
#   binary: target/fast/finch   outdir: /tmp/finch-narrow-<stamp>   side: right   tabs: 3
# `right` narrows the second pane (under the preview button), `left` the first (the tree shows,
# so it isn't the corner pane). The narrow pane holds `tabs` tabs, the other one; the last tab of
# the narrow pane shows. Writes <outdir>/narrow-<side>-<tabs>.png and .json (the `rects` reply).
set -uo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bin=${1:-$root/target/fast/finch}
out=${2:-/tmp/finch-narrow-$(date +%Y%m%d-%H%M%S)}
side=${3:-right}
count=${4:-3}
[[ -x "$bin" ]] || { echo "narrow: no $bin; build it first" >&2; exit 1; }
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
source "$root/scripts/bg-launch.sh"
mkdir -p "$out"

winid="$root/target/window-id"
if [[ ! -x "$winid" || "$root/scripts/window-id.swift" -nt "$winid" ]]; then
    mkdir -p "$root/target"
    swiftc -O -o "$winid" "$root/scripts/window-id.swift" || exit 1
fi

dir=$(mktemp -d /tmp/finch-narrow.XXXXXX) # short: a unix socket path tops out near 100 bytes
pid=""
trap '[[ -n "$pid" ]] && kill "$pid" 2>/dev/null; rm -rf "$dir"' EXIT
mkdir -p "$dir/fix" "$dir/drafts"
for i in $(seq 0 "$count"); do printf 'note %s\n' "$i" >"$dir/fix/notes-$i.md"; done
printf '[window]\nwidth = 900.0\nheight = 500.0\ntheme = "dark"\n' >"$dir/settings.toml"
export GPUI_EDITOR_CONTROL=$dir/control.sock GPUI_EDITOR_SETTINGS=$dir/settings.toml GPUI_EDITOR_DRAFTS=$dir/drafts GPUI_EDITOR_RECTS=1
BG_LOG=$dir/log bg_launch "$bin" -- "$dir/fix" "$dir/fix/notes-0.md" "$dir/fix/notes-1.md" || exit 1
pid=$BG_PID
bin=$BG_BIN
id=""
for _ in $(seq 100); do
    id=$("$winid" "$pid" || true)
    [[ -n "$id" ]] && break
    sleep 0.1
done
[[ -n "$id" ]] || { echo "narrow: no window after 10 s" >&2; exit 1; }
v() { "$bin" debug "$@"; }
for _ in $(seq 50); do v launch >/dev/null 2>&1 && break; sleep 0.1; done
v draw-as-key on >/dev/null

# The showing tab moves into a second pane, which is active: right fills it, left goes back to the first.
v split right >/dev/null || { echo "narrow: split refused" >&2; exit 1; }
[[ $side == left ]] && v focus-pane 1 >/dev/null
for i in $(seq 2 "$count"); do v open-tab "$dir/fix/notes-$i.md" >/dev/null; done
share=$([[ $side == left ]] && echo 0 || echo 1)
echo "share kept: $(v split-share 1 "$share")"
name=narrow-$side-$count
# A background window draws only when asked, and motion steps at most a frame's time per draw: draw until it settles.
for _ in $(seq "${FRAMES:-60}"); do v frame >/dev/null; sleep 0.016; done
v rects >"$out/$name.json"
screencapture -x -o -l "$id" "$out/$name.png"
echo "narrow: $out/$name.png $out/$name.json"
