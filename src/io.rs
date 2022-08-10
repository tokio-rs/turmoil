use crate::*;

use std::marker::PhantomData;
use std::net::SocketAddr;
use std::rc;
use tokio::time::Instant;

/// Bi-directional stream for the node.
pub struct Io<M: Message> {
    /// Handle to shared state
    pub(crate) inner: rc::Weak<super::Inner>,

    /// Socket address of the host owning the message stream
    pub addr: SocketAddr,

    /// Inbox receiver
    pub(crate) inbox: inbox::Receiver,

    /// Io message typing
    pub(crate) _p: PhantomData<M>,
}

impl<M: Message> Io<M> {
    /// Send a message to a remote host
    pub fn send(&self, dst: impl dns::ToSocketAddr, message: M) {
        let inner = self.inner.upgrade().unwrap();

        let dst = inner.dns.lookup(dst);
        let hosts = inner.hosts.borrow();
        let mut topology = inner.topology.borrow_mut();
        let mut rand = inner.rand.borrow_mut();

        let (src, _elapsed) = version::get_current();

        // Don't delay messages to or from clients.
        if hosts[&dst].is_client() || hosts[&self.addr].is_client() {
            hosts[&dst].send(src, Duration::default(), Box::new(message));
        } else if let Some(delay) = topology.send_delay(&mut *rand, self.addr, dst) {
            hosts[&dst].send(src, delay, Box::new(message));
        }
    }

    /// Receive a message
    pub async fn recv(&self) -> (M, SocketAddr) {
        let inbox::Envelope { message: value, src, .. } = self.inbox.recv(|| self.now()).await;
        version::inc_current();
        let typed = value.downcast::<M>().unwrap();
        (*typed, src.host)
    }

    /// Receive a message from a specific address
    pub async fn recv_from(&self, src: impl dns::ToSocketAddr) -> M {
        let inner = self.inner.upgrade().unwrap();
        let src = inner.dns.lookup(src);
        let inbox::Envelope { message: value, src, .. } = self.inbox.recv_from(src, || self.now()).await;
        version::inc_current();
        *value.downcast::<M>().unwrap()
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

impl<M: Message> Clone for Io<M> {
    fn clone(&self) -> Io<M> {
        Io {
            inner: self.inner.clone(),
            addr: self.addr,
            inbox: self.inbox.clone(),
            _p: PhantomData,
        }
    }
}
