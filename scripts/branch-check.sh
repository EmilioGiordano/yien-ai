#!/usr/bin/env bash
# Mechanical checks for a YIEN branch against its base.
# Usage: scripts/branch-check.sh <worktree> [base]
#   base  the ref to diff against (default origin/main)
# Prints one PASS|FAIL|WARN|SKIP line per check plus a final line,
# "branch-check: PASS" or "branch-check: FAIL (n failed)".
# Exit code: number of failed checks, capped at 99 (0 = pass, 2 = usage/setup error).
set -uo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

usage() {
  echo "usage: scripts/branch-check.sh <worktree> [base]" >&2
  exit 2
}

# ---------- pure helpers (unit-tested by scripts/tests/branch-check.test.sh) ----------

commit_lint_ok() { # <subject> <body> -> 0 ok, 1 bad
  printf '%s' "$1" | grep -qE '^(core|feat|fix|ui|docs|test|chore|refactor|perf|build)/[a-z0-9]' || return 1
  [ -z "$2" ] || return 1
  if printf '%s\n%s' "$1" "$2" | grep -iqE 'co-authored-by|generated with'; then return 1; fi
  return 0
}

crates_for_paths() { # paths on stdin -> package names, sorted unique
  local path name
  local -A seen=()
  while IFS= read -r path; do
    case "$path" in
      crates/*)
        name=${path#crates/}
        name=${name%%/*}
        [ -z "$name" ] && continue
        seen[$name]=1 ;;
      xtask/*) seen[xtask]=1 ;;
    esac
  done
  if [ ${#seen[@]} -gt 0 ]; then printf '%s\n' "${!seen[@]}" | LC_ALL=C sort; fi
}

is_forbidden_path() { # <path> -> 0 forbidden
  case "${1##*/}" in
    TASK-*.md|REPORT-*.md|LOOP-*.md|opencode.json|.yien-slot) return 0 ;;
    *) return 1 ;;
  esac
}

# Lists the line numbers that belong to #[cfg(test)] modules of a HEAD file.
test_region_lines() { # <worktree> <path>
  git -C "$1" show "HEAD:$2" | awk '
    function count(s, ch,   n, i) { n = 0; for (i = 1; i <= length(s); i++) if (substr(s, i, 1) == ch) n++; return n }
    /#\[cfg\(test\)\]/ { pending = 1; next }
    pending && /mod[ \t]/ {
      pending = 0
      active = count($0, "{") - count($0, "}")
      if (active > 0) print NR
      next
    }
    active > 0 { active += count($0, "{") - count($0, "}"); print NR }
  '
}

# ---------- checks ----------

main() {
  case "${1:-}" in -h|--help) usage ;; esac
  [ $# -ge 1 ] && [ $# -le 2 ] || usage
  local worktree=$1 base=${2:-origin/main}
  [ -d "$worktree" ] || { echo "branch-check.sh: worktree $worktree is not a directory" >&2; exit 2; }
  git -C "$worktree" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
    || { echo "branch-check.sh: $worktree is not a git worktree" >&2; exit 2; }
  git -C "$worktree" rev-parse --verify --quiet "$base^{commit}" >/dev/null \
    || { echo "branch-check.sh: base ref $base not found" >&2; exit 2; }
  worktree=$(cd "$worktree" && pwd)

  local cargo_sh="$worktree/scripts/cargo.sh"

  local failed=0
  local slot=""
  if [ -f "$worktree/.yien-slot" ]; then slot=$(tr -d '\r\n' < "$worktree/.yien-slot"); fi
  local log_dir
  if [ -n "$slot" ]; then
    log_dir="$slot/branch-check-logs"
    mkdir -p "$log_dir" || { echo "branch-check.sh: cannot create $log_dir" >&2; exit 2; }
  else
    log_dir=$(mktemp -d)
    echo "branch-check.sh: no .yien-slot; logs go to $log_dir" >&2
  fi

  local diff_files committed_files touched
  diff_files=$(git -C "$worktree" diff --name-only "$base...HEAD")
  committed_files=$(git -C "$worktree" log --no-merges --name-only --pretty=format: "$base..HEAD")
  touched=$(printf '%s\n' "$diff_files" | crates_for_paths)

  if [ -n "$touched" ] && [ ! -f "$cargo_sh" ]; then
    echo "branch-check.sh: crates are touched but $cargo_sh is missing" >&2
    exit 2
  fi

  # Keep only summary lines (errors, test results, rustfmt diffs) in the output.
  cargo_check() { # <name> <logfile> <cargo args...>
    local name=$1 log=$2
    shift 2
    local rc=0
    ( cd "$worktree" && bash "$cargo_sh" "$@" ) > "$log" 2>&1 || rc=$?
    if [ $rc -eq 0 ]; then
      echo "$name: PASS"
      grep -iE '\berror|test result|^Diff in' "$log" | tail -n 30 | sed 's/^/  /'
    elif [ $rc -eq 75 ]; then
      echo "$name: FAIL (build gate timeout; not a code failure)"
      grep -iE '\berror|test result|^Diff in' "$log" | tail -n 30 | sed 's/^/  /'
      echo "  log: $log"
      failed=$((failed + 1))
    else
      echo "$name: FAIL"
      grep -iE '\berror|test result|^Diff in' "$log" | tail -n 30 | sed 's/^/  /'
      echo "  log: $log"
      failed=$((failed + 1))
    fi
  }

  if [ -z "$touched" ]; then
    echo "fmt: SKIP (no crates or xtask touched)"
    echo "clippy: SKIP (no crates or xtask touched)"
    echo "test: SKIP (no crates or xtask touched)"
  else
    local pkg_args=() p
    while IFS= read -r p; do [ -n "$p" ] && pkg_args+=(-p "$p"); done <<< "$touched"
    cargo_check fmt "$log_dir/fmt.log" fmt --all -- --check
    cargo_check clippy "$log_dir/clippy.log" clippy "${pkg_args[@]}" --all-targets -- -D warnings
    cargo_check test "$log_dir/test.log" test "${pkg_args[@]}"
  fi

  if grep -q 'check-deps' "$worktree/xtask/src/main.rs" 2>/dev/null; then
    cargo_check check-deps "$log_dir/check-deps.log" xtask check-deps
  else
    echo "check-deps: SKIP (xtask has no check-deps)"
  fi

  # commit lint for base..HEAD (merges excluded: they are never authored by agents)
  local details="" sha subject body
  while IFS= read -r sha; do
    [ -z "$sha" ] && continue
    subject=$(git -C "$worktree" log -1 --format=%s "$sha")
    body=$(git -C "$worktree" log -1 --format=%b "$sha")
    if ! commit_lint_ok "$subject" "$body"; then
      details+="  ${sha:0:10} $subject"$'\n'
    fi
  done < <(git -C "$worktree" rev-list --no-merges "$base..HEAD")
  if [ -z "$details" ]; then
    echo "commit-lint: PASS"
  else
    echo "commit-lint: FAIL"
    printf '%s' "$details"
    failed=$((failed + 1))
  fi

  # unsafe: added lines outside yien-pty/yien-platform fail; inside them a
  # // SAFETY: comment must appear within the 3 lines above the unsafe line.
  details=""
  local cur_file="" new_line=0 line content hunk
  while IFS= read -r line; do
    case "$line" in
      '+++ b/'*) cur_file=${line#+++ b/}; continue ;;
      '+++ '*) cur_file=""; continue ;;
      '@@ '*)
        hunk=${line#*+}; hunk=${hunk%% *}; new_line=${hunk%%,*}; continue ;;
      '+'*)
        content=${line:1}
        if printf '%s' "$content" | grep -qE '\bunsafe\b'; then
          case "$cur_file" in
            crates/yien-pty/*|crates/yien-platform/*)
              local end=$((new_line - 1)) start
              if [ "$end" -lt 1 ]; then
                start=1
              else
                start=$((end - 2)); [ "$start" -lt 1 ] && start=1
              fi
              if ! git -C "$worktree" show "HEAD:$cur_file" | sed -n "${start},${end}p" | grep -q '// SAFETY:'; then
                details+="  $cur_file:$new_line: unsafe without // SAFETY: within 3 lines above"$'\n'
              fi ;;
            *)
              details+="  $cur_file:$new_line: unsafe outside yien-pty/yien-platform"$'\n' ;;
          esac
        fi
        new_line=$((new_line + 1)) ;;
      '-'*) ;;
      *) new_line=$((new_line + 1)) ;;
    esac
  done < <(git -C "$worktree" diff "$base...HEAD" -- '*.rs')
  if [ -z "$details" ]; then
    echo "unsafe: PASS"
  else
    echo "unsafe: FAIL"
    printf '%s' "$details"
    failed=$((failed + 1))
  fi

  # library panics: WARN only, the reviewer judges them
  details=""
  local warns=0
  cur_file=""; new_line=0
  while IFS= read -r line; do
    case "$line" in
      '+++ b/'*) cur_file=${line#+++ b/}; continue ;;
      '+++ '*) cur_file=""; continue ;;
      '@@ '*)
        hunk=${line#*+}; hunk=${hunk%% *}; new_line=${hunk%%,*}; continue ;;
      '+'*)
        content=${line:1}
        case "$cur_file" in
          crates/*/src/*.rs)
            if printf '%s' "$content" | grep -qE '\.unwrap\(\)|\.expect\(|panic!\('; then
              if ! test_region_lines "$worktree" "$cur_file" | grep -qx "$new_line"; then
                warns=$((warns + 1))
                details+="  $cur_file:$new_line: $(printf '%s' "$content" | sed 's/^[[:space:]]*//')"$'\n'
              fi
            fi ;;
        esac
        new_line=$((new_line + 1)) ;;
      '-'*) ;;
      *) new_line=$((new_line + 1)) ;;
    esac
  done < <(git -C "$worktree" diff "$base...HEAD" -- '*.rs')
  if [ "$warns" -eq 0 ]; then
    echo "library-panics: PASS"
  else
    echo "library-panics: WARN ($warns; a reviewer must judge each)"
    printf '%s' "$details"
  fi

  # PowerShell: every touched .ps1/.psm1 must parse with zero errors
  details=""
  local ps_touched=0 f tmp out
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    case "$f" in
      *.ps1|*.psm1) ;;
      *) continue ;;
    esac
    git -C "$worktree" cat-file -e "HEAD:$f" 2>/dev/null || continue
    ps_touched=$((ps_touched + 1))
    tmp=$(mktemp)
    git -C "$worktree" show "HEAD:$f" > "$tmp"
    if ! out=$(powershell -NoProfile -Command "\$e=\$null; [void][System.Management.Automation.Language.Parser]::ParseFile('$tmp', [ref]\$null, [ref]\$e); if (\$e -and \$e.Count -gt 0) { \$e | ForEach-Object { \$_.Message }; exit 1 }" 2>&1); then
      details+="  $f: $out"$'\n'
    fi
    rm -f "$tmp"
  done <<< "$diff_files"
  if [ "$ps_touched" -eq 0 ]; then
    echo "powershell: SKIP (no touched .ps1/.psm1)"
  elif [ -z "$details" ]; then
    echo "powershell: PASS"
  else
    echo "powershell: FAIL"
    printf '%s' "$details"
    failed=$((failed + 1))
  fi

  # forbidden files committed on this branch
  details=""
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    if is_forbidden_path "$f"; then details+="  $f"$'\n'; fi
  done <<< "$committed_files"
  if [ -z "$details" ]; then
    echo "forbidden-files: PASS"
  else
    echo "forbidden-files: FAIL"
    printf '%s' "$details"
    failed=$((failed + 1))
  fi

  if [ "$failed" -eq 0 ]; then
    echo "branch-check: PASS"
    exit 0
  fi
  echo "branch-check: FAIL ($failed failed)"
  if [ "$failed" -gt 99 ]; then failed=99; fi
  exit "$failed"
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
