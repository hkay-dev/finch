#!/usr/bin/env bash
# Opens Finch in the background on a file, captures its window and quits it.
# Never takes focus and never touches the user's settings (a scratch settings file
# unless GPUI_EDITOR_SETTINGS is set). Its control socket
# is under /tmp, and `frame` runs before the capture. SHOTS_SNAPSHOT=1, or a failed screencapture,
# takes the picture with `debug snapshot` (an offscreen render).
#
#   scripts/shot.sh <file> <out.png> [binary]    binary: target/fast/finch
set -euo pipefail
[[ $# -ge 2 ]] || { echo "usage: scripts/shot.sh <file> <out.png> [binary]" >&2; exit 2; }
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
file=$1
out=$2
bin=${3:-$root/target/fast/finch}
[[ -x "$bin" ]] || { echo "shot: no $bin; build it first" >&2; exit 1; }

winid="$root/target/window-id"
if [[ ! -x "$winid" || "$root/scripts/window-id.swift" -nt "$winid" ]]; then
    mkdir -p "$root/target"
    swiftc -O -o "$winid" "$root/scripts/window-id.swift"
fi

settings=${GPUI_EDITOR_SETTINGS:-$(mktemp -t finch-settings)}
# A socket of its own per launch, so shots never talk to another app.
export GPUI_EDITOR_CONTROL=${GPUI_EDITOR_CONTROL:-$(mktemp -u -t finch-control).sock}
# Starts through the LSUIElement bundle (scripts/bg-launch.sh), never the bare binary.
source "$root/scripts/bg-launch.sh"
export GPUI_EDITOR_SETTINGS=$settings
file=$(cd "$(dirname "$file")" && pwd)/$(basename "$file")
bg_launch "$bin" -- "$file"
pid=$BG_PID
bin=$BG_BIN
trap 'kill "$pid" 2>/dev/null || true; rm -f "$GPUI_EDITOR_CONTROL"' EXIT

id=""
for _ in $(seq 100); do
    id=$("$winid" "$pid" || true)
    [[ -n "$id" ]] && break
    kill -0 "$pid" 2>/dev/null || { echo "shot: finch exited before its window showed" >&2; exit 1; }
    sleep 0.1
done
[[ -n "$id" ]] || { echo "shot: no window after 10 s" >&2; exit 1; }
# Force one frame and wait for it: GPUI stops drawing a covered window, so a
# capture alone can be stale. If the verb fails, a pause stands in.
"$bin" debug frame >/dev/null || sleep 1
# When screencapture fails (or SHOTS_SNAPSHOT=1), `debug snapshot` writes Metal's offscreen render of
# the window's last frame instead: no sidebar material, traffic lights, caret layer or shadow.
if [[ -z "${SHOTS_SNAPSHOT:-}" ]] && screencapture -x -o -l "$id" "$out" 2>/dev/null; then
    echo "shot: $out (window $id, pid $pid)"
else
    "$bin" debug snapshot "$out" >/dev/null || { echo "shot: screencapture and snapshot failed" >&2; exit 1; }
    echo "shot: $out (snapshot, window $id, pid $pid)"
fi
