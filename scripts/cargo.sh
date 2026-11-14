#!/usr/bin/env bash
# Runs cargo in the current worktree behind a machine-wide memory gate.
# Usage: scripts/cargo.sh [--full] <cargo args...>
#   --full  waits for 4000 MB free instead of 2000 MB (full gate, release builds)
# The target dir comes from .yien-slot in the worktree root (untracked):
#   one line, <build-root>/slot-<n>
# The build root and the lock come from YIEN_BUILD_ROOT and YIEN_BUILD_LOCK,
# read from the environment or else from .yien-local in the main checkout
# (see .yien-local.example).
# Exit codes: 2 setup error, 75 gate not acquired within YIEN_GATE_MAX_WAIT
# seconds (default 3600); otherwise cargo's own exit code.
set -euo pipefail

min_mb=2000
jobs=2
if [ "${1:-}" = "--full" ]; then min_mb=4000; jobs=4; shift; fi
max_wait=${YIEN_GATE_MAX_WAIT:-3600}
stale_after=$((4 * 3600))

root=$(git rev-parse --show-toplevel)
slot_file="$root/.yien-slot"
if [ ! -f "$slot_file" ]; then
  echo "cargo.sh: $slot_file is missing; ask the orchestrator for a slot" >&2
  exit 2
fi
target_dir=$(tr -d '\r\n' < "$slot_file")

# .yien-local lives in the main checkout, the parent of the common git dir.
main_root=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")
local_file="$main_root/.yien-local"
# Parsed, not sourced, so the file cannot run code. The last line of a key wins.
local_value() {
  [ -f "$local_file" ] || return 0
  tr -d '\r' < "$local_file" | sed -n "s/^[[:space:]]*$1=//p" | tail -n 1 \
    | sed -e 's/[[:space:]]*$//' -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/"
}
build_root=${YIEN_BUILD_ROOT:-$(local_value YIEN_BUILD_ROOT)}
build_root=${build_root%/}
lock=${YIEN_BUILD_LOCK:-$(local_value YIEN_BUILD_LOCK)}
if [ -z "$lock" ] && [ -n "$build_root" ]; then lock="$build_root/yien-build.lock"; fi
if [ -z "$lock" ]; then
  echo "cargo.sh: the build lock location is not set. Copy .yien-local.example to" \
    "$local_file and set YIEN_BUILD_ROOT (or YIEN_BUILD_LOCK), or export them." >&2
  exit 2
fi
if [ ! -d "$(dirname "$lock")" ]; then
  echo "cargo.sh: the folder of the build lock $lock does not exist; create it or fix $local_file" >&2
  exit 2
fi
if [ -n "$build_root" ]; then
  lower_slashes() { printf '%s' "$1" | tr '\\' '/' | tr '[:upper:]' '[:lower:]'; }
  case "$(lower_slashes "$target_dir")" in
    "$(lower_slashes "$build_root")"/*) ;;
    *) echo "cargo.sh: target dir $target_dir is outside the build root $build_root" >&2; exit 2 ;;
  esac
fi

ps_int() {
  local v
  v=$(powershell -NoProfile -Command "$1" 2>/dev/null | tr -d '\r ' || true)
  case "$v" in ''|*[!0-9]*) echo "-1" ;; *) echo "$v" ;; esac
}
avail_mb() { ps_int "(Get-CimInstance Win32_PerfFormattedData_PerfOS_Memory).AvailableMBytes"; }
cargo_count() { ps_int "(Get-Process cargo,rustc -ErrorAction SilentlyContinue | Measure-Object).Count"; }
# Alive unless PowerShell positively answers 0; an unreadable answer counts as alive.
win_pid_dead() { [ "$(ps_int "if (Get-Process -Id $1 -ErrorAction SilentlyContinue) { 1 } else { 0 }")" = "0" ]; }

break_stale_lock() {
  [ -d "$lock" ] || return 0
  local owner="$lock/owner" pid since now
  if [ -f "$owner" ]; then
    read -r pid since < "$owner" || true
  fi
  now=$(date +%s)
  if [ -z "${pid:-}" ] || [ -z "${since:-}" ]; then
    # Lock without an owner file: only break it once it is old.
    since=$(stat -c %Y "$lock" 2>/dev/null || echo "$now")
    if [ $((now - since)) -gt 600 ]; then rm -rf "$lock"; echo "cargo.sh: removed ownerless lock" >&2; fi
    return 0
  fi
  if win_pid_dead "$pid" || [ $((now - since)) -gt "$stale_after" ]; then
    rm -rf "$lock"
    echo "cargo.sh: removed stale lock of pid $pid" >&2
  fi
}

my_winpid=$(cat /proc/$$/winpid 2>/dev/null || echo "$$")
waited=0
while true; do
  break_stale_lock
  a=$(avail_mb)
  c=$(cargo_count)
  if [ "$a" -lt 0 ] || [ "$c" -lt 0 ]; then
    echo "cargo.sh: could not read memory or process count from PowerShell" >&2
  elif [ "$a" -ge "$min_mb" ] && [ "$c" -eq 0 ] && mkdir "$lock" 2>/dev/null; then
    echo "$my_winpid $(date +%s)" > "$lock/owner"
    break
  fi
  if [ "$waited" -ge "$max_wait" ]; then
    echo "cargo.sh: gate not acquired after ${waited}s (free ${a} MB, need ${min_mb}; cargo processes ${c})" >&2
    exit 75
  fi
  if [ $((waited % 300)) -eq 0 ]; then
    echo "cargo.sh: waiting for the build gate (free ${a} MB, need ${min_mb}; cargo processes ${c})" >&2
  fi
  sleep 20
  waited=$((waited + 20))
done
release_lock() {
  local pid=""
  read -r pid _ < "$lock/owner" 2>/dev/null || true
  # The lock may have been broken and retaken by another build.
  if [ "$pid" = "$my_winpid" ]; then rm -rf "$lock"; fi
}
trap release_lock EXIT INT TERM

CARGO_TARGET_DIR="$target_dir" CARGO_BUILD_JOBS="$jobs" cargo "$@"
