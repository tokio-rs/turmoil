use bytes::{Buf, BufMut, Bytes};
use tokio::sync::mpsc;

use crate::{
    envelope::{Datagram, Protocol},
    trace, ToSocketAddr, World,
};

use std::{cell::RefCell, cmp, io::Result, net::SocketAddr};

/// A simulated UDP socket.
pub struct UdpSocket {
    local_addr: SocketAddr,
    rx: RefCell<mpsc::UnboundedReceiver<(Datagram, SocketAddr)>>,
}

impl UdpSocket {
    pub(crate) fn new(
        local_addr: SocketAddr,
        rx: mpsc::UnboundedReceiver<(Datagram, SocketAddr)>,
    ) -> Self {
        Self {
            local_addr,
            rx: RefCell::new(rx),
        }
    }

    /// Create a new simulated UDP socket and attempt to bind it to the `addr`
    /// provided.
    ///
    /// Only 0.0.0.0 is currently supported.
    ///
    /// Must be called from a host within a turmoil simulation.
    pub async fn bind<A: ToSocketAddr>(addr: A) -> Result<UdpSocket> {
        World::current(|world| {
            let host = world.current_host_mut();

            let mut addr = addr.to_socket_addr();
            if !addr.ip().is_unspecified() {
                panic!("{} is not supported", addr);
            }

            // Unspecified -> host's IP
            addr.set_ip(host.addr);

            let ret = host.udp.bind(addr);

            if ret.is_ok() {
                trace!("Bind {} UDP", addr);
            }

            ret
        })
    }

    /// Sends data on the socket to the given address. On success, returns the
    /// number of bytes written.
    pub async fn send_to<A: ToSocketAddr>(&self, buf: &[u8], target: A) -> Result<usize> {
        World::current(|world| {
            let dst = target.to_socket_addr();

            world.send_message(
                self.local_addr,
                dst,
                Protocol::Udp(Datagram(Bytes::copy_from_slice(buf))),
            );

            Ok(buf.len())
        })
    }

    /// Receives a single datagram message on the socket. On success, returns
    /// the number of bytes read and the origin.
    ///
    /// The function must be called with valid byte array buf of sufficient size
    /// to hold the message bytes. If a message is too long to fit in the
    /// supplied buffer, excess bytes may be discarded.
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let (datagram, origin) = self.rx.borrow_mut().recv().await.unwrap();

        // Grab the trace before we mutate things
        let trace = format!("Recv {} {} {}", self.local_addr, origin, datagram);

        let bytes = datagram.0;
        let limit = cmp::min(buf.len(), bytes.len());

        buf.as_mut().put(bytes.take(limit));

        trace!("{}", trace);

        Ok((limit, origin))
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        World::current_if_set(|world| {
            world.current_host_mut().udp.unbind(self.local_addr);

            trace!("Unbind {} UDP", self.local_addr)
        });
    }
}
