use std::time::Duration;

use tokio::{task::JoinHandle, time::Instant};

use crate::rt::Rt;

/// Differentiates runtime fields for different host types
pub(crate) enum Role {
    /// A client handle
    Client { rt: Rt, handle: JoinHandle<()> },

    /// A simulated host
    Simulated(Rt),
}

impl Role {
    pub(crate) fn client(rt: Rt, handle: JoinHandle<()>) -> Self {
        Self::Client { rt, handle }
    }

    pub(crate) fn simulated(rt: Rt) -> Self {
        Self::Simulated(rt)
    }

    pub(crate) fn tick(&self, duration: Duration) -> Instant {
        let rt = match self {
            Role::Client { rt, .. } => rt,
            Role::Simulated(rt) => rt,
        };

        rt.tick(duration)
    }
}
