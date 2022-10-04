use std::mem;

use futures::Future;
use tokio::runtime::Runtime;
use tokio::task::LocalSet;
use tokio::time::{sleep, Duration, Instant};

/// Per host simulated runtime
///
/// The tokio runtime is paused (see [`Builder::start_paused`]), which gives us
/// control over when and how to advance time. In particular, see [`Rt::tick`],
/// which lets the runtime do a bit more work.
pub(crate) struct Rt {
    /// Handle to the Tokio runtime driving this simulated host. Each runtime
    /// may have a different sense of "now" which simulates clock skew.
    tokio: Runtime,

    /// Local task set used for running !Send tasks.
    local: LocalSet,
}

impl Rt {
    pub(crate) fn new() -> Rt {
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .unhandled_panic(tokio::runtime::UnhandledPanic::ShutdownRuntime)
            .build()
            .unwrap();

        tokio.block_on(async {
            // Sleep to "round" `Instant::now()` to the closest `ms`
            tokio::time::sleep(Duration::from_millis(1)).await;
        });

        Rt {
            tokio,
            local: new_local(),
        }
    }

    pub(crate) fn block_on<R>(&self, f: impl Future<Output = R>) -> R {
        self.tokio.block_on(f)
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        self.block_on(async { self.local.run_until(async { f() }).await })
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
    // If the host is running blocking code (computing fib), then it is possible
    // that step 3 exceeds the intended duration. For this reason, we return the
    // finish time and store that in [`crate::Host::now`].
    pub(crate) fn tick(&self, duration: Duration) -> Instant {
        self.block_on(async {
            self.local
                .run_until(async {
                    sleep(duration).await;

                    Instant::now()
                })
                .await
        })
    }

    pub(crate) fn cancel_tasks(&mut self) {
        _ = mem::replace(&mut self.local, new_local());
    }
}

fn new_local() -> LocalSet {
    let mut local = LocalSet::new();
    local.unhandled_panic(tokio::runtime::UnhandledPanic::ShutdownRuntime);
    local
}
