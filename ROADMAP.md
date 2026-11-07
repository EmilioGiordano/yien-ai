# ROADMAP

The index for agents: where the project stands and what comes next. Each branch updates its own row as its last commit. Rules are in `AGENTS.md`; behaviour in the ERS; engineering in the plan (absolute paths in `AGENTS.md`).

## Target: MVP

A YIEN that Emilio can use for real work on Windows:

- One window. Projects in a sidebar, vertical tabs per project, and split terminal panes per tab, following the prototype `design/prototipo-navegacion.html`.
- Each pane runs a shell or an agent CLI (Claude Code, Codex, OpenCode) through ConPTY, with correct rendering, input (IME, AltGr), copy and paste, and scrollback.
- Panes split, resize, close, swap and get focused by keyboard and mouse. Command palette.
- Claude Code state detection (working / needs you / idle) with the red mark and the attention counter (RN-01, RN-02).
- The layout and sessions survive a restart.
- `yien --test-control` so every feature is validated in the running app (ADR-21).

This is plan milestones M0–M2, the core of M3 and the Claude part of M4. Phase 0 is cut down to the spikes that decide the stack: S2 (ConPTY) and S1 (GPUI), sized to one night.

## Status

| Step | Branch | State |
|---|---|---|
| Agent workflow | `chore/agent-workflow` | merged |
| S2 ConPTY spike | `spike/s2-conpty` | next |
| S1 GPUI spike | `spike/s1-gpui` | next |
| M0 skeleton: workspace, CI, deny, proto, testkit, xtask, foothy layout and input | `build/workspace-skeleton` | done on the branch: all of these, green locally; CI not yet run on the three systems |
| M0 rest: `tracing` setup, error conventions | follow-up `build/` | pending |
| M1 one pane: PTY, VT, terminal element, input, scrollback | several `feat/` | pending |
| M2 workspace: projects, tabs, splits, focus, palette, commands, theme | several `feat/` | pending |
| Test control pipe | `feat/test-control` | pending, from M1 on |
| M3 core: snapshot and restore of layout and sessions | `feat/` | pending |
| M4 Claude: profiles, signals, pure state machine, hook settings and payload decoding (`yien-agent`) | `feat/agent-state-machine` | done on the branch, in review; no UI yet |
| M4 Claude: `yien-hook`, hook pipe, red mark, counter | `feat/` | pending |

## Decisions taken on the way

Record here what a spike or a branch decided, with the branch name. The plan's spike log in `docs/` holds the details.

- `build/workspace-skeleton`: layer-0 crates do not depend on each other. `yien-layout` keeps its own `PaneId` (a `u64` handed out by the tree); mapping it to `yien_proto::PaneId` is the core's job.
- `build/workspace-skeleton`: foothy's `win32.rs` (win32-input-mode encoding) was left out of `yien-input` as the TASK asked. It makes no OS calls, and plan §3.2 places win32-input-mode in `yien-input`, so it should come back there with M1 input, not go to `yien-platform`.
- `build/workspace-skeleton`: `xtask check-deps` also requires `#![forbid(unsafe_code)]` in every crate root except `yien-pty` and `yien-platform`, limits `yien-proto` to `serde`, and holds layers 0 and 1 to an allowlist of external crates kept in `xtask/src/check_deps.rs` (adding one is a reviewed change).
- `feat/agent-state-machine`: the per-pane hook token travels in the `YIEN_HOOK_TOKEN` environment variable (04 §2.4), never in the `--settings` file. Registered Claude events: the eight of 04 §2.4 plus `PostToolUse`, the real exit from a permission wait.
- `feat/agent-state-machine`: OSC 9, OSC 777 and BEL never change the state: they cannot tell "done" from "waiting" (RN-02). Codex and OpenCode stay idle-only until S7 and S8.
- `feat/agent-state-machine`: an unverified CLI version uses the newest rule (highest lower bound), not the last one listed; a rule without `versions` is never counted as verified (RF-AGE-11).
- `feat/agent-state-machine`: `regex` joins the layer-1 allowlist (plan §3.2).

## In flight

Branches with an implementer running, and their slots.

- (none)
