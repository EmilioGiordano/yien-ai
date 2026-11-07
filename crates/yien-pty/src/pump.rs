//! The three blocking loops behind a session, written over plain `Read`,
//! `Write` and a blocking exit wait so they are testable without a real PTY.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::{Drain, ExitStatus, PtyEvent, WriterMsg};

/// Plan §4.1: the reader takes up to 64 KB per read.
pub(crate) const READ_BUFFER: usize = 64 * 1024;
const DRAIN_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Default)]
struct ReaderState {
    /// When the reader last entered `read()`; `None` while it holds bytes.
    parked_since: Option<Instant>,
    /// Time spent in `read()` before the current park.
    parked_before: Duration,
    eof: bool,
}

impl ReaderState {
    fn parked_total(&self, now: Instant) -> Duration {
        self.parked_before
            + self
                .parked_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }
}

/// What the reader tells the waiter.
#[derive(Debug, Default)]
pub(crate) struct ReaderShared {
    state: Mutex<ReaderState>,
}

impl ReaderShared {
    fn lock(&self) -> MutexGuard<'_, ReaderState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Forwards everything `reader` yields until end of stream.
///
/// When the receiver is gone it keeps reading and discards: if nobody drains
/// ConPTY's output pipe, `ClosePseudoConsole` blocks forever.
pub(crate) fn run_reader(
    mut reader: impl Read,
    events: SyncSender<PtyEvent>,
    shared: Arc<ReaderShared>,
) {
    let mut buf = vec![0u8; READ_BUFFER];
    let mut receiver_gone = false;
    loop {
        shared.lock().parked_since = Some(Instant::now());
        let read = reader.read(&mut buf);
        {
            let mut state = shared.lock();
            let now = Instant::now();
            state.parked_before = state.parked_total(now);
            state.parked_since = None;
        }
        match read {
            Ok(0) => break,
            Ok(n) => {
                if !receiver_gone && events.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                    receiver_gone = true;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    shared.lock().eof = true;
}

/// Blocks in `wait` until the child exits, then waits for its output to end
/// and sends [`PtyEvent::Exited`].
///
/// `Exited` is only sent while the reader is parked in `read()` (or at end of
/// stream), so every byte read before it is already queued. Both the quiet
/// window and the cap count only time the reader spends parked: a reader
/// stuck in a full channel still holds bytes the consumer has not seen.
pub(crate) fn run_waiter(
    wait: impl FnOnce() -> io::Result<ExitStatus>,
    events: SyncSender<PtyEvent>,
    shared: Arc<ReaderShared>,
    exited: Arc<AtomicBool>,
    drain: Drain,
) {
    let status = wait().unwrap_or(ExitStatus { code: None });
    exited.store(true, Ordering::SeqCst);
    let exit_at = Instant::now();
    let parked_at_exit = shared.lock().parked_total(exit_at);
    loop {
        let state = shared.lock();
        let now = Instant::now();
        let quiet = state
            .parked_since
            .is_some_and(|since| now.saturating_duration_since(since.max(exit_at)) >= drain.quiet);
        let capped = state.parked_since.is_some()
            && state.parked_total(now).saturating_sub(parked_at_exit) >= drain.cap;
        if state.eof || quiet || capped {
            // Tried with the lock held, so the reader cannot queue bytes
            // between this decision and the exit; on a full channel the lock
            // is released and the decision taken again.
            match events.try_send(PtyEvent::Exited(status)) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(_)) => {}
            }
        }
        drop(state);
        thread::sleep(DRAIN_POLL);
    }
}

/// Applies input and resizes in queue order until every sender is gone or
/// the child stops accepting input.
pub(crate) fn run_writer(
    mut writer: impl Write,
    mut resize: impl FnMut(u16, u16),
    input: Receiver<WriterMsg>,
) {
    for msg in input {
        match msg {
            WriterMsg::Input(bytes) => {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    return;
                }
            }
            WriterMsg::Resize { cols, rows } => resize(cols, rows),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn fast_drain() -> Drain {
        Drain {
            quiet: Duration::from_millis(60),
            cap: Duration::from_secs(3),
        }
    }

    /// For tests that check ordering only: a quiet window far longer than the
    /// gap between the exit and the test's next write, even on a slow runner.
    fn ordering_drain() -> Drain {
        Drain {
            quiet: Duration::from_millis(500),
            cap: Duration::from_secs(3),
        }
    }

    /// Returns when the test calls [`Pump::exit`], as `Child::wait` does on
    /// exit, and tells the test it is returning.
    fn wait_on(
        exit: Receiver<()>,
        returning: SyncSender<()>,
    ) -> impl FnOnce() -> io::Result<ExitStatus> {
        move || {
            let _ = exit.recv();
            let _ = returning.send(());
            Ok(ExitStatus::from_code(0))
        }
    }

    struct Pump {
        events: Receiver<PtyEvent>,
        exit: SyncSender<()>,
        returning: Receiver<()>,
        exited: Arc<AtomicBool>,
    }

    impl Pump {
        /// Ends the child and returns once the waiter has seen it, so what
        /// the test does next happens after the exit.
        fn exit(&self) {
            self.exit.send(()).unwrap();
            self.returning.recv_timeout(TIMEOUT).unwrap();
        }
    }

    fn start(reader: io::PipeReader, capacity: usize, drain: Drain) -> Pump {
        let (tx, events) = sync_channel(capacity);
        let shared = Arc::new(ReaderShared::default());
        let (exit, exit_rx) = sync_channel(1);
        let (returning_tx, returning) = sync_channel(1);
        let exited = Arc::new(AtomicBool::new(false));
        {
            let (tx, shared) = (tx.clone(), Arc::clone(&shared));
            thread::spawn(move || run_reader(reader, tx, shared));
        }
        {
            let wait = wait_on(exit_rx, returning_tx);
            let exited = Arc::clone(&exited);
            thread::spawn(move || run_waiter(wait, tx, shared, exited, drain));
        }
        Pump {
            events,
            exit,
            returning,
            exited,
        }
    }

    /// Collects output until `Exited`, failing on timeout.
    fn until_exit(events: &Receiver<PtyEvent>) -> (Vec<u8>, ExitStatus) {
        let deadline = Instant::now() + TIMEOUT;
        let mut out = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(PtyEvent::Output(bytes)) => out.extend(bytes),
                Ok(PtyEvent::Exited(status)) => return (out, status),
                Err(e) => panic!("no exit before the timeout: {e:?}"),
            }
        }
    }

    #[test]
    fn rf_age_02_reader_passes_every_byte_value_unaltered() {
        let (reader, mut writer) = io::pipe().unwrap();
        let pump = start(reader, 4, fast_drain());
        let sent: Vec<u8> = (0..=255u8).cycle().take(200_000).collect();
        let to_write = sent.clone();
        let feeder = thread::spawn(move || writer.write_all(&to_write));

        let deadline = Instant::now() + TIMEOUT;
        let mut got = Vec::new();
        while got.len() < sent.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            match pump.events.recv_timeout(left).unwrap() {
                PtyEvent::Output(bytes) => got.extend(bytes),
                PtyEvent::Exited(_) => panic!("exit before the child exited"),
            }
        }
        feeder.join().unwrap().unwrap();
        assert_eq!(got, sent);
        pump.exit();
    }

    #[test]
    fn output_after_exit_inside_the_quiet_window_comes_before_exited() {
        let (reader, mut writer) = io::pipe().unwrap();
        let pump = start(reader, 4, ordering_drain());
        writer.write_all(b"before").unwrap();
        pump.exit();
        writer.write_all(b" after").unwrap();

        let (out, status) = until_exit(&pump.events);
        assert_eq!(out, b"before after");
        assert!(status.success());
        drop(writer);
    }

    #[test]
    fn exited_waits_for_the_quiet_window_while_the_pipe_stays_open() {
        let (reader, writer) = io::pipe().unwrap();
        let drain = fast_drain();
        let pump = start(reader, 4, drain);
        let start = Instant::now();
        pump.exit();

        let (_, status) = until_exit(&pump.events);
        assert!(status.success());
        assert!(start.elapsed() >= drain.quiet);
        assert!(pump.exited.load(Ordering::SeqCst));
        drop(writer);
    }

    #[test]
    fn end_of_stream_after_exit_ends_the_wait_early() {
        let (reader, writer) = io::pipe().unwrap();
        let drain = Drain {
            quiet: Duration::from_secs(4),
            cap: Duration::from_secs(4),
        };
        let pump = start(reader, 4, drain);
        let start = Instant::now();
        drop(writer);
        pump.exit();

        let (_, status) = until_exit(&pump.events);
        assert!(status.success());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn output_after_an_exit_that_followed_a_long_idle_still_comes_before_exited() {
        let (reader, mut writer) = io::pipe().unwrap();
        let drain = ordering_drain();
        let pump = start(reader, 4, drain);
        // The reader has been parked longer than the quiet window.
        thread::sleep(drain.quiet + Duration::from_millis(100));
        pump.exit();
        writer.write_all(b"final frame").unwrap();

        let (out, status) = until_exit(&pump.events);
        assert_eq!(out, b"final frame");
        assert!(status.success());
        drop(writer);
    }

    #[test]
    fn a_child_that_never_goes_quiet_still_exits_at_the_cap() {
        let (reader, mut writer) = io::pipe().unwrap();
        let drain = Drain {
            quiet: Duration::from_millis(200),
            cap: Duration::from_millis(400),
        };
        let pump = start(reader, 64, drain);
        let stop = Arc::new(AtomicBool::new(false));
        let chatter = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if writer.write_all(b".").is_err() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            })
        };
        let start = Instant::now();
        pump.exit();

        let (out, status) = until_exit(&pump.events);
        assert!(status.success());
        assert!(start.elapsed() >= drain.cap);
        assert!(!out.is_empty() && out.iter().all(|&b| b == b'.'));
        stop.store(true, Ordering::SeqCst);
        chatter.join().unwrap();
    }

    #[test]
    fn a_full_channel_holds_the_exit_behind_every_byte_already_read() {
        let (reader, mut writer) = io::pipe().unwrap();
        let pump = start(reader, 1, fast_drain());
        for chunk in [&b"one "[..], b"two ", b"three"] {
            writer.write_all(chunk).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        pump.exit();
        // Longer than the quiet window, while the reader is stuck in `send`.
        thread::sleep(Duration::from_millis(300));

        let (out, _) = until_exit(&pump.events);
        assert_eq!(out, b"one two three");
        drop(writer);
    }

    #[test]
    fn a_consumer_behind_for_longer_than_the_cap_gets_every_byte_before_exited() {
        let (reader, mut writer) = io::pipe().unwrap();
        let drain = Drain {
            quiet: Duration::from_millis(30),
            cap: Duration::from_millis(80),
        };
        let pump = start(reader, 1, drain);
        for chunk in [&b"one "[..], b"two ", b"three"] {
            writer.write_all(chunk).unwrap();
            thread::sleep(Duration::from_millis(10));
        }
        pump.exit();
        // Several caps long, while the reader is stuck in `send`.
        thread::sleep(Duration::from_millis(400));

        let (out, _) = until_exit(&pump.events);
        assert_eq!(out, b"one two three");
        drop(writer);
    }

    #[test]
    fn a_failed_wait_ends_the_session_with_an_unknown_status() {
        let (tx, events) = sync_channel(4);
        let shared = Arc::new(ReaderShared::default());
        shared.lock().eof = true;
        let exited = Arc::new(AtomicBool::new(false));
        thread::spawn(move || {
            run_waiter(
                || Err(io::Error::other("gone")),
                tx,
                shared,
                exited,
                Drain::default(),
            )
        });
        assert_eq!(
            events.recv_timeout(TIMEOUT).unwrap(),
            PtyEvent::Exited(ExitStatus { code: None })
        );
    }

    #[test]
    fn the_reader_keeps_draining_after_the_receiver_is_dropped() {
        let (reader, mut writer) = io::pipe().unwrap();
        let (tx, events) = sync_channel(1);
        let shared = Arc::new(ReaderShared::default());
        let done = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || run_reader(reader, tx, shared))
        };
        drop(events);
        // Far more than the pipe buffer: blocks unless the reader drains.
        let feeder = thread::spawn(move || writer.write_all(&vec![b'x'; 1 << 20]));
        let deadline = Instant::now() + TIMEOUT;
        while !feeder.is_finished() || !done.is_finished() {
            assert!(Instant::now() < deadline, "the reader stopped draining");
            thread::sleep(Duration::from_millis(10));
        }
        feeder.join().unwrap().unwrap();
        assert!(shared.lock().eof);
    }

    #[test]
    fn the_writer_applies_input_and_resizes_in_queue_order() {
        let (tx, rx) = sync_channel(8);
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        struct Log(Arc<Mutex<Vec<String>>>);
        impl Write for Log {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(buf).into_owned());
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let resizes = Arc::clone(&log);
        let writer = Log(Arc::clone(&log));
        tx.send(WriterMsg::Input(b"a".to_vec())).unwrap();
        tx.send(WriterMsg::Resize { cols: 9, rows: 3 }).unwrap();
        tx.send(WriterMsg::Input(b"b".to_vec())).unwrap();
        drop(tx);
        run_writer(
            writer,
            move |c, r| resizes.lock().unwrap().push(format!("{c}x{r}")),
            rx,
        );
        assert_eq!(*log.lock().unwrap(), ["a", "9x3", "b"]);
    }

    #[test]
    fn the_writer_stops_when_the_child_stops_accepting_input() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (tx, rx) = sync_channel(8);
        let done = thread::spawn(move || run_writer(Broken, |_, _| {}, rx));
        tx.send(WriterMsg::Input(b"x".to_vec())).unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while !done.is_finished() {
            assert!(Instant::now() < deadline, "the writer kept running");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(tx.send(WriterMsg::Input(b"y".to_vec())).is_err());
    }
}
