use std::{pin::Pin, time::Duration};

use futures::Future;
use tokio::{task::JoinHandle, time::Instant};

use crate::{rt::Rt, Result};

/// To support re-creation, we need to store a factory of the future that
/// represents the software. This is somewhat annoying in that it requires
/// boxxing to avoid generics.
type Software<'a> = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result>>> + 'a>;

/// Differentiates runtime fields for different host types
enum RoleType<'a> {
    /// A client handle
    Client,

    /// A simulated host
    Simulated { software: Software<'a> },
}

/// Encapsulates state and type of the host.
pub(crate) struct Role<'a> {
    role: RoleType<'a>,
    /// Dedicate runtime to run host's task
    rt: Rt,
    /// The handle is consumed as soon as task finishes or host crashes,
    /// replaced with None to indicate finished task.
    handle: Option<JoinHandle<Result>>,
}

impl<'a> Role<'a> {
    pub(crate) fn client(rt: Rt, handle: JoinHandle<Result>) -> Self {
        Self {
            role: RoleType::Client,
            rt,
            handle: Some(handle),
        }
    }

    pub(crate) fn simulated<F, Fut>(rt: Rt, software: F, handle: JoinHandle<Result>) -> Self
    where
        F: Fn() -> Fut + 'a,
        Fut: Future<Output = Result> + 'static,
    {
        let wrapped: Software = Box::new(move || Box::pin(software()));

        Self {
            role: RoleType::Simulated { software: wrapped },
            rt,
            handle: Some(handle),
        }
    }

    pub(crate) fn is_client(&self) -> bool {
        matches!(self.role, RoleType::Client)
    }

    /// Runtime's current time
    pub(crate) fn now(&self) -> Instant {
        self.rt.now()
    }

    /// Ticks host's runtime.
    ///
    /// Returns whatever the host's software has completed after the tick.
    /// Returns an error if the host's software has completed in error.
    pub(crate) fn tick(&mut self, duration: Duration) -> Result<bool> {
        self.rt.tick(duration);

        match &self.handle {
            Some(handle) if handle.is_finished() => {
                // Consume handle to extract task result
                if let Some(h) = self.handle.take() {
                    match self.rt.block_on(h) {
                        // If the host was crashed the JoinError is cancelled, which
                        // needs to be handled to not fail the simulation.
                        Err(je) if je.is_cancelled() => {}
                        res => res??,
                    }
                };
                Ok(true)
            }
            Some(_) => Ok(false),
            None => Ok(true),
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.handle.is_none()
    }

    pub(crate) fn is_running(&self) -> bool {
        !self.is_finished()
    }

    /// Crashes the host. Nothing will be running on the host after this call.
    /// You can use [`Role::bounce`] to start the hosts up again.
    pub(crate) fn crash(&mut self) {
        match self.role {
            RoleType::Client => panic!("can only bounce hosts, not clients"),
            RoleType::Simulated { .. } => {
                if self.handle.take().is_some() {
                    self.rt.cancel_tasks();
                };
            }
        }
    }

    /// Bounces the host. The software is restarted.
    pub(crate) fn bounce(&mut self) {
        match &self.role {
            RoleType::Client => panic!("can only bounce hosts, not clients"),
            RoleType::Simulated { software } => {
                self.rt.cancel_tasks();

                let h = self.rt.with(|| tokio::task::spawn_local(software()));
                self.handle.replace(h);
            }
        }
    }
}
