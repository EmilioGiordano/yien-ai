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
| VT engine: `yien-vt`, `VtEngine` on alacritty, `osc_scan`, frames, hot scrollback, search, links | `feat/vt-engine` | done on the branch, in review; cold scrollback and frame diffs pending |
| M2 workspace: projects, tabs, splits, focus, palette, commands, theme | several `feat/` | started: `feat/ui-shell` draws the window (tab bar, panes from a `yien-layout` tree, context bar, status bar, both themes) from a view model with sample data; rest pending |
| Test control pipe | `feat/test-control` | pending, from M1 on |
| M3 core: snapshot and restore of layout and sessions | `feat/session-store`, then `feat/` | started: `feat/session-store` adds `yien-session` (model, invariants, versioned JSON with migrations, 5 generations, resume plan), done on the branch, in review; core wiring, screens, single instance and previous-session listing pending |
| M4 Claude: hooks, state machine, red mark, counter | `feat/` | pending |

## Decisions taken on the way

Record here what a spike or a branch decided, with the branch name. The plan's spike log in `docs/` holds the details.

- `build/workspace-skeleton`: layer-0 crates do not depend on each other. `yien-layout` keeps its own `PaneId` (a `u64` handed out by the tree); mapping it to `yien_proto::PaneId` is the core's job.
- `build/workspace-skeleton`: foothy's `win32.rs` (win32-input-mode encoding) was left out of `yien-input` as the TASK asked. It makes no OS calls, and plan §3.2 places win32-input-mode in `yien-input`, so it should come back there with M1 input, not go to `yien-platform`.
- `build/workspace-skeleton`: `xtask check-deps` also requires `#![forbid(unsafe_code)]` in every crate root except `yien-pty` and `yien-platform`, limits `yien-proto` to `serde`, and holds layers 0 and 1 to an allowlist of external crates kept in `xtask/src/check_deps.rs` (adding one is a reviewed change).
- `feat/vt-engine`: the engine has no clock. A synchronized update (mode 2026) holds output and replies until it closes; the pane actor calls `flush_sync` once `sync_pending` has been true for `SYNC_UPDATE_TIMEOUT` (150 ms), restarting that timer whenever `sync_generation` changes.
- `feat/vt-engine`: `Frame.cursor` is `None` only when scrolled out of view; a hidden cursor keeps its point (`Cursor::visible` false) so the IME window can follow it (RF-PAN-15).
- `feat/vt-engine`: `osc_scan` treats `OSC 9;<number>;…` as a ConEmu command (`9;4` progress, `9;9` cwd), never as a notification, so it cannot raise a false red mark (RN-02). Only 7-bit `ESC ]`, BEL and `ESC \` are recognised; C1 bytes are UTF-8 continuation bytes in practice.
- `feat/vt-engine`: link detection covers OSC 8, URLs and absolute Windows drive paths; relative and Unix paths are left for later.
- `feat/ui-shell`: ADR-02 taken: the UI is GPUI through the crates.io package `gpui-kit =0.7.1` (bundles gpui and gpui-component), the pin Zenkai builds and runs on this PC. The lock holds `gpui-base`, `gpui-component` and `gpui-kit-assets` at 0.7.1 to match Zenkai. Its tree needs `0BSD` and `bzip2-1.0.6` in the deny licence list and seven unmaintained-crate advisories ignored (listed in `deny.toml`).
- `feat/ui-shell`: the `yien` binary is layer 4 in `check-deps`; it may depend on every layer and nothing depends on it. `yien-ui` owns `run()`, so the binary does not depend on gpui.
- `feat/ui-shell`: the view model (`WorkspaceView`) lives in `yien-ui`, keyed by `yien-proto` ids, and models all five RF-AGE-05 states. The counter counts panes (RF-ATN-03); project heads count tabs.
- `feat/ui-shell`: the sumi `shu` red is below WCAG AA on `panel` (4.39:1) and `raised` (4.11:1), where the tab bar and the counter use it (RNF-18). Left as designed, pending a token change.

- `feat/session-store`: the state file is `workspace.json` (plan §7.2), with backups `workspace.1.json` to `workspace.4.json`: five copies in all (RF-SES-06). `save` rotates only when the snapshot is `ROTATION_INTERVAL_MS` (60 s) past the one in `workspace.1.json`, or the current file is damaged; other saves replace `workspace.json` atomically.
- `feat/session-store`: a generation with a newer `schema_version` stops `load` without restoring and makes `save` refuse; `preserve_newer` renames it to `workspace.v<N>.json` (plan §5.2), after which saving works.
- `feat/session-store`: split ratios are checked as positive, one per child and summing to 1 (within 1e-3), but not against `yien-layout`'s 5 % minimum, because repeated splits legitimately go below it.
- `feat/session-store`: file I/O goes through the `Store` trait; the `fs` feature's `FsStore` uses only `std::fs` (temp file, `sync_all`, `rename`) and lacks `MOVEFILE_WRITE_THROUGH`, so `yien-platform` should supply the production store.

## In flight

Branches with an implementer running, and their slots.

- (none)
