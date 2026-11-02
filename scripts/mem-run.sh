#!/usr/bin/env bash
# Launch a build in the background, wait, print footprint totals and the graphics line, kill it.
# usage: mem-run.sh <wait-secs> <binary> [absolute path ...]   (gpui-editor#szw6)
# Starts through the LSUIElement bundle (scripts/bg-launch.sh), never the bare binary (gpui-editor#a6gn).
source "$(dirname "$0")/bg-launch.sh"
wait=$1; shift
bin=$1; shift
bg_launch "$bin" -- "$@" || exit 1
pid=$BG_PID
sleep "$wait"
echo "load $(sysctl -n vm.loadavg) wait ${wait}s: $bin $*"
footprint "$pid" 2>&1 | grep -E "Footprint:|graphics|IOAccelerator|IOSurface|CoreAnimation|Owned physical|Dirty|untagged"
kill "$pid"
