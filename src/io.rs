use crate::{message, Message, ToSocketAddr, World};

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

    /// Return the socket address associated with this `Io`
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
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

                world.recv(self.addr)
            });

            if let Some(envelope) = maybe_envelope {
                let message = message::downcast::<M>(envelope.message);
                return (message, envelope.src.host);
            }

            self.notify.notified().await;
        }
    }

    /// Receive a message from the specific host
    pub async fn recv_from(&self, peer: impl ToSocketAddr) -> M {
        let peer = self.lookup(peer);

        loop {
            let maybe_envelope = World::current(|world| {
                if let Some(current) = world.current {
                    assert_eq!(current, self.addr);
                }

                world.recv_from(self.addr, peer)
            });

            if let Some(envelope) = maybe_envelope {
                return message::downcast::<M>(envelope.message);
            }

            self.notify.notified().await;
        }
    }

    /// Lookup a socket address by host name.
    pub fn lookup(&self, addr: impl ToSocketAddr) -> SocketAddr {
        World::current(|world| world.lookup(addr))
    }
}

impl<M: Message> Clone for Io<M> {
    fn clone(&self) -> Io<M> {
        Io {
            addr: self.addr,
            notify: self.notify.clone(),
            _p: std::marker::PhantomData,
        }
    }
}
