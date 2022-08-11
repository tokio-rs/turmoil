use std::net::SocketAddr;

/// A "dot" is a host + pair tuple. The term originated from the "dotted version
/// vector" [1].
///
/// [1]: https://riak.com/posts/technical/vector-clocks-revisited-part-2-dotted-version-vectors/index.html
#[derive(Debug, Copy, Clone, serde::Serialize)]
pub(crate) struct Dot {
    pub(crate) host: SocketAddr,
    pub(crate) version: u64,
}
