#!/usr/bin/env bash
# Delegate a task or brief to an OpenCode worker (implementer, reviewer or
# test-engineer) running in a YIEN worktree.
# Usage: scripts/oc.sh <worktree> <role> <task-or-brief-file> [session-id]
#   role  implementer | reviewer | test-engineer
# The model comes from YIEN_OC_MODEL (default opencode/inkling-2-small-free#high).
# Writes <worktree>/opencode.json (overwritten every run) with the role's
# permissions, and saves the full JSON log to <slot>/oc-logs/<role>-<ts>.jsonl,
# where the slot comes from <worktree>/.yien-slot.
# stdout: one "session: <id>" line, then the final assistant text. Nothing else.
# Exit codes: 2 setup error; otherwise the opencode exit code (or 0 on success).
set -euo pipefail

usage() {
  echo "usage: scripts/oc.sh <worktree> <role> <task-or-brief-file> [session-id]" >&2
  echo "  role: implementer | reviewer | test-engineer" >&2
  exit 2
}

[ $# -ge 3 ] && [ $# -le 4 ] || usage
worktree=$1
role=$2
task_file=$3
session_id=${4:-}

case "$role" in
  implementer|reviewer|test-engineer) ;;
  *) usage ;;
esac

[ -d "$worktree" ] || { echo "oc.sh: worktree $worktree is not a directory" >&2; exit 2; }
[ -f "$task_file" ] || { echo "oc.sh: task/brief file $task_file does not exist" >&2; exit 2; }
command -v opencode >/dev/null 2>&1 || { echo "oc.sh: opencode is not on PATH" >&2; exit 2; }
command -v node >/dev/null 2>&1 || { echo "oc.sh: node is not on PATH (needed to parse the JSON log)" >&2; exit 2; }

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
brief_file="$script_dir/oc-roles/$role.md"
[ -f "$brief_file" ] || { echo "oc.sh: role brief $brief_file is missing" >&2; exit 2; }

branch=$(git -C "$worktree" rev-parse --abbrev-ref HEAD) \
  || { echo "oc.sh: $worktree is not a git worktree on a branch" >&2; exit 2; }

slot_file="$worktree/.yien-slot"
[ -f "$slot_file" ] || { echo "oc.sh: $slot_file is missing; ask the orchestrator for a slot" >&2; exit 2; }
slot=$(tr -d '\r\n' < "$slot_file")
log_dir="$slot/oc-logs"
mkdir -p "$log_dir"
log_file="$log_dir/$role-$(date +%Y%m%d-%H%M%S).jsonl"

# Rule order matters: OpenCode applies the LAST matching rule, so "*" comes
# first and the denies after it. The reviewer is read-only.
if [ "$role" = "reviewer" ]; then
  cat > "$worktree/opencode.json" <<'EOF'
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "edit": "deny",
    "bash": {
      "*": "allow",
      "git push*": "deny",
      "git merge*": "deny",
      "git reset --hard*": "deny",
      "git branch -D*": "deny",
      "git worktree remove*": "deny",
      "git commit*": "deny",
      "gh *": "deny",
      "cargo *": "deny",
      "rm -rf*": "deny",
      "Remove-Item*": "deny"
    }
  }
}
EOF
else
  cat > "$worktree/opencode.json" <<'EOF'
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "edit": "allow",
    "bash": {
      "*": "allow",
      "git push*": "deny",
      "git merge*": "deny",
      "git reset --hard*": "deny",
      "git branch -D*": "deny",
      "git worktree remove*": "deny",
      "gh *": "deny",
      "cargo *": "deny",
      "rm -rf*": "deny",
      "Remove-Item*": "deny"
    }
  }
}
EOF
fi

prompt=$(cat "$brief_file"; printf '\n\n'; cat "$task_file")
model=${YIEN_OC_MODEL:-opencode/inkling-2-small-free#high}

cmd=(opencode run -m "$model" --auto --archived --title "$branch-$role" --format json)
if [ -n "$session_id" ]; then cmd+=(-s "$session_id"); fi
cmd+=("$prompt")

rc=0
( cd "$worktree" && "${cmd[@]}" < /dev/null ) > "$log_file" 2>&1 || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "oc.sh: opencode exited with $rc; full log at $log_file" >&2
  exit "$rc"
fi

node - "$log_file" <<'NODE'
const fs = require('fs');
const file = process.argv[2];
const lines = fs.readFileSync(file, 'utf8').split(/\r?\n/);
let session = '';
const seen = new Set();
const textParts = [];
function walk(o) {
  if (!o || typeof o !== 'object') return;
  if (Array.isArray(o)) { for (const v of o) walk(v); return; }
  for (const k of Object.keys(o)) {
    const v = o[k];
    if (/^session[_-]?id$/i.test(k) && typeof v === 'string' && !session) session = v;
    if (k === 'text' && typeof v === 'string' && v.length) {
      const t = typeof o.type === 'string' ? o.type : '';
      if (!t || t === 'text' || t === 'message') {
        if (!seen.has(v)) { seen.add(v); textParts.push(v); }
      }
    }
    if (v && typeof v === 'object') walk(v);
  }
}
for (const line of lines) {
  const t = line.trim();
  if (!t) continue;
  let obj;
  try { obj = JSON.parse(t); } catch { continue; }
  walk(obj);
}
if (!session) console.error('oc.sh: no session id found in ' + file);
process.stdout.write('session: ' + (session || 'unknown') + '\n');
const text = textParts.join('');
process.stdout.write(text);
if (text && !text.endsWith('\n')) process.stdout.write('\n');
NODE
