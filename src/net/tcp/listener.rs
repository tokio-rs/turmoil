use std::{
    io::{self, Result},
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
    /// If you bind to the 0.0.0.0, you're effectivly binding to the generated
    /// IP address of the host. Each host gets an IP from 192.168.0.0/24 subnet.
    /// 
    /// You can bind to `localhost`, which translates to 127.0.0.1. Binding
    /// directly 127.0.0.1 or ::1 is also possible. It allows for the TCP socket
    /// to be only visible within a host and reachable *only* via localhost
    /// IPv4/IPv6 addresses.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<TcpListener> {
        World::current(|world| {
            let mut addr = addr.to_socket_addr(&world.dns);
            let host = world.current_host_mut();

            // Unspecified -> host's IP
            if addr.ip().is_unspecified() {
                addr.set_ip(host.addr);
            } else if addr.ip() != host.addr && !addr.ip().is_loopback() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    addr.to_string(),
                ));
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

                tracing::trace!(target: TRACING_TARGET, dst = ?origin, src = ?self.local_addr, protocol = %"TCP SYN", "Recv");

                // Send SYN-ACK -> origin. If Ok we proceed (acts as the ACK),
                // else we return early to avoid host mutations.
                let ack = syn.ack.send(());
                tracing::trace!(target: TRACING_TARGET, src = ?self.local_addr, dst = ?origin, protocol = %"TCP SYN-ACK", "Send");

                if ack.is_err() {
                    return None;
                }

                let pair = SocketPair::new(self.local_addr, origin);
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
