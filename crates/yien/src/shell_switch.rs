//! `terminal.shell` and `terminal.shell_integration` applied without a
//! restart (RF-CFG-01): every valid reading of the settings file tells the
//! core which shell panes started from then on run, and whether they report
//! their folder (RF-SES-01).

use std::sync::{Mutex, PoisonError};

use yien_config::{Config, ShellSetting};
use yien_proto::ShellKind;
use yien_ui::ConfigUpdate;

/// What the file asks of the shells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellWanted {
    /// `terminal.shell`; `None` for YIEN's own pick.
    pub shell: Option<ShellKind>,
    /// `terminal.shell_integration`.
    pub integration: bool,
}

impl ShellWanted {
    /// The shells `config` asks for.
    pub(crate) fn of(config: &Config) -> Self {
        let shell = match config.shell {
            ShellSetting::Auto => None,
            ShellSetting::Cmd => Some(ShellKind::Cmd),
            ShellSetting::PowerShell => Some(ShellKind::PowerShell),
            ShellSetting::Pwsh => Some(ShellKind::Pwsh),
            ShellSetting::GitBash => Some(ShellKind::GitBash),
        };
        Self {
            shell,
            integration: config.shell_integration,
        }
    }
}

/// Delivers a value to the core; `true` once the core took it.
pub(crate) type Deliver = Box<dyn Fn(ShellWanted) -> bool + Send>;

/// What the file asks for and what the core runs with.
pub(crate) struct ShellSwitch {
    state: Mutex<State>,
}

struct State {
    wanted: ShellWanted,
    core_has: ShellWanted,
    deliver: Option<Deliver>,
}

impl ShellSwitch {
    /// The core starts with `initial`, so nothing is sent until it changes.
    pub(crate) fn new(initial: ShellWanted) -> Self {
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
        let Some(wanted) = wanted(update) else { return };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.wanted = wanted;
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
fn wanted(update: &ConfigUpdate) -> Option<ShellWanted> {
    match update {
        ConfigUpdate::Loaded(loaded) => Some(ShellWanted::of(&loaded.config)),
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

    fn recorder(sent: &Arc<Mutex<Vec<ShellWanted>>>, accepts: &Arc<AtomicBool>) -> Deliver {
        let (sent, accepts) = (Arc::clone(sent), Arc::clone(accepts));
        Box::new(move |wanted| {
            sent.lock().unwrap().push(wanted);
            accepts.load(Ordering::SeqCst)
        })
    }

    const DEFAULT: ShellWanted = ShellWanted {
        shell: None,
        integration: true,
    };

    fn integration(on: bool) -> ShellWanted {
        ShellWanted {
            integration: on,
            ..DEFAULT
        }
    }

    fn shell(kind: ShellKind) -> ShellWanted {
        ShellWanted {
            shell: Some(kind),
            ..DEFAULT
        }
    }

    #[test]
    fn rf_cfg_01_each_change_of_the_setting_reaches_the_core_once() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicBool::new(true));
        let switch = ShellSwitch::new(DEFAULT);
        switch.reading(&loaded("terminal.shell_integration = false"));
        assert!(sent.lock().unwrap().is_empty(), "no core yet");
        switch.attach(recorder(&sent, &accepts));
        assert_eq!(
            *sent.lock().unwrap(),
            [integration(false)],
            "sent once the core runs"
        );

        switch.reading(&loaded("terminal.shell_integration = false"));
        switch.reading(&ConfigUpdate::Invalid(
            yien_config::parse("terminal.shell_integration = 1").unwrap_err(),
        ));
        switch.reading(&ConfigUpdate::Unreadable("gone".into()));
        assert_eq!(
            *sent.lock().unwrap(),
            [integration(false)],
            "unchanged or not valid"
        );

        switch.reading(&loaded(""));
        assert_eq!(
            *sent.lock().unwrap(),
            [integration(false), DEFAULT],
            "the default is on"
        );
    }

    #[test]
    fn rf_cfg_01_the_default_shell_reaches_the_core_on_each_change() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicBool::new(true));
        let switch = ShellSwitch::new(DEFAULT);
        switch.attach(recorder(&sent, &accepts));
        switch.reading(&loaded("terminal.shell = \"cmd\""));
        switch.reading(&loaded("terminal.shell = \"cmd\""));
        switch.reading(&ConfigUpdate::Invalid(
            yien_config::parse("terminal.shell = \"wsl\"").unwrap_err(),
        ));
        switch.reading(&loaded("[terminal]\nshell = \"git-bash\""));
        switch.reading(&loaded("terminal.shell = \"auto\""));
        assert_eq!(
            *sent.lock().unwrap(),
            [shell(ShellKind::Cmd), shell(ShellKind::GitBash), DEFAULT]
        );
    }

    #[test]
    fn rf_cfg_01_every_setting_value_names_its_shell() {
        let kinds = [
            ("auto", None),
            ("cmd", Some(ShellKind::Cmd)),
            ("powershell", Some(ShellKind::PowerShell)),
            ("pwsh", Some(ShellKind::Pwsh)),
            ("git-bash", Some(ShellKind::GitBash)),
        ];
        assert_eq!(kinds.len(), ShellSetting::NAMES.len());
        for (name, kind) in kinds {
            let config = yien_config::parse(&format!("terminal.shell = \"{name}\""))
                .unwrap()
                .config;
            assert_eq!(ShellWanted::of(&config).shell, kind, "{name}");
            if let Some(kind) = kind {
                assert_eq!(kind.id(), name, "the file and the core use one id");
            }
        }
    }

    #[test]
    fn rf_cfg_01_a_refused_delivery_is_tried_again_on_the_next_reading() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicBool::new(false));
        let switch = ShellSwitch::new(DEFAULT);
        switch.attach(recorder(&sent, &accepts));
        switch.reading(&loaded("terminal.shell_integration = false"));
        accepts.store(true, Ordering::SeqCst);
        switch.reading(&loaded("terminal.shell_integration = false"));
        switch.reading(&loaded("terminal.shell_integration = false"));
        assert_eq!(
            *sent.lock().unwrap(),
            [integration(false), integration(false)]
        );
    }

    #[test]
    fn rf_cfg_01_a_core_started_with_the_value_gets_nothing() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let accepts = Arc::new(AtomicBool::new(true));
        let switch = ShellSwitch::new(ShellWanted {
            shell: Some(ShellKind::Pwsh),
            integration: false,
        });
        switch.attach(recorder(&sent, &accepts));
        switch.reading(&loaded(
            "terminal.shell = \"pwsh\"\nterminal.shell_integration = false",
        ));
        assert!(sent.lock().unwrap().is_empty());
    }
}
