---
name: yien-integrate
description: Test a batch of reviewed YIEN branches together on a staging branch, run the full gate and the UI validation, then merge their PRs into main one by one. Use when one or more branches have a PASS and a draft PR.
---

# yien-integrate

Usage: `yien-integrate [branch...]`; with no argument, every branch with a PASS and a draft PR. `AGENTS.md` "Integration and merge" and "Validating in the UI" are the rules; this is the checklist.

On Windows PowerShell 5.1, `&&` is a parse error (use `;` or `git -C <path>`) and bash scripts run through Git's `sh.exe`:

```powershell
$sh = (Get-Command sh.exe -ErrorAction SilentlyContinue).Source
if (-not $sh) { $sh = "$(Split-Path (Split-Path (Get-Command git).Source))\bin\sh.exe" }
& $sh scripts/cargo.sh ...
```

1. `git fetch origin`; in the main checkout, `git pull --ff-only` on `main`.
2. In the integration worktree `<worktrees>/staging` (`YIEN_WORKTREES_DIR` in `.yien-local`; create it once with `git worktree add <path> main --detach` and give it a `.yien-slot`), run `git switch -c staging/<date> main`.
3. Merge each branch with `--no-ff`, then run `scripts/cargo.sh check --workspace --all-targets` through Git's `sh.exe`. On a break, drop that merge (`git merge --abort` or `git reset --hard HEAD~1`), send the failure back to its implementer (reuse its `conversationId` via `send_message`, or spawn `implementer` via `invoke_subagent` with the PowerShell 5.1 and `sh.exe` notes in its brief — registering with `define_subagent(..., EnableWriteTools: true)` if it starts without write tools), and continue with the rest.
4. Run the full gate once, in the background, with `scripts/cargo.sh --full` through Git's `sh.exe` (and `xtask check-deps` / `xtask trace` through `scripts/cargo.sh` once they exist). Map a failing test to its branch by crate and module.
5. Run the UI validation of each feature against the staging release build.
6. For each PR, in staging order: `gh pr ready <n>`, a short comment with the gate and UI results, then wait for `gh pr checks <n>` to be green on all three systems (watch the run in the background), then `gh pr merge <n> --merge`. Never `--delete-branch`. On a conflict with `main`, merge `origin/main` into the PR branch with the staging resolution and push first.
7. `git pull --ff-only` on `main`, then check that `git diff staging/<date> main` is empty. If it is not, find out why before doing anything else.
8. Update `ROADMAP.md` once for the whole batch: cut `docs/roadmap-<batch>` from `origin/main`, take each row and decision from the merged PR bodies, open its own PR and merge it when CI is green on the three systems. It needs no reviewer PASS: it only summarises decisions already reviewed on their branches. Branches never edit `ROADMAP.md`.
9. Report to the developer in Spanish: what was merged, what was held out and why, and what only a human can check.
10. Write or overwrite `<specs>/docs/handoff.md` as `AGENTS.md` "Sessions and handoff" says. The batch is closed; the next one starts in a new session.

Never push `staging/*`, never delete a branch. A breakage that only exists when branches are combined gets its own `fix/<topic>` branch through `yien-branch`.
