//! The YIEN OS layer (layer 2): what needs the operating system, behind plain
//! functions and values the pure layers can be handed.
//!
//! For now: per-user paths ([`paths`]), the log file ([`init_logging`]), the
//! pipe that receives Claude Code hook events ([`hook_pipe`]), the DLL search
//! restriction ([`dll_search`]) and the file store for the saved session
//! ([`state_store`]), and the toast, taskbar badge and flash that call the
//! user back to the window ([`attention`], [`badge`]), the claim that
//! keeps one YIEN per data dir ([`single_instance`]), the settings file
//! on disk ([`config_file`]), the read-only listing of each agent CLI's
//! past sessions ([`agent_sessions`]), the user's agent profile files
//! ([`user_profiles`]), and a tab's folder for the file
//! tree ([`file_tree`]).
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
pub mod config_file;
pub mod dll_search;
pub mod file_tree;
pub mod hook_pipe;
pub mod http;
mod logging;
pub mod paths;
pub mod single_instance;
pub mod state_store;
pub mod user_profiles;

pub use logging::{LogError, LogGuard, init_logging, init_logging_with};
