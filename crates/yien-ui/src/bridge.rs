//! How the window reaches the core: a sender the binary provides, and a
//! queue that holds messages while the core is busy, so nothing the user
//! typed is dropped or reordered (plan §4.2).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::Context;
use yien_proto::ClientMsg;

/// Why the core did not take a message.
#[derive(Debug)]
pub enum Refused {
    /// Its queue is full; the message comes back to be retried.
    Busy(ClientMsg),
    /// It has stopped; nothing will be delivered again.
    Stopped,
}

/// Delivers messages to the core without waiting. The binary implements it
/// over the core's handle, so the UI never sees core types (ADR-03).
///
/// ```
/// use yien_proto::ClientMsg;
/// use yien_ui::{CoreClient, Refused};
///
/// struct Nowhere;
/// impl CoreClient for Nowhere {
///     fn send(&self, _msg: ClientMsg) -> Result<(), Refused> {
///         Err(Refused::Stopped)
///     }
/// }
/// assert!(Nowhere.send(ClientMsg::WindowFocus { focused: true }).is_err());
/// ```
pub trait CoreClient: 'static {
    /// Queues `msg` for the core.
    fn send(&self, msg: ClientMsg) -> Result<(), Refused>;
}

/// What became of a message handed to the [`Link`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// The core has it.
    Delivered,
    /// It waits in the queue; call [`Link::flush`] later.
    Queued,
    /// The core has stopped.
    Stopped,
}

/// The sender plus the queue in front of it.
pub(crate) struct Link {
    client: Box<dyn CoreClient>,
    queue: RefCell<VecDeque<ClientMsg>>,
    stopped: Cell<bool>,
    /// A retry is already scheduled.
    pub(crate) flushing: Cell<bool>,
}

impl Link {
    pub(crate) fn new(client: Box<dyn CoreClient>) -> Self {
        Self {
            client,
            queue: RefCell::new(VecDeque::new()),
            stopped: Cell::new(false),
            flushing: Cell::new(false),
        }
    }

    /// Sends `msg`, or queues it behind earlier messages still waiting.
    pub(crate) fn send(&self, msg: ClientMsg) -> Sent {
        if self.stopped.get() {
            return Sent::Stopped;
        }
        if !self.queue.borrow().is_empty() {
            self.queue.borrow_mut().push_back(msg);
            return Sent::Queued;
        }
        match self.client.send(msg) {
            Ok(()) => Sent::Delivered,
            Err(Refused::Busy(msg)) => {
                self.queue.borrow_mut().push_back(msg);
                Sent::Queued
            }
            Err(Refused::Stopped) => self.stop(),
        }
    }

    /// Sends what is queued, in order, until the core refuses again.
    /// Returns whether anything is still waiting.
    pub(crate) fn flush(&self) -> bool {
        loop {
            let Some(msg) = self.queue.borrow_mut().pop_front() else {
                return false;
            };
            match self.client.send(msg) {
                Ok(()) => {}
                Err(Refused::Busy(msg)) => {
                    self.queue.borrow_mut().push_front(msg);
                    return true;
                }
                Err(Refused::Stopped) => {
                    self.stop();
                    return false;
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.get()
    }

    fn stop(&self) -> Sent {
        self.stopped.set(true);
        self.queue.borrow_mut().clear();
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
            self.0.room.set(self.0.room.get() - 1);
            self.0.got.borrow_mut().push(msg);
            Ok(())
        }
    }

    fn input(n: u8) -> ClientMsg {
        ClientMsg::Input {
            pane: PaneId::new(1),
            bytes: InputBytes::new(vec![n]),
        }
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
