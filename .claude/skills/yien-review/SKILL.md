---
name: yien-review
description: Review a YIEN worktree or branch with the quality-reviewer subagent and report the verdict without fixing anything. Use for a standalone check outside the yien-branch loop, or on a branch someone else wrote.
---

# yien-review

Invoke as `/yien-review [path]`. The path is a worktree; with no argument, the current checkout.

1. Spawn `quality-reviewer` with the path and nothing else: it determines its own target (uncommitted work, the branch against `main`, or both) and states it.
2. Wait for the report. Never predict it.
3. Relay the report, then sanity-check it. If a finding contradicts something the plan or this session already settled, say so next to it instead of dropping it.
4. Do not fix, commit, push or open anything. Fixing is a separate step after Emilio sees the findings.
