//! Getting the user's attention from outside the window (RF-ATN-09, -10):
//! a system toast, the taskbar badge and a taskbar flash.
//!
//! Windows only; elsewhere every call does nothing and succeeds. Failures are
//! values the caller logs; none of them may hide the window's own red mark
//! (RF-ATN-16).
//!
//! An unpackaged app needs an AppUserModelID for its toasts to show. The
//! process takes [`APP_ID`] in [`OsAttention::new`], and the first toast
//! registers its display name under
//! `HKEY_CURRENT_USER\Software\Classes\AppUserModelId\YIEN.App`, which is
//! what lets Windows show it without a Start-menu shortcut.
//!
//! ```no_run
//! use yien_platform::attention::{OsAttention, Toast};
//!
//! // In `main`, before the window opens.
//! let attention = OsAttention::new();
//! attention.on_activate(|launch| println!("clicked {launch}"));
//! // Once the window exists:
//! # let hwnd = 0;
//! attention.attach(hwnd);
//! attention.set_badge(2)?;
//! attention.toast(&Toast {
//!     title: "Claude Code needs you".into(),
//!     body: "Tab 1 · yien".into(),
//!     launch: "pane:7".into(),
//!     tag: "pane-7".into(),
//! })?;
//! # Ok::<(), yien_platform::attention::AttentionError>(())
//! ```

#![allow(unsafe_code)]

use std::fmt;

/// The AppUserModelID YIEN's process and toasts carry.
pub const APP_ID: &str = "YIEN.App";

/// The name Windows shows above YIEN's toasts.
pub const APP_NAME: &str = "YIEN";

/// What the toast says, and what its click hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// The first line, in bold.
    pub title: String,
    /// The text under it.
    pub body: String,
    /// Given back to [`OsAttention::on_activate`] when the toast is clicked.
    pub launch: String,
    /// A new toast replaces the one with the same tag (at most 64
    /// characters).
    pub tag: String,
}

/// Which call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Taking the AppUserModelID.
    AppId,
    /// Registering the display name.
    Register,
    /// Building or showing the toast.
    Toast,
    /// Drawing or setting the taskbar badge.
    Badge,
    /// Flashing the taskbar button.
    Flash,
    /// The attention thread's queue was full, so the call was dropped.
    Queue,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AppId => "app id",
            Self::Register => "app id registration",
            Self::Toast => "toast",
            Self::Badge => "taskbar badge",
            Self::Flash => "taskbar flash",
            Self::Queue => "attention queue (full, dropped)",
        })
    }
}

/// An OS call refused. Carries only the step and the OS error code, never
/// the toast's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{step} failed ({code:#010x})")]
pub struct AttentionError {
    /// The call that failed.
    pub step: Step,
    /// The OS error code (an HRESULT on Windows).
    pub code: u32,
}

/// Toasts, the taskbar badge and the flash for one window. Lives on the UI
/// thread and never blocks it: the OS calls run in order on a thread of
/// their own, which ends with the process.
pub struct OsAttention {
    imp: imp::Imp,
}

impl OsAttention {
    /// Gives the process YIEN's AppUserModelID. Call it before the window
    /// opens, so the taskbar button carries the same id as the toasts. A
    /// refusal is logged at debug level; toasts may then not show.
    pub fn new() -> Self {
        let imp = imp::Imp::new();
        if let Err(error) = imp.set_app_id() {
            tracing::debug!("{error}");
        }
        Self { imp }
    }

    /// The window the badge and the flash apply to (a Win32 `HWND`).
    pub fn attach(&self, hwnd: isize) {
        self.imp.attach(hwnd);
    }

    /// Shows `toast`, replacing YIEN's previous toast with the same tag. Its
    /// failures are logged at debug level on the attention thread; an error
    /// here means it could not be handed over (queue full or thread gone).
    pub fn toast(&self, toast: &Toast) -> Result<(), AttentionError> {
        self.imp.toast(toast)
    }

    /// Shows how many agents need the user on the taskbar button, or clears
    /// the badge at 0; calling it again with the same count sets it again.
    /// Does nothing before [`attach`](Self::attach).
    pub fn set_badge(&self, count: usize) -> Result<(), AttentionError> {
        self.imp.set_badge(count)
    }

    /// Flashes the taskbar button until the window comes forward. Does
    /// nothing before [`attach`](Self::attach). Dropped when the queue is
    /// full.
    pub fn flash(&self) -> Result<(), AttentionError> {
        self.imp.flash()
    }

    /// Runs `callback` with the toast's `launch` when the user clicks it.
    /// It runs on an OS thread, not the UI thread. A later call replaces it.
    pub fn on_activate(&self, callback: impl Fn(String) + Send + Sync + 'static) {
        self.imp.on_activate(Box::new(callback));
    }
}

impl Default for OsAttention {
    fn default() -> Self {
        Self::new()
    }
}

type Activate = Box<dyn Fn(String) + Send + Sync>;

#[cfg(windows)]
mod imp {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, PoisonError, mpsc};

    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{
        ToastActivatedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier,
    };
    use windows::Win32::Foundation::{ERROR_SUCCESS, HWND};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegSetValueExW,
    };
    use windows::Win32::UI::Shell::{
        ITaskbarList3, SetCurrentProcessExplicitAppUserModelID, TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIcon, DestroyIcon, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx,
        GetSystemMetrics, HICON, SM_CXSMICON,
    };
    use windows::core::{HSTRING, IInspectable, Interface as _, PCWSTR, h};

    use super::{APP_ID, APP_NAME, Activate, AttentionError, Step, Toast};
    use crate::badge::{badge_text, render};

    /// Jobs waiting for the attention thread; more are dropped.
    const QUEUE: usize = 4;
    /// Toasts whose click is still listened for, the latest last.
    const KEPT_TOASTS: usize = 16;

    enum Job {
        Toast(Toast),
        /// Apply the latest badge; `force` sets it even when unchanged.
        Badge {
            force: bool,
        },
        Flash(isize),
    }

    /// The badge the UI wants: the window and the count.
    type Wanted = Arc<Mutex<Option<(isize, usize)>>>;

    pub(super) struct Imp {
        hwnd: Cell<isize>,
        /// The attention thread's queue, started with the first job.
        jobs: RefCell<Option<mpsc::SyncSender<Job>>>,
        badge: Wanted,
        activate: Arc<Mutex<Option<Activate>>>,
    }

    fn fail(step: Step) -> impl Fn(windows::core::Error) -> AttentionError {
        move |e| AttentionError {
            step,
            code: e.code().0 as u32,
        }
    }

    impl Imp {
        pub(super) fn new() -> Self {
            Self {
                hwnd: Cell::new(0),
                jobs: RefCell::new(None),
                badge: Arc::new(Mutex::new(None)),
                activate: Arc::new(Mutex::new(None)),
            }
        }

        pub(super) fn set_app_id(&self) -> Result<(), AttentionError> {
            let id = HSTRING::from(APP_ID);
            // SAFETY: `id` is a NUL-terminated wide string that outlives the call.
            unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr())) }
                .map_err(fail(Step::AppId))
        }

        pub(super) fn attach(&self, hwnd: isize) {
            self.hwnd.set(hwnd);
        }

        pub(super) fn on_activate(&self, callback: Activate) {
            *self.activate.lock().unwrap_or_else(PoisonError::into_inner) = Some(callback);
        }

        /// Hands `job` to the attention thread without waiting.
        fn send(&self, job: Job, step: Step) -> Result<(), AttentionError> {
            let mut jobs = self.jobs.borrow_mut();
            if jobs.is_none() {
                let (send, recv) = mpsc::sync_channel::<Job>(QUEUE);
                let badge = Arc::clone(&self.badge);
                let activate = Arc::clone(&self.activate);
                std::thread::Builder::new()
                    .name("yien-attention".into())
                    .spawn(move || attention_thread(&recv, &badge, &activate))
                    .map_err(|e| AttentionError {
                        step,
                        code: e.raw_os_error().unwrap_or(0) as u32,
                    })?;
                *jobs = Some(send);
            }
            let Some(sender) = jobs.as_ref() else {
                return Err(AttentionError { step, code: 0 });
            };
            match sender.try_send(job) {
                Ok(()) => Ok(()),
                Err(mpsc::TrySendError::Full(_)) => Err(AttentionError {
                    step: Step::Queue,
                    code: 0,
                }),
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    *jobs = None;
                    Err(AttentionError { step, code: 0 })
                }
            }
        }

        pub(super) fn toast(&self, toast: &Toast) -> Result<(), AttentionError> {
            self.send(Job::Toast(toast.clone()), Step::Toast)
        }

        pub(super) fn set_badge(&self, count: usize) -> Result<(), AttentionError> {
            let hwnd = self.hwnd.get();
            if hwnd == 0 {
                return Ok(());
            }
            let wanted = Some((hwnd, count));
            let force = {
                let mut badge = self.badge.lock().unwrap_or_else(PoisonError::into_inner);
                let same = *badge == wanted;
                *badge = wanted;
                same
            };
            // A dropped wake-up only delays the badge: the thread applies the
            // latest one after every job it runs.
            self.send(Job::Badge { force }, Step::Badge)
        }

        pub(super) fn flash(&self) -> Result<(), AttentionError> {
            let hwnd = self.hwnd.get();
            if hwnd == 0 {
                return Ok(());
            }
            self.send(Job::Flash(hwnd), Step::Flash)
        }
    }

    /// Runs the toasts, badges and flashes it is sent, in order, and ends
    /// with the process. Each is a cross-process call: on the UI thread,
    /// COM's modal loop or the messages the call sends would dispatch into
    /// gpui while it is mid-update.
    ///
    /// This STA never pumps messages, so nothing may need a call into it:
    /// whatever another thread calls must be agile (the toast objects, and the
    /// `Activated` handler, which WinRT runs on a pool thread). ITaskbarList3
    /// is in-process and only called from here, so it needs no pump.
    fn attention_thread(
        recv: &mpsc::Receiver<Job>,
        badge: &Wanted,
        activate: &Arc<Mutex<Option<Activate>>>,
    ) {
        // SAFETY: initialises COM for this new thread, which owns no other
        // COM state; never uninitialised, since the thread lives until exit.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let mut notifier: Option<ToastNotifier> = None;
        let mut taskbar: Option<ITaskbarList3> = None;
        let mut applied: Option<(isize, usize)> = None;
        // Dropping a toast would stop its click from reaching `on_activate`.
        let mut shown: VecDeque<(String, ToastNotification)> = VecDeque::new();
        while let Ok(job) = recv.recv() {
            let mut force = false;
            match job {
                Job::Toast(toast) => match show(&toast, &mut notifier, activate) {
                    Ok(notification) => {
                        shown.retain(|(tag, _)| *tag != toast.tag);
                        shown.push_back((toast.tag, notification));
                        if shown.len() > KEPT_TOASTS {
                            shown.pop_front();
                        }
                        tracing::debug!("toast shown");
                    }
                    Err(error) => tracing::debug!("{error}"),
                },
                Job::Badge { force: again } => force = again,
                Job::Flash(hwnd) => flash(hwnd),
            }
            let wanted = *badge.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((hwnd, count)) = wanted
                && (force || applied != wanted)
            {
                match set_badge(&mut taskbar, hwnd, count) {
                    Ok(()) => {
                        applied = wanted;
                        tracing::debug!(count, "taskbar badge set");
                    }
                    Err(error) => tracing::debug!("{error}"),
                }
            }
        }
    }

    fn set_badge(
        taskbar: &mut Option<ITaskbarList3>,
        hwnd: isize,
        count: usize,
    ) -> Result<(), AttentionError> {
        let list = match taskbar {
            Some(list) => list.clone(),
            None => {
                // SAFETY: COM is initialised on this thread; the CLSID and
                // interface match.
                let list: ITaskbarList3 =
                    unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) }
                        .map_err(fail(Step::Badge))?;
                // SAFETY: `list` is a live ITaskbarList3.
                unsafe { list.HrInit() }.map_err(fail(Step::Badge))?;
                *taskbar = Some(list.clone());
                list
            }
        };
        let hwnd = HWND(hwnd as *mut _);
        let Some(text) = badge_text(count) else {
            // SAFETY: a null icon and description clear the overlay of our
            // own window.
            return unsafe { list.SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null()) }
                .map_err(fail(Step::Badge));
        };
        let icon = badge_icon(&text)?;
        let description = HSTRING::from(if count == 1 {
            "1 agent needs you".to_owned()
        } else {
            format!("{text} agents need you")
        });
        // SAFETY: `icon` is a live icon we own and `description` a
        // NUL-terminated wide string; both outlive the call.
        let set = unsafe { list.SetOverlayIcon(hwnd, icon, PCWSTR(description.as_ptr())) };
        // SAFETY: the taskbar keeps its own copy, so ours can go now.
        let _ = unsafe { DestroyIcon(icon) };
        set.map_err(fail(Step::Badge))
    }

    fn flash(hwnd: isize) {
        let info = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd: HWND(hwnd as *mut _),
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 1,
            dwTimeout: 0,
        };
        // SAFETY: `info` is a valid FLASHWINFO for the duration of the call.
        // The result is the window's previous state, not an error.
        let _ = unsafe { FlashWindowEx(&info) };
    }

    fn show(
        toast: &Toast,
        notifier: &mut Option<ToastNotifier>,
        activate: &Arc<Mutex<Option<Activate>>>,
    ) -> Result<ToastNotification, AttentionError> {
        let current = match notifier {
            Some(n) => n.clone(),
            None => {
                register_display_name()?;
                let n = ToastNotificationManager::CreateToastNotifierWithId(&APP_ID.into())
                    .map_err(fail(Step::Toast))?;
                *notifier = Some(n.clone());
                n
            }
        };
        let document = document(toast).map_err(fail(Step::Toast))?;
        let notification =
            ToastNotification::CreateToastNotification(&document).map_err(fail(Step::Toast))?;
        notification
            .SetTag(&toast.tag.as_str().into())
            .map_err(fail(Step::Toast))?;
        let activate = Arc::clone(activate);
        notification
            .Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
                move |_, arguments| {
                    let launch = arguments
                        .as_ref()
                        .and_then(|a| a.cast::<ToastActivatedEventArgs>().ok())
                        .and_then(|a| a.Arguments().ok())
                        .map(|a| a.to_string())
                        .unwrap_or_default();
                    if let Some(callback) = activate
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .as_ref()
                    {
                        callback(launch);
                    }
                    Ok(())
                },
            ))
            .map_err(fail(Step::Toast))?;
        current.Show(&notification).map_err(fail(Step::Toast))?;
        Ok(notification)
    }

    /// The badge drawn at the small-icon size, as an icon we must destroy.
    fn badge_icon(text: &str) -> Result<HICON, AttentionError> {
        // SAFETY: reads a system metric.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as usize;
        let color = render(text, size);
        // A 1-bit mask with rows padded to 16 bits; all zero, so the
        // colour's alpha alone decides what shows.
        let mask = vec![0u8; size.div_ceil(16) * 2 * size];
        // SAFETY: both buffers hold the sizes CreateIcon reads for a
        // `size` x `size` icon with a 1-bit mask and 32-bit colour.
        unsafe {
            CreateIcon(
                None,
                size as i32,
                size as i32,
                1,
                32,
                mask.as_ptr(),
                color.as_ptr(),
            )
        }
        .map_err(fail(Step::Badge))
    }

    fn document(toast: &Toast) -> windows::core::Result<XmlDocument> {
        let document = XmlDocument::new()?;
        let root = document.CreateElement(h!("toast"))?;
        root.SetAttribute(h!("launch"), &toast.launch.as_str().into())?;
        document.AppendChild(&root)?;
        let visual = document.CreateElement(h!("visual"))?;
        root.AppendChild(&visual)?;
        let binding = document.CreateElement(h!("binding"))?;
        binding.SetAttribute(h!("template"), h!("ToastGeneric"))?;
        visual.AppendChild(&binding)?;
        for line in [&toast.title, &toast.body] {
            let text = document.CreateElement(h!("text"))?;
            text.SetInnerText(&line.as_str().into())?;
            binding.AppendChild(&text)?;
        }
        Ok(document)
    }

    /// Registers YIEN's display name for its AppUserModelID under the
    /// user's own classes, so Windows shows the toasts of an app with no
    /// Start-menu shortcut. Never HKLM.
    fn register_display_name() -> Result<(), AttentionError> {
        let fail = |code: u32| AttentionError {
            step: Step::Register,
            code,
        };
        let path = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
        let mut key = HKEY::default();
        // SAFETY: `path` outlives the call and `key` receives the new handle.
        let created = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if created != ERROR_SUCCESS {
            return Err(fail(created.0));
        }
        let name: Vec<u8> = APP_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: `key` is open for writing and `name` is a NUL-terminated
        // UTF-16 string as REG_SZ expects.
        let set = unsafe { RegSetValueExW(key, h!("DisplayName"), None, REG_SZ, Some(&name)) };
        // SAFETY: `key` was opened above and is closed once.
        let _ = unsafe { RegCloseKey(key) };
        if set != ERROR_SUCCESS {
            return Err(fail(set.0));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Activate, AttentionError, Toast};

    pub(super) struct Imp;

    impl Imp {
        pub(super) fn new() -> Self {
            Self
        }

        pub(super) fn set_app_id(&self) -> Result<(), AttentionError> {
            Ok(())
        }

        pub(super) fn attach(&self, _hwnd: isize) {}

        pub(super) fn on_activate(&self, _callback: Activate) {}

        pub(super) fn toast(&self, _toast: &Toast) -> Result<(), AttentionError> {
            Ok(())
        }

        pub(super) fn set_badge(&self, _count: usize) -> Result<(), AttentionError> {
            Ok(())
        }

        pub(super) fn flash(&self) -> Result<(), AttentionError> {
            Ok(())
        }
    }
}
