#!/usr/bin/env bash
# The jury's pictures: every state, dark and light, from one
# background launch per state at a 1440 x 900 pt window. Each launch has its own control socket,
# settings file and drafts folder under a scratch dir, and is driven only by control verbs
# (see README.md). Never takes focus, never touches the real app's socket, settings or drafts.
#
#   scripts/look/shots.sh [binary] [outdir]
#   binary: target/fast/finch   outdir: /tmp/finch-shots-<stamp>
# Writes <outdir>/<state>-<dark|light>.png. A state the app can't reach prints `skip <state>: <why>`.
# When screencapture fails (or SHOTS_SNAPSHOT=1) each picture comes from `debug snapshot` instead,
# printed as `shot <name> (snapshot)`, with SNAPSHOT.txt in outdir naming what that leaves out.
set -uo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bin=${1:-$root/target/fast/finch}
out=${2:-/tmp/finch-shots-$(date +%Y%m%d-%H%M%S)}
[[ -x "$bin" ]] || { echo "shots: no $bin; build it first" >&2; exit 1; }
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
# Every launch goes through the signed LSUIElement bundle (scripts/bg-launch.sh); `bin` then names
# the bundle's copy, which the `debug` verbs run from.
source "$root/scripts/bg-launch.sh"
orig_bin=$bin
mkdir -p "$out"

winid="$root/target/window-id"
if [[ ! -x "$winid" || "$root/scripts/window-id.swift" -nt "$winid" ]]; then
    mkdir -p "$root/target"
    swiftc -O -o "$winid" "$root/scripts/window-id.swift" || exit 1
fi

scratch=$(mktemp -d /tmp/gpui-shots.XXXXXX) # short: a unix socket path tops out near 100 bytes
pid=""
cleanup() {
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null
    rm -rf "$scratch"
}

trap cleanup EXIT
# Lists a pid's windows as `<id> <w>x<h> <title>`, to find the Settings window beside the main one.
cat >"$scratch/ids.swift" <<'EOF'
import CoreGraphics
import Foundation
guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]) else { exit(2) }
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
let levels = [0, Int(CGWindowLevelForKey(.minimumWindow))]
for w in windows {
    guard (w[kCGWindowOwnerPID as String] as? Int32) == pid,
          levels.contains(w[kCGWindowLayer as String] as? Int ?? 1),
          let b = w[kCGWindowBounds as String] as? [String: Any], (b["Width"] as? Double ?? 0) > 100,
          let n = w[kCGWindowNumber as String] as? Int else { continue }
    print("\(n)\t\(Int(b["Width"] as? Double ?? 0))x\(Int(b["Height"] as? Double ?? 0))\t\(w[kCGWindowName as String] as? String ?? "")")
}
EOF
swiftc -O -o "$scratch/ids" "$scratch/ids.swift" || exit 1

# ---- fixtures -------------------------------------------------------------------------------
fix=$scratch/fixture
mkdir -p "$fix/src" "$fix/docs/assets"
cat >"$fix/src/main.rs" <<'EOF'
//! A small tokenizer, as a Rust file at rest.
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Word(String),
    Number(f64),
    Symbol(char),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Word(w) => write!(f, "{w}"),
            Token::Number(n) => write!(f, "{n}"),
            Token::Symbol(c) => write!(f, "{c}"),
        }
    }
}

/// Splits `input` into tokens; whitespace separates and is dropped.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c.is_ascii_digit() {
            let mut text = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() || d == '.' {
                    text.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token::Number(text.parse().unwrap_or(0.0)));
        } else if c.is_alphabetic() {
            let mut text = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_alphanumeric() || d == '_' {
                    text.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token::Word(text));
        } else {
            tokens.push(Token::Symbol(c));
            chars.next();
        }
    }
    tokens
}

fn main() {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for token in tokenize("let total = 42 + count_items * 3.5;") {
        *counts.entry(token.to_string()).or_default() += 1;
    }
    let mut pairs: Vec<_> = counts.into_iter().collect();
    pairs.sort();
    for (token, n) in pairs {
        println!("{token}: {n}");
    }
}
EOF
cp "$fix/src/main.rs" "$fix/src/lib.rs"
cp "$fix/src/main.rs" "$fix/src/tokens_and_counting_helpers.rs"
python3 - "$fix/docs/assets/pic.png" <<'EOF'
import sys, zlib, struct
w, h = 320, 160
rows = b"".join(b"\x00" + bytes(v for x in range(w) for v in (x * 255 // w, y * 255 // h, 180)) for y in range(h))
def chunk(t, d):
    c = struct.pack(">I", len(d)) + t + d
    return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
EOF
cat >"$fix/docs/guide.md" <<'EOF'
# Getting started with the tokenizer

A short guide, with **bold**, *italic* and `inline code` in a paragraph that is long enough to wrap onto a second line in the preview pane.

## Steps

- Install the toolchain
- Build the project
  - Debug first
  - Release after
- Run the tests

1. Open a file
2. Press Cmd-F
3. Type a query

## Code

```rust
fn main() {
    let tokens = tokenize("let x = 1;");
    println!("{}", tokens.len());
}
```

## Table

| Name   | Kind   | Count |
| ------ | ------ | ----: |
| Word   | text   |    12 |
| Number | digits |     4 |
| Symbol | other  |     9 |

## Image

![A gradient](assets/pic.png)

> A quote closes the page.
EOF
cp "$fix/docs/guide.md" "$fix/README.md"
deep=$fix/notes-for-the-quarterly-planning-review/design-explorations-and-long-form-writing/archive-of-superseded-drafts-2025/final-reviewed-versions
mkdir -p "$deep" "$fix/notes-for-the-quarterly-planning-review/meeting-minutes-and-follow-ups"
for n in "an-unusually-long-file-name-that-keeps-going-on-and-on.md" "short.txt" "second-draft-of-the-planning-summary-v2.md" "data.json"; do
    echo "# $n" >"$deep/$n"
done
echo "# minutes" >"$fix/notes-for-the-quarterly-planning-review/meeting-minutes-and-follow-ups/minutes-2025-10-30.md"
cp "$fix/src/main.rs" "$deep/example.rs"
printf '# generated\n' >"$fix/Cargo.lock"
printf 'target\n' >"$fix/.gitignore"
(cd "$fix" && git init -q && git add -A >/dev/null 2>&1 && git -c user.name=shots -c user.email=shots@example.com commit -qm fixture >/dev/null 2>&1) || true

rust=$fix/src/main.rs
md=$fix/docs/guide.md
deepdir=$deep

# ---- launch and drive -----------------------------------------------------------------------
n=0
launch() { # <state> [file]: a fresh background launch with its own socket, settings and drafts
    state=$1
    shift
    [[ $n -gt 0 ]] && sleep 3
    n=$((n + 1))
    local dir=$scratch/$state
    mkdir -p "$dir/drafts"
    printf '[window]\nwidth = 1440.0\nheight = 900.0\ntheme = "dark"\n' >"$dir/settings.toml"
    # A launch with no file roots the tree at the home folder; walking it blocked the app's main
    # thread in opendir (a protected folder), so the window never showed (finch#nftg). Root it at the fixture.
    [[ $# -eq 0 ]] && printf '\n[explorer]\nlast_root = "%s"\n' "${launch_root:-$fix}" >>"$dir/settings.toml"
    export GPUI_EDITOR_CONTROL=$dir/control.sock
    export GPUI_EDITOR_SETTINGS=$dir/settings.toml
    export GPUI_EDITOR_DRAFTS=$dir/drafts
    export GPUI_EDITOR_MENU_DUMP=$dir/menu.txt
    BG_LOG=$dir/log bg_launch "$orig_bin" -- "$@" || { echo "skip $state: launch failed" >&2; pid=""; return 1; }
    pid=$BG_PID
    bin=$BG_BIN
    id=""
    for _ in $(seq 100); do
        id=$("$winid" "$pid" || true)
        [[ -n "$id" ]] && break
        kill -0 "$pid" 2>/dev/null || { echo "skip $state: app exited before its window showed" >&2; pid=""; return 1; }
        sleep 0.1
    done
    [[ -n "$id" ]] || { echo "skip $state: no window after 10 s" >&2; return 1; }
    for _ in $(seq 50); do "$bin" debug launch >/dev/null 2>&1 && break; sleep 0.1; done
    v draw-as-key on >/dev/null || { echo "skip $state: draw-as-key refused" >&2; return 1; }
}
v() { "$bin" debug "$@"; } # one verb; prints its reply
stop() {
    [[ -n "$pid" ]] && kill "$pid" 2>/dev/null
    pid=""
}
# SHOTS_SNAPSHOT=1 takes every picture with `debug snapshot`; without it, snapshot is the fallback when
# screencapture fails. `snap_window` is "settings" while the Settings window is the subject.
snap_window=""
snapped=""
settle() { v frame >/dev/null; sleep 0.3; v frame >/dev/null; }
capture() { # <state> <theme>: the picture of the window as it is, drawn as a key window
    v draw-as-key on >/dev/null
    if [[ -z "${SHOTS_SNAPSHOT:-}" ]] && screencapture -x -o -l "$id" "$out/$1-$2.png" 2>/dev/null; then
        echo "shot $1-$2"
    elif v snapshot "$out/$1-$2.png" $snap_window >/dev/null; then
        # Metal's offscreen render of the last frame, not a screen capture (finch#7nbd).
        echo "shot $1-$2 (snapshot)"
        snapped=1
    else
        echo "skip $1-$2: screencapture and snapshot failed" >&2
    fi
}
cap() { # <state> <theme>
    v theme "$2" >/dev/null
    settle
    capture "$1" "$2"
}
both() { cap "$1" dark; cap "$1" light; }
step() { # a verb that must work, else the state is skipped
    local reply
    reply=$(v "$@" 2>&1) || { echo "skip $state: debug $* -> $reply" >&2; return 1; }
}

run() { # <state> <file or empty>: launch, then the state's function, then stop
    local st=$1 file=${2:-}
    [[ -z "${SHOTS_ONLY:-}" || " $SHOTS_ONLY " == *" $st "* ]] || return 0
    snap_window=""
    if launch "$st" ${file:+"$file"}; then
        "state_${st//-/_}" || true
    fi
    stop
}

state_rust_rest() { both rust-rest; }
state_markdown_preview() {
    step action toggle-preview || return 1
    both markdown-preview
}
state_explorer_deep() {
    step explorer root "$fix" || return 1
    step explorer click "$deepdir/short.txt" || return 1
    step open-tab "$deepdir/short.txt" || return 1
    step open-tab "$deepdir/example.rs" || return 1
    step open-tab "$deepdir/data.json" || return 1
    step open-tab "$deepdir/an-unusually-long-file-name-that-keeps-going-on-and-on.md" || return 1
    step pin 1 || return 1
    step tab 2 && step pin 2
    step tab 4
    step explorer click "$deepdir/second-draft-of-the-planning-summary-v2.md"
    both explorer-deep
}
state_split() {
    step open-tab "$fix/src/lib.rs" || return 1
    step split right || return 1
    both split
}
state_find_matches() {
    step find --open "token" || return 1
    both find-matches
}
state_explorer_filter() {
    step explorer root "$fix" || return 1
    step explorer filter "draft" || return 1
    both explorer-filter
}
state_rename_error() { # the theme change ends an open name field, so each theme renames afresh
    step explorer root "$fix" || return 1
    local theme reply
    for theme in dark light; do
        v theme "$theme" >/dev/null
        settle
        reply=$(v explorer rename "$fix/README.md" "->" "bad/name" 2>&1) || true
        [[ $reply == err* ]] || { v explorer focus-rename "$fix/README.md" >/dev/null; v type "bad/name" >/dev/null; }
        settle
        capture rename-error "$theme"
    done
}
state_tab_drag() { # a tab held over the other pane's bar: the strip parts, leaving the gap where it lands beside the pane's own tab
    # The held ghost has its left edge at the bar's middle and covers whatever tab follows the gap, so the target pane holds one
    # tab: the gap opens after it, at the end, and the ghost covers the gap, not a neighbor.
    step open-tab "$fix/src/lib.rs" || return 1
    step open-tab "$fix/README.md" || return 1
    step split right || return 1
    step drag-hold tab:1:1 2 bar || return 1
    both tab-drag
    v drag-release >/dev/null
}
state_empty_window() { # the launch with no file opens an Untitled tab; closing it leaves the empty pane, over a tree rooted at an empty folder
    step close-tab || return 1
    both empty-window
}
state_settings() { # the Settings window (finch#wg9e), not settings.toml in a tab; it keeps its own size
    step settings general || return 1
    local new=""
    for _ in $(seq 40); do # its id shows up about 2 s after the verb
        new=$("$scratch/ids" "$pid" | cut -f1 | grep -vx "$id" | head -n 1 || true)
        [[ -n "$new" ]] && break
        sleep 0.1
    done
    [[ -n "$new" ]] || { echo "skip $state: no Settings window after 4 s" >&2; return 1; }
    id=$new
    snap_window=settings
    both settings
}

run rust-rest "$rust"
run markdown-preview "$md"
run explorer-deep "$deepdir/short.txt"
run split "$rust"
run find-matches "$rust"
run explorer-filter "$rust"
run rename-error "$rust"
run tab-drag "$rust"
launch_root=$scratch/empty-folder # Finch always shows a tree (a fresh window with no folder roots it at home), so the empty window has an empty one
mkdir -p "$launch_root"
run empty-window ""
launch_root=""
run settings "$rust"

if [[ -n "$snapped" ]]; then
    # What a snapshot leaves out against a screen capture, beside the pictures.
    cat >"$out/SNAPSHOT.txt" <<'EOF2'
Files marked (snapshot) came from `finch debug snapshot` (Metal's offscreen render of the window's last
frame at device scale), not from screencapture. Against a real capture they leave out: the sidebar
material (the see-through explorer column and top band get a flat gray, 38 dark or 236 light, not the
blurred tinted desktop), the traffic
lights and title bar, native layers such as the caret layer, and the window shadow and rounded corners.
EOF2
    echo "shots: pictures marked (snapshot) are an offscreen render; see $out/SNAPSHOT.txt"
fi
echo "shots: $out"
ls "$out"
