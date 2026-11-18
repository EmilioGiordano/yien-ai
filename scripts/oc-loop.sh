#!/usr/bin/env bash
# Run the implement -> check -> review -> fix loop on one task.
# Usage: scripts/oc-loop.sh <worktree> <task-file> [--rounds N] [--impl-session ID]
#   --rounds N        max implementer fix rounds (default 3)
#   --impl-session ID continue an existing implementer session instead of starting one
# Prints only: rounds used, the final verdict, the implementer and reviewer
# session ids, the last reviewer reply, and where the logs are.
# Exit: 0 PASS, 1 FAIL after N rounds, 2 setup error.
# Writes <worktree>/LOOP-<topic>.md (gitignored) with each round's verdict and
# findings, where <topic> is the task file's basename without a .md extension
# and without a leading TASK-.
set -uo pipefail

usage() {
  echo "usage: scripts/oc-loop.sh <worktree> <task-file> [--rounds N] [--impl-session ID]" >&2
  exit 2
}

# Runs oc.sh and splits its output into session id and reply.
run_oc() { # <role> <brief-file> [session-id] -> sets session, reply; returns rc
  local role=$1 brief=$2 sid=${3:-}
  local args=("$worktree" "$role" "$brief")
  [ -n "$sid" ] && args+=("$sid")
  local out rc=0
  out=$(bash "$script_dir/oc.sh" "${args[@]}" 2>&1) || rc=$?
  session=""
  reply=""
  local l
  while IFS= read -r l; do
    if [ -z "$session" ] && [[ "$l" == session:\ * ]]; then
      session=${l#session: }
    else
      reply+="$l"$'\n'
    fi
  done <<< "$out"
  return $rc
}

new_brief() { # <name> -> fresh brief file path in log_dir
  mktemp "$log_dir/loop-${1}-XXXXXXXX.md"
}

record() { # <text> -> append to LOOP file
  printf '%s\n' "$1" >> "$loop_file"
}

detect_verdict() { # reviewer reply -> PASS|FAIL|UNKNOWN
  local v
  v=$(printf '%s\n' "$1" | sed -nE 's/^## Verdict: (PASS|FAIL)[[:space:]]*$/\1/p' | head -n1)
  echo "${v:-UNKNOWN}"
}

main() {
[ $# -ge 2 ] || usage
worktree=$1
task_file=$2
shift 2
max_rounds=3
impl_session=""
while [ $# -gt 0 ]; do
  case "$1" in
    --rounds) [ $# -ge 2 ] || usage; max_rounds=$2; shift 2 ;;
    --impl-session) [ $# -ge 2 ] || usage; impl_session=$2; shift 2 ;;
    *) usage ;;
  esac
done
case "$max_rounds" in ''|*[!0-9]*|0) echo "oc-loop.sh: --rounds must be a positive integer" >&2; exit 2 ;; esac

[ -d "$worktree" ] || { echo "oc-loop.sh: worktree $worktree is not a directory" >&2; exit 2; }
[ -f "$task_file" ] || { echo "oc-loop.sh: task file $task_file does not exist" >&2; exit 2; }
git -C "$worktree" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || { echo "oc-loop.sh: $worktree is not a git worktree" >&2; exit 2; }
worktree=$(cd "$worktree" && pwd)

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
[ -f "$script_dir/oc.sh" ] || { echo "oc-loop.sh: $script_dir/oc.sh is missing" >&2; exit 2; }
[ -f "$script_dir/branch-check.sh" ] || { echo "oc-loop.sh: $script_dir/branch-check.sh is missing" >&2; exit 2; }

slot_file="$worktree/.yien-slot"
[ -f "$slot_file" ] || { echo "oc-loop.sh: $slot_file is missing; ask the orchestrator for a slot" >&2; exit 2; }
slot=$(tr -d '\r\n' < "$slot_file")
[ -n "$slot" ] || { echo "oc-loop.sh: $slot_file is empty; ask the orchestrator for a slot" >&2; exit 2; }
log_dir="$slot/oc-logs"
mkdir -p "$log_dir" || { echo "oc-loop.sh: cannot create $log_dir" >&2; exit 2; }

base_name=$(basename "$task_file")
topic=${base_name%.*}
topic=${topic#TASK-}
loop_file="$worktree/LOOP-$topic.md"

{
  echo "# Loop — $topic"
  echo
  echo "- worktree: $worktree"
  echo "- task: $task_file"
  echo "- started: $(date '+%Y-%m-%d %H:%M:%S')"
  echo "- max fix rounds: $max_rounds"
  echo
} > "$loop_file" || { echo "oc-loop.sh: cannot write $loop_file" >&2; exit 2; }

rounds=0
reviewer_session=""
reviewer_reply=""
prev_findings="(first review round)"
verdict="FAIL"

# Step 1: implementer (or continue --impl-session)
impl_brief=$(new_brief impl)
cp "$task_file" "$impl_brief"
run_oc implementer "$impl_brief" "$impl_session"
impl_rc=$?
impl_session=$session
record "## Round 1 — implementer finished (rc=$impl_rc)"
record
if [ $impl_rc -ne 0 ]; then
  echo "oc-loop.sh: implementer run failed (oc.sh exit $impl_rc); see $log_dir" >&2
  record "implementer run failed (oc.sh exit $impl_rc)"
  echo "logs: $log_dir" >&2
  exit 2
fi
if [ -z "$impl_session" ]; then
  echo "oc-loop.sh: no implementer session id found; see $log_dir" >&2
  record "implementer returned no session id"
  exit 2
fi

while true; do
  # Step 2: mechanical checks
  check_out=$(bash "$script_dir/branch-check.sh" "$worktree" 2>&1)
  check_rc=$?
  record "## Round $((rounds + 1)) — branch-check"
  record '```'
  record "$check_out"
  record '```'
  record
  if [ $check_rc -ne 0 ]; then
    if [ $check_rc -eq 2 ] && ! printf '%s' "$check_out" | grep -q '^branch-check:'; then
      echo "oc-loop.sh: branch-check setup error; see $log_dir" >&2
      echo "$check_out" >&2
      record "branch-check setup error"
      exit 2
    fi
    rounds=$((rounds + 1))
    if [ $rounds -gt $max_rounds ]; then
      verdict="FAIL"
      record "stopping: $rounds fix rounds exceeded the limit ($max_rounds); the branch still fails branch-check"
      break
    fi
    fix_brief=$(new_brief fix)
    {
      echo "# Fix brief — round $rounds"
      echo
      echo "The mechanical branch-check failed. Fix every failure below. Do not work around the checks."
      echo
      echo '```'
      echo "$check_out"
      echo '```'
    } > "$fix_brief"
    run_oc implementer "$fix_brief" "$impl_session"
    impl_rc=$?
    impl_session=$session
    record "## Round $((rounds + 1)) — fix from branch-check (rc=$impl_rc)"
    record
    if [ $impl_rc -ne 0 ] || [ -z "$impl_session" ]; then
      echo "oc-loop.sh: implementer fix round failed (oc.sh exit $impl_rc); see $log_dir" >&2
      record "implementer fix round failed (oc.sh exit $impl_rc)"
      exit 1
    fi
    continue
  fi

  # Step 3: strict review
  review_brief=$(new_brief review)
  {
    echo "# Review brief — round $((rounds + 1))"
    echo
    echo "Worktree: $worktree"
    echo "Round: $((rounds + 1)) (fix rounds used so far: $rounds of max $max_rounds)"
    echo
    echo "## TASK"
    echo
    cat "$task_file"
    echo
    echo "## Previous findings and their resolution"
    echo
    echo "$prev_findings"
    echo
    echo "Verify the implementer's fixes in the diff. Do not trust the reply."
  } > "$review_brief"
  run_oc strict-reviewer "$review_brief"
  review_rc=$?
  reviewer_session=$session
  reviewer_reply=$reply
  if [ $review_rc -ne 0 ]; then
    echo "oc-loop.sh: strict-reviewer run failed (oc.sh exit $review_rc); see $log_dir" >&2
    record "strict-reviewer run failed (oc.sh exit $review_rc)"
    exit 2
  fi
  record "## Round $((rounds + 1)) — strict-reviewer session ${reviewer_session:-unknown}"
  record '```'
  record "$reviewer_reply"
  record '```'
  record
  verdict=$(detect_verdict "$reviewer_reply")
  if [ "$verdict" = "PASS" ]; then
    break
  fi

  # Step 4: send the findings back
  rounds=$((rounds + 1))
  if [ $rounds -gt $max_rounds ]; then
    verdict="FAIL"
    record "stopping: $rounds fix rounds exceeded the limit ($max_rounds)"
    break
  fi
  fix_brief=$(new_brief fix)
  {
    echo "# Fix brief — round $rounds"
    echo
    echo "The strict reviewer failed the branch (verdict: $verdict). Address every finding."
    echo "For a finding you believe is wrong, argue against it in REPORT-<topic>.md instead of changing code."
    echo
    echo '```'
    echo "$reviewer_reply"
    echo '```'
  } > "$fix_brief"
  run_oc implementer "$fix_brief" "$impl_session"
  impl_rc=$?
  impl_session=$session
  record "## Round $((rounds + 1)) — fix from review (rc=$impl_rc)"
  record
  if [ $impl_rc -ne 0 ] || [ -z "$impl_session" ]; then
    echo "oc-loop.sh: implementer fix round failed (oc.sh exit $impl_rc); see $log_dir" >&2
    record "implementer fix round failed (oc.sh exit $impl_rc)"
    exit 1
  fi
  prev_findings="$reviewer_reply"
done

record "## Final verdict: $verdict (fix rounds used: $rounds of max $max_rounds)"

echo "rounds: $rounds"
echo "verdict: $verdict"
echo "implementer session: ${impl_session:-unknown}"
echo "reviewer session: ${reviewer_session:-none}"
echo "last reviewer reply:"
printf '%s\n' "$reviewer_reply"
echo "logs: $log_dir"
echo "loop log: $loop_file"

if [ "$verdict" = "PASS" ]; then exit 0; fi
exit 1
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
