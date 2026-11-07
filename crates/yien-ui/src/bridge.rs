//! How the window reaches the core: a sender the binary provides, and
//! queues that hold messages while the core is busy, so nothing the user
//! typed is dropped or reordered (plan §4.2).
//!
//! Input and reports wait in one queue per pane: a pane whose program stops
//! reading makes the core refuse that pane only, and must not hold up typing
//! in the others. Everything else waits in one shared queue.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::Context;
use yien_proto::frame::Frame;
use yien_proto::{ClientMsg, PaneId};

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
    /// The core has stopped.
    Stopped,
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

/// The sender plus the queues in front of it.
pub(crate) struct Link {
    client: Box<dyn CoreClient>,
    queues: RefCell<BTreeMap<Lane, VecDeque<ClientMsg>>>,
    stopped: Cell<bool>,
    /// A retry is already scheduled.
    pub(crate) flushing: Cell<bool>,
}

impl Link {
    pub(crate) fn new(client: Box<dyn CoreClient>) -> Self {
        Self {
            client,
            queues: RefCell::new(BTreeMap::new()),
            stopped: Cell::new(false),
            flushing: Cell::new(false),
        }
    }

    /// Sends `msg`, or queues it behind earlier messages of its lane.
    pub(crate) fn send(&self, msg: ClientMsg) -> Sent {
        if self.stopped.get() {
            return Sent::Stopped;
        }
        let lane = lane_of(&msg);
        if let Some(queue) = self.queues.borrow_mut().get_mut(&lane) {
            queue.push_back(msg);
            return Sent::Queued;
        }
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
    }

    /// Sends what is queued, each lane in order until the core refuses that
    /// lane again. Returns whether anything is still waiting.
    pub(crate) fn flush(&self) -> bool {
        let lanes: Vec<Lane> = self.queues.borrow().keys().copied().collect();
        for lane in lanes {
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
                    Ok(()) => {}
                    Err(Refused::Busy(msg)) => {
                        if let Some(queue) = self.queues.borrow_mut().get_mut(&lane) {
                            queue.push_front(msg);
                        }
                        break;
                    }
                    Err(Refused::Stopped) => {
                        self.stop();
                        return false;
                    }
                }
            }
        }
        !self.queues.borrow().is_empty()
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

/// How soon queued messages are offered to a busy core again.
const RETRY_AFTER: Duration = Duration::from_millis(5);

/// Sends `msg` from any view, scheduling retries while the core is busy.
pub(crate) fn deliver<T: 'static>(link: &Rc<Link>, msg: ClientMsg, cx: &mut Context<T>) {
    if link.send(msg) == Sent::Queued && !link.flushing.replace(true) {
        let link = Rc::clone(link);
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(RETRY_AFTER).await;
                if !link.flush() {
                    break;
                }
            }
            link.flushing.set(false);
        })
        .detach();
    }
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
        assert!(link.flush());
        assert_eq!(fake.got.borrow().len(), 2);
        fake.stalled.borrow_mut().clear();
        assert!(!link.flush());
        let got = fake.got.borrow();
        assert_eq!(&got[2..], &[input_to(1, 1), input_to(1, 2)]);
    }

    fn link(room: usize) -> (Rc<Fake>, Link) {
        let fake = Rc::new(Fake::default());
        fake.room.set(room);
        let link = Link::new(Box::new(Shared(Rc::clone(&fake))));
        (fake, link)
    }

    #[test]
    fn a_busy_core_drops_nothing_and_keeps_the_order() {
        let (fake, link) = link(1);
        assert_eq!(link.send(input(1)), Sent::Delivered);
        assert_eq!(link.send(input(2)), Sent::Queued);
        fake.room.set(10);
        assert_eq!(link.send(input(3)), Sent::Queued);
        assert!(!link.flush());
        assert_eq!(*fake.got.borrow(), vec![input(1), input(2), input(3)]);
    }

    #[test]
    fn flush_stops_at_the_first_refusal() {
        let (fake, link) = link(0);
        link.send(input(1));
        link.send(input(2));
        fake.room.set(1);
        assert!(link.flush());
        assert_eq!(*fake.got.borrow(), vec![input(1)]);
        fake.room.set(1);
        assert!(!link.flush());
        assert_eq!(*fake.got.borrow(), vec![input(1), input(2)]);
    }

    #[test]
    fn a_stopped_core_ends_delivery() {
        let (fake, link) = link(0);
        link.send(input(1));
        fake.stopped.set(true);
        assert!(!link.flush());
        assert!(link.is_stopped());
        assert_eq!(link.send(input(2)), Sent::Stopped);
    }
}
