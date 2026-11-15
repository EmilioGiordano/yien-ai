//! OpenCode's turn failures, which it never reports on the terminal: one
//! watcher per machine reads its background service's event stream while at
//! least one pane runs OpenCode, and each pane fails only the turns of the
//! sessions its own OpenCode named over OSC 7501 (RN-02).

use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use yien_agent::opencode::{MAX_SERVICE_FILE, ServiceEvent, ServiceFile, SseDecoder, parse_event};
use yien_platform::http;
use yien_platform::opencode_service::read_service_file;
use yien_vt::{ProgramState, ProgramStatus, StatusApp, StatusId};

/// The profile id, and the program name the shell poller reports, of
/// OpenCode.
pub(crate) const OPENCODE: &str = "opencode";

/// The most sessions one pane remembers; the oldest goes first.
pub(crate) const MAX_SESSIONS_SEEN: usize = 64;

/// Failures waiting for the panes; a pane that falls further behind skips
/// some, which shows no error rather than a wrong one.
const FAILURES_QUEUED: usize = 16;

/// How the core reaches OpenCode's background service to see failed turns.
/// Read-only: the service file is only read, and only the event stream is
/// requested, with the file's password, from `127.0.0.1`.
///
/// ```
/// use yien_core::OpenCodeService;
///
/// let service = OpenCodeService::new(vec!["C:/fake/opencode/service.json".into()]);
/// assert!(service.retry.0 <= service.retry.1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeService {
    /// The files the service registers in, tried in order
    /// (`yien_platform::opencode_service::service_files`).
    pub files: Vec<PathBuf>,
    /// The first and the longest wait before the file is read and the stream
    /// opened again, doubling in between.
    pub retry: (Duration, Duration),
    /// The longest wait for data on an open stream before it is opened
    /// again. Also how long the watcher may outlive the last OpenCode pane.
    pub idle_timeout: Duration,
}

impl OpenCodeService {
    /// The service registered in `files`, retried after 1 s up to every
    /// 10 s, its stream reopened after 60 s without data.
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            retry: (Duration::from_secs(1), Duration::from_secs(10)),
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// The OpenCode sessions a pane's program named since it started, and
/// whether OpenCode reports there at all.
#[derive(Debug, Default)]
pub(crate) struct Sessions {
    ids: VecDeque<StatusId>,
    reported: bool,
}

impl Sessions {
    /// Takes one OSC 7501 record. Returns whether it named a session not
    /// seen before. A record about the whole program that says `clear`
    /// means it gave the terminal back: its sessions are no longer shown.
    pub fn absorb(&mut self, status: &ProgramStatus) -> bool {
        if !status.sub_session && status.state == ProgramState::Clear {
            self.clear();
            return false;
        }
        if status.app != StatusApp::OpenCode {
            return false;
        }
        self.reported = true;
        let Some(id) = status.id else {
            return false;
        };
        if self.ids.contains(&id) {
            return false;
        }
        if self.ids.len() >= MAX_SESSIONS_SEEN {
            self.ids.pop_front();
        }
        self.ids.push_back(id);
        true
    }

    /// Whether the program named the session `id`.
    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|seen| seen.as_str() == id)
    }

    /// Whether OpenCode has reported over OSC 7501 since it last gave the
    /// terminal back.
    pub fn reported(&self) -> bool {
        self.reported
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.reported = false;
    }
}

#[derive(Debug, Default)]
struct State {
    /// Panes that run OpenCode now.
    panes: usize,
    /// The watcher thread runs.
    running: bool,
    /// A pane learned a session: try the service now rather than after the
    /// current wait.
    poked: bool,
}

/// The one watcher of OpenCode's service. Its thread runs only while a
/// [`Watching`] lives, and ends within [`OpenCodeService::idle_timeout`] of
/// the last one dropping.
#[derive(Debug)]
pub(crate) struct OpenCodeWatch {
    service: OpenCodeService,
    state: Mutex<State>,
    wake: Condvar,
    failed: broadcast::Sender<Arc<str>>,
}

/// One pane's hold on the watcher, and the failed sessions it reports.
#[derive(Debug)]
pub(crate) struct Watching {
    watch: Arc<OpenCodeWatch>,
    failed: broadcast::Receiver<Arc<str>>,
}

enum Pause {
    Stop,
    Poked,
    Waited,
}

impl OpenCodeWatch {
    pub fn new(service: OpenCodeService) -> Arc<Self> {
        Arc::new(Self {
            service,
            state: Mutex::default(),
            wake: Condvar::new(),
            failed: broadcast::channel(FAILURES_QUEUED).0,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds the watcher for a pane that runs OpenCode, starting its thread
    /// when none runs.
    pub fn join(self: &Arc<Self>) -> Watching {
        let failed = self.failed.subscribe();
        let mut state = self.state();
        state.panes += 1;
        if !state.running {
            let watch = Arc::clone(self);
            let started = std::thread::Builder::new()
                .name("yien-opencode".to_owned())
                .spawn(move || watch.run());
            state.running = started.is_ok();
            if started.is_err() {
                tracing::warn!("the OpenCode watcher could not start");
            }
        }
        Watching {
            watch: Arc::clone(self),
            failed,
        }
    }

    fn leave(&self) {
        let mut state = self.state();
        state.panes = state.panes.saturating_sub(1);
        self.wake.notify_all();
    }

    fn poke(&self) {
        self.state().poked = true;
        self.wake.notify_all();
    }

    fn wanted(&self) -> bool {
        self.state().panes > 0
    }

    /// Panes that hold the watcher now.
    #[cfg(test)]
    pub fn panes(&self) -> usize {
        self.state().panes
    }

    /// Hands the panes a failure as if the service had reported it.
    #[cfg(test)]
    pub fn report_failed(&self, session: &str) {
        let _ = self.failed.send(session.into());
    }

    fn run(&self) {
        tracing::debug!("OpenCode watcher started");
        let (first, longest) = self.service.retry;
        let mut wait = first;
        loop {
            if self.listen() {
                wait = first;
            }
            match self.pause(wait) {
                Pause::Stop => break,
                Pause::Poked => wait = first,
                Pause::Waited => wait = (wait * 2).min(longest),
            }
        }
        tracing::debug!("OpenCode watcher stopped");
    }

    /// Waits `wait`, or less when poked; stops for good when no pane runs
    /// OpenCode any more.
    fn pause(&self, wait: Duration) -> Pause {
        let deadline = Instant::now() + wait;
        let mut state = self.state();
        loop {
            if state.panes == 0 {
                state.running = false;
                return Pause::Stop;
            }
            if std::mem::take(&mut state.poked) {
                return Pause::Poked;
            }
            let now = Instant::now();
            if now >= deadline {
                return Pause::Waited;
            }
            state = self
                .wake
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The first service file that reads as one.
    fn service_file(&self) -> Option<ServiceFile> {
        for path in &self.service.files {
            match read_service_file(path, MAX_SERVICE_FILE) {
                Ok(Some(bytes)) => match ServiceFile::parse(&bytes) {
                    Ok(service) => return Some(service),
                    Err(e) => tracing::debug!(error = %e, "OpenCode service file not used"),
                },
                Ok(None) => {}
                Err(e) => tracing::debug!(error = %e, "OpenCode service file unreadable"),
            }
        }
        None
    }

    /// Reads the event stream until it ends, fails or no pane needs it.
    /// Returns whether the service accepted the stream.
    fn listen(&self) -> bool {
        let Some(service) = self.service_file() else {
            return false;
        };
        let options = http::Options {
            allow_loopback_http: true,
            timeout: self.service.idle_timeout,
            ..http::Options::new(concat!("YIEN/", env!("CARGO_PKG_VERSION")), 0)
        }
        .header("Accept", "text/event-stream")
        .header("Authorization", service.authorization());
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        let mut opened = false;
        let result = http::get_stream(&service.events_url(), &options, &mut |chunk| {
            decoder.feed(chunk, &mut events);
            for data in events.drain(..) {
                match parse_event(&data) {
                    Some(ServiceEvent::Connected) => {
                        opened = true;
                        self.state().poked = false;
                    }
                    Some(ServiceEvent::Failed { session }) => {
                        // No pane listening is not an error.
                        let _ = self.failed.send(session.into());
                    }
                    None => {}
                }
            }
            if self.wanted() {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        });
        match result {
            Ok(()) => tracing::debug!(opened, "OpenCode event stream ended"),
            Err(e) => tracing::debug!(opened, error = %e, "OpenCode event stream failed"),
        }
        opened
    }
}

impl Watching {
    /// The next session the service says failed a turn. Never resolves
    /// once the watcher is gone; skips what the pane fell behind on.
    pub async fn failed(&mut self) -> Arc<str> {
        loop {
            match self.failed.recv().await {
                Ok(session) => return session,
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "OpenCode failures skipped");
                }
                Err(RecvError::Closed) => std::future::pending().await,
            }
        }
    }

    /// The pane learned a session: the watcher tries the service at once
    /// if it is waiting to.
    pub fn poke(&self) {
        self.watch.poke();
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        self.watch.leave();
    }
}

#[cfg(test)]
mod tests;
