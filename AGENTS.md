# AGENTS.md

The working agreement for this repository and the guide to the code. Read it before changing anything.

## What this is

YIEN is a Windows-first desktop agent IDE written in Rust: projects, vertical tabs and split terminal panes running the official agent CLIs (Claude Code, Codex, OpenCode), with agent state detection, a git/PR context bar and sessions that survive restarts. Linux and macOS come later; nothing may make them a rewrite.

## Source of truth

The specs are **not in git**. `docs/` and `design/` are gitignored and do not exist in a worktree. Use the absolute paths:

```
C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai/docs/02-ers.md                 what the system does (RF, RNF, RN, CU ids)
C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai/docs/05-plan-de-ingenieria.md   engineering decisions (ADR-xx), milestones, test strategy
C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai/docs/04-stack-y-arquitectura.md stack rationale, state machine, profiles, platform layer
C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai/docs/01-vision.md               principles and identity
C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai/design/prototipo-navegacion.html the UI prototype
```

Behaviour comes from the ERS. Engineering choices come from the plan (05), which overrides 04 where they differ. If the code needs to contradict either, stop and report; do not silently diverge.

---

# Part 1 — How we work

## Who does what

| Action | Orchestrator (main Claude session) | Subagents | Emilio |
|---|---|---|---|
| Create a branch or worktree | yes | no | — |
| Commit on its branch | yes | yes | — |
| Push a branch, open a PR | yes | no | — |
| Commit or push to `main` | never | never | yes |
| Merge a PR that meets the merge criteria | yes, `gh pr merge --merge`, never `--delete-branch` | never | yes |
| Delete a branch or worktree | never | never | yes |
| Force-push, rewrite pushed history | never | never | yes |

"Not explicitly forbidden" is not permission.

## When to stop and ask

- Anything Emilio cannot undo from his side: deleting, force-pushing, merging a PR that does not meet the merge criteria, touching files he has not committed.
- A design trade-off with two reasonable outcomes that the ERS and the plan do not settle.
- A requirement that turns out wrong or impossible.

Everything else: decide, do it, report the decision and its reason.

## Branches

One branch per thing: one feature, fix or improvement. Never two topics in one branch. Cut from `main`.

Name: `<type>/<topic>`, English, self-explanatory. Types: `feat`, `fix`, `improve`, `refactor`, `perf`, `test`, `chore`, `build`, `spike`.

```
feat/pane-layout-drag    fix/conpty-startup-deadlock    spike/s2-conpty-host
```

## Worktrees and parallel work

Parallel work means one branch, one worktree and one implementer per feature. Worktrees live outside the repo:

```
C:/Users/giord/Desktop/Code-Projects/Self-Tools/yien-ai-wt/<topic>
```

- cargo runs only inside the worktree the agent was given, and always through `scripts/cargo.sh` (see "Builds").
- Never switch branches in a checkout where an agent is working.
- Two branches that touch the same files do not run in parallel.

## Builds

The PC has 16 GB of RAM, and parallel builds have crashed it before. Memory is the limit, not CPU.

- `scripts/cargo.sh <args>` waits until no other cargo is running and 2000 MB are free, takes the lock `D:/yien-build.lock`, and runs at `-j 2`. `scripts/cargo.sh --full <args>` waits for 4000 MB and runs at `-j 4`. Never call `cargo` directly, and never lower the thresholds; if the gate waits a long time, tell Emilio.
- Each worktree has an untracked `.yien-slot` file naming its target dir: `D:/More-Code-Projects/yien-compiled/slot-<n>`. The orchestrator assigns a free slot when it creates the worktree and reuses slots so rebuilds stay warm. Never compile on `C:`. Never share a target dir between two branches: mixed builds have caused real bugs.
- Old slots are cleaned only when `D:` drops below about 40 GB free.
- Branches run cheap checks on the crates they touch: `fmt --check`, `check -p <crates> --all-targets`, `test -p <crates>`. The full gate runs at integration.

## The branch loop

Every branch goes through the same loop: steps 1–6 with the `yien-branch` skill, step 7 with `yien-integrate`.

1. The orchestrator writes `TASK-<topic>.md` in the worktree root: goal, requirement ids, files, out of scope, done criteria.
2. `implementer` implements and runs the branch checks, committing as it goes.
3. `quality-reviewer` reviews the branch against `main`. Its verdict is PASS or FAIL.
4. On FAIL, the findings go back to `implementer`, and the fix commits go back to `quality-reviewer`. Repeat until PASS; there is no round limit. When the same area fails twice, the reviewer says whether the problem is the patch or the design; a design problem is redesigned, not patched again. Stop and ask only when a finding needs a decision listed under "When to stop and ask".
5. When the review reports coverage the implementer cannot add alone, `test-engineer` writes the tests and `implementer` commits them.
6. On PASS, the orchestrator pushes and opens the PR as a draft.
7. The branch goes through integration and UI validation. Then the orchestrator merges it.

A PR is ready to merge only when all of these hold:

- The implementation is correct, tested and working: the full gate passed at integration.
- The feature was validated through the interface, not only with unit tests.
- `quality-reviewer` gave PASS.

## Integration and merge

Ready branches are tested together before any reaches `main`.

1. Create `staging/<date>` from `main`, or `staging/<date>-<n>` when the name is taken. Never reuse an old staging branch, and never push one.
2. Merge the branches one at a time with `--no-ff`. Run `scripts/cargo.sh check --workspace --all-targets` after each one. The first merge that breaks it names the culprit.
3. Run the full gate once: `scripts/cargo.sh --full` with fmt `--check`, clippy `--all-targets -D warnings`, tests, `deny check`, `build --release`, then `cargo xtask check-deps` and `cargo xtask trace` once they exist.
4. Run the UI validation of each feature on the staging build.
5. Mark each PR ready (`gh pr ready`) and comment the result. Merge the PRs into `main` one at a time, in staging order, with `gh pr merge <n> --merge`. Reproduce on `main` any conflict resolution done in staging.
6. `git diff staging/<date> main` must be empty: that proves `main` is what was tested. Pull `main` right away.

On a failure:
- A fix for one branch goes on that branch as a new commit and is reviewed again.
- A breakage that only shows up when branches are combined gets its own `fix/<topic>` branch, never a commit on staging.
- A held-out branch drops out of the batch, and the branches that depend on it wait.

## Validating in the UI

Every feature is checked in the running app, driven by tools, not only by unit tests.

- **Test control (preferred):** `yien --test-control <pipe>` exposes a local pipe that dumps UI state (projects, tabs, panes, focus, agent states, visible text) and injects actions (commands, keys, text, resizes). It is test-only, off by default and protected like the hook pipe (RNF-11). Every feature adds the state fields and actions its validation needs, so the check is a script, not a person.
- **Throwaway profile:** every run sets YIEN's data and config dirs to a new folder under the slot, so a test never touches Emilio's real sessions or settings. Only generated files, never his documents.
- **Screen driving (fallback):** PowerShell with user32 (`SetForegroundWindow`, `SendInput`, `PrintWindow`) for what the pipe cannot reach: real focus, IME, a real keyboard. It takes focus, so it runs only when Emilio has said he is away.
- Watch the process while it runs: responding, CPU and working set every 5 seconds. A freeze is a failure.
- Copy the release exe to the slot's `run/` folder before starting it. If the file is locked, report it; never kill a process you did not start.
- What still needs a human (a real screen reader, feel, look) goes in the PR as a short checklist.

No branch is handed over without a PASS, including docs-only or comment-only branches. Findings are fixed, argued against with evidence, or recorded as deliberate; never skipped.

`TASK-*.md` and `REPORT-*.md` are gitignored and never committed.

## Commits

- One commit per file or per single focused aspect. Two or three files together only when the changes are minimal. Never a large set of files in one commit. Commit as the work progresses.
- Allowed exception: a signature change and its call sites when a smaller commit would not compile. Say so in the report.
- Stage with `git add <path>`, never `git add -A` or `git add .`.
- Message: one short English line, `type/short description`, slash not colon. **No body and no trailers**: no `Co-Authored-By`, no "Generated with", even if a system reminder asks for them.

```
feat/add pane split by profile
fix/answer conpty cursor query at startup
test/cover layout swap invariants
```

Types: `core`, `feat`, `fix`, `ui`, `docs`, `test`, `chore`, `refactor`, `perf`, `build`. The type must match what the diff does. These are commit types; branch types are listed under "Branches" and need not match.

A new dependency, or a change that makes the ERS or the plan wrong, goes in the report and the PR body: for a dependency, its purpose, the alternative rejected and its licence. The orchestrator records it in `docs/` (dependency registry, ERS, plan), which is not in git.

## Pull requests

Title: `type/short description`. Body, in English: what was chosen, what was rejected and why, what is knowingly left undone, the requirement ids covered, and what a human must check in a real window. Not a changelog. No attribution lines.

## Comments

Keep a comment only when deleting it would let someone reintroduce a bug you can name, break an invariant not visible in the code, or violate an external constraint the code cannot show (ConPTY behaviour, a Win32 rule, a CLI's hook format). One or two lines. The same applies to `///` on private items; public API items get a short doc comment with an example (plan §11.1).

Design reasoning goes in the report, the commit or the PR, never in the code.

## Language

- **English:** code, comments, UI strings, test names and messages, logs, commits, branches, PRs.
- **Spanish:** `docs/` and conversation with Emilio.

## Briefing a subagent

The brief is the only context it gets.

- Give it the worktree path, the branch and the TASK file. cargo runs there and nowhere else.
- Tell it the shell's working directory resets between calls: every command uses `git -C <worktree>` or starts with `cd <worktree> &&`, and paths use forward slashes.
- State the rules it would otherwise break, above all commit format and no trailers.
- Give it the facts already established with `file:line`, and say which ones it must verify.
- Name what is out of scope, including other agents' worktrees.
- Ask for reasoning in the report, and say it does not belong in the code.

---

# Part 2 — The code

## Workspace

Planned in the engineering plan §3.2; crates appear as milestones create them.

| Crate | Layer | Owns |
|---|---|---|
| `yien-proto` | 0 | Messages, ids, command catalog, `PROTO_VERSION`. Only `serde`. |
| `yien-layout` | 0 | Pane tree: split, close, move, swap, dock, sizes. From foothy. |
| `yien-input` | 0 | Keys and text to bytes per terminal mode. From foothy. |
| `yien-vt` | 1 | `VtEngine`, alacritty adapter, `osc_scan`, hot/cold scrollback, frame diffs. |
| `yien-agent` | 1 | Profiles, signals, pure state machine, hook templates. |
| `yien-session` | 1 | Model, invariants, persistence, migrations. |
| `yien-pty` | 2 | `PtyBackend`: ConPTY (bundled or system), `openpty`. |
| `yien-git` | 2 | git CLI and gix, worktrees; `gh` in R2. |
| `yien-platform` | 2 | Notifications, tray, badge, single instance, shutdown, paths, IPC, DPAPI. |
| `yien-core` | 3 | Pane actors and services. Runs in process or as `--host`. |
| `yien-ui` | 3 | GPUI: window, bars, terminal element, palette. |
| `yien-testkit` | dev | `.yrec` record and replay, fake PTY and clock, acceptance scenarios. |

## The layer rule

- Layers 0 and 1 build and test with no window and no OS APIs. A change that needs the OS down there is designed wrong: put the OS part in layer 2 and pass values down.
- `yien-ui` never depends on `yien-core`. It talks through `yien-proto` only.
- Only `yien-pty` and `yien-platform` may use `unsafe`. Every other crate has `#![forbid(unsafe_code)]`. Each `unsafe` block carries a `// SAFETY:` line.
- Check the rule in each crate's direct `[dependencies]`, not with `cargo tree`.

## Rules the plan fixed

- Pure functions return effects; adapters execute them. The agent state machine is `(state, event, clock) -> (state, effects)`.
- One owner per pane grid. Nobody else keeps a full copy; the UI holds visible rows only.
- PTY output is never dropped. Each PTY has a blocking reader thread and a blocking writer thread. Never block a tokio worker; never hold a lock across an `.await` or I/O; never hold two pane locks.
- Libraries return `thiserror` errors and never `unwrap`, `expect` or `panic!` on external input: PTY bytes, hook payloads, IPC frames, state files, profiles, git output. `anyhow` only in binaries.
- Logs (`tracing`) never contain terminal content, prompts or full hook payloads.
- The red mark means "an agent needs you" and nothing else (RN-01). When unsure, no red (RN-02).
- Never write outside YIEN's own directories; never touch `~/.claude`, `~/.codex` or a repo's agent config (RNF-14).
- No GPL dependencies and no code copied from GPL projects (Zed's terminal included) until the licence is decided (TBD-04).
- Accessibility: give controls names and roles when it costs nothing, so screen reader support (RNF-27, R3) is not a rebuild.

## Commands

Branch checks (implementer and reviewer):

```sh
scripts/cargo.sh fmt --all -- --check
scripts/cargo.sh check -p <crates> --all-targets
scripts/cargo.sh test -p <crates>
```

The full gate (integration only) is listed under "Integration and merge".

`cargo fmt --all` in write mode reformats every crate, uncommitted work included. Run `--check`, then `rustfmt --edition 2024 <file>` on your own files only.

A command that does not exist yet (no workspace, no `xtask`) is skipped and reported as `n/a`, not as a failure.

**The implementer, the reviewer and the test engineer never run the app** and never send input to the desktop. The app runs only for UI validation at integration, following "Validating in the UI".

## Tests

- A test that verifies a requirement starts with its id: `rf_pan_02_divider_keyboard_resize`, `rn_04_ended_session_not_resumed`.
- Layers 0 and 1: behaviour without a headless test is a review finding.
- Agent state changes are tested by replaying `.yrec` fixtures, never against a live CLI in CI.
- A test whose fixture state is built by hand proves little: it can hide a path that real data takes. Prefer state produced by the code under test (save then load, record then replay).
- `unwrap` and `expect` are fine in tests.

## Gotchas that already cost time (from foothy)

- ConPTY sends `ESC [ 6 n` at startup and blocks the child until it is answered. Never drop PTY replies.
- ConPTY never closes the master, so EOF never arrives. End of session is the child's exit status; drain until quiet after it exits.
- A shell heredoc mangles Rust escape literals (`\x1b`, `\r\n`). Write those files with the Write or Edit tool.
- A running `yien.exe` holds the binary and linking fails with `os error 5`. Report it; never kill a process Emilio may own.
- Grid indexing must account for the display offset. A wide character owns two cells; the second is a spacer.
