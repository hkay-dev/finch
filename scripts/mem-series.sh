#!/usr/bin/env bash
# Footprint total plus the graphics line at several times after launch, one process.
# usage: mem-series.sh <binary> [absolute path ...]   (gpui-editor#szw6)
# Starts through the LSUIElement bundle (scripts/bg-launch.sh), never the bare binary (gpui-editor#a6gn).
source "$(dirname "$0")/bg-launch.sh"
bin=$1; shift
bg_launch "$bin" -- "$@" || exit 1
pid=$BG_PID
t=0
for s in 1 2 4 8 15 30; do
  sleep $((s - t)); t=$s
  f=$(footprint "$pid" 2>&1)
  tot=$(echo "$f" | sed -n 's/.*Footprint: \(.*\) (16384.*/\1/p')
  g=$(echo "$f" | grep "unmapped) (graphics)" | awk '{print $1 $2 " dirty, " $5 $6 " reclaimable"}')
  echo "t=${s}s total=$tot graphics: $g"
done
kill "$pid"
