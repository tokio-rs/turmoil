use std::mem;
use std::sync::Arc;

use super::Result;
use futures::Future;
use std::pin::Pin;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio::task::LocalSet;
use tokio::time::{sleep, Duration, Instant};

// To support re-creation, we need to store a factory of the future that
// represents the software. This is somewhat annoying in that it requires
// boxxing to avoid generics.
type Software<'a> = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result>>> + 'a>;

/// Runtime kinds.
enum Kind<'a> {
    /// A runtime for executing test code.
    Client,

    /// A runtime that is running simulated host software.
    Host { software: Software<'a> },

    /// A runtime without any software. The network topology uses this for
    /// time tracking and message delivery.
    NoSoftware,
}

/// Per host simulated runtime.
///
/// The tokio runtime is paused (see [`Builder::start_paused`]), which gives us
/// control over when and how to advance time. In particular, see [`Rt::tick`],
/// which lets the runtime do a bit more work.
pub(crate) struct Rt<'a> {
    kind: Kind<'a>,

    /// Handle to the Tokio runtime driving this simulated host. Each runtime
    /// may have a different sense of "now" which simulates clock skew.
    tokio: Runtime,

    /// Local task set used for running !Send tasks.
    local: LocalSet,

    /// A user readable name to identify the node.
    pub(crate) nodename: Arc<str>,

    /// Optional handle to a host's software. When software finishes, the handle is
    /// consumed to check for error, which is propagated up to fail the simulation.
    handle: Option<JoinHandle<Result>>,
}

impl<'a> Rt<'a> {
    pub(crate) fn client<F>(nodename: Arc<str>, client: F) -> Self
    where
        F: Future<Output = Result> + 'static,
    {
        let (tokio, local) = init();

        let handle = with(&tokio, &local, || tokio::task::spawn_local(client));

        Self {
            kind: Kind::Client,
            tokio,
            local,
            nodename,
            handle: Some(handle),
        }
    }

    pub(crate) fn host<F, Fut>(nodename: Arc<str>, software: F) -> Self
    where
        F: Fn() -> Fut + 'a,
        Fut: Future<Output = Result> + 'static,
    {
        let (tokio, local) = init();

        let software: Software = Box::new(move || Box::pin(software()));
        let handle = with(&tokio, &local, || tokio::task::spawn_local(software()));

        Self {
            kind: Kind::Host { software },
            tokio,
            local,
            nodename,
            handle: Some(handle),
        }
    }

    pub(crate) fn no_software() -> Self {
        let (tokio, local) = init();

        Self {
            kind: Kind::NoSoftware,
            tokio,
            local,
            nodename: String::new().into(),
            handle: None,
        }
    }

    pub(crate) fn is_client(&self) -> bool {
        matches!(self.kind, Kind::Client)
    }

    fn is_host(&self) -> bool {
        matches!(self.kind, Kind::Host { .. })
    }

    pub(crate) fn is_software_running(&self) -> bool {
        self.handle.is_some()
    }

    pub(crate) fn now(&self) -> Instant {
        let _guard = self.tokio.enter();
        Instant::now()
    }

    // This method is called by [`Sim::run`], which iterates through all the
    // runtimes and ticks each one. The magic of this method is described in the
    // documentation for [`LocalSet::run_until`], but it may not be entirely
    // obvious how things fit together.
    //
    // A [`LocalSet`] tracks the tasks to run, which may in turn spawn more
    // tasks. `run_until` drives a top level task to completion, but not its
    // children. If you look below, you may be confused. The task we run here
    // just sleeps and has no children! However, it's the _same `LocalSet`_ that
    // is used to run software on the host.
    //
    // In this way, every time `tick` is called, the following unfolds:
    //
    // 1. Time advances on the runtime
    // 2. We schedule a new task that simply sleeps
    // 3. Other tasks on the `LocalSet` get a chance to run
    // 4. The sleep finishes
    // 5. The runtime pauses
    //
    // Returns whether the software has finished successfully or the error
    // that caused failure. Subsequent calls do not return the error as it is
    // expected to fail the simulation.
    pub(crate) fn tick(&mut self, duration: Duration) -> Result<bool> {
        self.tokio.block_on(async {
            self.local
                .run_until(async {
                    sleep(duration).await;
                })
                .await
        });

        // pull for software completion
        match &self.handle {
            Some(handle) if handle.is_finished() => {
                // Consume handle to extract task result
                if let Some(h) = self.handle.take() {
                    match self.tokio.block_on(h) {
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

    pub(crate) fn crash(&mut self) {
        if !self.is_host() {
            panic!("can only crash host's software");
        }

        if self.handle.take().is_some() {
            self.cancel_tasks();
        };
    }

    pub(crate) fn bounce(&mut self) {
        if !self.is_host() {
            panic!("can only bounce host's software");
        }

        self.cancel_tasks();

        if let Kind::Host { software } = &self.kind {
            let handle = with(&self.tokio, &self.local, || {
                tokio::task::spawn_local(software())
            });
            self.handle.replace(handle);
        };
    }

    /// Cancel all tasks within the [`Rt`] by dropping the current tokio
    /// [`Runtime`].
    ///
    /// Dropping the runtime blocks the calling thread until all futures have
    /// completed, which is desired here to ensure host software completes and
    /// all resources are dropped.
    ///
    /// Both the [`Runtime`] and [`LocalSet`] are replaced with new instances.
    fn cancel_tasks(&mut self) {
        let (tokio, local) = init();

        _ = mem::replace(&mut self.tokio, tokio);
        drop(mem::replace(&mut self.local, local));
    }
}

fn init() -> (Runtime, LocalSet) {
    let mut builder = tokio::runtime::Builder::new_current_thread();

    #[cfg(tokio_unstable)]
    builder.unhandled_panic(tokio::runtime::UnhandledPanic::ShutdownRuntime);

    let tokio = builder.enable_time().start_paused(true).build().unwrap();

    tokio.block_on(async {
        // Sleep to "round" `Instant::now()` to the closest `ms`
        tokio::time::sleep(Duration::from_millis(1)).await;
    });

    (tokio, new_local())
}

fn new_local() -> LocalSet {
    let mut local = LocalSet::new();

    #[cfg(tokio_unstable)]
    local.unhandled_panic(tokio::runtime::UnhandledPanic::ShutdownRuntime);

    local
}

fn with<R>(tokio: &Runtime, local: &LocalSet, f: impl FnOnce() -> R) -> R {
    tokio.block_on(async { local.run_until(async { f() }).await })
}
