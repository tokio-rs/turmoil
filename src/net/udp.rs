use bytes::Bytes;
use tokio::sync::{mpsc, Mutex};

use crate::{
    envelope::{Datagram, Protocol},
    ToSocketAddrs, World, TRACING_TARGET,
};

use std::{
    cmp,
    io::{self, Result},
    net::SocketAddr,
};

/// A simulated UDP socket.
///
/// All methods must be called from a host within a Turmoil simulation.
pub struct UdpSocket {
    local_addr: SocketAddr,
    rx: Mutex<Rx>,
}

struct Rx {
    recv: mpsc::Receiver<(Datagram, SocketAddr)>,
    /// A buffered received message.
    ///
    /// This is used to support the `readable` method, as [`mpsc::Receiver`]
    /// doesn't expose a way to query channel readiness.
    buffer: Option<(Datagram, SocketAddr)>,
}

impl Rx {
    /// Tries to receive from either the buffered message or the mpsc channel
    pub fn try_recv_from(&mut self, buf: &mut [u8]) -> Result<(usize, Datagram, SocketAddr)> {
        let (datagram, origin) = if let Some(datagram) = self.buffer.take() {
            datagram
        } else {
            self.recv.try_recv().map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "socket receive queue is empty")
            })?
        };

        let bytes = &datagram.0;
        let limit = cmp::min(buf.len(), bytes.len());

        buf[..limit].copy_from_slice(&bytes[..limit]);

        Ok((limit, datagram, origin))
    }

    /// Waits for the socket to become readable.
    ///
    /// This function is usually paired with `try_recv_from()`.
    ///
    /// The function may complete without the socket being readable. This is a
    /// false-positive and attempting a `try_recv_from()` will return with
    /// `io::ErrorKind::WouldBlock`.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. Once a readiness event occurs, the method
    /// will continue to return immediately until the readiness event is
    /// consumed by an attempt to read that fails with `WouldBlock` or
    /// `Poll::Pending`.
    pub async fn readable(&mut self) -> Result<()> {
        if self.buffer.is_some() {
            return Ok(());
        }

        let datagram = self
            .recv
            .recv()
            .await
            .expect("sender should never be dropped");

        self.buffer = Some(datagram);

        Ok(())
    }
}

impl UdpSocket {
    pub(crate) fn new(local_addr: SocketAddr, rx: mpsc::Receiver<(Datagram, SocketAddr)>) -> Self {
        Self {
            local_addr,
            rx: Mutex::new(Rx {
                recv: rx,
                buffer: None,
            }),
        }
    }

    /// Create a new simulated UDP socket and attempt to bind it to the `addr`
    /// provided.
    ///
    /// Only 0.0.0.0 is currently supported.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<UdpSocket> {
        World::current(|world| {
            let mut addr = addr.to_socket_addr(&world.dns);
            let host = world.current_host_mut();

            if !addr.ip().is_unspecified() {
                panic!("{addr} is not supported");
            }

            // Unspecified -> host's IP
            addr.set_ip(host.addr);

            host.udp.bind(addr)
        })
    }

    /// Sends data on the socket to the given address. On success, returns the
    /// number of bytes written.
    ///
    /// Address type can be any implementor of [`ToSocketAddrs`] trait. See its
    /// documentation for concrete examples.
    ///
    /// It is possible for `addr` to yield multiple addresses, but `send_to`
    /// will only send data to the first address yielded by `addr`.
    ///
    /// This will return an error when the IP version of the local socket does
    /// not match that returned from [`ToSocketAddrs`].
    ///
    /// [`ToSocketAddrs`]: crate::ToSocketAddrs
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. If `send_to` is used as the event in a
    /// [`tokio::select!`](crate::select) statement and some other branch
    /// completes first, then it is guaranteed that the message was not sent.
    pub async fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> Result<usize> {
        World::current(|world| {
            let dst = target.to_socket_addr(&world.dns);

            world.send_message(
                self.local_addr,
                dst,
                Protocol::Udp(Datagram(Bytes::copy_from_slice(buf))),
            );

            Ok(buf.len())
        })
    }

    /// Tries to send data on the socket to the given address, but if the send is
    /// blocked this will return right away.
    ///
    /// This function is usually paired with `writable()`.
    ///
    /// # Returns
    ///
    /// If successful, returns the number of bytes sent
    ///
    /// Users should ensure that when the remote cannot receive, the
    /// [`ErrorKind::WouldBlock`] is properly handled. An error can also occur
    /// if the IP version of the socket does not match that of `target`.
    ///
    /// [`ErrorKind::WouldBlock`]: std::io::ErrorKind::WouldBlock
    pub fn try_send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> Result<usize> {
        World::current(|world| {
            let dst = target.to_socket_addr(&world.dns);

            world.send_message(
                self.local_addr,
                dst,
                Protocol::Udp(Datagram(Bytes::copy_from_slice(buf))),
            );

            Ok(buf.len())
        })
    }

    /// Waits for the socket to become writable.
    ///
    /// This function is usually paired with `try_send_to()`.
    ///
    /// The function may complete without the socket being writable. This is a
    /// false-positive and attempting a `try_send_to()` will return with
    /// `io::ErrorKind::WouldBlock`.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. Once a readiness event occurs, the method
    /// will continue to return immediately until the readiness event is
    /// consumed by an attempt to write that fails with `WouldBlock` or
    /// `Poll::Pending`.
    pub async fn writable(&self) -> Result<()> {
        // UDP sockets currently don't have any backpressure mechanisms so the socket is always writable
        Ok(())
    }

    /// Receives a single datagram message on the socket. On success, returns
    /// the number of bytes read and the origin.
    ///
    /// The function must be called with valid byte array buf of sufficient size
    /// to hold the message bytes. If a message is too long to fit in the
    /// supplied buffer, excess bytes may be discarded.
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let mut rx = self.rx.lock().await;
        rx.readable().await?;

        let (limit, datagram, origin) = rx
            .try_recv_from(buf)
            .expect("queue should be ready after readable yields");

        tracing::trace!(target: TRACING_TARGET, dst = ?self.local_addr, src = ?origin, protocol = %datagram, "Recv");

        Ok((limit, origin))
    }

    /// Tries to receive a single datagram message on the socket. On success,
    /// returns the number of bytes read and the origin.
    ///
    /// The function must be called with valid byte array buf of sufficient size
    /// to hold the message bytes. If a message is too long to fit in the
    /// supplied buffer, excess bytes may be discarded.
    ///
    /// When there is no pending data, `Err(io::ErrorKind::WouldBlock)` is
    /// returned. This function is usually paired with `readable()`.
    pub fn try_recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let mut rx = self.rx.try_lock().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "socket is being read by another task",
            )
        })?;

        let (limit, datagram, origin) = rx.try_recv_from(buf).map_err(|_| {
            io::Error::new(io::ErrorKind::WouldBlock, "socket receive queue is empty")
        })?;

        tracing::trace!(target: TRACING_TARGET, dst = ?self.local_addr, src = ?origin, protocol = %datagram, "Recv");

        Ok((limit, origin))
    }

    /// Waits for the socket to become readable.
    ///
    /// This function is usually paired with `try_recv_from()`.
    ///
    /// The function may complete without the socket being readable. This is a
    /// false-positive and attempting a `try_recv_from()` will return with
    /// `io::ErrorKind::WouldBlock`.
    ///
    /// # Cancel safety
    ///
    /// This method is cancel safe. Once a readiness event occurs, the method
    /// will continue to return immediately until the readiness event is
    /// consumed by an attempt to read that fails with `WouldBlock` or
    /// `Poll::Pending`.
    pub async fn readable(&self) -> Result<()> {
        let mut rx = self.rx.lock().await;
        rx.readable().await?;
        Ok(())
    }

    /// Returns the local address that this socket is bound to.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use turmoil::net::UdpSocket;
    /// # use std::{io, net::SocketAddr};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> io::Result<()> {
    /// let addr = "0.0.0.0:8080".parse::<SocketAddr>().unwrap();
    /// let sock = UdpSocket::bind(addr).await?;
    /// // the address the socket is bound to
    /// let local_addr = sock.local_addr()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        World::current_if_set(|world| world.current_host_mut().udp.unbind(self.local_addr));
    }
}
