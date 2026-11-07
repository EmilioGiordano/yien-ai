//! How the window reaches the core: a sender the binary provides, and
//! bounded queues that hold messages while the core is busy (plan §4.2).
//!
//! Input and reports wait in one queue per pane: a pane whose program stops
//! reading makes the core refuse that pane only, and must not hold up typing
//! in the others. Everything else waits in one shared queue. A full queue
//! refuses new messages instead of dropping queued ones, and the window says
//! so.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::Context;
use yien_proto::frame::Frame;
use yien_proto::{ClientMsg, PaneId};

/// Bytes of input and reports a pane may have waiting for its program.
pub(crate) const PANE_BUDGET: usize = 64 * 1024;

/// Messages that may wait in the shared queue.
pub(crate) const SHARED_BUDGET: usize = 1024;

/// The first retry delay, and the longest one while nothing gets through.
const RETRY_FIRST: Duration = Duration::from_millis(5);
const RETRY_MAX: Duration = Duration::from_millis(250);

/// Why the core did not take a message.
#[derive(Debug)]
pub enum Refused {
    /// Its queue, or the pane's input queue, is full; the message comes back
    /// to be retried.
    Busy(ClientMsg),
    /// It has stopped; nothing will be delivered again.
    Stopped,
}

/// The window's way to the core, without waiting. The binary implements it
/// over the core's handle, so the UI never sees core types (ADR-03).
///
/// ```
/// use yien_proto::frame::Frame;
/// use yien_proto::{ClientMsg, PaneId};
/// use yien_ui::{CoreClient, Refused};
///
/// struct Nowhere;
/// impl CoreClient for Nowhere {
///     fn send(&self, _msg: ClientMsg) -> Result<(), Refused> {
///         Err(Refused::Stopped)
///     }
///     fn take_frame(&self, _pane: PaneId) -> Option<Frame> {
///         None
///     }
/// }
/// assert!(Nowhere.send(ClientMsg::WindowFocus { focused: true }).is_err());
/// assert!(Nowhere.take_frame(PaneId::new(1)).is_none());
/// ```
pub trait CoreClient: 'static {
    /// Queues `msg` for the core.
    fn send(&self, msg: ClientMsg) -> Result<(), Refused>;

    /// The newest frame of `pane` not taken yet, if any. Called after
    /// `CoreMsg::PaneFrameReady` and whenever a pane comes into view.
    fn take_frame(&self, pane: PaneId) -> Option<Frame>;
}

/// What became of a message handed to the [`Link`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// The core has it.
    Delivered,
    /// It waits in a queue; call [`Link::flush`] later.
    Queued,
    /// Its queue is full, so it was not taken; the caller tells the user.
    Full,
    /// The core has stopped.
    Stopped,
}

/// How a queue stands, as the window shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Backlog {
    /// Nothing waits.
    #[default]
    Clear,
    /// Messages wait for the core to take them.
    Waiting,
    /// The budget is spent: new messages are refused.
    Full,
}

/// What a [`Link::flush`] achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Flushed {
    /// Something is still queued.
    pub(crate) waiting: bool,
    /// At least one message got through.
    pub(crate) progressed: bool,
}

/// Which queue a message waits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lane {
    Shared,
    Pane(PaneId),
}

fn lane_of(msg: &ClientMsg) -> Lane {
    match msg {
        ClientMsg::Input { pane, .. } | ClientMsg::Report { pane, .. } => Lane::Pane(*pane),
        _ => Lane::Shared,
    }
}

/// What a message costs against its lane's budget.
fn cost(msg: &ClientMsg) -> usize {
    match msg {
        ClientMsg::Input { bytes, .. } | ClientMsg::Report { bytes, .. } => bytes.as_slice().len(),
        _ => 1,
    }
}

fn budget(lane: Lane) -> usize {
    match lane {
        Lane::Shared => SHARED_BUDGET,
        Lane::Pane(_) => PANE_BUDGET,
    }
}

/// The sender plus the queues in front of it.
pub(crate) struct Link {
    client: Box<dyn CoreClient>,
    queues: RefCell<BTreeMap<Lane, VecDeque<ClientMsg>>>,
    stopped: Cell<bool>,
    /// Bumped whenever a queue's [`Backlog`] changes, so the window knows to
    /// redraw its indicators.
    revision: Cell<u64>,
    /// A retry is already scheduled.
    pub(crate) flushing: Cell<bool>,
}

impl Link {
    pub(crate) fn new(client: Box<dyn CoreClient>) -> Self {
        Self {
            client,
            queues: RefCell::new(BTreeMap::new()),
            stopped: Cell::new(false),
            revision: Cell::new(0),
            flushing: Cell::new(false),
        }
    }

    fn backlog(&self, lane: Lane) -> Backlog {
        let queues = self.queues.borrow();
        match queues.get(&lane) {
            None => Backlog::Clear,
            Some(queue) if queue.iter().map(cost).sum::<usize>() >= budget(lane) => Backlog::Full,
            Some(_) => Backlog::Waiting,
        }
    }

    /// How a pane's input queue stands.
    pub(crate) fn pane_backlog(&self, pane: PaneId) -> Backlog {
        self.backlog(Lane::Pane(pane))
    }

    /// How the shared queue stands.
    pub(crate) fn shared_backlog(&self) -> Backlog {
        self.backlog(Lane::Shared)
    }

    /// Changes whenever any queue's backlog changes.
    pub(crate) fn revision(&self) -> u64 {
        self.revision.get()
    }

    fn noting<R>(&self, lanes: &[Lane], f: impl FnOnce() -> R) -> R {
        let before: Vec<Backlog> = lanes.iter().map(|l| self.backlog(*l)).collect();
        let out = f();
        let after: Vec<Backlog> = lanes.iter().map(|l| self.backlog(*l)).collect();
        if before != after {
            self.revision.set(self.revision.get().wrapping_add(1));
        }
        out
    }

    /// Sends `msg`, queues it behind earlier messages of its lane, or refuses
    /// it when that lane's budget is spent.
    pub(crate) fn send(&self, msg: ClientMsg) -> Sent {
        if self.stopped.get() {
            return Sent::Stopped;
        }
        let lane = lane_of(&msg);
        self.noting(&[lane], || {
            let mut queues = self.queues.borrow_mut();
            if let Some(queue) = queues.get_mut(&lane) {
                let used: usize = queue.iter().map(cost).sum();
                if used + cost(&msg) > budget(lane) {
                    return Sent::Full;
                }
                queue.push_back(msg);
                return Sent::Queued;
            }
            drop(queues);
            match self.client.send(msg) {
                Ok(()) => Sent::Delivered,
                Err(Refused::Busy(msg)) => {
                    self.queues
                        .borrow_mut()
                        .entry(lane)
                        .or_default()
                        .push_back(msg);
                    Sent::Queued
                }
                Err(Refused::Stopped) => self.stop(),
            }
        })
    }

    /// Sends what is queued, each lane in order until the core refuses that
    /// lane again.
    pub(crate) fn flush(&self) -> Flushed {
        let lanes: Vec<Lane> = self.queues.borrow().keys().copied().collect();
        let mut progressed = false;
        let stopped = self.noting(&lanes, || {
            for &lane in &lanes {
                loop {
                    let next = self
                        .queues
                        .borrow_mut()
                        .get_mut(&lane)
                        .and_then(VecDeque::pop_front);
                    let Some(msg) = next else {
                        self.queues.borrow_mut().remove(&lane);
                        break;
                    };
                    match self.client.send(msg) {
                        Ok(()) => progressed = true,
                        Err(Refused::Busy(msg)) => {
                            if let Some(queue) = self.queues.borrow_mut().get_mut(&lane) {
                                queue.push_front(msg);
                            }
                            break;
                        }
                        Err(Refused::Stopped) => {
                            self.stop();
                            return true;
                        }
                    }
                }
            }
            false
        });
        Flushed {
            waiting: !stopped && !self.queues.borrow().is_empty(),
            progressed,
        }
    }

    /// The newest frame of `pane` the core holds for the UI.
    pub(crate) fn take_frame(&self, pane: PaneId) -> Option<Frame> {
        self.client.take_frame(pane)
    }

    #[cfg(test)]
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.get()
    }

    fn stop(&self) -> Sent {
        self.stopped.set(true);
        self.queues.borrow_mut().clear();
        Sent::Stopped
    }
}

/// The wait before the next retry: back to the shortest once something got
/// through, doubling up to [`RETRY_MAX`] while nothing does.
pub(crate) fn next_retry(current: Duration, progressed: bool) -> Duration {
    if progressed {
        RETRY_FIRST
    } else {
        (current * 2).min(RETRY_MAX)
    }
}

/// What the status bar says about the shared queue.
pub(crate) fn core_busy(backlog: Backlog) -> Option<&'static str> {
    match backlog {
        Backlog::Clear => None,
        Backlog::Waiting => Some("The YIEN core is busy"),
        Backlog::Full => Some("The YIEN core is busy: new commands are not being sent"),
    }
}

/// What a pane says about its input queue.
pub(crate) fn pane_busy(backlog: Backlog) -> Option<&'static str> {
    match backlog {
        Backlog::Clear => None,
        Backlog::Waiting => Some("Not reading input. What you type is sent when it reads again."),
        Backlog::Full => Some(
            "Not reading input. Typing is paused until it reads again; \
             keys pressed now are not sent.",
        ),
    }
}

/// Sends `msg` from any view, scheduling retries while the core is busy. The
/// retries stop once every queue is empty; each change of a backlog redraws
/// the windows so the indicators follow.
pub(crate) fn deliver<T: 'static>(link: &Rc<Link>, msg: ClientMsg, cx: &mut Context<T>) -> Sent {
    let sent = link.send(msg);
    if sent == Sent::Queued && !link.flushing.replace(true) {
        let link = Rc::clone(link);
        cx.spawn(async move |_, cx| {
            let mut delay = RETRY_FIRST;
            loop {
                cx.background_executor().timer(delay).await;
                let before = link.revision();
                let flushed = link.flush();
                if link.revision() != before {
                    cx.update(|cx| cx.refresh_windows());
                }
                if !flushed.waiting {
                    break;
                }
                delay = next_retry(delay, flushed.progressed);
            }
            link.flushing.set(false);
        })
        .detach();
    }
    sent
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use yien_proto::{InputBytes, PaneId};

    use super::*;

    /// Takes up to `room` messages, then reports busy.
    #[derive(Default)]
    struct Fake {
        got: RefCell<Vec<ClientMsg>>,
        room: Cell<usize>,
        stopped: Cell<bool>,
        /// Panes whose program is not reading: their input is refused.
        stalled: RefCell<Vec<PaneId>>,
    }

    struct Shared(Rc<Fake>);

    impl CoreClient for Shared {
        fn send(&self, msg: ClientMsg) -> Result<(), Refused> {
            if self.0.stopped.get() {
                return Err(Refused::Stopped);
            }
            if self.0.room.get() == 0 {
                return Err(Refused::Busy(msg));
            }
            if let ClientMsg::Input { pane, .. } = &msg
                && self.0.stalled.borrow().contains(pane)
            {
                return Err(Refused::Busy(msg));
            }
            self.0.room.set(self.0.room.get() - 1);
            self.0.got.borrow_mut().push(msg);
            Ok(())
        }

        fn take_frame(&self, _pane: PaneId) -> Option<Frame> {
            None
        }
    }

    fn input(n: u8) -> ClientMsg {
        input_to(1, n)
    }

    fn input_to(pane: u64, n: u8) -> ClientMsg {
        ClientMsg::Input {
            pane: PaneId::new(pane),
            bytes: InputBytes::new(vec![n]),
        }
    }

    fn big_input(pane: u64, len: usize) -> ClientMsg {
        ClientMsg::Input {
            pane: PaneId::new(pane),
            bytes: InputBytes::new(vec![b'x'; len]),
        }
    }

    fn link(room: usize) -> (Rc<Fake>, Link) {
        let fake = Rc::new(Fake::default());
        fake.room.set(room);
        let link = Link::new(Box::new(Shared(Rc::clone(&fake))));
        (fake, link)
    }

    #[test]
    fn a_pane_that_stopped_reading_does_not_hold_up_the_others() {
        let (fake, link) = link(100);
        fake.stalled.borrow_mut().push(PaneId::new(1));
        assert_eq!(link.send(input_to(1, 1)), Sent::Queued);
        assert_eq!(link.send(input_to(1, 2)), Sent::Queued);
        assert_eq!(link.send(input_to(2, 3)), Sent::Delivered);
        assert_eq!(
            link.send(ClientMsg::WindowFocus { focused: true }),
            Sent::Delivered
        );
        assert!(link.flush().waiting);
        assert_eq!(fake.got.borrow().len(), 2);
        fake.stalled.borrow_mut().clear();
        assert!(!link.flush().waiting);
        let got = fake.got.borrow();
        assert_eq!(&got[2..], &[input_to(1, 1), input_to(1, 2)]);
    }

    #[test]
    fn a_busy_core_drops_nothing_and_keeps_the_order() {
        let (fake, link) = link(1);
        assert_eq!(link.send(input(1)), Sent::Delivered);
        assert_eq!(link.send(input(2)), Sent::Queued);
        fake.room.set(10);
        assert_eq!(link.send(input(3)), Sent::Queued);
        let flushed = link.flush();
        assert!(!flushed.waiting);
        assert!(flushed.progressed);
        assert_eq!(*fake.got.borrow(), vec![input(1), input(2), input(3)]);
    }

    #[test]
    fn flush_stops_at_the_first_refusal() {
        let (fake, link) = link(0);
        link.send(input(1));
        link.send(input(2));
        fake.room.set(1);
        assert!(link.flush().waiting);
        assert_eq!(*fake.got.borrow(), vec![input(1)]);
        fake.room.set(1);
        assert!(!link.flush().waiting);
        assert_eq!(*fake.got.borrow(), vec![input(1), input(2)]);
    }

    #[test]
    fn a_stopped_core_ends_delivery() {
        let (fake, link) = link(0);
        link.send(input(1));
        fake.stopped.set(true);
        assert!(!link.flush().waiting);
        assert!(link.is_stopped());
        assert_eq!(link.send(input(2)), Sent::Stopped);
    }

    #[test]
    fn a_full_pane_queue_refuses_new_input_and_keeps_what_it_holds() {
        let (fake, link) = link(100);
        let pane = PaneId::new(1);
        fake.stalled.borrow_mut().push(pane);
        assert_eq!(link.pane_backlog(pane), Backlog::Clear);
        assert_eq!(link.send(big_input(1, PANE_BUDGET - 1)), Sent::Queued);
        assert_eq!(link.pane_backlog(pane), Backlog::Waiting);
        assert_eq!(link.send(input(2)), Sent::Queued);
        assert_eq!(link.pane_backlog(pane), Backlog::Full);
        assert_eq!(link.send(input(3)), Sent::Full);
        assert_eq!(link.send(input_to(2, 4)), Sent::Delivered);
        assert_eq!(link.pane_backlog(PaneId::new(2)), Backlog::Clear);
        let flushed = link.flush();
        assert!(flushed.waiting);
        assert!(!flushed.progressed);
        assert_eq!(fake.got.borrow().len(), 1);
    }

    #[test]
    fn a_full_pane_recovers_once_its_program_reads_again() {
        let (fake, link) = link(100);
        let pane = PaneId::new(1);
        fake.stalled.borrow_mut().push(pane);
        link.send(big_input(1, PANE_BUDGET));
        assert_eq!(link.pane_backlog(pane), Backlog::Full);
        let before = link.revision();
        fake.stalled.borrow_mut().clear();
        let flushed = link.flush();
        assert!(!flushed.waiting);
        assert!(flushed.progressed);
        assert_eq!(link.pane_backlog(pane), Backlog::Clear);
        assert_ne!(link.revision(), before);
        assert_eq!(link.send(input(9)), Sent::Delivered);
    }

    #[test]
    fn the_shared_queue_is_bounded_too() {
        let (_fake, link) = link(0);
        for _ in 0..SHARED_BUDGET {
            assert_eq!(
                link.send(ClientMsg::WindowFocus { focused: true }),
                Sent::Queued
            );
        }
        assert_eq!(link.shared_backlog(), Backlog::Full);
        assert_eq!(
            link.send(ClientMsg::WindowFocus { focused: false }),
            Sent::Full
        );
    }

    #[test]
    fn only_a_backlog_is_shown_and_a_full_one_says_input_is_refused() {
        assert_eq!(pane_busy(Backlog::Clear), None);
        assert_eq!(core_busy(Backlog::Clear), None);
        assert!(pane_busy(Backlog::Waiting).is_some_and(|s| s.starts_with("Not reading input")));
        assert!(pane_busy(Backlog::Full).is_some_and(|s| s.contains("not sent")));
        assert!(core_busy(Backlog::Full).is_some_and(|s| s.contains("not being sent")));
    }

    #[test]
    fn retries_back_off_while_stuck_and_reset_on_progress() {
        let mut delay = RETRY_FIRST;
        for _ in 0..10 {
            delay = next_retry(delay, false);
        }
        assert_eq!(delay, RETRY_MAX);
        assert_eq!(next_retry(delay, true), RETRY_FIRST);
        assert_eq!(next_retry(RETRY_FIRST, false), Duration::from_millis(10));
    }
}
