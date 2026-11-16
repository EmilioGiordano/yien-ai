//! No test control transport outside Windows yet.

use super::{Handler, TestControlError};

pub(super) struct Server;

impl Server {
    pub(super) fn start(_path: &str, _handler: Handler) -> Result<Self, TestControlError> {
        Err(TestControlError::Unsupported)
    }
}
