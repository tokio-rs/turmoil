use std::{cell::RefCell, io::Result, net::SocketAddr};

use tokio::sync::mpsc;

use crate::{envelope::Syn, net::SocketPair, trace, world::World, ToSocketAddr};

use super::TcpStream;

/// A simulated TCP socket server, listening for connections.
///
/// All methods must be called from a host within a Turmoil simulation.
pub struct TcpListener {
    local_addr: SocketAddr,
    receiver: RefCell<mpsc::UnboundedReceiver<(Syn, SocketAddr)>>,
}

impl TcpListener {
    pub(crate) fn new(
        local_addr: SocketAddr,
        receiver: mpsc::UnboundedReceiver<(Syn, SocketAddr)>,
    ) -> Self {
        Self {
            local_addr,
            receiver: RefCell::new(receiver),
        }
    }

    /// Creates a new TcpListener, which will be bound to the specified address.
    ///
    /// The returned listener is ready for accepting connections.
    ///
    /// Only 0.0.0.0 is currently supported.
    pub async fn bind<A: ToSocketAddr>(addr: A) -> Result<TcpListener> {
        World::current(|world| {
            let mut addr = addr.to_socket_addr(&world.dns);
            let host = world.current_host_mut();

            if !addr.ip().is_unspecified() {
                panic!("{} is not supported", addr);
            }

            // Unspecified -> host's IP
            addr.set_ip(host.addr);

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
            let (syn, origin) = self.receiver.borrow_mut().recv().await.unwrap();
            trace!(dst = ?origin, src = ?self.local_addr, protocol = %"TCP SYN", "Recv");

            let maybe = World::current(|world| {
                let host = world.current_host_mut();

                // Send SYN-ACK -> origin. If Ok we proceed (acts as the ACK),
                // else we return early to avoid host mutations.
                let ack = syn.ack.send(());
                trace!(src = ?self.local_addr, dst = ?origin, protocol = %"TCP SYN-ACK", "Send");

                if ack.is_err() {
                    return None;
                }

                let pair = SocketPair::new(self.local_addr, origin);
                let rx = host.tcp.new_stream(pair);

                Some((TcpStream::new(pair, rx), origin))
            });

            if let Some(accepted) = maybe {
                return Ok(accepted);
            }
        }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        World::current_if_set(|world| world.current_host_mut().tcp.unbind(self.local_addr));
    }
}
