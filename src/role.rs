use std::{pin::Pin, time::Duration};

use futures::Future;
use tokio::{task::JoinHandle, time::Instant};

use crate::{rt::Rt, Result};

// To support re-creation, we need to store a factory of the future that
// represents the software. This is somewhat annoying in that it requires
// boxxing to avoid generics.
type Software<'a> = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result>>> + 'a>;

/// Differentiates runtime fields for different host types
pub(crate) enum Role<'a> {
    /// A client handle
    Client { 
        rt: Rt, 
        /// When the client is finished the handle is None
        handle: Option<JoinHandle<Result>> 
    },

    /// A simulated host
    Simulated {
        rt: Rt,
        software: Software<'a>,
        /// When the host finished the handle is None
        handle: Option<JoinHandle<Result>>,
    },
}

impl<'a> Role<'a> {
    pub(crate) fn client(rt: Rt, handle: JoinHandle<Result>) -> Self {
        Self::Client { rt, handle: Some(handle) }
    }

    pub(crate) fn simulated<F, Fut>(rt: Rt, software: F, handle: JoinHandle<Result>) -> Self
    where
        F: Fn() -> Fut + 'a,
        Fut: Future<Output = Result> + 'static,
    {
        let wrapped: Software = Box::new(move || Box::pin(software()));

        Self::Simulated {
            rt,
            software: wrapped,
            handle: Some(handle),
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
