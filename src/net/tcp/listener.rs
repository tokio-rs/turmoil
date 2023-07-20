use std::{
    io::{Error, ErrorKind, Result},
    net::SocketAddr,
    sync::Arc,
};

use tokio::sync::Notify;

use crate::{
    net::{SocketPair, TcpStream},
    world::World,
    ToSocketAddrs, TRACING_TARGET,
};

/// A simulated TCP socket server, listening for connections.
///
/// All methods must be called from a host within a Turmoil simulation.
pub struct TcpListener {
    local_addr: SocketAddr,
    notify: Arc<Notify>,
}

impl TcpListener {
    pub(crate) fn new(local_addr: SocketAddr, notify: Arc<Notify>) -> Self {
        Self { local_addr, notify }
    }

    /// Creates a new TcpListener, which will be bound to the specified address.
    ///
    /// The returned listener is ready for accepting connections.
    ///
    /// Binding with a port number of 0 will request that the OS assigns a port
    /// to this listener. The port allocated can be queried via the `local_addr`
    /// method.
    ///
    /// Only `0.0.0.0`, `::`, or localhost are currently supported.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<TcpListener> {
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

            host.tcp.bind(addr)
        })
    }

    /// Accepts a new incoming connection from this listener.
    ///
    /// This function will yield once a new TCP connection is established. When
    /// established, the corresponding [`TcpStream`] and the remote peer’s
    /// address will be returned.
    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        loop {
            let maybe_accept = World::current(|world| {
                let host = world.current_host_mut();
                let (syn, origin) = host.tcp.accept(self.local_addr)?;

                tracing::trace!(target: TRACING_TARGET, src = ?origin, dst = ?self.local_addr, protocol = %"TCP SYN", "Recv");

                // Send SYN-ACK -> origin. If Ok we proceed (acts as the ACK),
                // else we return early to avoid host mutations.
                let ack = syn.ack.send(());
                tracing::trace!(target: TRACING_TARGET, src = ?self.local_addr, dst = ?origin, protocol = %"TCP SYN-ACK", "Send");

                if ack.is_err() {
                    return None;
                }

                let mut my_addr = self.local_addr;
                if origin.ip().is_loopback() {
                    my_addr.set_ip(origin.ip());
                }
                if my_addr.ip().is_unspecified() {
                    my_addr.set_ip(host.addr);
                }

                let pair = SocketPair::new(my_addr, origin);
                let rx = host.tcp.new_stream(pair);

                Some((TcpStream::new(pair, rx), origin))
            });

            if let Some(accepted) = maybe_accept {
                return Ok(accepted);
            }

            self.notify.notified().await;
        }
    }

    /// Returns the local address that this listener is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        World::current_if_set(|world| world.current_host_mut().tcp.unbind(self.local_addr));
    }
}
