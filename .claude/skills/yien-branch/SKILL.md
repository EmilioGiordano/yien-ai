---
name: yien-branch
description: Run one YIEN branch from goal to draft PR - worktree and build slot, TASK file, implementer, quality review loop, push and draft PR. Use for every feature, fix or improvement; run several at once for parallel work, one per worktree. Integration and merge run afterwards with yien-integrate.
---

# yien-branch

Run one branch from a plain-words goal to a reviewed draft PR. Invoke as `/yien-branch <type>/<topic> <goal>`.

Read `AGENTS.md` first; it is the rulebook. The briefs in `.claude/agents/` are the source of truth for each role: tell subagents to read them, never paste their content.

## 1. Worktree and slot

From the main checkout, run `git fetch origin`, then cut from `origin/main` (or `main` while the remote has no `main`):

```
git worktree add -b <type>/<topic> C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai-wt/<topic> origin/main
```

Write `.yien-slot` in the worktree root with a slot no running branch uses: `D:/More-Code-Projects/yien-compiled/slot-<n>`.

If another running branch touches the same files, do not start this one in parallel; say so.

## 2. TASK file

Write `TASK-<topic>.md` in the worktree root (gitignored):

- **Goal:** one paragraph.
- **Requirements:** the RF, RNF and RN ids from the ERS, and the plan sections (ADR, milestone) it implements.
- **Files:** where the change goes, found by reading the code.
- **Steps:** implementation order.
- **Out of scope:** what must not be touched, including other worktrees.
- **Done when:** observable criteria, the tests that must exist (named with requirement ids), the UI validation script (test-control state and actions it checks), and what only a human can check.

## 3. Implement

Spawn `implementer` with a fresh context. Give it the worktree path, the branch and the TASK path, and the rules from AGENTS.md "Briefing a subagent". Wait for its report.

## 4. Review loop

1. Spawn `quality-reviewer` with the worktree path and the TASK path, a fresh context and none of the implementer's reasoning.
2. Before relaying findings, sanity-check them. A finding that contradicts the plan is noted, not obeyed. A finding that turns out wrong goes into the reviewer's "Known false positives".
3. On `FAIL`, send the findings to the implementer to fix, then send the fix commits back to review. Every FAIL needs a later PASS.
4. Repeat until PASS; there is no round limit. When the reviewer calls a problem a design problem, redesign before patching again. Stop and ask Emilio only when a finding needs a decision AGENTS.md reserves for him.

When the review reports coverage the implementer cannot add alone, spawn `test-engineer` with the worktree path, then have the implementer commit its tests.

## 5. Draft PR

On `PASS`:

1. Push the branch: `git push -u origin <type>/<topic>`.
2. Open a draft PR with `gh pr create --draft`. The body covers:
   - the decisions taken and the options rejected;
   - what is left undone;
   - the requirement ids covered;
   - new dependencies and spec impact from the implementer's report;
   - the UI validation to run and what only a human can check.

   No attribution lines.
3. Record new dependencies and spec impact in `docs/` (absolute paths in `AGENTS.md`).
4. Write `REPORT-<topic>.md` in the worktree (gitignored): verdict, commits, checks, findings and their resolution, risks, manual checks.
5. Report to Emilio in Spanish: the PR link and what changed.

The branch is now ready for `yien-integrate`. Never delete the branch or the worktree, never push to `main` directly.
