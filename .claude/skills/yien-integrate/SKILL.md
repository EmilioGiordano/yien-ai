---
name: yien-integrate
description: Test a batch of reviewed YIEN branches together on a staging branch, run the full gate and the UI validation, then merge their PRs into main one by one. Use when one or more branches have a PASS and a draft PR.
---

# yien-integrate

Invoke as `/yien-integrate [branch...]`; with no argument, every branch with a PASS and a draft PR. Follow `AGENTS.md` "Integration and merge" and "Validating in the UI"; this is the checklist.

1. `git fetch origin`; update local `main` with `git pull --ff-only`. Create `staging/<date>` (or `-<n>`) from `main` in the integration worktree `C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai-wt/staging`, with its own slot.
2. Merge each branch with `--no-ff`, then run `scripts/cargo.sh check --workspace --all-targets`. On a break, the last merged branch is the culprit: drop it from the batch, send the failure back to its implementer, and continue with the rest.
3. Run the full gate once with `scripts/cargo.sh --full`. Map a failing test to its branch by crate and module.
4. Run the UI validation of each feature against the staging release build: test control first, screen driving only if Emilio said he is away. Throwaway profile, process watch, exe copied to the slot's `run/`.
5. For each PR, in staging order: `gh pr ready <n>`, a short comment with the gate and UI results, then `gh pr merge <n> --merge`. Never `--delete-branch`.
6. Pull `main` and check that `git diff staging/<date> main` is empty. If it is not, find why before doing anything else.
7. Report to Emilio in Spanish: what was merged, what was held out and why, and what only a human can check.

Never push `staging/*`, never commit on staging, never delete a branch. A breakage that only exists when branches are combined gets its own `fix/<topic>` branch through `yien-branch`.
