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
| M1 one pane: PTY, VT, terminal element, input, scrollback | several `feat/` | started: `feat/terminal-view` (in review) draws each pane's frame, sends keys, IME text, AltGr, wheel, mouse reports, selection, copy and paste to the core; link opening, search and real terminal modes wait for the core-panes API |
| VT engine: `yien-vt`, `VtEngine` on alacritty, `osc_scan`, frames, hot scrollback, search, links | `feat/vt-engine` | done on the branch, in review; cold scrollback and frame diffs pending |
| M2 workspace: projects, tabs, splits, focus, palette, commands, theme | several `feat/` | started: `feat/ui-shell` draws the window (tab bar, panes from a `yien-layout` tree, context bar, status bar, both themes) from a view model with sample data; `feat/terminal-view` replaces the sample data with the core's workspace and adds split, close, focus, resize, swap and maximize keys; palette, divider drag and pane drag pending |
| M1 PTY backend: `yien-pty`, portable-pty over the system ConPTY, reader/writer/waiter threads, `FakePty` | `feat/pty-backend` | done on the branch: tests green on Windows; unix tests not yet run |
| M2 core: `yien-core`, workspace model (projects, tabs, layouts), one actor per pane (PTY, VT, agent state), `ClientMsg`/`CoreMsg` in `yien-proto` | `feat/core-panes` | done on the branch: tests green on Windows with `FakePty`; no UI wiring yet |
| Test control pipe | `feat/test-control` | pending, from M1 on |
| M3 core: snapshot and restore of layout and sessions | `feat/session-store`, then `feat/` | started: `feat/session-store` adds `yien-session` (model, invariants, versioned JSON with migrations, 5 generations, resume plan), done on the branch, in review; core wiring, screens, single instance and previous-session listing pending |
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
- `feat/agent-state-machine`, open: parallel tool calls (a `PostToolUse` from another tool clears a pending or confirmed red; S6 fixtures decide); no warning channel for unknown profile keys (plan §7.4); value errors in profiles name the field path, not the line; no `.yrec` fixtures for the Claude version ranges (RNF-23); Codex and OpenCode resume arguments unverified (S7, S8); Windows hook shell and quoting unverified (S6).
- `feat/core-panes`: the frame types (`Frame`, `Cell`, `Rgb` and the rest) moved from `yien-vt` to `yien_proto::frame` with serde; `yien-vt` re-exports them unchanged, so the UI can draw frames without `yien-vt`. `Frame` carries the terminal `Modes` (DECCKM, bracketed paste, mouse, focus reporting). `PROTO_VERSION` is 3.
- `feat/core-panes`: workspace actions are `yien_proto::Command` variants (ADR-19), wrapped in `ClientMsg::Command`; terminal traffic (input, reports, resize, scroll, selection, window focus) is a separate `ClientMsg` variant. The catalog has no titles or shortcuts yet (palette work).
- `feat/core-panes`: the core owns a 2-thread tokio runtime (`tokio 1.54.1`, already in the lock through gpui). Each pane is a task plus one reader thread; the engine sits behind one mutex the reader takes per chunk and the actor takes to copy a frame. Every PTY write goes through the actor's outbox in this order: VT replies, then a pending resize, then input; a refused write is retried every 5 ms. Every queue is bounded (plan §4.2): `ClientMsg::Input` and `Report` go from the handle straight to the pane's own queue (`PANE_INPUT_QUEUE`, 256), and the actor takes the next chunk only once the PTY accepted the last, so a program that stops reading makes `try_send` return `Busy` for that pane alone while commands (close, terminate) still pass. Control commands have a 256-slot queue the actor always drains; more than 64 unwritten replies stop the actor taking output, which stalls the child. Actors report to the service through a mailbox that keeps lifecycle events in order and only the latest state, title, directory, session id and selection between them, so a stalled UI cannot grow it.
- `feat/core-panes`: frames are "last one wins" (plan §4.2): each pane has one frame slot; the actor puts at most one frame per 8 ms while dirty and visible, and sends `CoreMsg::PaneFrameReady { pane }` only when the UI has taken the previous one; the UI calls `CoreHandle::take_frame(pane)`. A notice refused by a full event queue is sent as soon as there is room. Every other event waits for room. A pane in a hidden tab, or hidden by a maximized pane, publishes no frames.
- `feat/core-panes`: swap exchanges the core's leaf-to-pane mapping (no layout change, no respawn); `ResizeSplit` is `Tree::resize(pane, delta)`; `yien-layout` gained `Tree::shape()` so the layout reaches the UI as `LayoutNode`. Closing the last pane of a tab closes the tab; a split focuses the new pane.
- `feat/core-panes`: agent profiles launch their detected command; launch arguments and env vars that need a placeholder only the hooks pipe or session layer can fill (`{new_uuid}`, `{session_id}`, `{hooks_settings_file}`, `{hook_token}`) are left out, so Claude starts with no arguments for now. The CLI version is not probed, so the newest rule runs unverified.
- `feat/core-panes`: OSC 133 `B` (typing a command) has no agent meaning; `C` maps to the agent's command start and `D` to its end. `yien-vt` does not parse OSC 7501, so that source stays silent.
- `feat/core-panes`: the service keeps each pane's launch plan and last size, so relaunching a pane whose actor panicked starts a fresh actor at that size, and the workspace sends it the view again (§4.5, RF-PAN-19). Every session runs on a fresh engine, so modes and screen of the last run do not leak and an old reader cannot feed the new one. A reader that dies before `Exited` ends the program and fails the pane with a message; the pane can be relaunched.
- `feat/core-panes`, open: frames are not a `CoreMsg`, so a future `--host` transport must carry them and fill the client side of `take_frame` (ADR-03); `Effect::Notify`, OSC 52 clipboard and the bell are not forwarded; tab branch is always `None` until `yien-git`; no `tracing`; core tests run on real time (`FakeClock` is `!Sync`); `ResizeSplit` spreads the change over all siblings rather than one divider.
- `feat/agent-state-machine`, open: parallel tool calls (a `PostToolUse` from another tool clears a pending or confirmed red; S6 fixtures decide); no warning channel for unknown profile keys (plan §7.4); value errors in profiles name the field path, not the line; no `.yrec` fixtures for the Claude version ranges (RNF-23); Codex and OpenCode resume arguments unverified (S7, S8); `AgentState` not yet in `yien-proto`; Windows hook shell and quoting unverified (S6).
- `feat/session-store`: the state file is `workspace.json` (plan §7.2), with backups `workspace.1.json` to `workspace.4.json`: five copies in all (RF-SES-06). `save` rotates only when the snapshot is `ROTATION_INTERVAL_MS` (60 s) past the one in `workspace.1.json`; other saves replace `workspace.json` atomically. A damaged `workspace.json` is moved to `workspace.damaged.json` for diagnosis, leaving the backups untouched.
- `feat/session-store`: a generation with a newer `schema_version` stops `load` without restoring and makes `save` refuse; `preserve_newer` renames it to `workspace.v<N>.json` (plan §5.2), after which saving works.
- `feat/session-store`: split ratios are checked as positive, one per child and summing to 1 (within 1e-3), but not against `yien-layout`'s 5 % minimum, because repeated splits legitimately go below it.
- `feat/session-store`: schema v1 carries every plan §6.1 field before it ships: `selected` (project and tab, both optional, validated to exist), tab `branch`, `worktree` and `archived`, pane `name`, project `setup_script`. All have serde defaults, so a v1 file without them still loads.
- `feat/session-store`: file I/O goes through the `Store` trait; the `fs` feature's `FsStore` uses only `std::fs` (temp file, `sync_all`, `rename`) and lacks `MOVEFILE_WRITE_THROUGH`, so `yien-platform` should supply the production store.

- `feat/terminal-view`: the UI reaches the core through a `CoreClient` trait the binary implements over `CoreHandle`, and reads events from the core's tokio receiver on GPUI's executor (`yien-ui` gains `tokio` with `sync` only). A busy core never drops input: the UI queues messages in order and retries every 5 ms.
- `feat/terminal-view`: printable text reaches a pane only through GPUI's input handler (WM_CHAR, IME, AltGr, dead keys); key-down handles named keys and Ctrl/Alt combinations and stops propagation, which on Windows suppresses the WM_CHAR. Keys with `prefer_character_input` (AltGr) are always text.
- `feat/terminal-view`: rows are shaped in runs of equal style with GPUI's forced cell width; every wide character is its own two-cell run. Cell size comes from the mono font's advance and ascent plus descent at 13 px, snapped to device pixels (RNF-19).
- `feat/terminal-view`: tab selection goes through `Command::SelectTab`, since the core sends frames only for the active tab. Closing the window quits the app so the core's panes end.
- `feat/terminal-view`: shortcuts follow Windows Terminal and the prototype: Alt+Shift+= / Alt+Shift+- split, Ctrl+Shift+W closes, Alt+arrows focus, Alt+Shift+arrows grow or shrink within the pane's split, Ctrl+Alt+Shift+arrows swap, Ctrl+Shift+Enter maximizes, Ctrl+Shift+C/V copy and paste.
- `feat/terminal-view`, open: until `Frame` carries `modes`, the UI reads default modes (no app cursor, bracketed paste or mouse mode); link opening (RF-PAN-11) needs a link lookup message; search (RF-PAN-08) has no UI; divider drag (RF-PAN-02) and pane drag (RF-PAN-05) are not done.

## In flight

Branches with an implementer running, and their slots.

- (none)
