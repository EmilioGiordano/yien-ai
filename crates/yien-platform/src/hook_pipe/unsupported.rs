//! No hook pipe transport outside Windows yet.

use std::fs::File;
use std::sync::Arc;
use std::time::Instant;

use super::{HookPipeError, Shared};

pub(super) struct Server;

impl Server {
    pub(super) fn start(_name: &str, _shared: Arc<Shared>) -> Result<Self, HookPipeError> {
        Err(HookPipeError::Unsupported)
    }
}

pub(super) fn connect(_name: &str, _deadline: Instant) -> Option<File> {
    None
}
