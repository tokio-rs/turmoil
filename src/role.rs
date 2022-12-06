use std::{pin::Pin, time::Duration};

use futures::Future;
use tokio::{task::JoinHandle, time::Instant};

use crate::{rt::Rt, Result};

// To support re-creation, we need to store a factory of the future that
// represents the software. This is somewhat annoying in that it requires
// boxxing to avoid generics.
type Software<'a> = Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()>>> + 'a>;

/// Differentiates runtime fields for different host types
pub(crate) enum Role<'a> {
    /// A client handle
    Client { rt: Rt, handle: JoinHandle<Result> },

    /// A simulated host
    Simulated { rt: Rt, software: Software<'a> },
}

impl<'a> Role<'a> {
    pub(crate) fn client(rt: Rt, handle: JoinHandle<Result>) -> Self {
        Self::Client { rt, handle }
    }

    pub(crate) fn simulated<F, Fut>(rt: Rt, software: F) -> Self
    where
        F: Fn() -> Fut + 'a,
        Fut: Future<Output = ()> + 'static,
    {
        let wrapped: Software = Box::new(move || Box::pin(software()));

        Self::Simulated {
            rt,
            software: wrapped,
        }
    }

    pub(crate) fn now(&self) -> Instant {
        let rt = match self {
            Role::Client { rt, .. } => rt,
            Role::Simulated { rt, .. } => rt,
        };

        rt.now()
    }

    pub(crate) fn tick(&self, duration: Duration) {
        let rt = match self {
            Role::Client { rt, .. } => rt,
            Role::Simulated { rt, .. } => rt,
        };

        rt.tick(duration)
    }
}
