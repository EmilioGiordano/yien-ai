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
| M0 rest: `tracing` setup, error conventions | `build/tracing-and-input-mode` | done on the branch, in review: `yien-platform` with `init_logging` (daily file, 7 kept, `YIEN_LOG`) and `paths::data_dir` (`YIEN_DATA_DIR` override), wired into the binary; win32-input-mode back in `yien-input`. Error conventions need no code: the thiserror/anyhow rule is in AGENTS.md "Rules the plan fixed" |
| M1 one pane: PTY, VT, terminal element, input, scrollback | several `feat/` | pending |
| VT engine: `yien-vt`, `VtEngine` on alacritty, `osc_scan`, frames, hot scrollback, search, links | `feat/vt-engine` | done on the branch, in review; cold scrollback and frame diffs pending |
| M2 workspace: projects, tabs, splits, focus, palette, commands, theme | several `feat/` | started: `feat/ui-shell` draws the window (tab bar, panes from a `yien-layout` tree, context bar, status bar, both themes) from a view model with sample data; rest pending |
| M1 PTY backend: `yien-pty`, portable-pty over the system ConPTY, reader/writer/waiter threads, `FakePty` | `feat/pty-backend` | done on the branch: tests green on Windows; unix tests not yet run |
| M2 workspace: projects, tabs, splits, focus, palette, commands, theme | several `feat/` | pending |
| Test control pipe | `feat/test-control` | pending, from M1 on |
| M3 core: snapshot and restore of layout and sessions | `feat/` | pending |
| M4 Claude: profiles, signals, pure state machine, hook settings and payload decoding (`yien-agent`) | `feat/agent-state-machine` | done on the branch, in review; no UI yet |
| M4 Claude: `yien-hook`, hook pipe, red mark, counter | `feat/` | pending |

## Decisions taken on the way

Record here what a spike or a branch decided, with the branch name. The plan's spike log in `docs/` holds the details.

- `build/workspace-skeleton`: layer-0 crates do not depend on each other. `yien-layout` keeps its own `PaneId` (a `u64` handed out by the tree); mapping it to `yien_proto::PaneId` is the core's job.
- `build/workspace-skeleton`: foothy's `win32.rs` (win32-input-mode encoding) was left out of `yien-input` as the TASK asked. It makes no OS calls, and plan §3.2 places win32-input-mode in `yien-input`, so it should come back there with M1 input, not go to `yien-platform`. Done in `build/tracing-and-input-mode` (`yien_input::win32`).
- `build/workspace-skeleton`: `xtask check-deps` also requires `#![forbid(unsafe_code)]` in every crate root except `yien-pty` and `yien-platform`, limits `yien-proto` to `serde`, and holds layers 0 and 1 to an allowlist of external crates kept in `xtask/src/check_deps.rs` (adding one is a reviewed change).
- `feat/vt-engine`: the engine has no clock. A synchronized update (mode 2026) holds output and replies until it closes; the pane actor calls `flush_sync` once `sync_pending` has been true for `SYNC_UPDATE_TIMEOUT` (150 ms), restarting that timer whenever `sync_generation` changes.
- `feat/vt-engine`: `Frame.cursor` is `None` only when scrolled out of view; a hidden cursor keeps its point (`Cursor::visible` false) so the IME window can follow it (RF-PAN-15).
- `feat/vt-engine`: `osc_scan` treats `OSC 9;<number>;…` as a ConEmu command (`9;4` progress, `9;9` cwd), never as a notification, so it cannot raise a false red mark (RN-02). Only 7-bit `ESC ]`, BEL and `ESC \` are recognised; C1 bytes are UTF-8 continuation bytes in practice.
- `feat/vt-engine`: link detection covers OSC 8, URLs and absolute Windows drive paths; relative and Unix paths are left for later.
- `feat/ui-shell`: ADR-02 taken: the UI is GPUI through the crates.io package `gpui-kit =0.7.1` (bundles gpui and gpui-component), the pin Zenkai builds and runs on this PC. The lock holds `gpui-base`, `gpui-component` and `gpui-kit-assets` at 0.7.1 to match Zenkai. Its tree needs `0BSD` and `bzip2-1.0.6` in the deny licence list and seven unmaintained-crate advisories ignored (listed in `deny.toml`).
- `feat/ui-shell`: the `yien` binary is layer 4 in `check-deps`; it may depend on every layer and nothing depends on it. `yien-ui` owns `run()`, so the binary does not depend on gpui.
- `feat/ui-shell`: the view model (`WorkspaceView`) lives in `yien-ui`, keyed by `yien-proto` ids, and models all five RF-AGE-05 states. The counter counts panes (RF-ATN-03); project heads count tabs.
- `build/tracing-and-input-mode`: logs go to `<data dir>/logs`, where the data dir is `%LOCALAPPDATA%\YIEN` or `$YIEN_DATA_DIR`, not to the state dir that plan §5.4 names, so a throwaway profile redirects them too. Debug builds also log to stderr. `yien-platform` uses `#![deny(unsafe_code)]`, so later OS code can allow `unsafe` per module with a `// SAFETY:` line.
- `build/tracing-and-input-mode`: the log guard is dropped in the callback `yien_ui::run` takes for gpui's `on_app_quit`, because gpui calls `process::exit` at Windows end of session without returning from `run`. `vte` (off) and `alacritty_terminal` (warn) are capped on top of `YIEN_LOG`, since their debug and trace records carry terminal content. The binary logs panics (location and a message capped at 200 chars) before the default hook.
- `feat/ui-shell`: the sumi `shu` red is below WCAG AA on `panel` (4.39:1) and `raised` (4.11:1), where the tab bar and the counter use it (RNF-18). Left as designed, pending a token change.
- `feat/pty-backend`: resizes go through the writer thread with the input (plan §4.1), so `resize` reports only "queued"; an OS resize failure is not surfaced.
- `feat/pty-backend`: end of session is the child's exit (a blocking `wait`; `try_wait` reads exit code 259 as still running), then 100 ms of quiet output measured from the later of the exit and the reader's last wait, capped at 2 s. Both clocks count only time the reader spends waiting in `read`, so the cap does not bound wall time while a consumer is behind; and `Exited` is queued only then, so every byte read before it is delivered first. Bytes that arrive after `Exited` are still forwarded.
- `feat/pty-backend` open: closing a session while the writer is inside a long `write_all` (a large paste into a stalled child) waits for that write; not verified.
- `feat/pty-backend`: `write` never blocks: a full input queue returns the bytes in `PtyError::InputQueueFull`. Dropping the session kills the child.
- `feat/pty-backend`: portable-pty 0.9 loads a `conpty.dll` found on the DLL search path before the kernel32 one. That is the seam for the bundled ConPTY (TBD-05); today nothing ships it, so the system ConPTY is used.
- `feat/agent-state-machine`: the per-pane hook token travels in the `YIEN_HOOK_TOKEN` environment variable (04 §2.4), never in the `--settings` file. Registered Claude events: the eight of 04 §2.4 plus `PostToolUse` and `PostToolUseFailure`, the real exits from a permission wait.
- `feat/agent-state-machine`: hooks use Claude Code's exec form (`"command": "<yien-hook.exe>", "args": ["<event>"]`), so no shell runs them and nothing depends on Git Bash or PowerShell. Since which Claude Code version first accepts `args` is unknown, S6 must check it against the 2.0 rule; `yien-hook` should read the event from the payload first.
- `feat/agent-state-machine`: a `PermissionRequest` carrying `agent_id` (a subagent) or with an unreadable payload never lights the red mark (RN-02); `agent_type` alone does not count, since a main session run with `--agent` carries it too. S6 must confirm with fixtures whether subagent requests show a dialog.
- `feat/agent-state-machine`: user input followed by output during the 300 ms confirmation window cancels the pending red, as it leaves a confirmed one.
- `feat/agent-state-machine`: OSC 9, OSC 777 and BEL never change the state: they cannot tell "done" from "waiting" (RN-02). Codex and OpenCode stay idle-only until S7 and S8.
- `feat/agent-state-machine`: diverges from 04 §4.7 ("se usa la última"): an unverified CLI version uses the newest rule (highest lower bound), not the last one listed; a rule without `versions` is never counted as verified (RF-AGE-11).
- `feat/agent-state-machine`: `regex` joins the layer-1 allowlist (plan §3.2).
- `feat/agent-state-machine`: diverges from 04 §4.5: a `SessionStart` with `source = compact` or `resume` during a turn keeps the state and the red mark; `startup`, `clear` and the rest go to Idle as 04 says.
- `feat/agent-state-machine`: a hook payload that is too large or malformed falls back to the event name `yien-hook` got in argv, with no payload fields (`decode_claude_hook_or_event`), instead of `Ambiguous`.
- `feat/agent-state-machine`, open: parallel tool calls (a `PostToolUse` from another tool clears a pending or confirmed red; S6 fixtures decide); no warning channel for unknown profile keys (plan §7.4); value errors in profiles name the field path, not the line; no `.yrec` fixtures for the Claude version ranges (RNF-23); Codex and OpenCode resume arguments unverified (S7, S8); `AgentState` not yet in `yien-proto`; Windows hook shell and quoting unverified (S6).

## In flight

Branches with an implementer running, and their slots.

- (none)
