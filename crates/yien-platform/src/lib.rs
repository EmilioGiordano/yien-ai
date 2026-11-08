//! The YIEN OS layer (layer 2): what needs the operating system, behind plain
//! functions and values the pure layers can be handed.
//!
//! For now: per-user paths ([`paths`]), the log file ([`init_logging`]), the
//! pipe that receives Claude Code hook events ([`hook_pipe`]), the DLL search
//! restriction ([`dll_search`]) and the file store for the saved session
//! ([`state_store`]), and the toast, taskbar badge and flash that call the
//! user back to the window ([`attention`], [`badge`]), and the claim that
//! keeps one YIEN per data dir ([`single_instance`]), and the read-only
//! listing of each agent CLI's past sessions ([`agent_sessions`]).
//!
//! ```no_run
//! let dir = yien_platform::paths::logs_dir()?;
//! let _guard = yien_platform::init_logging(&dir)?;
//! tracing::info!("started");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(unsafe_code)]

pub mod agent_sessions;
pub mod attention;
pub mod badge;
pub mod dll_search;
pub mod hook_pipe;
mod logging;
pub mod paths;
pub mod single_instance;
pub mod state_store;

pub use logging::{LogError, LogGuard, init_logging, init_logging_with};
