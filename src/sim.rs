use crate::{Message, ToSocketAddr, Rt, World};

use indexmap::IndexMap;
use std::cell::RefCell;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

/// Network simulation
pub struct Sim {
    /// Tracks the simulated world state.
    /// 
    /// This is what is stored in the thread-local
    world: RefCell<World>,

    /// Per simulated host Tokio runtimes
    /// 
    /// When `None`, this signifies the host is a "client"
    rts: IndexMap<SocketAddr, Option<Rt>>,
}

impl Sim {
    pub(crate) fn new(world: World) -> Sim {
        Sim {
            world: RefCell::new(world),
            rts: IndexMap::new(),
        }
    }

    /// Register a host with the simulation
    pub fn register<F, M, R>(&mut self, addr: impl ToSocketAddr, host: F)
    where
        F: FnOnce(Io<M>) -> R,
        M: Message,
        R: Future<Output = ()> + 'static,
    {
        let world = RefCell::get_mut(&mut self.world);
        let addr = world.lookup(addr);

        let rt = Rt::new();
        let epoch = rt.now();
        let notify = Arc::new(Notify::new());

        // Register host state with the world
        world.register(addr, epoch, notify.clone());

        let io = Io::new(addr, notify);

        rt.with(|| {
            tokio::task::spawn_local(host(io));
        });

        self.rts.insert(addr, Some(rt));
    }

    /// Lookup a socket address by host name
    pub fn lookup(&self, addr: impl ToSocketAddr) -> SocketAddr {
        self.world.borrow_mut().lookup(addr)
    }

    /// Create a client handle
    pub fn client<M: Message>(&mut self, addr: impl ToSocketAddr) -> Io<M> {
        let world = RefCell::get_mut(&mut self.world);
        let addr = world.lookup(addr);
        let notify = Arc::new(Notify::new());

        world.register(addr, Instant::now(), notify.clone());
        self.rts.insert(addr, None);

        Io::new(addr, notify)
    }

    pub fn run_until<R>(&mut self, until: impl Future<Output = R>) -> R {
        use std::task::Poll;

        let mut task = tokio_test::task::spawn(until);
        let mut elapsed = Duration::default();

        // TODO: use config value for tick
        let tick = Duration::from_millis(1);

        loop {
            if let Poll::Ready(ret) = task.poll() {
                return ret;
            }

            for (addr, maybe_rt) in self.rts.iter() {
                if let Some(rt) = maybe_rt {
                    let now = rt.tick(tick);

                    let world = self.world.borrow_mut();
                    world.tick(addr, Some(now));
                } else {
                    let world = self.world.borrow_mut();
                    world.tick(addr, None);
                }
            }

            // TODO: use config value for tick
            elapsed += tick;
        }
    }
}

// ===== TODO: Move this to io.rs =====

pub struct Io<M: Message> {
    /// The address also serves as the identifier to access host state from the
    /// world.
    addr: SocketAddr,

    /// Signaled when a new message becomes ready to consume.
    notify: Arc<Notify>,

    _p: std::marker::PhantomData<M>,
}

impl<M: Message> Io<M> {
    pub(crate) fn new(addr: SocketAddr, notify: Arc<Notify>) -> Io<M> {
        Io {
            addr,
            notify,
            _p: std::marker::PhantomData,
        }
    }

    pub fn send(&self, dst: impl ToSocketAddr, message: M) {
        World::current(|world| {
            let dst = world.lookup(dst);

            world.send(self.addr, dst, Box::new(message));
        });
    }

    pub async fn recv(&self) -> (M, SocketAddr) {
        loop {
            let maybe_envelope = World::current(|world| {
                let host = world.current_mut();

                assert_eq!(self.addr, host.addr);

                host.recv()
            });

            if let Some(envelope) = maybe_envelope {
                let message = *envelope.message.downcast::<M>().unwrap();
                return (message, envelope.src.host);
            }

            self.notify.notified().await;
        }
    }

    /// Receive a message from the specific host
    pub async fn recv_from(&self, src: impl ToSocketAddr) -> M {
        let src = self.lookup(src);

        loop {
            let maybe_envelope = World::current(|world| {
                let host = world.current_mut();

                assert_eq!(self.addr, host.addr);

                host.recv_from(src)
            });

            if let Some(envelope) = maybe_envelope {
                return *envelope.message.downcast::<M>().unwrap();
            }

            self.notify.notified().await;
        }
    }

    /// Lookup a socket address by host name.
    pub fn lookup(&self, addr: impl ToSocketAddr) -> SocketAddr {
        World::current(|world| world.lookup(addr))
    }
}