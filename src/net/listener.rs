use std::{io, net::SocketAddr, sync::Arc};

use tokio::sync::Notify;

use crate::world::World;

use super::TcpStream;

/// A simulated socket server, listening for connections.
///
/// All methods must be called from a host within a Turmoil simulation.
pub struct TcpListener {
    notify: Arc<Notify>,
}

impl TcpListener {
    /// Creates a new listener, which will be bound to the currently executing
    /// host's address.
    ///
    /// The returned listener is ready for accepting connections.
    pub async fn bind() -> io::Result<Self> {
        Ok(Self {
            notify: World::current(|world| {
                let ret = world.current_host_mut().bind();

                if let Ok(_) = &ret {
                    let host = world.current_host();
                    world.log.bind(&world.dns, host.dot(), host.elapsed());
                }

                ret
            })?,
        })
    }

    /// Accepts a new incoming connection from this listener.
    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            let maybe_accept = World::current(|world| world.accept());

            if let Some(accept) = maybe_accept {
                return Ok(accept);
            }

            self.notify.notified().await;
        }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        World::current_if_set(|world| {
            world.current_host_mut().unbind();

            let host = world.current_host();
            world.log.unbind(&world.dns, host.dot(), host.elapsed());
        })
    }
}
