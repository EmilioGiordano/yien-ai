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
//! the child. Capturing never touches a real profile dir; the caller points
//! the program at throwaway ones. When the steps are done and the child is
//! still running, the child is terminated; the exit status is reported.
//! Only on success the bytes are written to the output file; on failure a
//! `.partial` file next to it holds what was captured so far.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use yien_input::{Key, KeyModes, KeyQuirks, Mods, encode_with, focus_report};
use yien_pty::{ExitStatus, PortablePty, PtyBackend, PtyEvent, SpawnSpec};
use yien_vt::{AlacrittyEngine, VtConfig, VtEngine};

const USAGE: &str = "usage: cargo xtask capture --steps <file> --out <file.bin> \\
    [--cwd <dir>] [--size <cols>x<rows>] [--env KEY=VALUE]... [--env-remove KEY]... \\
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
    cwd: Option<PathBuf>,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
    env_remove: Vec<String>,
    tail_ms: u64,
    program: String,
    args: Vec<String>,
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
                    tail_ms = value(&mut at)?
                        .parse()
                        .map_err(|_| "--tail-ms wants milliseconds".to_string())?
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
            cwd,
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

    let mut spec = SpawnSpec::new(&options.program)
        .args(options.args.iter())
        .size(options.cols, options.rows);
    if let Some(cwd) = &options.cwd {
        if !cwd.is_dir() {
            return Err(Failure {
                message: format!("--cwd {} is not a directory", cwd.display()),
                partial: None,
            });
        }
        spec = spec.cwd(cwd);
    }
    for key in &options.env_remove {
        spec = spec.env_remove(key);
    }
    for (key, value) in &options.env {
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
