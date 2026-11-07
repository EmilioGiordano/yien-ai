---
name: yien-integrate
description: Test a batch of reviewed YIEN branches together on a staging branch, run the full gate and the UI validation, then merge their PRs into main one by one. Use when one or more branches have a PASS and a draft PR.
---

# yien-integrate

Invoke as `/yien-integrate [branch...]`; with no argument, every branch with a PASS and a draft PR. `AGENTS.md` "Integration and merge" and "Validating in the UI" are the rules; this is the checklist.

1. `git fetch origin`; in the main checkout, `git pull --ff-only` on `main`.
2. In the integration worktree `C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai-wt/staging` (create it once with `git worktree add <path> main --detach` and give it a `.yien-slot`), run `git switch -c staging/<date> main`.
3. Merge each branch with `--no-ff`, then `scripts/cargo.sh check --workspace --all-targets`. On a break, drop that merge (`git merge --abort` or `git reset --hard HEAD~1`), send the failure back to its implementer, and continue with the rest.
4. Run the full gate once, in the background, with `scripts/cargo.sh --full`. Map a failing test to its branch by crate and module.
5. Run the UI validation of each feature against the staging release build.
6. For each PR, in staging order: `gh pr ready <n>`, a short comment with the gate and UI results, then wait for `gh pr checks <n>` to be green on all three systems (watch the run in the background), then `gh pr merge <n> --merge`. Never `--delete-branch`. On a conflict with `main`, merge `origin/main` into the PR branch with the staging resolution and push first.
7. `git pull --ff-only` on `main`, then check that `git diff staging/<date> main` is empty. If it is not, find out why before doing anything else.
8. Report to Emilio in Spanish: what was merged, what was held out and why, and what only a human can check.

Never push `staging/*`, never delete a branch. A breakage that only exists when branches are combined gets its own `fix/<topic>` branch through `yien-branch`.
