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
