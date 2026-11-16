//! `cargo xtask capture`: run a program in a pseudo terminal, drive it with
//! a step file and write the bytes it printed to a `.bin` fixture.
//!
//! Steps, one per line. `#` starts a comment; blank lines are ignored. The
//! file is UTF-8; each line is one step:
//!
//! - `text <string>` — write `<string>` as typed, without a newline.
//! - `key <name>` — one key press: `enter`, `esc`, `tab`, `shift-tab`,
//!   `backspace`, `space`, `up`, `down`, `left`, `right`, `home`, `end`,
//!   `pageup`, `pagedown`, `insert`, `delete`, `f1`..`f12`, or `ctrl-<letter>`.
//!   Keys are encoded with yien-input from the modes the program set, the
//!   same way YIEN encodes them.
//! - `wait <ms>` — let the child run for `<ms>` milliseconds.
//! - `wait-for <ms> <text>` — wait until the screen shows `<text>`, at most
//!   `<ms>` milliseconds, then fail.
//! - `wait-exit <ms>` — wait until the child exits, at most `<ms>`
//!   milliseconds, then fail.
//!
//! The terminal's startup queries (cursor position, device attributes and
//! the rest the engine answers) are answered the way YIEN answers them:
//! the output feeds a fresh [`AlacrittyEngine`] and its replies go back to
//! the child. The child starts from an allowlist of its caller's environment
//! (`PATH`, `PATHEXT`, `SystemRoot`, temp and processor variables, `OS`,
//! `TERM`/`COLORTERM`) plus whatever `--env` passes, and the platform's
//! registry merge at spawn is suppressed with an empty start: credentials
//! from the session or the registry never reach it. `HOME`, `USERPROFILE`,
//! `CLAUDE_CONFIG_DIR`, `APPDATA`, `LOCALAPPDATA`, `HOMEDRIVE`, `HOMEPATH`
//! and `XDG_*` are likewise only passed when `--env` names them, and then
//! only with a throwaway location under `--cwd` or the slot named in the
//! repository's `.yien-slot`. Windows cannot fully isolate a CLI from the
//! user's profile: Node's `os.homedir()` and Bun resolve it from the OS,
//! not from any variable. Captures must therefore run only with throwaway
//! config dirs and the mocks; treat a capture as if the OS profile is
//! reachable for anything that reads it directly.
//! When the steps are done and the child is still running, the child is
//! terminated; the exit status is reported. Only on success the bytes are
//! written to the output file; on failure a `.partial` file next to it
//! holds what was captured so far.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use yien_input::{Key, KeyModes, KeyQuirks, Mods, encode_with, focus_report};
use yien_pty::{ExitStatus, PortablePty, PtyBackend, PtyEvent, SpawnSpec};
use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};

const USAGE: &str = "usage: cargo xtask capture --steps <file> --out <file.bin> \\
    --cwd <dir> [--size <cols>x<rows>] [--env KEY=VALUE]... [--env-remove KEY]... \\
    [--tail-ms <ms>] -- <program> [args...]";

pub fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("xtask capture: {message}");
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(2));
        }
    };
    match capture(&options) {
        Ok(summary) => {
            println!(
                "captured {} bytes -> {} (exit {})",
                summary.bytes, summary.out, summary.exit
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(failure) => {
            eprintln!("xtask capture: {}", failure.message);
            if let Some(partial) = failure.partial {
                eprintln!("partial capture kept at {}", partial.display());
            }
            Ok(ExitCode::FAILURE)
        }
    }
}

struct Options {
    steps: PathBuf,
    out: PathBuf,
    cwd: PathBuf,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
    env_remove: Vec<String>,
    tail_ms: u64,
    program: String,
    args: Vec<String>,
}

/// `Instant` overflows far beyond any capture; refuse the input instead.
const TAIL_MS_MAX: u64 = 3_600_000;

/// What the child gets from the caller's environment: what starting a CLI
/// needs, nothing else. Matched case-insensitively.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
    "TERM",
    "COLORTERM",
];
/// Variables that would point the child at a real user location. They are
/// never inherited; from `--env` they are allowed only with a throwaway
/// location.
const ENV_PROTECTED_EXACT: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "CLAUDE_CONFIG_DIR",
    "APPDATA",
    "LOCALAPPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
];
const ENV_PROTECTED_PREFIX: &[&str] = &["XDG_"];

fn is_protected(key: &str) -> bool {
    ENV_PROTECTED_EXACT.contains(&key)
        || ENV_PROTECTED_PREFIX
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

fn env_key(key: &std::ffi::OsStr) -> String {
    key.to_string_lossy().to_ascii_uppercase()
}

/// The child's environment: the allowlisted variables from the caller's
/// environment, overlaid with whatever `--env` passed (their value wins).
#[derive(Debug)]
struct ChildEnv {
    set: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

/// The child does not inherit the caller's environment and is not merged
/// with the registry's: it starts from an allowlist plus whatever `--env`
/// passes. A protected variable passed with `--env` must be an absolute
/// path inside `--cwd` or the slot named in the repository's `.yien-slot`,
/// which is the throwaway tree every recapture lives in; anything else is
/// refused before the child spawns.
fn child_env(
    inherited: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    explicit: &[(String, String)],
    remove: &[String],
    cwd: &Path,
    slot: Option<&Path>,
) -> Result<ChildEnv, String> {
    let explicit_of = |name: &str| {
        explicit
            .iter()
            .find(|(key, _)| key.to_ascii_uppercase() == name)
    };
    let mut out: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (key, value) in inherited {
        let upper = env_key(&key);
        if !ENV_ALLOWLIST.contains(&upper.as_str()) {
            continue;
        }
        if explicit_of(&upper).is_some() {
            continue;
        }
        out.push((key, value));
    }
    for (key, value) in explicit {
        let upper = key.to_ascii_uppercase();
        if is_protected(&upper) {
            let path = Path::new(value);
            if value.is_empty() || !path.is_absolute() {
                return Err(format!(
                    "--env {upper} wants an absolute path inside --cwd or the slot"
                ));
            }
            if !within(path, cwd) && slot.is_none_or(|slot| !within(path, slot)) {
                return Err(format!(
                    "--env {upper} points at {}; use a throwaway folder under --cwd or the slot",
                    path.display()
                ));
            }
        }
        let entry = (
            std::ffi::OsString::from(key),
            std::ffi::OsString::from(value),
        );
        if let Some(at) = out.iter().position(|(k, _)| env_key(k) == upper) {
            out[at] = entry;
        } else {
            out.push(entry);
        }
    }
    for remove in remove {
        let upper = remove.to_ascii_uppercase();
        out.retain(|(key, _)| env_key(key) != upper);
    }
    Ok(ChildEnv { set: out })
}

/// Whether `path` sits inside `root` or equals it. Both sides are
/// canonicalized first (with their deepest existing ancestor standing in for
/// not-yet-created throwaway dirs), so junctions, symlinks and `..` cannot
/// smuggle the real profile in through a path that looks safe.
fn within(path: &Path, root: &Path) -> bool {
    fn part(part: &std::path::Component<'_>) -> String {
        match part {
            std::path::Component::Prefix(prefix) => {
                normalize(&prefix.as_os_str().to_string_lossy())
            }
            std::path::Component::RootDir => String::new(),
            std::path::Component::Normal(part) => normalize(&part.to_string_lossy()),
            std::path::Component::CurDir | std::path::Component::ParentDir => String::new(),
        }
    }
    #[cfg(windows)]
    fn normalize(text: &str) -> String {
        text.to_lowercase()
    }
    #[cfg(not(windows))]
    fn normalize(text: &str) -> String {
        text.to_string()
    }
    fn canonicalize_lenient(path: &Path) -> PathBuf {
        let mut suffix = Vec::new();
        let mut current = path.to_path_buf();
        loop {
            if let Ok(mut base) = current.canonicalize() {
                for part in suffix.iter().rev() {
                    base.push(part);
                }
                return base;
            }
            match current.file_name() {
                Some(name) => suffix.push(name.to_os_string()),
                None => return current,
            }
            current = match current.parent() {
                Some(parent) => parent.to_path_buf(),
                None => return current,
            };
        }
    }
    let wrap = |path: &Path| {
        path.components()
            .map(|item| part(&item))
            .collect::<Vec<_>>()
    };
    let parts = wrap(&canonicalize_lenient(path));
    let root = wrap(&canonicalize_lenient(root));
    root.len() <= parts.len() && parts[..root.len()] == root[..]
}

/// The slot named in the repository's `.yien-slot`, if it can be read.
fn slot_dir() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent()?;
    let text = std::fs::read_to_string(root.join(".yien-slot")).ok()?;
    let slot = text.lines().next()?.trim();
    if slot.is_empty() {
        return None;
    }
    let path = PathBuf::from(slot);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut steps = None;
        let mut out = None;
        let mut cwd = None;
        let mut size = None;
        let mut env = Vec::new();
        let mut env_remove = Vec::new();
        let mut tail_ms = 10_000;
        let mut program_args = None;
        let mut at = 0;
        while at < args.len() {
            let flag = &args[at];
            if flag == "--" {
                program_args = Some(args[at + 1..].to_vec());
                break;
            }
            let value = |at: &mut usize| -> Result<String, String> {
                *at += 1;
                args.get(*at)
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match flag.as_str() {
                "--steps" => steps = Some(PathBuf::from(value(&mut at)?)),
                "--out" => out = Some(PathBuf::from(value(&mut at)?)),
                "--cwd" => cwd = Some(PathBuf::from(value(&mut at)?)),
                "--size" => size = Some(value(&mut at)?),
                "--env" => {
                    let pair = value(&mut at)?;
                    let (key, val) = pair
                        .split_once('=')
                        .ok_or_else(|| format!("--env wants KEY=VALUE, got '{pair}'"))?;
                    env.push((key.to_owned(), val.to_owned()));
                }
                "--env-remove" => env_remove.push(value(&mut at)?),
                "--tail-ms" => {
                    let tail: u64 = value(&mut at)?
                        .parse()
                        .map_err(|_| "--tail-ms wants milliseconds".to_string())?;
                    if tail > TAIL_MS_MAX {
                        return Err(format!("--tail-ms wants at most {TAIL_MS_MAX}"));
                    }
                    tail_ms = tail;
                }
                _ => return Err(format!("unknown flag '{flag}'")),
            }
            at += 1;
        }
        let program_args = program_args.ok_or("missing -- <program> [args...]")?;
        let (program, args) = program_args
            .split_first()
            .ok_or("missing -- <program> [args...]")?;
        let (cols, rows) = match size.as_deref() {
            Some(text) => parse_size(text)?,
            None => (120, 40),
        };
        Ok(Self {
            steps: steps.ok_or("--steps is required")?,
            out: out.ok_or("--out is required")?,
            cwd: cwd.ok_or("--cwd is required")?,
            cols,
            rows,
            env,
            env_remove,
            tail_ms,
            program: program.clone(),
            args: args.to_vec(),
        })
    }
}

fn parse_size(text: &str) -> Result<(u16, u16), String> {
    let (cols, rows) = text
        .split_once('x')
        .ok_or_else(|| format!("--size wants <cols>x<rows>, got '{text}'"))?;
    let cols: u16 = cols
        .parse()
        .map_err(|_| format!("bad columns in '{text}'"))?;
    let rows: u16 = rows.parse().map_err(|_| format!("bad rows in '{text}'"))?;
    if cols == 0 || rows == 0 {
        return Err(format!("--size wants at least 1x1, got '{text}'"));
    }
    Ok((cols, rows))
}

#[derive(Debug, Clone)]
enum Step {
    Text(Vec<u8>),
    Key(Key, Mods),
    Wait(Duration),
    WaitFor { timeout: Duration, needle: String },
    WaitExit(Duration),
}

impl Step {
    fn describe(&self) -> String {
        match self {
            Self::Text(_) => "text".to_string(),
            Self::Key(key, mods) => format!("key {key:?} {mods:?}"),
            Self::Wait(d) => format!("wait {}", d.as_millis()),
            Self::WaitFor { timeout, needle } => {
                format!("wait-for {} {needle}", timeout.as_millis())
            }
            Self::WaitExit(d) => format!("wait-exit {}", d.as_millis()),
        }
    }
}

fn parse_steps(text: &str) -> anyhow::Result<Vec<Step>> {
    let mut steps = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        // A CRLF step file: drop the CR; everything else is significant.
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let step = parse_step(line).with_context(|| format!("step {}", number + 1))?;
        steps.push(step);
    }
    Ok(steps)
}

fn parse_step(line: &str) -> anyhow::Result<Step> {
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim_start();
    match command {
        "text" => Ok(Step::Text(rest.as_bytes().to_vec())),
        "key" => {
            let (key, mods) = parse_key(rest)?;
            Ok(Step::Key(key, mods))
        }
        "wait" => Ok(Step::Wait(parse_ms(rest)?)),
        "wait-for" => {
            let (timeout, needle) = rest
                .split_once(' ')
                .ok_or_else(|| anyhow::anyhow!("wait-for wants <ms> <text>"))?;
            let timeout = parse_ms(timeout)?;
            let needle = needle.trim().to_string();
            if needle.is_empty() {
                bail!("wait-for wants a non-empty <text>");
            }
            Ok(Step::WaitFor { timeout, needle })
        }
        "wait-exit" => Ok(Step::WaitExit(parse_ms(rest)?)),
        _ => bail!("unknown step '{command}'"),
    }
}

fn parse_ms(text: &str) -> anyhow::Result<Duration> {
    let ms: u64 = text
        .trim()
        .parse()
        .with_context(|| format!("want milliseconds, got '{text}'"))?;
    Ok(Duration::from_millis(ms))
}

fn parse_key(name: &str) -> anyhow::Result<(Key, Mods)> {
    let name = name.trim();
    if let Some(letter) = name.strip_prefix("ctrl-")
        && letter.len() == 1
        && let Some(ch) = letter.chars().next()
        && ch.is_ascii_lowercase()
    {
        return Ok((Key::Char(ch), Mods::CTRL));
    }
    if let Some(digit) = name.strip_prefix('f')
        && let Ok(number) = digit.parse::<u8>()
        && (1..=12).contains(&number)
    {
        return Ok((Key::F(number), Mods::NONE));
    }
    let key = match name {
        "enter" => Key::Enter,
        "esc" | "escape" => Key::Escape,
        "tab" => return Ok((Key::Tab, Mods::NONE)),
        "shift-tab" => return Ok((Key::Tab, Mods::SHIFT)),
        "backspace" => Key::Backspace,
        "space" => Key::Char(' '),
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        _ => bail!("unknown key '{name}'"),
    };
    Ok((key, Mods::NONE))
}

struct Failure {
    message: String,
    partial: Option<PathBuf>,
}

struct Summary {
    bytes: usize,
    out: String,
    exit: String,
}

fn capture(options: &Options) -> Result<Summary, Failure> {
    let steps_text = std::fs::read_to_string(&options.steps).map_err(|err| Failure {
        message: format!("cannot read {}: {err}", options.steps.display()),
        partial: None,
    })?;
    let steps = parse_steps(&steps_text).map_err(|err| Failure {
        message: format!("{}: {err:#}", options.steps.display()),
        partial: None,
    })?;

    if !options.cwd.is_dir() {
        return Err(Failure {
            message: format!("--cwd {} is not a directory", options.cwd.display()),
            partial: None,
        });
    }
    let cwd = std::path::absolute(&options.cwd).map_err(|err| Failure {
        message: format!("--cwd {}: {err}", options.cwd.display()),
        partial: None,
    })?;
    let env = match child_env(
        std::env::vars_os(),
        &options.env,
        &options.env_remove,
        &cwd,
        slot_dir().as_deref(),
    ) {
        Ok(env) => env,
        Err(message) => {
            return Err(Failure {
                message,
                partial: None,
            });
        }
    };

    let mut spec = SpawnSpec::new(&options.program)
        .args(options.args.iter())
        .size(options.cols, options.rows)
        .cwd(cwd.clone())
        .env_clear();
    for (key, value) in &env.set {
        spec = spec.env(key, value);
    }

    let session = PortablePty::new().spawn(spec).map_err(|err| Failure {
        message: format!("cannot start {}: {err}", options.program),
        partial: None,
    })?;

    let mut engine = AlacrittyEngine::new(VtConfig::default(), options.cols, options.rows);
    let mut capture = Vec::new();
    let mut exited: Option<ExitStatus> = None;
    let mut index = 0usize;
    let mut step_started = Instant::now();
    let mut pump = Pump {
        engine: &mut engine,
        capture: &mut capture,
        focus_sent: false,
    };

    // Instant steps first, then the timed step, one event at a time.
    let result = drive(
        &steps,
        &session,
        &mut pump,
        &mut exited,
        &mut index,
        &mut step_started,
    );

    // Steps are done (or failed): stop the child and let the reader drain.
    if exited.is_none() {
        let _ = session.kill();
    }
    let tail_deadline = Instant::now() + Duration::from_millis(options.tail_ms);
    loop {
        if Instant::now() >= tail_deadline {
            break;
        }
        match session.events().recv_timeout(Duration::from_millis(25)) {
            Ok(PtyEvent::Output(bytes)) => pump.offer(&session, &bytes),
            Ok(PtyEvent::Exited(status)) => {
                if exited.is_none() {
                    exited = Some(status);
                }
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if exited.is_some() {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Err(message) = result {
        let partial = keep_partial(&options.out, &capture);
        return Err(Failure { message, partial });
    }

    if let Some(parent) = options.out.parent()
        && !parent.as_os_str().is_empty()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        let partial = keep_partial(&options.out, &capture);
        return Err(Failure {
            message: format!("cannot create {}: {err}", parent.display()),
            partial,
        });
    }
    if let Err(err) = std::fs::write(&options.out, &capture) {
        let partial = keep_partial(&options.out, &capture);
        return Err(Failure {
            message: format!("cannot write {}: {err}", options.out.display()),
            partial,
        });
    }
    Ok(Summary {
        bytes: capture.len(),
        out: options.out.display().to_string(),
        exit: match exited.and_then(|status| status.code) {
            Some(code) => code.to_string(),
            None => "unknown".to_string(),
        },
    })
}

fn keep_partial(out: &Path, capture: &[u8]) -> Option<PathBuf> {
    if capture.is_empty() {
        return None;
    }
    let mut name = out.as_os_str().to_owned();
    name.push(".partial");
    let partial = PathBuf::from(name);
    match std::fs::write(&partial, capture) {
        Ok(()) => Some(partial),
        Err(_) => None,
    }
}

/// One output path for the whole run: the bytes go to the capture and the
/// engine; the engine's replies go back to the child; once the child asks
/// for focus reports the pane claims focus, like a focused terminal.
struct Pump<'a> {
    engine: &'a mut AlacrittyEngine,
    capture: &'a mut Vec<u8>,
    focus_sent: bool,
}

impl Pump<'_> {
    fn offer(&mut self, session: &yien_pty::PtySession, bytes: &[u8]) {
        self.capture.extend_from_slice(bytes);
        let out = self.engine.feed(bytes);
        if !out.replies.is_empty() {
            let _ = session.write(out.replies);
        }
        if !self.focus_sent && self.engine.modes().focus_reporting {
            let _ = session.write(focus_report(true).to_vec());
            self.focus_sent = true;
        }
    }
}

fn drive(
    steps: &[Step],
    session: &yien_pty::PtySession,
    pump: &mut Pump<'_>,
    exited: &mut Option<ExitStatus>,
    index: &mut usize,
    step_started: &mut Instant,
) -> Result<(), String> {
    loop {
        // Instant steps first, then the timed step, one event at a time.
        while let Some(step) = steps.get(*index) {
            match step {
                Step::Text(bytes) => {
                    session
                        .write(bytes.clone())
                        .map_err(|err| err.to_string())?;
                    *index += 1;
                    *step_started = Instant::now();
                }
                Step::Key(key, mods) => {
                    let modes = pump.engine.modes();
                    let key_modes = KeyModes {
                        app_cursor: modes.app_cursor,
                        win32_input: modes.win32_input,
                        kitty_flags: modes.kitty_keyboard,
                        quirks: KeyQuirks::default(),
                    };
                    let bytes = encode_key(key, *mods, key_modes)?;
                    session.write(bytes).map_err(|err| err.to_string())?;
                    *index += 1;
                    *step_started = Instant::now();
                }
                _ => break,
            }
        }
        let Some(step) = steps.get(*index) else {
            return Ok(());
        };
        match session.events().recv_timeout(Duration::from_millis(10)) {
            Ok(PtyEvent::Output(bytes)) => pump.offer(session, &bytes),
            Ok(PtyEvent::Exited(status)) => {
                if exited.is_none() {
                    *exited = Some(status);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if exited.is_none() {
                    *exited = Some(ExitStatus::from_code(0));
                }
            }
        }
        let elapsed = step_started.elapsed();
        match step {
            Step::Wait(duration) => {
                if elapsed >= *duration {
                    *index += 1;
                    *step_started = Instant::now();
                }
            }
            Step::WaitFor { timeout, needle } => {
                if pump.engine.frame().text().contains(needle.as_str()) {
                    *index += 1;
                    *step_started = Instant::now();
                } else if elapsed >= *timeout {
                    return Err(timeout_error(*index, step, *timeout));
                }
            }
            Step::WaitExit(timeout) => {
                if exited.is_some() {
                    *index += 1;
                    *step_started = Instant::now();
                } else if elapsed >= *timeout {
                    return Err(timeout_error(*index, step, *timeout));
                }
            }
            Step::Text(_) | Step::Key(_, _) => unreachable!(),
        }
    }
}

/// Keys as a kitty-conformant terminal sends them when the child turned on
/// the kitty keyboard protocol: with disambiguate, Escape is `CSI 27u` and
/// a control or alt combination is `CSI <codepoint>;<mods>u`; everything
/// else falls back to yien-input, exactly as YIEN encodes it. Capturing a
/// kitty-mode child with legacy bytes would test the encoder, not the CLI.
fn encode_key(key: &Key, mods: Mods, modes: KeyModes) -> Result<Vec<u8>, String> {
    const KITTY_DISAMBIGUATE: u8 = 1;
    const KITTY_REPORT_ALL: u8 = 8;
    let kitty = modes.kitty_flags & (KITTY_DISAMBIGUATE | KITTY_REPORT_ALL) != 0;
    if kitty {
        let kitty_mods =
            1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);
        match key {
            Key::Escape => {
                let suffix = if mods == Mods::NONE {
                    "27u".to_string()
                } else {
                    format!("27;{kitty_mods}u")
                };
                return Ok(format!("\x1b[{suffix}").into_bytes());
            }
            Key::Char(c) if mods.ctrl || mods.alt => {
                let code = u32::from(c.to_ascii_lowercase());
                return Ok(format!("\x1b[{code};{kitty_mods}u").into_bytes());
            }
            _ => {}
        }
    }
    encode_with(key, mods, modes).ok_or_else(|| format!("key {key:?} {mods:?} encodes to nothing"))
}

/// Step timeout message for the caller.
fn timeout_error(index: usize, step: &Step, timeout: Duration) -> String {
    format!(
        "step {} ({}) timed out after {} ms",
        index + 1,
        step.describe(),
        timeout.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        list.iter()
            .map(|(key, value)| (OsString::from(*key), OsString::from(*value)))
            .collect()
    }

    fn root(label: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:/{label}"))
        } else {
            PathBuf::from(format!("/{label}"))
        }
    }

    fn set_names(env: &ChildEnv) -> Vec<String> {
        env.set
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn child_starts_from_an_empty_environment() {
        let inherited = &[
            ("ANTHROPIC_API_KEY", "sk-ant-real"),
            ("AWS_SECRET_ACCESS_KEY", "aws-real"),
            ("GOOGLE_APPLICATION_CREDENTIALS", "gcloud.json"),
            ("AZURE_CLIENT_SECRET", "azure-real"),
            ("MY_API_KEY", "k"),
            ("MY_TOKEN", "t"),
            ("HOME", "/home/real"),
            ("USERPROFILE", "C:/Users/real"),
            ("APPDATA", "C:/Users/real/AppData/Roaming"),
            ("LOCALAPPDATA", "C:/Users/real/AppData/Local"),
            ("HOMEDRIVE", "C:"),
            ("HOMEPATH", "/home/real"),
            ("CLAUDE_CONFIG_DIR", "/home/real/.claude"),
            ("XDG_CONFIG_HOME", "/home/real/.config"),
            ("USERNAME", "real"),
            ("PATH", "C:/Windows"),
            ("PATHEXT", ".EXE"),
            ("SYSTEMROOT", "C:/WINDOWS"),
            ("SYSTEMDRIVE", "C:"),
            ("WINDIR", "C:/WINDOWS"),
            ("COMSPEC", "C:/WINDOWS/system32/cmd.exe"),
            ("TEMP", "C:/Temp"),
            ("TMP", "C:/Temp"),
            ("NUMBER_OF_PROCESSORS", "16"),
            ("PROCESSOR_ARCHITECTURE", "AMD64"),
            ("OS", "Windows_NT"),
            ("TERM", "xterm"),
            ("COLORTERM", "truecolor"),
        ];
        let env = child_env(pairs(inherited), &[], &[], &root("work"), None).unwrap();
        let mut kept = set_names(&env);
        kept.sort();
        assert_eq!(
            kept,
            [
                "COLORTERM",
                "COMSPEC",
                "NUMBER_OF_PROCESSORS",
                "OS",
                "PATH",
                "PATHEXT",
                "PROCESSOR_ARCHITECTURE",
                "SYSTEMDRIVE",
                "SYSTEMROOT",
                "TEMP",
                "TERM",
                "TMP",
                "WINDIR",
            ]
        );
    }

    #[test]
    fn allowlist_matches_case_insensitively() {
        let inherited = &[("path", "C:/x"), ("systemroot", "C:/WINDOWS")];
        let env = child_env(pairs(inherited), &[], &[], &root("work"), None).unwrap();
        let mut kept = set_names(&env);
        kept.sort();
        assert_eq!(kept, ["path", "systemroot"]);
    }

    #[test]
    fn explicit_env_wins_over_the_allowlist_and_adds() {
        let inherited = &[("PATH", "C:/parent"), ("TERM", "xterm")];
        let explicit = vec![
            ("PATH".to_string(), "C:/custom".to_string()),
            ("MY_TOKEN".to_string(), "t".to_string()),
        ];
        let env = child_env(pairs(inherited), &explicit, &[], &root("work"), None).unwrap();
        let path = env
            .set
            .iter()
            .find(|(key, _)| env_key(key) == "PATH")
            .expect("PATH present");
        assert_eq!(path.1, OsString::from("C:/custom"));
        assert_eq!(
            env.set
                .iter()
                .filter(|(key, _)| env_key(key) == "PATH")
                .count(),
            1
        );
        assert!(env.set.iter().any(|(key, _)| env_key(key) == "MY_TOKEN"));
        assert!(set_names(&env).contains(&"TERM".to_string()));
    }

    #[test]
    fn env_remove_drops_from_the_final_set() {
        let explicit = vec![("PATH".to_string(), "C:/custom".to_string())];
        let env = child_env(
            pairs(&[("PATH", "C:/parent")]),
            &explicit,
            &["PATH".to_string()],
            &root("work"),
            None,
        )
        .unwrap();
        assert!(
            !set_names(&env)
                .iter()
                .any(|key| key.eq_ignore_ascii_case("PATH"))
        );
    }

    #[test]
    fn protected_env_under_cwd_is_accepted() {
        let explicit = vec![(
            "APPDATA".to_string(),
            format!("{}/appdata", root("work").display()),
        )];
        let env = child_env(vec![], &explicit, &[], &root("work"), None).unwrap();
        assert_eq!(env.set.len(), 1);
    }

    #[test]
    fn protected_env_under_slot_is_accepted() {
        let slot = root("slot-4");
        let explicit = vec![(
            "HOMEPATH".to_string(),
            format!("{}/run/capture/home", slot.display()),
        )];
        let env = child_env(vec![], &explicit, &[], &root("work"), Some(&slot)).unwrap();
        assert_eq!(env.set.len(), 1);
    }

    #[test]
    fn protected_env_outside_cwd_and_slot_is_refused() {
        let explicit = vec![(
            "LOCALAPPDATA".to_string(),
            "C:/Users/real/AppData/Local".to_string(),
        )];
        let err =
            child_env(vec![], &explicit, &[], &root("work"), Some(&root("slot-4"))).unwrap_err();
        assert!(err.contains("LOCALAPPDATA"), "{err}");
    }

    #[test]
    fn protected_env_relative_is_refused() {
        let explicit = vec![("XDG_DATA_HOME".to_string(), "throwaway".to_string())];
        let err = child_env(vec![], &explicit, &[], &root("work"), None).unwrap_err();
        assert!(err.contains("XDG_DATA_HOME"), "{err}");
    }

    #[test]
    fn protected_env_sibling_prefix_is_refused() {
        let cwd = root("a/b");
        let explicit = vec![(
            "CLAUDE_CONFIG_DIR".to_string(),
            root("a/bc").to_string_lossy().into_owned(),
        )];
        let err = child_env(vec![], &explicit, &[], &cwd, None).unwrap_err();
        assert!(err.contains("CLAUDE_CONFIG_DIR"), "{err}");
    }

    #[test]
    fn without_a_slot_only_cwd_counts() {
        let cwd = root("work");
        let outside = vec![(
            "HOMEDRIVE".to_string(),
            root("elsewhere").to_string_lossy().into_owned(),
        )];
        assert!(child_env(vec![], &outside, &[], &cwd, None).is_err());
        let inside = vec![("HOMEDRIVE".to_string(), format!("{}/drive", cwd.display()))];
        assert!(child_env(vec![], &inside, &[], &cwd, None).is_ok());
    }

    #[test]
    fn within_matches_prefixes_not_names() {
        assert!(within(&root("a/b"), &root("a/b")));
        assert!(within(&root("a/b/c"), &root("a/b")));
        assert!(!within(&root("a/bc"), &root("a/b")));
        assert!(within(&root(""), &root("")));
        assert!(!within(&root("a"), &root("a/b")));
    }

    #[test]
    fn within_canonicalizes_junctions_and_dot_dot() {
        // A path through a junction resolves to the target, not the link.
        let base = std::env::temp_dir().join(format!("yien-within-{}", std::process::id()));
        let target = base.join("target");
        let link = base.join("link");
        std::fs::create_dir_all(&target).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .output()
                .unwrap();
            assert!(status.status.success(), "junction creates: {:?}", status);
            assert!(within(&link.join("home"), &target));
            assert!(within(&link, &target));
            assert!(!within(&link, &base.join("elsewhere")));
            std::fs::remove_dir(&link).unwrap();
        }
        let _ = std::fs::remove_dir_all(&base);
        // `..` inside a path cannot escape the root.
        assert!(!within(&root("a/../evil"), &root("a")));
        assert!(within(&root("a/../inside"), &root("")));
    }

    #[test]
    fn steps_skip_comments_and_blanks() {
        let steps = parse_steps("# comment\n\nwait 10\n   \n# another\ntext hi\r\n").unwrap();
        assert!(matches!(&steps[0], Step::Wait(d) if d.as_millis() == 10));
        assert!(matches!(&steps[1], Step::Text(b) if b == b"hi"));
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn steps_report_the_line_number_on_error() {
        let err = parse_steps("wait 10\nnope 20\n").unwrap_err();
        assert!(format!("{err:#}").contains("step 2"), "{err:#}");
    }

    #[test]
    fn wait_for_parses_timeout_then_text() {
        let steps = parse_steps("wait-for 30000 esc to interrupt\n").unwrap();
        assert!(matches!(
            &steps[0],
            Step::WaitFor { timeout, needle }
                if timeout.as_millis() == 30_000 && needle == "esc to interrupt"
        ));
    }

    #[test]
    fn wait_for_needs_a_timeout_and_text() {
        assert!(parse_steps("wait-for 30000\n").is_err());
        assert!(parse_steps("wait-for 30000   \n").is_err());
        assert!(parse_steps("wait-for nope text\n").is_err());
    }

    #[test]
    fn wait_and_wait_exit_parse_milliseconds() {
        let steps = parse_steps("wait 1500\nwait-exit 30000\n").unwrap();
        assert!(matches!(&steps[0], Step::Wait(d) if d.as_millis() == 1500));
        assert!(matches!(&steps[1], Step::WaitExit(d) if d.as_millis() == 30_000));
        assert!(parse_steps("wait soon\n").is_err());
    }

    #[test]
    fn ctrl_letter_and_function_keys() {
        assert!(matches!(
            parse_step("key ctrl-a").unwrap(),
            Step::Key(Key::Char('a'), Mods::CTRL)
        ));
        assert!(parse_step("key ctrl-A").is_err());
        assert!(matches!(
            parse_step("key f1").unwrap(),
            Step::Key(Key::F(1), _)
        ));
        assert!(matches!(
            parse_step("key f12").unwrap(),
            Step::Key(Key::F(12), _)
        ));
        assert!(parse_step("key f0").is_err());
        assert!(parse_step("key f13").is_err());
    }

    #[test]
    fn plain_keys_fall_back_to_the_legacy_encoder() {
        let modes = KeyModes::default();
        assert_eq!(
            encode_key(&Key::Escape, Mods::NONE, modes).unwrap(),
            vec![0x1b]
        );
        assert_eq!(encode_key(&Key::Enter, Mods::NONE, modes).unwrap(), b"\r");
        assert_eq!(
            encode_key(&Key::Tab, Mods::SHIFT, modes).unwrap(),
            b"\x1b[Z"
        );
        assert_eq!(
            encode_key(&Key::F(5), Mods::NONE, modes).unwrap(),
            b"\x1b[15~"
        );
    }

    #[test]
    fn kitty_disambiguate_encodes_escape_and_ctrl_keys() {
        let modes = KeyModes {
            kitty_flags: 1,
            ..KeyModes::default()
        };
        assert_eq!(
            encode_key(&Key::Escape, Mods::NONE, modes).unwrap(),
            b"\x1b[27u"
        );
        assert_eq!(
            encode_key(&Key::Escape, Mods::SHIFT, modes).unwrap(),
            b"\x1b[27;2u"
        );
        assert_eq!(
            encode_key(&Key::Char('c'), Mods::CTRL, modes).unwrap(),
            b"\x1b[99;5u"
        );
        assert_eq!(
            encode_key(
                &Key::Char('a'),
                Mods {
                    ctrl: true,
                    shift: true,
                    alt: false
                },
                modes
            )
            .unwrap(),
            b"\x1b[97;6u"
        );
        assert_eq!(
            encode_key(
                &Key::Char('x'),
                Mods {
                    ctrl: false,
                    alt: true,
                    shift: false
                },
                modes
            )
            .unwrap(),
            b"\x1b[120;3u"
        );
        // Unmodified text is not remapped by disambiguation.
        assert_eq!(
            encode_key(&Key::Char('x'), Mods::NONE, modes).unwrap(),
            b"x"
        );
    }

    #[test]
    fn kitty_shift_tab_stays_legacy() {
        let modes = KeyModes {
            kitty_flags: 1,
            ..KeyModes::default()
        };
        assert_eq!(
            encode_key(&Key::Tab, Mods::SHIFT, modes).unwrap(),
            b"\x1b[Z"
        );
    }

    #[test]
    fn steps_report_line_number_on_wait_for_error() {
        let err = parse_steps("text hi\nwait-for nope x\n").unwrap_err();
        assert!(format!("{err:#}").contains("step 2"), "{err:#}");
    }
}
