use std::{
    fmt::Debug,
    io::{self, Error, Result},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::oneshot,
};

use crate::{
    envelope::{Protocol, Segment, Syn},
    host::is_same,
    kernel::Fd,
    world::World,
    ToSocketAddrs,
};

use super::split_owned::{split_owned, OwnedReadHalf, OwnedWriteHalf};

/// A simulated TCP stream between a local and a remote socket.
///
/// All methods must be called from a host within a Turmoil simulation.
#[derive(Debug)]
pub struct TcpStream {
    fd: Fd,
}

impl TcpStream {
    /// Opens a TCP connection to a remote host.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<TcpStream> {
        let (ack, syn_ack) = oneshot::channel();

        let (fd, dst) = World::current(|world| {
            let dst = addr.to_socket_addr(&world.dns);

            let host = world.current_host_mut();
            let mut local_addr = SocketAddr::new(host.addr, host.assign_ephemeral_port());
            if dst.ip().is_loopback() {
                local_addr.set_ip(dst.ip());
            }

            let fd = host.tcp.connect(local_addr);

            let syn = Protocol::Tcp(Segment::Syn(Syn { ack }));
            if !is_same(local_addr, dst) {
                world.send_message(local_addr, dst, syn)?;
            } else {
                unimplemented!("add loopback")
                // send_loopback(local_addr, dst, syn);
            };

            Ok::<_, Error>((fd, dst))
        })?;

        syn_ack
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::ConnectionRefused, dst.to_string()))?;

        // tracing::trace!(target: TRACING_TARGET, src = ?pair.remote, dst = ?pair.local, protocol = %"TCP SYN-ACK", "Recv");

        Ok(Self { fd })
    }

    /// Returns the local address that this stream is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        todo!()
    }

    /// Returns the remote address that this stream is connected to.
    pub fn peer_addr(&self) -> Result<SocketAddr> {
        todo!()
    }

    /// Splits a `TcpStream` into a read half and a write half, which can be used
    /// to read and write the stream concurrently.
    ///
    /// **Note:** Dropping the write half will shut down the write half of the TCP
    /// stream. This is equivalent to calling [`shutdown()`] on the `TcpStream`.
    ///
    /// [`shutdown()`]: fn@tokio::io::AsyncWriteExt::shutdown
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        split_owned(self)
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<Result<()>> {
        todo!()
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        todo!()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        todo!()
    }
}
