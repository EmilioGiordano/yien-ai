//! The tab bar's "new tab in the default folder" button and command
//! (RF-ESP-01, RF-ESP-03): a shell tab in `ui.tabs.default_dir`, the home
//! folder by default, in the project on that folder or a new one.

use std::path::Path;

use gpui_kit::*;
use yien_proto::Command;

use super::status::Notice;
use super::{Shell, icon_button};
use crate::actions::{DEFAULT_TAB_KEYS, NewDefaultTab};
use crate::assets;

const FILE: &str = yien_config::FILE_NAME;

/// The folder a new default tab opens in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Picked {
    /// The configured folder, or the home folder when none is set.
    Dir(String),
    /// The configured folder is not there: the home folder, and why.
    Fallback { dir: String, warning: String },
    /// No folder at all: what the user is told.
    Nowhere(String),
}

/// Where a new default tab opens: `configured` expanded with `home` and
/// `var`, when it is a full path to a folder that `is_dir` finds; else the
/// home folder, with a warning when a folder was configured.
pub(crate) fn pick_dir(
    configured: Option<&str>,
    home: Option<&str>,
    var: &dyn Fn(&str) -> Option<String>,
    is_dir: &dyn Fn(&str) -> bool,
) -> Picked {
    let Some(raw) = configured else {
        return match home {
            Some(home) => Picked::Dir(home.to_owned()),
            None => Picked::Nowhere(format!(
                "There is no home folder to open a tab in; set ui.tabs.default_dir in {FILE}."
            )),
        };
    };
    let dir = yien_config::expand_dir(raw, home, var);
    let named = if dir == raw {
        format!("\"{raw}\"")
    } else {
        format!("\"{raw}\" ({dir})")
    };
    let why = if !Path::new(&dir).is_absolute() {
        "is not a full path"
    } else if !is_dir(&dir) {
        "is not a folder"
    } else {
        return Picked::Dir(dir);
    };
    match home {
        Some(home) => Picked::Fallback {
            dir: home.to_owned(),
            warning: format!(
                "{FILE}: ui.tabs.default_dir {named} {why}; the tab opens in your home folder."
            ),
        },
        None => Picked::Nowhere(format!(
            "{FILE}: ui.tabs.default_dir {named} {why}, and there is no home folder to use."
        )),
    }
}

/// The accessible name of the tab bar's new default tab button.
pub(crate) fn default_tab_label(configured: Option<&str>) -> String {
    let folder = configured.unwrap_or("your home folder");
    format!("New tab in {folder} ({DEFAULT_TAB_KEYS})")
}

impl Shell {
    /// Finds the folder off the UI thread, as a missing network share can
    /// take seconds to answer, then opens the tab.
    pub(super) fn new_default_tab(
        &mut self,
        _: &NewDefaultTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let configured = self.settings.config().default_tab_dir.clone();
        let home = self.home.clone();
        let picking = cx.background_executor().spawn(async move {
            pick_dir(
                configured.as_deref(),
                home.as_deref(),
                &|name| std::env::var(name).ok(),
                &|dir| Path::new(dir).is_dir(),
            )
        });
        cx.spawn(async move |this, cx| {
            let picked = picking.await;
            let _ = this.update(cx, |shell, cx| shell.open_default_tab(picked, cx));
        })
        .detach();
    }

    /// A folder that is a project gets a new shell tab in it; any other
    /// becomes a project, whose first tab the core opens and selects.
    fn open_default_tab(&mut self, picked: Picked, cx: &mut Context<Self>) {
        let dir = match picked {
            Picked::Dir(dir) => dir,
            Picked::Fallback { dir, warning } => {
                self.show(Notice::warning(warning), cx);
                dir
            }
            Picked::Nowhere(error) => {
                self.fail(error, cx);
                return;
            }
        };
        match self.workspace.project_at(&dir) {
            Some(project) => {
                if self.workspace.is_folded(project) {
                    self.toggle_fold(project, cx);
                }
                self.new_tab_in(project, None, cx);
            }
            None => self.command(
                Command::NewProject {
                    root: dir,
                    name: None,
                },
                cx,
            ),
        }
    }

    /// The button beside "Add project" in the tab bar's head.
    pub(super) fn default_tab_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let configured = self.settings.config().default_tab_dir.as_deref();
        icon_button("new-default-tab", assets::TERMINAL, &self.theme)
            .aria_label(default_tab_label(configured))
            .on_click(cx.listener(|this, _, window, cx| {
                this.new_default_tab(&NewDefaultTab, window, cx);
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Picked, default_tab_label, pick_dir};
    use crate::catalog::default_keys;

    fn rooted(dir: &str) -> String {
        if cfg!(windows) {
            format!("C:{dir}")
        } else {
            dir.to_owned()
        }
    }

    fn vars(name: &str) -> Option<String> {
        (name == "CODE").then(|| rooted("/mnt/code"))
    }

    fn pick(configured: Option<&str>, home: Option<&str>) -> Picked {
        let folders = ["/home/me", "/home/me/work", "/mnt/code/yien"];
        let home = home.map(rooted);
        let configured = configured.map(|c| {
            if c.starts_with('/') {
                rooted(c)
            } else {
                c.to_owned()
            }
        });
        let picked = pick_dir(configured.as_deref(), home.as_deref(), &vars, &|dir| {
            folders.iter().any(|f| rooted(f) == dir)
        });
        // Back to the Unix spelling, so one expectation holds on both.
        let plain = |s: String| s.replace("C:/", "/");
        match picked {
            Picked::Dir(dir) => Picked::Dir(plain(dir)),
            Picked::Fallback { dir, warning } => Picked::Fallback {
                dir: plain(dir),
                warning: plain(warning),
            },
            Picked::Nowhere(error) => Picked::Nowhere(plain(error)),
        }
    }

    #[test]
    fn rf_esp_03_unset_means_the_home_folder() {
        assert_eq!(pick(None, Some("/home/me")), Picked::Dir("/home/me".into()));
        assert!(
            matches!(pick(None, None), Picked::Nowhere(e) if e.contains("ui.tabs.default_dir"))
        );
    }

    #[test]
    fn rf_esp_03_a_configured_folder_is_expanded_and_used() {
        let home = Some("/home/me");
        assert_eq!(
            pick(Some("~/work"), home),
            Picked::Dir("/home/me/work".into())
        );
        assert_eq!(
            pick(Some("%CODE%/yien"), home),
            Picked::Dir("/mnt/code/yien".into())
        );
    }

    #[test]
    fn rf_esp_03_a_missing_folder_warns_and_falls_back_to_home() {
        let home = Some("/home/me");
        assert_eq!(
            pick(Some("~/gone"), home),
            Picked::Fallback {
                dir: "/home/me".into(),
                warning: "config.toml: ui.tabs.default_dir \"~/gone\" (/home/me/gone) is not a \
                          folder; the tab opens in your home folder."
                    .into(),
            }
        );
        assert_eq!(
            pick(Some("code"), home),
            Picked::Fallback {
                dir: "/home/me".into(),
                warning: "config.toml: ui.tabs.default_dir \"code\" is not a full path; the tab \
                          opens in your home folder."
                    .into(),
            },
            "a relative path would depend on where YIEN was started"
        );
        assert!(
            matches!(pick(Some("/nope"), None), Picked::Nowhere(e) if e.contains("\"/nope\" is not a folder")),
        );
    }

    #[test]
    fn rf_esp_03_the_button_names_its_folder_and_keys() {
        let keys = default_keys("tab.new_default").unwrap();
        assert_eq!(
            default_tab_label(Some("D:\\Code")),
            format!("New tab in D:\\Code ({keys})")
        );
        assert_eq!(
            default_tab_label(None),
            "New tab in your home folder (Ctrl+Shift+H)"
        );
    }
}
