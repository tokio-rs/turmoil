use std::{
    future::poll_fn,
    io::Result,
    net::SocketAddr,
    task::{ready, Context, Poll},
};

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
}

impl TcpListener {
    pub(crate) fn new(local_addr: SocketAddr) -> Self {
        Self { local_addr }
    }

    /// Creates a new TcpListener, which will be bound to the specified address.
    ///
    /// The returned listener is ready for accepting connections.
    ///
    /// Only 0.0.0.0 is currently supported.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<TcpListener> {
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
        poll_fn(|cx| self.poll_accept(cx)).await
    }

    /// Returns the local address that this listener is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }

    /// Polls to accept a new incoming connection to this listener.
    ///
    /// If there is no connection to accept, `Poll::Pending` is returned and the
    /// current task will be notified by a waker.  Note that on multiple calls
    /// to `poll_accept`, only the `Waker` from the `Context` passed to the most
    /// recent call is scheduled to receive a wakeup.
    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<Result<(TcpStream, SocketAddr)>> {
        World::current(|world| {
            let host = world.current_host_mut();
            let (syn, origin) = ready!(host.tcp.poll_accept(self.local_addr, cx));

            tracing::trace!(target: TRACING_TARGET, dst = ?origin, src = ?self.local_addr, protocol = %"TCP SYN", "Recv");

            // Send SYN-ACK -> origin. If Ok we proceed (acts as the ACK),
            // else we return early to avoid host mutations.
            let ack = syn.ack.send(());
            tracing::trace!(target: TRACING_TARGET, src = ?self.local_addr, dst = ?origin, protocol = %"TCP SYN-ACK", "Send");

            if ack.is_err() {
                return Poll::Pending;
            }

            let pair = SocketPair::new(self.local_addr, origin);
            let rx = host.tcp.new_stream(pair);

            Poll::Ready(Ok((TcpStream::new(pair, rx), origin)))
        })
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        World::current_if_set(|world| world.current_host_mut().tcp.unbind(self.local_addr));
    }
}
