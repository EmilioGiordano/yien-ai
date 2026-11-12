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
    /// Each newer release the check finds.
    pub offers: mpsc::UnboundedReceiver<UpdateOffer>,
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
}

impl Default for UpdateModel {
    fn default() -> Self {
        Self {
            offer: None,
            stage: Stage::Idle,
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
            return None;
        }
        let notice = available_notice(&offer.version);
        self.offer = Some(offer);
        Some(notice)
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
    fn rnf_21_install_downloads_then_quits() {
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
}
