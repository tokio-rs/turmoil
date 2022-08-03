use crate::*;

use std::net::SocketAddr;
use std::rc;
use tokio::time::Instant;

/// Bi-directional stream for the node.
pub struct Io<T: 'static> {
    /// Handle to shared state
    pub(crate) inner: rc::Weak<super::Inner<T>>,

    /// Socket address of the host owning the message stream
    pub addr: SocketAddr,

    /// Inbox receiver
    pub(crate) inbox: inbox::Receiver<T>,

    /// Handle to the hostname lookup
    pub(crate) dns: Dns,
}

impl<T: 'static> Io<T> {
    /// Send a message to a remote host
    pub fn send(&self, dst: impl dns::ToSocketAddr, message: T) {
        let inner = self.inner.upgrade().unwrap();

        let dst = self.dns.lookup(dst);
        let hosts = inner.hosts.borrow();
        let mut topology = inner.topology.borrow_mut();
        let mut rand = inner.rand.borrow_mut();

        let client = hosts[&dst].is_client() || hosts[&self.addr].is_client();

        if let Some(delay) = topology.send_delay(&mut *rand, self.addr, dst, client) {
            hosts[&dst].send(self.addr, delay, message);
        }
    }

    /// Receive a message
    pub async fn recv(&self) -> (T, SocketAddr) {
        self.inbox.recv(|| self.now()).await
    }

    /// Receive a message from a specific address
    pub async fn recv_from(&self, src: impl dns::ToSocketAddr) -> T {
        let src = self.dns.lookup(src);
        self.inbox.recv_from(src, || self.now()).await
    }

    pub fn lookup(&self, addr: impl crate::dns::ToSocketAddr) -> SocketAddr {
        self.dns.lookup(addr)
    }

    fn now(&self) -> Instant {
        let inner = self.inner.upgrade().unwrap();
        let hosts = inner.hosts.borrow();
        hosts[&self.addr].now()
    }
}

impl<T: 'static> Clone for Io<T> {
    fn clone(&self) -> Io<T> {
        Io {
            inner: self.inner.clone(),
            addr: self.addr,
            inbox: self.inbox.clone(),
            dns: self.dns.clone(),
        }
    }
}
