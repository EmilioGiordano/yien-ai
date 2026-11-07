---
name: yien-branch
description: Run one YIEN branch end to end - worktree, TASK file, implementer, quality review loop, push and pull request. Use for every feature, fix or improvement; run several at once for parallel work, one per worktree.
---

# yien-branch

Run one branch from a plain-words goal to an open PR. Invoke as `/yien-branch <type>/<topic> <goal>`.

Read `AGENTS.md` first; it is the rulebook. The briefs in `.claude/agents/` are the source of truth for each role: tell subagents to read them, never paste their content.

## 1. Worktree

From the main checkout, with `main` up to date:

```
git worktree add -b <type>/<topic> C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai-wt/<topic> main
```

If another running branch touches the same files, do not start this one in parallel; say so.

## 2. TASK file

Write `TASK-<topic>.md` in the worktree root (gitignored):

- **Goal:** one paragraph.
- **Requirements:** the RF, RNF and RN ids from the ERS, and the plan sections (ADR, milestone) it implements.
- **Files:** where the change goes, found by reading the code.
- **Steps:** implementation order.
- **Out of scope:** what must not be touched, including other worktrees.
- **Done when:** observable criteria, the tests that must exist (named with requirement ids), and what a human must check in a window.

## 3. Implement

Spawn `implementer` with a fresh context. Give it:
- the worktree path, the branch and the TASK path;
- the rule that cargo runs only in that worktree;
- the commit format with no trailers.

Wait for its report.

## 4. Review loop

1. Spawn `quality-reviewer` with the worktree path and the TASK path, a fresh context and none of the implementer's reasoning.
2. Before relaying findings, sanity-check them. A finding that contradicts the plan is noted, not obeyed.
3. On `FAIL`, send the findings to the implementer to fix, then send the fix commits back to review. Every FAIL needs a later PASS.
4. After 3 failed rounds, stop and report to Emilio with the open findings.

When the review reports coverage the implementer cannot add alone, spawn `test-engineer` with the worktree path, then have the implementer commit its tests.

## 5. Hand over

On `PASS`:

1. Check `git log main..HEAD`: every message is one `type/short description` line with no trailers, and the commits are small.
2. Push the branch: `git push -u origin <type>/<topic>`.
3. Open the PR with `gh pr create`. The body covers:
   - the decisions taken and the options rejected;
   - what is left undone;
   - the requirement ids covered;
   - what a human must check.

   No attribution lines.
4. Write `REPORT-<topic>.md` in the worktree (gitignored): verdict, commits, checks, findings and their resolution, risks, manual checks.
5. Report to Emilio in Spanish: the PR link, what changed, and what he should try.

Never merge, never delete the branch or the worktree, never push to `main`.
