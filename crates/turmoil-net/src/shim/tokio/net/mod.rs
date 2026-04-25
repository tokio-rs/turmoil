//! Drop-in replacements for `tokio::net` types.
//!
//! TODO: `TcpStream`, `TcpListener`, `UdpSocket`, `UnixStream`,
//! `UnixListener`, owned split halves. Each implements the same
//! `AsyncRead`/`AsyncWrite` surface as its tokio counterpart.
