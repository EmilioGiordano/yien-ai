//! No hook pipe transport outside Windows yet.

use std::sync::Arc;

use super::{HookPipeError, Shared};

pub(super) struct Server;

impl Server {
    pub(super) fn start(_name: &str, _shared: Arc<Shared>) -> Result<Self, HookPipeError> {
        Err(HookPipeError::Unsupported)
    }
}
