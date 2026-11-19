---
name: yien-review
description: Review a YIEN worktree or branch with the quality-reviewer subagent and report the verdict without fixing anything. Use for a standalone check outside the yien-branch loop, or on a branch someone else wrote.
---

# yien-review

Usage: `yien-review [path]`. The path is a worktree; with no argument, the current checkout.

1. Make sure the worktree has a `.yien-slot`; assign a free slot if not. Record `git -C <worktree> rev-parse HEAD` and `git -C <worktree> status --porcelain` before the review round.
2. Spawn `quality-reviewer` via `invoke_subagent` with a fresh context (or run OpenCode's `scripts/oc.sh <worktree> strict-reviewer <task-file>` through Git's `sh.exe` when delegating per `AGENTS.md`). Give it the absolute worktree path, the TASK path if there is one, and the Windows PowerShell 5.1 notes (`&&` is a parse error; chain with `;` or use `git -C <worktree>`; the working directory resets between `run_command` calls; use forward slashes in paths) plus the Git `sh.exe` form for scripts:
   ```powershell
   $sh = (Get-Command sh.exe -ErrorAction SilentlyContinue).Source
   if (-not $sh) { $sh = "$(Split-Path (Split-Path (Get-Command git).Source))\bin\sh.exe" }
   & $sh scripts/cargo.sh <args>
   & $sh scripts/branch-check.sh <worktree>
   ```
   It reviews the union of uncommitted work and the branch against `main`, and states the target.
3. Wait for the report. Never predict it. After the reviewer finishes, check `git -C <worktree> rev-parse HEAD` and `git -C <worktree> status --porcelain` again: any change (a new commit or a modified file) voids that review round.
4. Relay the report, then sanity-check it. If a finding contradicts something the plan or this session already settled, say so next to it instead of dropping it.
5. Do not fix, commit, push or open anything. Fixing is a separate step after the developer sees the findings.
