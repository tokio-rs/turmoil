use std::{
    io::{Error, ErrorKind, Result},
    net::SocketAddr,
    task::{Context, Poll},
};

use futures::future::poll_fn;

use crate::{kernel::Fd, net::TcpStream, world::World, ToSocketAddrs};

/// A simulated TCP socket server, listening for connections.
///
/// All methods must be called from a host within a Turmoil simulation.
pub struct TcpListener {
    fd: Fd,
}

impl TcpListener {
    /// Creates a new TcpListener, which will be bound to the specified address.
    ///
    /// The returned listener is ready for accepting connections.
    ///
    /// Binding with a port number of 0 will request that the OS assigns a port
    /// to this listener. The port allocated can be queried via the `local_addr`
    /// method.
    ///
    /// Only `0.0.0.0`, `::`, or localhost are currently supported.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> std::io::Result<TcpListener> {
        World::current(|world| {
            let mut addr = addr.to_socket_addr(&world.dns);
            let host = world.current_host_mut();

            if !addr.ip().is_unspecified() && !addr.ip().is_loopback() {
                return Err(Error::new(
                    ErrorKind::AddrNotAvailable,
                    format!("{addr} is not supported"),
                ));
            }

            if addr.is_ipv4() != host.addr.is_ipv4() {
                panic!("ip version mismatch: {:?} host: {:?}", addr, host.addr)
            }

            if addr.port() == 0 {
                addr.set_port(host.assign_ephemeral_port());
            }

            Ok(TcpListener {
                fd: host.tcp.bind(addr)?,
            })
        })
    }

    /// Polls to accept a new incoming connection to this listener.
    ///
    /// If there is no connection to accept, `Poll::Pending` is returned and the
    /// current task will be notified by a waker.  Note that on multiple calls
    /// to `poll_accept`, only the `Waker` from the `Context` passed to the most
    /// recent call is scheduled to receive a wakeup.
    pub fn poll_accept(
        &self,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(TcpStream, SocketAddr)>> {
        World::current(|world| {
            let host = world.current_host_mut();

            match host.tcp.accept(self.fd, host.addr, cx.waker()) {
                Some((fd, remote)) => Poll::Ready(Ok((TcpStream { fd }, remote))),
                None => Poll::Pending,
            }
        })
    }

    /// Accepts a new incoming connection from this listener.
    ///
    /// This function will yield once a new TCP connection is established. When
    /// established, the corresponding [`TcpStream`] and the remote peer’s
    /// address will be returned.
    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        poll_fn(|cx| self.poll_accept(cx)).await
    }

    /// Returns the local address that this listener is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        todo!()
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        World::current_if_set(|world| world.current_host_mut().tcp.close(self.fd))
    }
}
