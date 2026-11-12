//! A newer YIEN as the window sees it: what the binary's update check
//! found, the notice that says so, and the steps of an install the user
//! asked for. Nothing here touches the network or the disk.

use tokio::sync::mpsc;

/// A newer YIEN the update check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOffer {
    /// The version, such as `0.2.0`.
    pub version: String,
    /// The release notes, GitHub markdown.
    pub notes: String,
}

/// Downloads and installs an offered update, on the binary's side.
///
/// ```
/// use yien_ui::UpdateHost;
///
/// struct Offline;
/// impl UpdateHost for Offline {
///     fn download(&self, _version: &str, done: Box<dyn FnOnce(Result<(), String>) + Send>) {
///         done(Err("offline".into()));
///     }
///     fn install_on_quit(&self, _armed: bool) {}
/// }
/// ```
pub trait UpdateHost {
    /// Downloads the installer of `version` and checks its SHA-256. Returns
    /// at once; `done` runs later, from any thread, with what went wrong.
    fn download(&self, version: &str, done: Box<dyn FnOnce(Result<(), String>) + Send>);
    /// Whether the downloaded installer starts once YIEN has quit.
    fn install_on_quit(&self, armed: bool);
}

/// What the binary hands the window about updates.
pub struct Updates {
    /// Each newer release the check finds; `None` withdraws the offer, as
    /// when `update.check` is turned off.
    pub offers: mpsc::UnboundedReceiver<Option<UpdateOffer>>,
    /// Downloads and installs.
    pub host: Box<dyn UpdateHost>,
}

/// Where an install stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Nothing asked yet, or the last try failed.
    Idle,
    /// The installer of this version is being downloaded.
    Downloading(String),
    /// The installer of this version is downloaded and checked.
    Ready(String),
}

/// What the window does after the user asks to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Install {
    /// Say this and do nothing.
    Say(String),
    /// Download this version.
    Download(String),
    /// The installer is ready: quit, then it runs.
    Quit,
}

/// The offer the window holds and where its install stands.
#[derive(Debug, Clone)]
pub(crate) struct UpdateModel {
    offer: Option<UpdateOffer>,
    stage: Stage,
    /// The user took the "available" notice down.
    dismissed: bool,
    /// The installer is set to start at quit.
    armed: bool,
    /// A newer offer that came during an install; it takes over if the
    /// install fails.
    pending: Option<UpdateOffer>,
}

impl Default for UpdateModel {
    fn default() -> Self {
        Self {
            offer: None,
            stage: Stage::Idle,
            dismissed: false,
            armed: false,
            pending: None,
        }
    }
}

impl UpdateModel {
    pub(crate) fn offer(&self) -> Option<&UpdateOffer> {
        self.offer.as_ref()
    }

    /// Takes an offer; the notice to show when it is a version not seen
    /// before. An offer during an install of another version waits behind it.
    pub(crate) fn offered(&mut self, offer: UpdateOffer) -> Option<String> {
        if self
            .offer
            .as_ref()
            .is_some_and(|o| o.version == offer.version)
        {
            self.offer = Some(offer);
            return None;
        }
        if self.stage != Stage::Idle {
            self.pending = Some(offer);
            return None;
        }
        let notice = available_notice(&offer.version);
        self.offer = Some(offer);
        self.dismissed = false;
        Some(notice)
    }

    /// The offer is withdrawn: nothing to show or install, and an install
    /// on its way is dropped. Whether there was one.
    pub(crate) fn withdraw(&mut self) -> bool {
        let had = self.offer.take().is_some();
        self.pending = None;
        self.stage = Stage::Idle;
        self.armed = false;
        had
    }

    /// The "available" notice to stand in the status bar while nothing else
    /// is shown: until the user takes it down or an install starts.
    pub(crate) fn standing_notice(&self) -> Option<String> {
        let offer = self.offer.as_ref()?;
        (!self.dismissed && self.stage == Stage::Idle).then(|| available_notice(&offer.version))
    }

    /// Whether `text` is the "available" notice of the current offer.
    pub(crate) fn is_available_notice(&self, text: &str) -> bool {
        self.offer
            .as_ref()
            .is_some_and(|o| available_notice(&o.version) == text)
    }

    /// The user took the "available" notice down.
    pub(crate) fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// The installer will start at quit.
    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    /// The quit was cancelled; whether an install had been set to run.
    pub(crate) fn disarm(&mut self) -> bool {
        std::mem::take(&mut self.armed)
    }

    /// The user asked to install.
    pub(crate) fn install(&mut self) -> Install {
        let Some(offer) = &self.offer else {
            return Install::Say("No YIEN update has been found.".into());
        };
        match &self.stage {
            Stage::Idle => {
                self.stage = Stage::Downloading(offer.version.clone());
                Install::Download(offer.version.clone())
            }
            Stage::Downloading(version) => {
                Install::Say(format!("YIEN {version} is still downloading."))
            }
            Stage::Ready(_) => Install::Quit,
        }
    }

    /// The download of `version` ended; the notice to show.
    pub(crate) fn downloaded(&mut self, version: &str, result: Result<(), String>) -> Downloaded {
        if self.stage != Stage::Downloading(version.to_owned()) {
            return Downloaded::Stale;
        }
        match result {
            Ok(()) => {
                self.stage = Stage::Ready(version.to_owned());
                Downloaded::Ready
            }
            Err(why) => {
                self.stage = Stage::Idle;
                if let Some(pending) = self.pending.take() {
                    self.offer = Some(pending);
                    self.dismissed = false;
                }
                Downloaded::Failed(format!("YIEN {version} was not installed: {why}"))
            }
        }
    }
}

/// How a download ended, for the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Downloaded {
    /// Checked: quit so the installer runs.
    Ready,
    /// Show this error.
    Failed(String),
    /// Not the download the model waits for.
    Stale,
}

/// The status bar notice for a newer version.
pub(crate) fn available_notice(version: &str) -> String {
    format!("YIEN {version} is available")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(version: &str) -> UpdateOffer {
        UpdateOffer {
            version: version.into(),
            notes: format!("# {version}"),
        }
    }

    #[test]
    fn a_new_version_is_announced_once() {
        let mut m = UpdateModel::default();
        assert_eq!(
            m.offered(offer("0.2.0")).as_deref(),
            Some("YIEN 0.2.0 is available")
        );
        assert_eq!(m.offered(offer("0.2.0")), None, "the same one again");
        assert_eq!(
            m.offered(offer("0.3.0")).as_deref(),
            Some("YIEN 0.3.0 is available")
        );
        assert_eq!(m.offer().unwrap().version, "0.3.0");
    }

    #[test]
    fn install_downloads_then_quits() {
        let mut m = UpdateModel::default();
        assert!(matches!(m.install(), Install::Say(_)), "nothing offered");
        m.offered(offer("0.2.0"));
        assert_eq!(m.install(), Install::Download("0.2.0".into()));
        assert_eq!(
            m.install(),
            Install::Say("YIEN 0.2.0 is still downloading.".into())
        );
        assert_eq!(m.offered(offer("0.3.0")), None, "waits behind the install");
        assert_eq!(m.offer().unwrap().version, "0.2.0");
        assert_eq!(m.downloaded("0.2.0", Ok(())), Downloaded::Ready);
        assert_eq!(
            m.install(),
            Install::Quit,
            "asked again after a cancelled quit"
        );
    }

    #[test]
    fn a_failed_download_can_be_tried_again() {
        let mut m = UpdateModel::default();
        m.offered(offer("0.2.0"));
        m.install();
        assert_eq!(
            m.downloaded("0.2.0", Err("the checksum did not match".into())),
            Downloaded::Failed("YIEN 0.2.0 was not installed: the checksum did not match".into())
        );
        assert_eq!(m.install(), Install::Download("0.2.0".into()));
        assert_eq!(m.downloaded("0.1.0", Ok(())), Downloaded::Stale);
    }

    #[test]
    fn the_available_notice_stands_until_dismissed_or_withdrawn() {
        let mut m = UpdateModel::default();
        assert_eq!(m.standing_notice(), None);
        m.offered(offer("0.2.0"));
        assert_eq!(
            m.standing_notice().as_deref(),
            Some("YIEN 0.2.0 is available")
        );
        assert!(m.is_available_notice("YIEN 0.2.0 is available"));
        assert!(!m.is_available_notice("Saved."));
        m.dismiss();
        assert_eq!(m.standing_notice(), None);
        m.offered(offer("0.3.0"));
        assert!(m.standing_notice().is_some(), "a newer one shows again");
        m.install();
        assert_eq!(m.standing_notice(), None, "not while downloading");
        assert!(m.withdraw());
        assert_eq!(m.offer(), None);
        assert_eq!(m.downloaded("0.3.0", Ok(())), Downloaded::Stale);
        assert!(matches!(m.install(), Install::Say(_)));
        assert!(!m.withdraw());
    }

    #[test]
    fn only_an_armed_install_is_reported_as_waiting() {
        let mut m = UpdateModel::default();
        m.offered(offer("0.2.0"));
        assert!(!m.disarm(), "an offer alone is not armed");
        m.arm();
        assert!(m.disarm());
        assert!(!m.disarm());
    }

    #[test]
    fn a_newer_offer_during_a_failed_download_takes_over() {
        let mut m = UpdateModel::default();
        m.offered(offer("0.2.0"));
        m.install();
        assert_eq!(m.offered(offer("0.3.0")), None);
        assert!(matches!(
            m.downloaded("0.2.0", Err("x".into())),
            Downloaded::Failed(_)
        ));
        assert_eq!(m.offer().unwrap().version, "0.3.0");
        assert_eq!(
            m.standing_notice().as_deref(),
            Some("YIEN 0.3.0 is available")
        );
        assert_eq!(m.install(), Install::Download("0.3.0".into()));
    }
}
