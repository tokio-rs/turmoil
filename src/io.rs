use crate::*;

use std::fmt::Debug;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::rc;
use tokio::time::Instant;

/// Bi-directional stream for the node.
pub struct Io<M: Debug + 'static> {
    /// Handle to shared state
    pub(crate) inner: rc::Weak<super::Inner>,

    /// Socket address of the host owning the message stream
    pub addr: SocketAddr,

    /// Inbox receiver
    pub(crate) inbox: inbox::Receiver,

    /// Io message typing
    pub(crate) _p: PhantomData<M>,
}

impl<M: Debug + 'static> Io<M> {
    /// Send a message to a remote host
    pub fn send(&self, dst: impl dns::ToSocketAddr, message: M) {
        let inner = self.inner.upgrade().unwrap();

        let dst = inner.dns.lookup(dst);
        let hosts = inner.hosts.borrow();
        let mut topology = inner.topology.borrow_mut();
        let mut rand = inner.rand.borrow_mut();

        let client = hosts[&dst].is_client() || hosts[&self.addr].is_client();

        if let Some(delay) = topology.send_delay(&mut *rand, self.addr, dst, client) {
            hosts[&dst].send(self.addr, delay, Box::new(message));
        }
    }

    /// Receive a message
    pub async fn recv(&self) -> (M, SocketAddr) {
        let (msg, addr) = self.inbox.recv(|| self.now()).await;
        let typed = msg.downcast::<M>().unwrap();
        (*typed, addr)
    }

    /// Receive a message from a specific address
    pub async fn recv_from(&self, src: impl dns::ToSocketAddr) -> M {
        let inner = self.inner.upgrade().unwrap();
        let src = inner.dns.lookup(src);
        let msg = self.inbox.recv_from(src, || self.now()).await;
        *msg.downcast::<M>().unwrap()
    }

    pub fn lookup(&self, addr: impl crate::dns::ToSocketAddr) -> SocketAddr {
        let inner = self.inner.upgrade().unwrap();
        inner.dns.lookup(addr)
    }

    fn now(&self) -> Instant {
        let inner = self.inner.upgrade().unwrap();
        let hosts = inner.hosts.borrow();
        hosts[&self.addr].now()
    }
}

impl<M: Debug + 'static> Clone for Io<M> {
    fn clone(&self) -> Io<M> {
        Io {
            inner: self.inner.clone(),
            addr: self.addr,
            inbox: self.inbox.clone(),
            _p: PhantomData,
        }
    }
}
