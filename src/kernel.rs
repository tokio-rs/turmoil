#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct Fd(u16);

pub mod tcp {
    use std::{
        collections::VecDeque,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        task::Waker,
    };

    use bytes::BytesMut;
    use indexmap::IndexMap;

    use crate::{envelope::Syn, Protocol, Segment, TRACING_TARGET};

    use super::Fd;

    pub struct Tcp {
        fds: IndexMap<SocketAddr, Fd>,
        // These could recycle, but for now we just increment for the lifetime
        // of the host.
        next_fd: u16,

        binds: IndexMap<Fd, Bind>,
        sockets: IndexMap<Fd, Socket>,
    }

    struct Bind {
        addr: SocketAddr,
        buf: VecDeque<(Syn, SocketAddr)>,
        waker: Option<Waker>,
    }

    struct Socket {
        buf: BytesMut,
        // delivered: IndexMap<seq_no, seg>
        waker: Option<Waker>,
    }

    impl Tcp {
        pub fn new() -> Self {
            Self {
                fds: IndexMap::new(),
                next_fd: 0,
                binds: IndexMap::new(),
                sockets: IndexMap::new(),
            }
        }

        pub fn accept(
            &mut self,
            fd: Fd,
            public_ip: IpAddr,
            waker: &Waker,
        ) -> Option<(Fd, SocketAddr)> {
            let bind = &mut self.binds[&fd];

            let Some((syn, origin)) = bind.buf.pop_front() else {
                bind.waker.replace(waker.clone());
                return None;
            };

            tracing::trace!(target: TRACING_TARGET, src = ?origin, dst = ?bind.addr, protocol = %"TCP SYN", "Recv");

            // Send SYN-ACK -> origin. If Ok we proceed (acts as the ACK),
            // else we return early to avoid host mutations.
            let ack = syn.ack.send(());
            tracing::trace!(target: TRACING_TARGET, src = ?bind.addr, dst = ?origin, protocol = %"TCP SYN-ACK", "Send");

            if ack.is_err() {
                bind.waker.replace(waker.clone());
                return None;
            }

            // Move to host.rs?
            let mut my_addr = bind.addr;
            if origin.ip().is_loopback() {
                my_addr.set_ip(origin.ip());
            }
            if my_addr.ip().is_unspecified() {
                my_addr.set_ip(public_ip);
            }

            let fd = Fd(self.next_fd);
            self.next_fd += 1;

            self.fds.insert(my_addr, fd);
            self.sockets.insert(
                fd,
                Socket {
                    buf: BytesMut::new(),
                    waker: None,
                },
            );

            Some((fd, origin))
        }

        pub fn bind(&mut self, addr: SocketAddr) -> std::io::Result<Fd> {
            let fd = Fd(self.next_fd);

            match self.fds.entry(addr) {
                indexmap::map::Entry::Occupied(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        addr.to_string(),
                    ));
                }
                indexmap::map::Entry::Vacant(entry) => {
                    entry.insert(fd);
                    self.binds.insert(
                        fd,
                        Bind {
                            addr,
                            buf: VecDeque::new(),
                            waker: None,
                        },
                    )
                }
            };

            self.next_fd += 1;
            tracing::info!(target: TRACING_TARGET, ?addr, protocol = %"TCP", "Bind");
            Ok(fd)
        }

        pub fn connect(&mut self, local_addr: SocketAddr) -> Fd {
            let fd = Fd(self.next_fd);
            self.next_fd += 1;

            self.fds.insert(local_addr, fd);
            self.sockets.insert(
                fd,
                Socket {
                    buf: BytesMut::new(),
                    waker: None,
                },
            );

            fd
        }

        pub fn shut_wr(&mut self, fd: Fd) {}

        pub fn close(&mut self, fd: Fd) {}

        pub fn receive_from_network(
            &mut self,
            src: SocketAddr,
            dst: SocketAddr,
            segment: Segment,
        ) -> Result<(), Protocol> {
            match segment {
                Segment::Syn(syn) => {
                    // FIXME:
                    let key: SocketAddr = (Ipv4Addr::UNSPECIFIED, dst.port()).into();
                    // If bound, queue the syn; else we drop the syn triggering
                    // connection refused on the client.
                    if let Some(fd) = self.fds.get(&key) {
                        if let Some(bind) = self.binds.get_mut(fd) {
                            bind.buf.push_back((syn, src));

                            if let Some(waker) = bind.waker.take() {
                                waker.wake()
                            }
                        }
                    }
                }
                _ => unimplemented!("Only connect / accept exists"),
            };

            Ok(())
        }
    }
}

// TODO: Move from host.rs
mod udp {}
