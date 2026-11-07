#!/usr/bin/env bash
# Runs cargo in the current worktree behind a machine-wide memory gate.
# Usage: scripts/cargo.sh [--full] <cargo args...>
#   --full  waits for 4000 MB free instead of 2000 MB (full gate, release builds)
# The target dir comes from .yien-slot in the worktree root (untracked):
#   one line, e.g. D:/More-Code-Projects/yien-compiled/slot-1
# Builds never share a target dir across branches and never land on C:.
set -euo pipefail

min_mb=2000
if [ "${1:-}" = "--full" ]; then min_mb=4000; shift; fi

root=$(git rev-parse --show-toplevel)
slot_file="$root/.yien-slot"
if [ ! -f "$slot_file" ]; then
  echo "cargo.sh: $slot_file is missing; ask the orchestrator for a slot" >&2
  exit 2
fi
target_dir=$(tr -d '\r\n' < "$slot_file")
case "$target_dir" in
  [Cc]:*) echo "cargo.sh: target dir must not be on C: ($target_dir)" >&2; exit 2 ;;
esac

lock=D:/yien-build.lock

avail_mb() {
  powershell -NoProfile -Command "(Get-CimInstance Win32_PerfFormattedData_PerfOS_Memory).AvailableMBytes" | tr -d '\r '
}
cargo_count() {
  powershell -NoProfile -Command "(Get-Process cargo,rustc -ErrorAction SilentlyContinue | Measure-Object).Count" | tr -d '\r '
}

waited=0
while true; do
  a=$(avail_mb)
  c=$(cargo_count)
  if [ "$a" -ge "$min_mb" ] && [ "$c" = "0" ] && mkdir "$lock" 2>/dev/null; then
    break
  fi
  if [ $((waited % 300)) -eq 0 ]; then
    echo "cargo.sh: waiting for the build gate (free ${a} MB, need ${min_mb}; cargo processes ${c})" >&2
  fi
  sleep 20
  waited=$((waited + 20))
done
trap 'rmdir "$lock" 2>/dev/null || true' EXIT

jobs=2
if [ "$min_mb" -ge 4000 ]; then jobs=4; fi

CARGO_TARGET_DIR="$target_dir" CARGO_BUILD_JOBS="$jobs" cargo "$@"
