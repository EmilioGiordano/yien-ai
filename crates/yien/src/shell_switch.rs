//! `terminal.shell_integration` applied without a restart (RF-CFG-01): every
//! valid reading of the settings file tells the core whether shells started
//! from then on report their folder (RF-SES-01).

use std::sync::{Mutex, PoisonError};

use yien_ui::ConfigUpdate;

/// Delivers a value to the core; `true` once the core took it.
pub(crate) type Deliver = Box<dyn Fn(bool) -> bool + Send>;

/// What the file asks for and what the core runs with.
pub(crate) struct ShellSwitch {
    state: Mutex<State>,
}

struct State {
    wanted: bool,
    core_has: bool,
    deliver: Option<Deliver>,
}

impl ShellSwitch {
    /// The core starts with `initial`, so nothing is sent until it changes.
    pub(crate) fn new(initial: bool) -> Self {
        Self {
            state: Mutex::new(State {
                wanted: initial,
                core_has: initial,
                deliver: None,
            }),
        }
    }

    /// The core is running: a change read before now is sent at once.
    pub(crate) fn attach(&self, deliver: Deliver) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.deliver = Some(deliver);
        state.flush();
    }

    /// A new reading of the file. One that is not valid keeps the last
    /// valid value, as for every other setting.
    pub(crate) fn reading(&self, update: &ConfigUpdate) {
        let Some(on) = wanted(update) else { return };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.wanted = on;
        state.flush();
    }
}

impl State {
    fn flush(&mut self) {
        if self.wanted == self.core_has {
            return;
        }
        if let Some(deliver) = &self.deliver
            && deliver(self.wanted)
        {
            self.core_has = self.wanted;
        }
    }
}

/// The value a reading asks for; `None` when the file did not read.
fn wanted(update: &ConfigUpdate) -> Option<bool> {
    match update {
        ConfigUpdate::Loaded(loaded) => Some(loaded.config.shell_integration),
        ConfigUpdate::Invalid(_) | ConfigUpdate::Unreadable(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn loaded(text: &str) -> ConfigUpdate {
        ConfigUpdate::Loaded(yien_config::parse(text).unwrap())
    }

    fn recorder(sent: &Arc<Mutex<Vec<bool>>>, accepts: &Arc<AtomicBool>) -> Deliver {
        let (sent, accepts) = (Arc::clone(sent), Arc::clone(accepts));
        Box::new(move |on| {
            sent.lock().unwrap().push(on);
            accepts.load(Ordering::SeqCst)
        })
    }

    #[test]
    fn rf_cfg_01_each_change_of_the_setting_reaches_the_core_once() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicBool::new(true));
        let switch = ShellSwitch::new(true);
        switch.reading(&loaded("terminal.shell_integration = false"));
        assert!(sent.lock().unwrap().is_empty(), "no core yet");
        switch.attach(recorder(&sent, &accepts));
        assert_eq!(*sent.lock().unwrap(), [false], "sent once the core runs");

        switch.reading(&loaded("terminal.shell_integration = false"));
        switch.reading(&ConfigUpdate::Invalid(
            yien_config::parse("terminal.shell_integration = 1").unwrap_err(),
        ));
        switch.reading(&ConfigUpdate::Unreadable("gone".into()));
        assert_eq!(*sent.lock().unwrap(), [false], "unchanged or not valid");

        switch.reading(&loaded(""));
        assert_eq!(*sent.lock().unwrap(), [false, true], "the default is on");
    }

    #[test]
    fn rf_cfg_01_a_refused_delivery_is_tried_again_on_the_next_reading() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicBool::new(false));
        let switch = ShellSwitch::new(true);
        switch.attach(recorder(&sent, &accepts));
        switch.reading(&loaded("terminal.shell_integration = false"));
        accepts.store(true, Ordering::SeqCst);
        switch.reading(&loaded("terminal.shell_integration = false"));
        switch.reading(&loaded("terminal.shell_integration = false"));
        assert_eq!(*sent.lock().unwrap(), [false, false]);
    }

    #[test]
    fn rf_cfg_01_a_core_started_with_the_value_gets_nothing() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicBool::new(true));
        let switch = ShellSwitch::new(false);
        switch.attach(recorder(&sent, &accepts));
        switch.reading(&loaded("terminal.shell_integration = false"));
        assert!(sent.lock().unwrap().is_empty());
    }
}
