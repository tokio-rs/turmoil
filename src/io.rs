use crate::{Message, ToSocketAddr, World};

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;

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
                if let Some(current) = world.current {
                    assert_eq!(current, self.addr);
                }

                let host = world.host_mut(self.addr);

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
                if let Some(current) = world.current {
                    assert_eq!(current, self.addr);
                }

                let host = world.host_mut(self.addr);

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
