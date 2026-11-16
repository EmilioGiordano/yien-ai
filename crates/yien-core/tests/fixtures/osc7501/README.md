# OSC 7501 captures

Real bytes an agent CLI wrote to its pseudo console, replayed by
`crates/yien-core/src/status_replay.rs` (RNF-23: each supported CLI has
regression tests per version). Captures were recorded on Windows through
`cargo xtask capture` against the local mocks in `scripts/test-control/`,
with throwaway profiles; no account, API key or quota is involved. The
paths inside the fixtures name the build-drive slot of the recording machine
(`C:\yien-compiled\slot-4\…`); nothing in the tests depends on them.

Codex is not installed on the recording machine, so there is no Codex
capture here yet; the Codex screen fixtures live in
`crates/yien-agent/tests/fixtures/startup-screens/`.

## The table

| CLI | Version | Case | File | Step file |
|---|---|---|---|---|
| Claude Code | 2.1.325 | startup and folder trust screen, accepted, until the REPL reports | `claude-2.1.325-startup.bin` | `claude-2.1.325-startup.steps` |
| Claude Code | 2.1.325 | Esc mid-turn | `claude-2.1.325-esc.bin` | `claude-2.1.325-esc.steps` |
| Claude Code | 2.1.325 | Ctrl+C once mid-turn / Ctrl+C three times to exit | `claude-2.1.325-ctrl-c.bin` (reused from #67) | — |
| Claude Code | 2.1.325 | permission prompt | `claude-2.1.325-permission.bin` | `claude-2.1.325-permission.steps` |
| Claude Code | 2.1.325 | API error (mock `mockerror` trigger) | `claude-2.1.325-error.bin` | `claude-2.1.325-error.steps` |
| Claude Code | 2.1.325 | normal exit (`/exit`) | `claude-2.1.325-exit.bin` | `claude-2.1.325-exit.steps` |
| OpenCode | 2.3.0 | startup until it reports idle | `opencode-2.3.0-startup.bin` | `opencode-2.3.0-startup.steps` |
| OpenCode | 2.3.0 | Esc mid-turn (this build aborts on the second Esc) | `opencode-2.3.0-esc.bin` | `opencode-2.3.0-esc.steps` |
| OpenCode | 2.3.0 | Ctrl+C once mid-turn, which also quits (no "three times" case exists) | `opencode-2.3.0-ctrl-c.bin` | `opencode-2.3.0-ctrl-c.steps` |
| OpenCode | 2.3.0 | permission prompt | `opencode-2.3.0-permission.bin` | `opencode-2.3.0-permission.steps` |
| OpenCode | 2.3.0 | API error (mock `mockerror` trigger) | `opencode-2.3.0-error.bin` | `opencode-2.3.0-error.steps` |
| OpenCode | 2.3.0 | normal exit (Ctrl+C at the idle prompt) | `opencode-2.3.0-exit.bin` | `opencode-2.3.0-exit.steps` |

`claude-2.1.325-ctrl-c.bin` also carries the exit (`clear` records): reused,
not redone. `claude-2.1.325-typed-in-powershell.bin` and
`claude-2.1.325-ctrl-c-typed-in-powershell.bin` predate this fixture set
and stay as they are.

The user name inside the 2.1.325 captures is the build machine's slot path,
not a person; the permission and error fixtures send their turn to the mock,
and the reply is synthetic.

## Recapturing for a new CLI version

1. Build the capture tool once: `scripts/cargo.sh build -p xtask`.
2. For each case, make a fresh throwaway folder and point the CLI at the
   mocks (never at a real account):

   **Claude Code** — a fresh `CLAUDE_CONFIG_DIR` with `hasCompletedOnboarding:
   true`, a dummy `ANTHROPIC_API_KEY` (`sk-ant-api03-` + 80 `y` + `-MOCKAA`),
   `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>`, `DISABLE_AUTOUPDATER=1`,
   `DISABLE_TELEMETRY=1`, `DISABLE_ERROR_REPORTING=1`,
   `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and `YIEN_AGENT_HOME` under
   the root; `--env-remove` the `CLAUDECODE`, `CLAUDE_CODE_*` markers. Trust
   the work folder in `.claude.json` (projects map) unless the case is the
   trust screen itself. For the permission case, launch with
   `--permission-mode manual`.

   **OpenCode** — a fresh `HOME`/`USERPROFILE` and `XDG_*` under the root, a
   project-level `opencode.json` with an `oai` provider on the built-in
   OpenAI-compatible runtime pointing at `http://127.0.0.1:<port>/v1` (model
   `oai/mock`, dummy `MOCK_API_KEY`), a global config that only sets
   `update: disable`, and `YIEN_AGENT_HOME` under the root. Launch with
   `--standalone`: the managed service defaults to port 28894, which another
   instance on the machine may hold. For the permission case the project
   config needs `permission: { bash: "ask" }`, or nothing asks.

   The mocks are `scripts/test-control/mock-messages-api.js` and
   `mock-openai-api.js`; start one with `node <mock> --port 0 --log
   <root>/mock.log` and read the port from its `listening <port>` line.
3. Run the capture; the step file drives the child, the engine answers the
   terminal's queries the way YIEN does, and the raw bytes land in `--out`:

   ```
   cargo xtask capture --steps <case>.steps --out <case>.bin --cwd <work> \
       [--size 120x40] [--env KEY=VALUE]... [--env-remove KEY]... \
       -- <cli> [args...]
   ```

   Claude steps are run with the env of step 2; OpenCode with `HOME` et al.
   from step 2. The `.bin` is written only on success; a failure keeps a
   `.partial` next to it for inspection.
4. Check the records: `python -c "re.findall(rb'\\x1b\\]7501;[^\\x07]*',
   open('<case>.bin','rb').read())"` and update the tests in
   `crates/yien-core/src/status_replay.rs` if a state changed on purpose.
   Record new failures as findings; do not change product behaviour to match
   a fixture.
5. Update the table above and the tests named `rnf_23_`.

Key handling note: the capture tool encodes keys the way a kitty-conformant
terminal does when the child turns on the kitty keyboard protocol
(`Esc` → `CSI 27u`, control keys → `CSI <codepoint>;<mods>u`); other keys go
through yien-input exactly as YIEN sends them. Claude Code and OpenCode both
enable the protocol on Windows.
