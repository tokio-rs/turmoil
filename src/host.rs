use crate::*;

use tokio::runtime::Runtime;
use tokio::task::LocalSet;
use tokio::time::{Duration, Instant};

use std::fmt::Debug;
use std::net::SocketAddr;
use std::rc::Rc;

/// A host in the simulated network.
pub(crate) enum Host<T: Debug + 'static> {
    /// A simulated host may have its clock skewed, experience partitions, or
    /// become isolated.
    Simulated(Simulated<T>),
    Client {
        /// Sends messages to the client
        inbox: inbox::Sender<T>,

        /// Instant at which the client was created.
        epoch: Instant,
    },
}

/// A simulated host
pub(crate) struct Simulated<T: Debug> {
    /// Handle to the Tokio runtime driving this host. Each runtime may have a
    /// different sense of "now" which simulates clock skew.
    pub(crate) rt: Runtime,

    /// Local task set, used for running !Send tasks.
    pub(crate) local: LocalSet,

    /// Send messages to the host's inbox
    pub(crate) inbox: inbox::Sender<T>,

    /// Instant at which the host began
    pub(crate) epoch: Instant,
}

impl<T: Debug + 'static> Host<T> {
    /// Create a new simulated host in the simulated network
    pub(crate) fn new_simulated(
        addr: SocketAddr,
        dns: Dns,
        inner: &Rc<super::Inner<T>>,
    ) -> (Host<T>, Io<T>) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .unhandled_panic(tokio::runtime::UnhandledPanic::ShutdownRuntime)
            .build()
            .unwrap();

        let epoch = rt.block_on(async {
            // Sleep to "round" `Instant::now()` to the closes `ms`
            tokio::time::sleep(Duration::from_millis(1)).await;
            Instant::now()
        });

        let mut local = LocalSet::new();
        local.unhandled_panic(tokio::runtime::UnhandledPanic::ShutdownRuntime);

        let (tx, rx) = inbox::channel();

        let host = Host::Simulated(Simulated {
            rt,
            local,
            inbox: tx,
            epoch,
        });
        let stream = Io {
            inner: Rc::downgrade(inner),
            addr,
            inbox: rx,
            dns,
        };

        (host, stream)
    }

    pub(crate) fn new_client(
        addr: SocketAddr,
        dns: Dns,
        inner: &Rc<super::Inner<T>>,
    ) -> (Host<T>, Io<T>) {
        let (tx, rx) = inbox::channel();

        let host = Host::Client {
            inbox: tx,
            epoch: std::time::Instant::now().into(),
        };

        let stream = Io {
            inner: Rc::downgrade(&inner),
            addr,
            inbox: rx,
            dns,
        };

        (host, stream)
    }

    pub(crate) fn send(&self, self_addr: SocketAddr, delay: Duration, message: T) {
        match self {
            Host::Simulated(Simulated { inbox, .. }) => {
                let now = self.now();
                inbox.send(self_addr, now + delay, message);
            }
            Host::Client { inbox, epoch, .. } => {
                // Just send the message
                inbox.send(self_addr, *epoch, message);
            }
        }
    }

    pub(crate) fn tick(&self, config: &Config) {
        match self {
            Host::Simulated(Simulated {
                rt,
                local,
                epoch,
                inbox,
                ..
            }) => rt.block_on(async {
                local
                    .run_until(async {
                        tokio::time::sleep(config.tick).await;

                        if epoch.elapsed() > config.duration {
                            panic!("Ran for {:?} without completing", config.duration);
                        }

                        inbox.tick(Instant::now());
                    })
                    .await;
            }),
            _ => {}
        }
    }

    pub(crate) fn now(&self) -> Instant {
        match self {
            Host::Simulated(Simulated { rt, .. }) => {
                let _enter = rt.enter();
                Instant::now()
            }
            Host::Client { epoch, .. } => *epoch,
        }
    }

    pub(crate) fn is_client(&self) -> bool {
        match self {
            Host::Client { .. } => true,
            _ => false,
        }
    }
}
