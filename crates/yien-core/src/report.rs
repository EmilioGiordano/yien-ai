//! What pane actors tell the service. Reports are merged per pane, so a
//! program flooding titles or state changes takes constant room while the
//! service waits on the UI (plan §4.2: state events may be merged).

use std::collections::BTreeMap;
use std::mem;
use std::sync::{Mutex, PoisonError};

use tokio::sync::Notify;
use yien_proto::PaneId;

use crate::workspace::PaneEvent;

/// The reports waiting for the service. Actors never wait on it.
#[derive(Debug, Default)]
pub(crate) struct Reports {
    pending: Mutex<BTreeMap<PaneId, Vec<PaneEvent>>>,
    wake: Notify,
}

impl Reports {
    /// Records `event` for `pane`. A detail (state, title, directory, session
    /// id, selection, clipboard) replaces the same detail reported since the last
    /// lifecycle event; lifecycle events keep their order.
    pub fn push(&self, pane: PaneId, event: PaneEvent) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            merge(pending.entry(pane).or_default(), event);
        }
        self.wake.notify_one();
    }

    /// Waits for reports and takes all of them. Cancel safe: nothing is taken
    /// unless it is returned.
    pub async fn next(&self) -> BTreeMap<PaneId, Vec<PaneEvent>> {
        loop {
            let taken =
                mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner));
            if !taken.is_empty() {
                return taken;
            }
            self.wake.notified().await;
        }
    }
}

fn merge(list: &mut Vec<PaneEvent>, event: PaneEvent) {
    if event.is_detail() {
        let same = list
            .iter_mut()
            .rev()
            .take_while(|e| e.is_detail())
            .find(|e| mem::discriminant(*e) == mem::discriminant(&event));
        if let Some(slot) = same {
            *slot = event;
            return;
        }
    }
    list.push(event);
}

#[cfg(test)]
mod tests {
    use yien_proto::AgentState;

    use super::*;

    #[test]
    fn details_merge_and_lifecycle_keeps_its_order() {
        let reports = Reports::default();
        let pane = PaneId::new(1);
        for i in 0..10_000 {
            reports.push(pane, PaneEvent::Title(Some(format!("t{i}"))));
        }
        reports.push(pane, PaneEvent::State(AgentState::Working, None));
        reports.push(pane, PaneEvent::Exited(Some(0)));
        reports.push(pane, PaneEvent::State(AgentState::Idle, None));
        reports.push(pane, PaneEvent::Started);
        reports.push(pane, PaneEvent::State(AgentState::Done, None));
        reports.push(pane, PaneEvent::State(AgentState::Working, None));
        let taken = futures_now(reports.next());
        assert_eq!(
            taken[&pane],
            [
                PaneEvent::Title(Some("t9999".into())),
                PaneEvent::State(AgentState::Working, None),
                PaneEvent::Exited(Some(0)),
                PaneEvent::State(AgentState::Idle, None),
                PaneEvent::Started,
                PaneEvent::State(AgentState::Working, None),
            ]
        );
        assert!(reports.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn a_push_before_the_wait_is_not_lost() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let reports = Reports::default();
        rt.block_on(async {
            reports.push(PaneId::new(2), PaneEvent::Started);
            let taken = tokio::time::timeout(std::time::Duration::from_secs(5), reports.next())
                .await
                .unwrap();
            assert_eq!(taken[&PaneId::new(2)], [PaneEvent::Started]);
        });
    }

    /// Polls a future that must be ready at once.
    fn futures_now<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }
}
