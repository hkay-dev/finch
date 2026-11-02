#!/usr/bin/env bash
# The one way an agent starts a Finch build (gpui-editor#a6gn). A bare binary from target/ is
# a Regular app: it shows in the Dock and Cmd-Tab and can take the front. This wraps the binary in an
# LSUIElement bundle (id dev.hkay.finch.dev, one fixed path per worktree, rebuilt only when the
# binary changes), signs it (scripts/sign-identity.sh picks the identity), and starts it with `open -n -g -j`, so it
# never becomes the front app, takes the keyboard or switches Spaces.
#
#   scripts/bg-launch.sh <binary> [KEY=VAL ...] [-- <args>]     prints `pid=<n>` and `bin=<bundle exe>`
#   source scripts/bg-launch.sh; bg_launch <binary> [KEY=VAL ...] [-- <args>]   sets BG_PID, BG_BIN
#
# GPUI_EDITOR_BACKGROUND=1 is always set, and every GPUI_EDITOR_* variable in the caller's
# environment is passed on (`open` starts the app from launchd, which would drop them). Args must be
# absolute paths: the app starts in `/`. Run `debug <verb>` with BG_BIN, the signed copy. Stdout and
# stderr go to $BG_LOG (default: a temp file, printed as `log=<path>`). Quit by PID (`kill -TERM`).
#
# While any display shows a full-screen app's Space (someone watching a video), it starts nothing and
# returns 3: a window ordered in there lands in front of that app (finch#jxb8). Prove the change
# some other way (unit tests, counts) or try again later.

# Returns 0 when it's safe to order a test window in; prints why not and returns 3 otherwise.
bg_launch_safe() {
    local root shown
    root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    if ! shown=$(osascript -l JavaScript "$root/scripts/full-screen-space.js" 2>&1); then
        echo "bg-launch: can't tell whether an app is full screen ($shown); not launching" >&2
        return 3
    fi
    if [[ -n $shown ]]; then
        echo "bg-launch: an app is full screen on a display; not launching a test window over it (finch#jxb8)" >&2
        return 3
    fi
}

bg_launch() {
    local bin=${1:?bg_launch <binary> [KEY=VAL ...] [-- <args>]}
    shift
    local root
    root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    [[ -x "$bin" ]] || { echo "bg-launch: no $bin; build it first" >&2; return 1; }
    bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")

    local app="$root/target/bg/Finch Dev.app"
    local exe="$app/Contents/MacOS/finch"
    if [[ ! -x "$exe" ]] || ! cmp -s "$bin" "$exe"; then
        mkdir -p "$app/Contents/MacOS"
        rm -f "$exe" # a new file, so a copy still running keeps its own
        cp "$bin" "$exe"
        cat >"$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>finch</string>
<key>CFBundleIdentifier</key><string>dev.hkay.finch.dev</string>
<key>CFBundleName</key><string>Finch Dev</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0</string>
<key>LSUIElement</key><true/>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
        source "$root/scripts/sign-identity.sh"
        codesign --force -s "$SIGN_IDENTITY" --identifier dev.hkay.finch.dev "$app" >/dev/null 2>&1 \
            || { echo "bg-launch: codesign with \"$SIGN_IDENTITY\" failed; not starting an unsigned build" >&2; rm -rf "$app"; return 1; }
    fi

    local envs=(--env GPUI_EDITOR_BACKGROUND=1) kv
    while IFS= read -r kv; do
        [[ $kv == GPUI_EDITOR_BACKGROUND=* ]] || envs+=(--env "$kv")
    done < <(env | grep '^GPUI_EDITOR_')
    while [[ $# -gt 0 && $1 != -- ]]; do envs+=(--env "$1"); shift; done
    [[ ${1:-} == -- ]] && shift
    # Finch gives a folder that comes after a file its own window, and a path that doesn't exist
    # its own window too (a new file), both on purpose. A test launch wants the one window that
    # `resize` and shots both hit (finch#1mpw), so the first folder goes first and the files join
    # its window as tabs; a missing path is only reported. Args with a `-` option stay as given.
    local arg dir="" opts=""
    for arg in "$@"; do
        [[ $arg == -* ]] && opts=1
        [[ -z $dir && $arg == /* && -d $arg ]] && dir=$arg
        [[ $arg == /* && ! -e $arg ]] && echo "bg-launch: $arg doesn't exist; Finch opens a new window for it" >&2
    done
    if [[ -n $dir && -z $opts && $1 != "$dir" ]]; then
        local rest=()
        for arg in "$@"; do [[ $arg == "$dir" ]] || rest+=("$arg"); done
        set -- "$dir" "${rest[@]}"
    fi

    BG_LOG=${BG_LOG:-$(mktemp -t finch-bg)}
    bg_launch_safe || return 3
    local before
    before=$(pgrep -f "$exe" | sort || true)
    open -n -g -j "$app" --stdout "$BG_LOG" --stderr "$BG_LOG" "${envs[@]}" --args "$@" || return 1
    BG_PID=""
    local _ pid
    for _ in $(seq 100); do
        for pid in $(pgrep -f "$exe" || true); do
            grep -qx "$pid" <<<"$before" || { BG_PID=$pid; break 2; }
        done
        sleep 0.1
    done
    [[ -n $BG_PID ]] || { echo "bg-launch: the app never started (log $BG_LOG)" >&2; return 1; }
    BG_BIN=$exe
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    set -euo pipefail
    bg_launch "$@"
    echo "pid=$BG_PID"
    echo "bin=$BG_BIN"
    echo "log=$BG_LOG"
fi
