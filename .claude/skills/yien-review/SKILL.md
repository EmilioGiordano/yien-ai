---
name: yien-review
description: Review a YIEN worktree or branch with the quality-reviewer subagent and report the verdict without fixing anything. Use for a standalone check outside the yien-branch loop, or on a branch someone else wrote.
---

# yien-review

Invoke as `/yien-review [path]`. The path is a worktree; with no argument, the current checkout.

1. Make sure the worktree has a `.yien-slot`; assign a free slot if not. Spawn `quality-reviewer` with the absolute worktree path, the rule that every command uses `git -C <worktree>` or `cd <worktree> &&`, and the TASK path if there is one. It reviews the union of uncommitted work and the branch against `main`, and states the target.
2. Wait for the report. Never predict it.
3. Relay the report, then sanity-check it. If a finding contradicts something the plan or this session already settled, say so next to it instead of dropping it.
4. Do not fix, commit, push or open anything. Fixing is a separate step after Emilio sees the findings.
