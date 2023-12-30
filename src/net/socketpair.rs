use std::net::SocketAddr;

#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct SocketPair {
    pub local: SocketAddr,
    pub remote: SocketAddr,
}

impl SocketPair {
    pub fn new(local: SocketAddr, remote: SocketAddr) -> SocketPair {
        assert_ne!(local, remote);
        SocketPair { local, remote }
    }
}
