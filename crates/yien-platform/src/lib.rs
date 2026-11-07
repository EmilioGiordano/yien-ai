//! The YIEN OS layer (layer 2): what needs the operating system, behind plain
//! functions and values the pure layers can be handed.
//!
//! For now: per-user paths ([`paths`]), the log file ([`init_logging`]) and
//! the pipe that receives Claude Code hook events ([`hook_pipe`]).
//!
//! ```no_run
//! let dir = yien_platform::paths::logs_dir()?;
//! let _guard = yien_platform::init_logging(&dir)?;
//! tracing::info!("started");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(unsafe_code)]

pub mod hook_pipe;
mod logging;
pub mod paths;

pub use logging::{LogError, LogGuard, init_logging, init_logging_with};
