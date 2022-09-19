use std::{pin::Pin, time::Duration};

use futures::Future;
use tokio::{task::JoinHandle, time::Instant};

use crate::rt::Rt;

// To support re-creation, we need to store a factory of the future that
// represents the software. This is somewhat annoying in that it requires
// boxxing to avoid generics.
type Software = Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + 'static>>>;

/// Differentiates runtime fields for different host types
pub(crate) enum Role {
    /// A client handle
    Client { rt: Rt, handle: JoinHandle<()> },

    /// A simulated host
    Simulated { rt: Rt, software: Software },
}

impl Role {
    pub(crate) fn client(rt: Rt, handle: JoinHandle<()>) -> Self {
        Self::Client { rt, handle }
    }

    pub(crate) fn simulated<F, Fut>(rt: Rt, software: F) -> Self
    where
        F: Fn() -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let wrapped: Software = Box::new(move || Box::pin(software()));

        Self::Simulated {
            rt,
            software: wrapped,
        }
    }

    pub(crate) fn tick(&self, duration: Duration) -> Instant {
        let rt = match self {
            Role::Client { rt, .. } => rt,
            Role::Simulated { rt, .. } => rt,
        };

        rt.tick(duration)
    }
}
