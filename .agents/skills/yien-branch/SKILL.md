---
name: yien-branch
description: Run one YIEN branch from goal to draft PR - worktree and build slot, TASK file, implementer, quality review loop, push and draft PR. Use for every feature, fix or improvement; run several at once for parallel work, one per worktree. Integration and merge run afterwards with yien-integrate.
---

# yien-branch

Run one branch from a plain-words goal to a reviewed draft PR. Usage: `yien-branch <type>/<topic> <goal>`.

Read `AGENTS.md` first; it is the rulebook. The agent definitions in `.agents/agents/` are the source of truth for each role: spawn them by name with `invoke_subagent` instead of pasting their content into the brief (the sole exception is the `define_subagent` fallback below).

## Antigravity mechanics

- **Spawning and follow-ups:** spawn a role with `invoke_subagent` using its project agent name (`implementer`, `quality-reviewer`, `test-engineer`) and save the returned `conversationId`. For any follow-up round, reuse the same `conversationId` with `send_message`.
- **Write-tools fallback (observed on `agy` 1.5.12):** if a project agent that needs write access (`implementer`, `test-engineer`) starts without write tools, register it with `define_subagent(..., EnableWriteTools: true)` using the body of `.agents/agents/<name>.md` as `SystemPrompt`, then call `invoke_subagent` with that type.
- **Brief contents:** subagents see only the brief and their agent body. Every brief must include the Windows PowerShell 5.1 notes (`&&` is a parse error; chain with `;` or use `git -C <worktree>`; the working directory resets between `run_command` calls; use forward slashes in paths) and the Git `sh.exe` form for bash scripts:
  ```powershell
  $sh = (Get-Command sh.exe -ErrorAction SilentlyContinue).Source
  if (-not $sh) { $sh = "$(Split-Path (Split-Path (Get-Command git).Source))\bin\sh.exe" }
  & $sh scripts/<name>.sh ...
  ```
- **OpenCode delegation:** `scripts/oc.sh` and `scripts/oc-loop.sh` stay available as in `AGENTS.md` "Delegating to OpenCode", run through Git's `sh.exe`.

## 1. Worktree and slot

From the main checkout, run `git fetch origin`, then cut from `origin/main` (or `main` while the remote has no `main`):

```
git worktree add -b <type>/<topic> <worktrees>/<topic> origin/main
```

Write `.yien-slot` in the worktree root with a slot no running branch uses: `<build-root>/slot-<n>` (`<worktrees>` and `<build-root>` are `YIEN_WORKTREES_DIR` and `YIEN_BUILD_ROOT` from `.yien-local`, see `AGENTS.md` "Local setup").

If another running branch touches the same files, do not start this one in parallel; say so.

## 2. TASK file

Write `TASK-<topic>.md` in the worktree root (gitignored):

- **Goal:** one paragraph.
- **Requirements:** the RF, RNF and RN ids from the ERS, and the plan sections (ADR, milestone) it implements.
- **Files:** where the change goes, found by reading the code.
- **Steps:** implementation order.
- **Out of scope:** what must not be touched, including other worktrees.
- **Design:** for visual work (new marks, layouts, colours, icons), the option the developer picked from a canvas or mockup of the options. Do not start the branch before they pick one.
- **Done when:** observable criteria, the tests that must exist (named with requirement ids), the UI validation script (test-control state and actions it checks), and what only a human can check.

## 3. Implement

Spawn `implementer` via `invoke_subagent` with a fresh context (using the `define_subagent` fallback if it starts without write tools), or delegate via `scripts/oc-loop.sh` through Git's `sh.exe` as in `AGENTS.md`. Give it the worktree path, the branch, the TASK path, the PowerShell 5.1 and `sh.exe` notes above, and the rules from `AGENTS.md` "Briefing a subagent". Save its `conversationId` and wait for its report.

## 4. Review loop

1. Before starting the review round, record `git -C <worktree> rev-parse HEAD` and `git -C <worktree> status --porcelain`.
2. Spawn `quality-reviewer` via `invoke_subagent` with the worktree path, the TASK path, the PowerShell 5.1 and `sh.exe` notes, a fresh context and none of the implementer's reasoning. Save its `conversationId`.
3. After the reviewer replies, run `git -C <worktree> rev-parse HEAD` and `git -C <worktree> status --porcelain` again. Both must match the pre-review snapshot; any change during or after a review round (a new commit or a modified file) voids that round.
4. Before relaying findings, sanity-check them. A finding that contradicts the plan is noted, not obeyed. A finding that turns out wrong goes into the reviewer's "Known false positives" (keep `.agents/agents/quality-reviewer.md` and `.claude/agents/quality-reviewer.md` in sync).
5. On `FAIL`, send the findings to the existing `implementer` via `send_message` (reusing its `conversationId`) to fix. Once the fixes are committed, record `HEAD` and `status --porcelain` again and send the fix commits and previous-finding resolutions back to `quality-reviewer` via `send_message` (reusing its `conversationId`), verifying `HEAD` and `status --porcelain` again after it replies. Every FAIL needs a later PASS. A Low fix of one or two lines that touches no behaviour covered by another finding needs no new round: read its diff and run fmt, clippy and the touched crate's tests through `scripts/cargo.sh` (via Git's `sh.exe`) yourself. A Medium or higher finding, or a fix that touches logic, always goes back to review.
6. Repeat until PASS; there is no round limit. When the reviewer calls a problem a design problem, redesign before patching again. Stop and ask the developer only when a finding needs a decision `AGENTS.md` reserves for them.

When the review reports coverage the implementer cannot add alone, spawn `test-engineer` via `invoke_subagent` (using the `define_subagent` fallback if it starts without write tools) with the worktree path and the PowerShell 5.1 and `sh.exe` notes, then message `implementer` via `send_message` to commit its tests.

## 5. Draft PR

On `PASS`, confirm `git -C <worktree> rev-parse HEAD` and `git -C <worktree> status --porcelain` still match the passing review round (any new commit or modified file after a review round voids that round, except the Low-fix check and staging-resolution `origin/main` merge allowed by `AGENTS.md`):

1. Push the branch: `git -C <worktree> push -u origin <type>/<topic>`.
2. Open a draft PR with `gh pr create --draft`. The body covers:
   - the decisions taken and the options rejected;
   - what is left undone;
   - the requirement ids covered;
   - new dependencies and spec impact from the implementer's report;
   - the UI validation to run and what only a human can check.

   No attribution lines.
3. Record new dependencies and spec impact in `docs/` (absolute paths in `AGENTS.md`).
4. Complete `REPORT-<topic>.md` in the worktree (gitignored), which already holds the subagents' full reports: add the verdict, the findings and their resolution, risks and manual checks.
5. Report to the developer in Spanish: the PR link and what changed.

The branch is now ready for `yien-integrate`. Never delete the branch or the worktree, never push to `main` directly.
