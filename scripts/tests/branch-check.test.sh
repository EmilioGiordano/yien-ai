#!/usr/bin/env bash
# Unit tests for the pure helpers in ../branch-check.sh.
set -uo pipefail

test_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=../branch-check.sh
source "$test_dir/../branch-check.sh"

pass=0
fail=0
check() { # <description> <expected 0|1> <actual>
  if [ "$2" = "$3" ]; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    echo "not ok: $1 (expected $2, got $3)"
  fi
}

# commit_lint_ok: <subject> <body>
check "feat ok" 0 "$(commit_lint_ok 'feat/add pane split' ''; echo $?)"
check "core ok" 0 "$(commit_lint_ok 'core/export rows' ''; echo $?)"
check "docs ok" 0 "$(commit_lint_ok 'docs/explain slot rule' ''; echo $?)"
check "test ok" 0 "$(commit_lint_ok 'test/cover layout swap' ''; echo $?)"
check "perf ok" 0 "$(commit_lint_ok 'perf/cache frame diff' ''; echo $?)"
check "build ok" 0 "$(commit_lint_ok 'build/bump gpui' ''; echo $?)"
check "ui ok" 0 "$(commit_lint_ok 'ui/palette empty state' ''; echo $?)"
check "fix ok" 0 "$(commit_lint_ok 'fix/conpty startup deadlock' ''; echo $?)"
check "colon not allowed" 1 "$(commit_lint_ok 'feat: add pane split' ''; echo $?)"
check "no type prefix" 1 "$(commit_lint_ok 'add pane split' ''; echo $?)"
check "type case-sensitive" 1 "$(commit_lint_ok 'Feat/x' ''; echo $?)"
check "uppercase after slash" 1 "$(commit_lint_ok 'fix/ABC' ''; echo $?)"
check "empty after slash" 1 "$(commit_lint_ok 'fix/' ''; echo $?)"
check "empty subject" 1 "$(commit_lint_ok '' ''; echo $?)"
check "body rejected" 1 "$(commit_lint_ok 'feat/x' 'some body'; echo $?)"
check "co-authored-by in body" 1 "$(commit_lint_ok 'feat/x' 'Co-Authored-By: someone <x@y.z>'; echo $?)"
check "generated with in body" 1 "$(commit_lint_ok 'feat/x' 'Generated with Claude Code'; echo $?)"
check "co-authored-by in subject" 1 "$(commit_lint_ok 'feat/x Co-Authored-By: someone' ''; echo $?)"
check "generated with in subject" 1 "$(commit_lint_ok 'feat/generated with opencode' ''; echo $?)"
check "trailer lower-case in body" 1 "$(commit_lint_ok 'feat/x' 'co-authored-by: x'; echo $?)"
check "slash-digits ok" 0 "$(commit_lint_ok 'chore/3 pane split' ''; echo $?)"

# crates_for_paths: paths on stdin -> package names
check "pty maps" 0 "$(printf 'crates/yien-pty/src/lib.rs\n' | crates_for_paths >/dev/null; echo $?)"
check "pty name" "yien-pty" "$(printf 'crates/yien-pty/src/lib.rs\n' | crates_for_paths)"
check "two crates sorted unique" "yien-pty
yien-ui" "$(printf 'crates/yien-ui/src/main.rs\ncrates/yien-pty/src/lib.rs\ncrates/yien-ui/tests/x.rs\n' | crates_for_paths)"
check "crate root file" "yien-ui" "$(printf 'crates/yien-ui/Cargo.toml\n' | crates_for_paths)"
check "xtask maps" "xtask" "$(printf 'xtask/src/main.rs\n' | crates_for_paths)"
check "docs skipped" "" "$(printf 'README.md\ndocs/a.md\nscripts/x.sh\n' | crates_for_paths)"
check "deep path one crate" "yien-vt" "$(printf 'crates/yien-vt/src/osc_scan/scan_state.rs\n' | crates_for_paths)"
check "empty input" "" "$(printf '' | crates_for_paths)"

# is_forbidden_path: <path> -> 0 forbidden
check "task md" 0 "$(is_forbidden_path 'TASK-foo.md'; echo $?)"
check "report md" 0 "$(is_forbidden_path 'REPORT-x.md'; echo $?)"
check "loop md" 0 "$(is_forbidden_path 'LOOP-x.md'; echo $?)"
check "opencode.json" 0 "$(is_forbidden_path 'opencode.json'; echo $?)"
check "opencode.json nested" 0 "$(is_forbidden_path 'sub/opencode.json'; echo $?)"
check "yien-slot" 0 "$(is_forbidden_path '.yien-slot'; echo $?)"
check "normal file" 1 "$(is_forbidden_path 'README.md'; echo $?)"
check "source file" 1 "$(is_forbidden_path 'crates/yien-ui/src/main.rs'; echo $?)"
check "task md suffix" 1 "$(is_forbidden_path 'TASK-foo.md.bak'; echo $?)"

# detect_verdict: reviewer reply -> PASS|FAIL|UNKNOWN; anything but an exact
# "## Verdict: PASS|FAIL" line must not read as PASS
source "$test_dir/../oc-loop.sh"
check "detect template line" "UNKNOWN" "$(detect_verdict '## Verdict: PASS | FAIL')"
check "detect real pass" "PASS" "$(detect_verdict '## Verdict: PASS')"
check "detect real fail" "FAIL" "$(detect_verdict '## Verdict: FAIL')"
check "detect trailing period" "UNKNOWN" "$(detect_verdict '## Verdict: PASS.')"
check "detect empty reply" "UNKNOWN" "$(detect_verdict '')"
check "detect lowercase verdict" "UNKNOWN" "$(detect_verdict '## verdict: pass')"
check "detect pass with trailing spaces" "PASS" "$(detect_verdict '## Verdict: PASS   ')"
check "detect multiline with fail" "FAIL" "$(detect_verdict "$(printf 'intro\n## Verdict: FAIL\nmore')")"
check "detect multiline with pass" "PASS" "$(detect_verdict "$(printf 'intro\n## Verdict: PASS\nmore')")"

# integration: the PowerShell check must be able to pass a real good .ps1 and
# fail a broken one (a POSIX mktemp path made it unparseable before)
ps_repo=$(mktemp -d)
if git -C "$ps_repo" init -q -b master \
  && printf 'base\n' > "$ps_repo/README.md" \
  && git -C "$ps_repo" -c user.email=t@t -c user.name=t add README.md \
  && git -C "$ps_repo" -c user.email=t@t -c user.name=t commit -qm 'chore/init' \
  && git -C "$ps_repo" switch -qc feature \
  && printf 'Write-Host "ok"\n' > "$ps_repo/good.ps1" \
  && git -C "$ps_repo" -c user.email=t@t -c user.name=t add good.ps1 \
  && git -C "$ps_repo" -c user.email=t@t -c user.name=t commit -qm 'test/add a valid ps1'; then
  out=$(bash "$test_dir/../branch-check.sh" "$ps_repo" master 2>&1)
  check "powershell pass on valid ps1" 1 "$(printf '%s\n' "$out" | grep -c '^powershell: PASS$')"
  printf 'Write-Host "unterminated\n' > "$ps_repo/broken.ps1"
  git -C "$ps_repo" -c user.email=t@t -c user.name=t add broken.ps1
  git -C "$ps_repo" -c user.email=t@t -c user.name=t commit -qm 'test/add a broken ps1'
  out=$(bash "$test_dir/../branch-check.sh" "$ps_repo" master 2>&1)
  check "powershell fail on broken ps1" 1 "$(printf '%s\n' "$out" | grep -c '^powershell: FAIL$')"
else
  fail=$((fail + 2))
  echo "not ok: scratch repo for powershell tests could not be created"
fi
rm -rf "$ps_repo" 2>/dev/null || true

# integration: a git failure must exit 2 with a message, never fall through to
# PASS (finding: failing git in a command substitution was silently skipped)
git_fail_repo=$(mktemp -d)
if git -C "$git_fail_repo" init -q -b master \
  && printf 'base\n' > "$git_fail_repo/README.md" \
  && git -C "$git_fail_repo" -c user.email=t@t -c user.name=t add README.md \
  && git -C "$git_fail_repo" -c user.email=t@t -c user.name=t commit -qm 'chore/init'; then
  out=$(bash "$test_dir/../branch-check.sh" "$git_fail_repo" no-such-ref-xyz 2>&1); rc=$?
  check "bad base ref exits 2" 2 "$rc"
  git -C "$git_fail_repo" switch --orphan unborn -q
  out=$(bash "$test_dir/../branch-check.sh" "$git_fail_repo" master 2>&1); rc=$?
  check "unborn HEAD exits 2" 2 "$rc"
  check "unborn HEAD names the failing diff" 1 "$(printf '%s\n' "$out" | grep -c 'cannot diff')"
else
  fail=$((fail + 3))
  echo "not ok: scratch repo for git-failure tests could not be created"
fi
rm -rf "$git_fail_repo" 2>/dev/null || true

echo "branch-check.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
