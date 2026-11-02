#!/usr/bin/env bash
# Shift-Cmd-F with the find bar's replace row open (finch#bfjq): one background launch, driven by
# control verbs, with a capture after each press and of the replace button's tip. The focused
# field shows its ring, so the pictures say where each press left the keyboard.
#
#   scripts/look/find-replace-key.sh [binary] [outdir]
#   binary: target/fast/finch   outdir: /tmp/finch-bfjq-<stamp>
set -uo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bin=${1:-$root/target/fast/finch}
out=${2:-/tmp/finch-bfjq-$(date +%Y%m%d-%H%M%S)}
[[ -x "$bin" ]] || { echo "probe: no $bin; build it first" >&2; exit 1; }
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
source "$root/scripts/bg-launch.sh"
mkdir -p "$out"

winid="$root/target/window-id"
if [[ ! -x "$winid" || "$root/scripts/window-id.swift" -nt "$winid" ]]; then
    mkdir -p "$root/target"
    swiftc -O -o "$winid" "$root/scripts/window-id.swift" || exit 1
fi

scratch=$(mktemp -d /tmp/finch-bfjq.XXXXXX)
pid=""
trap '[[ -n "$pid" ]] && kill "$pid" 2>/dev/null; rm -rf "$scratch"' EXIT
mkdir -p "$scratch/drafts"
printf 'let token = 1;\nlet other = token + 2;\nprint(token);\n' >"$scratch/sample.rs"
printf '[window]\nwidth = 900.0\nheight = 500.0\ntheme = "dark"\n' >"$scratch/settings.toml"
export GPUI_EDITOR_CONTROL=$scratch/control.sock
export GPUI_EDITOR_SETTINGS=$scratch/settings.toml
export GPUI_EDITOR_DRAFTS=$scratch/drafts
export GPUI_EDITOR_RECTS=1
BG_LOG=$scratch/log bg_launch "$bin" -- "$scratch/sample.rs" || exit 1
pid=$BG_PID
bin=$BG_BIN
id=""
for _ in $(seq 100); do
    id=$("$winid" "$pid" || true)
    [[ -n "$id" ]] && break
    sleep 0.1
done
[[ -n "$id" ]] || { echo "probe: no window" >&2; exit 1; }
v() { "$bin" debug "$@"; }
for _ in $(seq 50); do v launch >/dev/null 2>&1 && break; sleep 0.1; done
v draw-as-key on >/dev/null
settle() { v frame >/dev/null; sleep 0.4; v frame >/dev/null; }
shot() { settle; screencapture -x -o -l "$id" "$out/$1.png" && echo "shot $out/$1.png"; }
press() { echo "$1: key $2 -> $(v key "$2")"; shot "$1"; }
# The replace button is the second from the bar's right edge: pad, close button, gap, then its middle.
tip() {
    local rect x y
    rect=$(v rects | /usr/bin/python3 -c 'import json,sys; r=json.load(sys.stdin)["elements"]["find"]; print(r["x"]+r["w"]-4-22-2-11, r["y"]+4+12)')
    read -r x y <<<"$rect"
    v hover off >/dev/null
    v hover find "$x" "$y" >/dev/null
    sleep 0.9
    shot "$1"
}

echo "find --open: $(v find --open token)"
press 1-row-opens shift-cmd-f
tip 2-tip-in-replace
press 3-tab-to-find shift-tab
tip 4-tip-in-find
press 5-to-replace shift-cmd-f
press 6-row-closes shift-cmd-f
# From the text, with the row open: the press goes to the Replace field too.
press 7-row-opens-again shift-cmd-f
echo "click in the text: $(v click 400 300)"
press 8-from-text-to-replace shift-cmd-f
