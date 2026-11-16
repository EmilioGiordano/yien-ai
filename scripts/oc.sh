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
# Exit codes: 2 setup error; 1 no session id found in the log (after printing
# what was found); otherwise the opencode exit code (or 0 on success).
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
[ -n "$slot" ] || { echo "oc.sh: $slot_file is empty; ask the orchestrator for a slot" >&2; exit 2; }
log_dir="$slot/oc-logs"
mkdir -p "$log_dir"
log_file="$log_dir/$role-$(date +%Y%m%d-%H%M%S)-$$.jsonl"

# Rule order matters: OpenCode applies the LAST matching rule, so "*" comes
# first and the specifics after it. The reviewer is read-only for the code
# and may write only REPORT-<topic>.md (the brief says the same).
if [ "$role" = "reviewer" ]; then
  cat > "$worktree/opencode.json" <<'EOF'
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "edit": {
      "*": "deny",
      "REPORT-*.md": "allow"
    },
    "webfetch": "allow",
    "websearch": "allow",
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
    "webfetch": "allow",
    "websearch": "allow",
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

parse_rc=0
node - "$log_file" <<'NODE' || parse_rc=$?
const fs = require('fs');
const file = process.argv[2];
const lines = fs.readFileSync(file, 'utf8').split(/\r?\n/);
let session = '';
const textByMessage = new Map();
let lastKey = '';
for (const line of lines) {
  const t = line.trim();
  if (!t) continue;
  let obj;
  try { obj = JSON.parse(t); } catch { continue; }
  if (!session && obj && typeof obj === 'object') {
    if (typeof obj.sessionID === 'string' && obj.sessionID) session = obj.sessionID;
    else if (typeof obj.session_id === 'string' && obj.session_id) session = obj.session_id;
    else if (typeof obj.sessionId === 'string' && obj.sessionId) session = obj.sessionId;
  }
  const part = obj && typeof obj === 'object' ? obj.part : null;
  if (!part || typeof part !== 'object') continue;
  if (part.type !== 'text' || typeof part.text !== 'string' || !part.text) continue;
  const key = typeof part.messageID === 'string' && part.messageID ? part.messageID
    : (typeof part.message_id === 'string' && part.message_id ? part.message_id : '\0');
  textByMessage.set(key, (textByMessage.get(key) || '') + part.text);
  lastKey = key;
}
if (!session) {
  console.error('oc.sh: no session id found in ' + file);
  process.exitCode = 1;
}
process.stdout.write('session: ' + (session || 'unknown') + '\n');
const text = lastKey ? (textByMessage.get(lastKey) || '') : '';
process.stdout.write(text);
if (text && !text.endsWith('\n')) process.stdout.write('\n');
NODE

if [ "$rc" -ne 0 ]; then
  echo "oc.sh: opencode exited with $rc; full log at $log_file" >&2
  exit "$rc"
fi
if [ "$parse_rc" -ne 0 ]; then exit 1; fi
