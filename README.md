# YIEN

A Windows-first desktop IDE for coding agents. YIEN keeps your work in projects, each with vertical tabs and split terminal panes that run the official agent CLIs (Claude Code, Codex and OpenCode) next to your shell. When an agent stops and waits for you, YIEN puts a red "needs you" mark on its pane, tab and project and counts it in the title bar, so you can leave agents running and come back when one needs an answer (Claude Code today; Codex and OpenCode later). Sessions are meant to survive restarts.

Linux and macOS are planned; the code keeps the platform layer separate so they are not a rewrite.

## Status

Early, working towards the first usable version (MVP). Today YIEN opens a window with a project on the current folder, tabs, and terminal panes running your shell (PowerShell 7, Windows PowerShell or `cmd.exe`, the first one found) through ConPTY. You can split, resize, swap, maximize and close panes, copy and paste, and run any agent CLI inside a pane. Claude Code panes report their state through hooks, which drives the red mark. Session restore and the command palette are in progress. Expect rough edges.

## Run the portable build

1. Build `yien-<version>-windows-x64.zip` with `cargo xtask dist` (see below; there is no published release yet) and unzip it anywhere.
2. Run `yien.exe`. It needs Windows 10 version 1809 or later. The binaries are not signed yet, so SmartScreen may ask for confirmation.

YIEN opens a project on the folder it was started from; start it from a terminal in your repository (`path\to\yien.exe`) to open that one. `yien-hook.exe` must stay next to `yien.exe`: it is the small program Claude Code runs to report its state to YIEN. Without it, Claude panes still work but show "signal lost" instead of the red mark.

Data and logs go to `%LOCALAPPDATA%\YIEN` (logs in `logs\`). Set `YIEN_DATA_DIR` to use another folder and `YIEN_LOG` (for example `debug`) to change the log level.

### Removing YIEN

Delete the unzipped folder and `%LOCALAPPDATA%\YIEN`. The first time YIEN shows a notification it also registers its name for Windows under `HKEY_CURRENT_USER\Software\Classes\AppUserModelId\YIEN.App`; remove that key too, for example with `reg delete "HKCU\Software\Classes\AppUserModelId\YIEN.App" /f`.

## Build from source

Install Rust with [rustup](https://rustup.rs); `rust-toolchain.toml` pins the toolchain, which rustup installs on the first build. Then:

```sh
cargo run -p yien
```

`cargo xtask dist` builds the release binaries and writes the portable folder and zip to `target/dist/`.

## Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+Shift+P | Command palette: every action, with its keys |
| Ctrl+Shift+O | Go to a tab, pane or project; Enter with an empty query goes back to the last tab |
| Ctrl+1 ... Ctrl+9 | Go to that tab of the current project |
| Ctrl+Shift+T | New tab with your shell |
| Ctrl+Shift+A | New tab with Claude Code |
| Ctrl+Shift+S | Split the focused pane, Claude Code on the right |
| Ctrl+Shift+J | Jump to the next pane that needs you, oldest first |
| Alt+Shift+= | Split the focused pane, new pane on the right |
| Alt+Shift+- | Split the focused pane, new pane below |
| Ctrl+Shift+W | Close the focused pane |
| Alt+Arrows | Move the focus to the pane in that direction |
| Alt+Shift+Right / Down | Grow the focused pane within its split |
| Alt+Shift+Left / Up | Shrink the focused pane within its split |
| Ctrl+Alt+Shift+Arrows | Swap the focused pane with its neighbour in that direction |
| Ctrl+Shift+Enter | Maximize the focused pane, or restore the layout |
| Ctrl+Shift+C | Copy the terminal selection |
| Ctrl+Shift+V | Paste into the terminal |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+Shift+E | Show or hide the tab bar |
| Ctrl+Shift+D | Show or hide the context bar |
| Ctrl+Shift+L | Switch between the dark (sumi) and light (washi) themes |

## Architecture

YIEN is a Rust workspace in layers, where each layer depends only on the ones below it. Layer 0 holds pure data and logic with no OS calls: the messages and command catalog (`yien-proto`), the pane layout tree (`yien-layout`) and key encoding (`yien-input`). Layer 1 holds the terminal engine (`yien-vt`, on `alacritty_terminal`), the agent profiles and state machine (`yien-agent`) the session model (`yien-session`) and the settings file (`yien-config`), still with no OS calls, so all of it is tested headless. Layer 2 adapts the OS: pseudo-terminals (`yien-pty`), git (`yien-git`) and notifications, paths, logging and IPC (`yien-platform`). Layer 3 is the core, one actor per pane (`yien-core`), and the GPUI window (`yien-ui`), which talks to the core only through `yien-proto` messages. The `yien` binary wires the two together, and `yien-hook` is the binary Claude Code runs for each hook.

## Licence

Licence: not decided yet (pre-release).
