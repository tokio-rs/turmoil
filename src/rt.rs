use std::mem;

use tokio::runtime::Runtime;
use tokio::task::LocalSet;
use tokio::time::{sleep, Duration, Instant};

/// Per host simulated runtime
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

    pub(crate) fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        self.tokio
            .block_on(async { self.local.run_until(async { f() }).await })
    }

    pub(crate) fn now(&self) -> Instant {
        let _guard = self.tokio.enter();
        Instant::now()
    }

    pub(crate) fn tick(&self, duration: Duration) -> Instant {
        self.tokio.block_on(async {
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
