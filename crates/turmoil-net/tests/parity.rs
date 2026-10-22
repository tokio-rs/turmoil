//! Compile-only parity check between our shim and real `tokio::net`.
//!
//! Run `RUSTFLAGS=--cfg parity_tokio cargo check -p turmoil-net --tests`
//! to verify the shim promises match tokio's. Any drift — a trait we
//! implement that tokio doesn't, a method we removed, a signature
//! mismatch — fails one build and succeeds the other.
//!
//! The test body below does no runtime work (it never runs); it
//! encodes the contract at the type level. To add a guarantee, add
//! another trait-bound call like `debug::<NewType>()`.

#![allow(dead_code, unused_imports)]

#[cfg(parity_tokio)]
use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf, ReadHalf, ReuniteError, WriteHalf},
    TcpListener, TcpStream, UdpSocket,
};
#[cfg(not(parity_tokio))]
use turmoil_net::shim::tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf, ReadHalf, ReuniteError, WriteHalf},
    TcpListener, TcpStream, UdpSocket,
};

use std::fmt::Debug;
use tokio::io::{AsyncRead, AsyncWrite};

fn debug<T: Debug>() {}
fn async_read<T: AsyncRead>() {}
fn async_write<T: AsyncWrite>() {}

#[test]
fn trait_bounds() {
    // Core types: Debug on everything, AsyncRead/AsyncWrite on TcpStream.
    debug::<TcpStream>();
    debug::<TcpListener>();
    debug::<UdpSocket>();
    async_read::<TcpStream>();
    async_write::<TcpStream>();

    // Borrowed split halves.
    debug::<ReadHalf<'_>>();
    debug::<WriteHalf<'_>>();
    async_read::<ReadHalf<'_>>();
    async_write::<WriteHalf<'_>>();

    // Owned split halves.
    debug::<OwnedReadHalf>();
    debug::<OwnedWriteHalf>();
    async_read::<OwnedReadHalf>();
    async_write::<OwnedWriteHalf>();

    // ReuniteError is a public Error type.
    debug::<ReuniteError>();
}

#[test]
fn try_read_vectored() {
    fn check(
        stream: &TcpStream,
        borrowed: &ReadHalf<'_>,
        owned: &OwnedReadHalf,
        bufs: &mut [std::io::IoSliceMut<'_>],
    ) {
        let _: std::io::Result<usize> = stream.try_read_vectored(bufs);
        let _: std::io::Result<usize> = borrowed.try_read_vectored(bufs);
        let _: std::io::Result<usize> = owned.try_read_vectored(bufs);
    }
    let _ = check;
}

#[test]
fn udp_cast_options() {
    fn check(socket: &UdpSocket, v4: std::net::Ipv4Addr, v6: std::net::Ipv6Addr) {
        let _: std::io::Result<bool> = socket.broadcast();
        let _: std::io::Result<()> = socket.set_broadcast(true);
        let _: std::io::Result<bool> = socket.multicast_loop_v4();
        let _: std::io::Result<()> = socket.set_multicast_loop_v4(false);
        let _: std::io::Result<bool> = socket.multicast_loop_v6();
        let _: std::io::Result<()> = socket.set_multicast_loop_v6(false);
        let _: std::io::Result<u32> = socket.multicast_ttl_v4();
        let _: std::io::Result<()> = socket.set_multicast_ttl_v4(1);
        let _: std::io::Result<()> = socket.join_multicast_v4(v4, std::net::Ipv4Addr::UNSPECIFIED);
        let _: std::io::Result<()> = socket.leave_multicast_v4(v4, std::net::Ipv4Addr::UNSPECIFIED);
        let _: std::io::Result<()> = socket.join_multicast_v6(&v6, 0);
        let _: std::io::Result<()> = socket.leave_multicast_v6(&v6, 0);
    }
    let _ = check;
}
