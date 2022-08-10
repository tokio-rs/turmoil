use crate::*;

use tokio::runtime::Runtime;
use tokio::task::LocalSet;
use tokio::time::{Duration, Instant};

use std::any::Any;
use std::cell::Cell;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::rc::Rc;

/// A host in the simulated network.
pub(crate) enum Host {
    /// A simulated host may have its clock skewed, experience partitions, or
    /// become isolated.
    Simulated(Simulated),
    Client {
        /// Sends messages to the client
        inbox: inbox::Sender,

        /// Instant at which the client was created.
        epoch: Instant,
    },
}

/// A simulated host
pub(crate) struct Simulated {
    /// Address
    pub(crate) addr: SocketAddr,

    /// Handle to the Tokio runtime driving this host. Each runtime may have a
    /// different sense of "now" which simulates clock skew.
    pub(crate) rt: Runtime,

    /// Local task set, used for running !Send tasks.
    pub(crate) local: LocalSet,

    /// Send messages to the host's inbox
    pub(crate) inbox: inbox::Sender,

    /// Instant at which the host began
    pub(crate) epoch: Instant,

    /// Current host version
    pub(crate) version: Cell<u64>,
}

impl Host {
    /// Create a new simulated host in the simulated network
    pub(crate) fn new_simulated<M: Message>(
        addr: SocketAddr,
        inner: &Rc<super::Inner>,
    ) -> (Host, Io<M>) {
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
            addr,
            rt,
            local,
            inbox: tx,
            epoch,
            version: Cell::new(0),
        });
        let stream = Io {
            inner: Rc::downgrade(inner),
            addr,
            inbox: rx,
            _p: PhantomData,
        };

        (host, stream)
    }

    pub(crate) fn new_client<M: Message>(
        addr: SocketAddr,
        inner: &Rc<super::Inner>,
    ) -> (Host, Io<M>) {
        let (tx, rx) = inbox::channel();

        let host = Host::Client {
            inbox: tx,
            epoch: std::time::Instant::now().into(),
        };

        let stream = Io {
            inner: Rc::downgrade(&inner),
            addr,
            inbox: rx,
            _p: PhantomData,
        };

        (host, stream)
    }

    pub(crate) fn send(&self, src: version::Dot, delay: Duration, message: Box<dyn Any>) {
        match self {
            Host::Simulated(Simulated { inbox, .. }) => {
                let now = self.now();
                inbox.send(inbox::Envelope {
                    src,
                    deliver_at: now + delay,
                    message,
                });
            }
            Host::Client { inbox, epoch, .. } => {
                // Just send the message
                inbox.send(inbox::Envelope {
                    src,
                    deliver_at: *epoch,
                    message,
                });
            }
        }
    }

    pub(crate) fn tick(&self, config: &Config) {
        match self {
            Host::Simulated(Simulated {
                addr,
                rt,
                local,
                epoch,
                inbox,
                version,
                ..
            }) => rt.block_on(async {
                version::set_current(version::Dot {
                    host: *addr,
                    version: version.get(),
                }, epoch.elapsed());

                local
                    .run_until(async {
                        inbox.tick(Instant::now());

                        tokio::time::sleep(config.tick).await;

                        if epoch.elapsed() > config.duration {
                            panic!("Ran for {:?} without completing", config.duration);
                        }
                    })
                    .await;

                let dot = version::take_current();
                version.set(dot.version);
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
