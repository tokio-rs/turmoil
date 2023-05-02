use std::{net::IpAddr, str::FromStr};

use turmoil::{
    net::{TcpListener, UdpSocket},
    *,
};

#[test]
fn ipv6_lookup() {
    let mut sim = Builder::new().ip_network(IpNetwork::V6).build();
    sim.client("client", async move {
        assert_eq!(lookup("client"), IpAddr::from_str("fe80::1").unwrap());
        Ok(())
    });
    sim.run().unwrap()
}

#[test]
fn ipv6_udp_bind() {
    let mut sim = Builder::new().ip_network(IpNetwork::V6).build();
    sim.client("client", async move {
        let sock = UdpSocket::bind(":::0").await.unwrap();
        println!("{:?}", sock.local_addr().unwrap());
        Ok(())
    });
    sim.run().unwrap()
}

#[test]
#[should_panic]
fn ipv6_udp_bind_panic_at_addr_missmatch() {
    let mut sim = Builder::new().ip_network(IpNetwork::V6).build();
    sim.client("client", async move {
        let sock = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        println!("{:?}", sock.local_addr().unwrap());
        Ok(())
    });
    sim.run().unwrap()
}

#[test]
#[should_panic]
fn ipv4_udp_bind_panic_at_addr_missmatch() {
    let mut sim = Builder::new().ip_network(IpNetwork::V4).build();
    sim.client("client", async move {
        let sock = UdpSocket::bind(":::0").await.unwrap();
        println!("{:?}", sock.local_addr().unwrap());
        Ok(())
    });
    sim.run().unwrap()
}

#[test]
fn ipv6_tcp_bind() {
    let mut sim = Builder::new().ip_network(IpNetwork::V6).build();
    sim.client("client", async move {
        let sock = TcpListener::bind(":::1000").await.unwrap();
        println!("{:?}", sock.local_addr().unwrap());
        Ok(())
    });
    sim.run().unwrap()
}

#[test]
#[should_panic]
fn ipv6_tcp_bind_panic_at_addr_missmatch() {
    let mut sim = Builder::new().ip_network(IpNetwork::V6).build();
    sim.client("client", async move {
        let sock = TcpListener::bind("0.0.0.0:1000").await.unwrap();
        println!("{:?}", sock.local_addr().unwrap());
        Ok(())
    });
    sim.run().unwrap()
}

#[test]
#[should_panic]
fn ipv4_tcp_bind_panic_at_addr_missmatch() {
    let mut sim = Builder::new().ip_network(IpNetwork::V4).build();
    sim.client("client", async move {
        let sock = TcpListener::bind(":::1000").await.unwrap();
        println!("{:?}", sock.local_addr().unwrap());
        Ok(())
    });
    sim.run().unwrap()
}
