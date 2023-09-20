use std::{
    io::ErrorKind,
    net::{Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::sleep,
};
use turmoil::{
    net::{TcpListener, TcpStream, UdpSocket},
    *,
};

fn double_ipv4_subnets() -> IpSubnets {
    let mut subnets = IpSubnets::new();
    subnets.add(IpSubnet::V4(Ipv4Subnet::new(
        Ipv4Addr::new(192, 168, 0, 0),
        16,
    )));
    subnets.add(IpSubnet::V4(Ipv4Subnet::new(
        Ipv4Addr::new(10, 1, 0, 0),
        16,
    )));
    subnets
}

#[test]
fn udp_ipv4_exclusive_conn_from_unspecifed() -> Result {
    let mut sim = Builder::new().build();
    sim.node("alice").build_client(async {
        let fail = UdpSocket::bind(":::0").await?;
        let err = fail
            .send_to(b"Never", "192.168.0.42:2000")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);

        sleep(Duration::from_secs(1)).await;
        let udp = UdpSocket::bind("0.0.0.0:0").await?;
        udp.send_to(b"Hello world", "192.168.0.42:2000").await?;
        Ok(())
    });

    sim.node("bob")
        .with_addr(Ipv4Addr::new(192, 168, 0, 42))
        .build_client(async {
            let udp = UdpSocket::bind("192.168.0.42:2000").await?;
            let mut buf = [0; 128];
            let (n, from) = udp.recv_from(&mut buf).await?;
            assert_eq!(from.ip(), Ipv4Addr::new(192, 168, 0, 1));
            assert_eq!(&buf[..n], b"Hello world");
            Ok(())
        });

    sim.run()
}

#[test]
fn udp_ipv6_exclusive_conn_from_unspecified() -> Result {
    let mut sim = Builder::new().build();
    sim.node("alice").build_client(async {
        let fail = UdpSocket::bind("0.0.0.0:0").await?;
        let err = fail.send_to(b"Never", "fe80::42:2000").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);

        sleep(Duration::from_secs(1)).await;
        let udp = UdpSocket::bind(":::0").await?;
        udp.send_to(b"Hello world", "fe80::42:2000").await?;
        Ok(())
    });

    sim.node("bob")
        .with_addr(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0x42))
        .build_client(async {
            let udp = UdpSocket::bind("fe80::42:2000").await?;
            let mut buf = [0; 128];
            let (n, from) = udp.recv_from(&mut buf).await?;
            assert_eq!(from.ip(), Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
            assert_eq!(&buf[..n], b"Hello world");
            Ok(())
        });

    sim.run()
}

#[test]
fn udp_interface_exclusivity() -> Result {
    let mut sim = Builder::new().ip_subnets(double_ipv4_subnets()).build();
    sim.node("bob").build_client(async {
        let sock = UdpSocket::bind("192.168.0.1:2000").await?;
        let mut buf = [0; 128];
        let (n, _) = sock.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], b"Hello");
        Ok(())
    });

    sim.node("alice").build_client(async {
        let fail = UdpSocket::bind("10.1.0.2:0").await?;
        let err = fail
            .send_to(b"Never", "192.168.0.1:2000")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionRefused);

        let succ = UdpSocket::bind("192.168.0.2:0").await?;
        succ.send_to(b"Hello", "192.168.0.1:2000").await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn udp_multi_interface_rx() -> Result {
    let mut sim = Builder::new().ip_subnets(double_ipv4_subnets()).build();
    sim.node("rx").build_client(async move {
        let sock = UdpSocket::bind("0.0.0.0:2000").await?;
        let mut buf = [0; 128];

        let (n, origin) = sock.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], b"Hello from TX 1");
        assert_eq!(origin.ip(), Ipv4Addr::new(192, 168, 0, 2));

        let (n, origin) = sock.recv_from(&mut buf).await?;
        assert_eq!(&buf[..n], b"Hello from TX 2");
        assert_eq!(origin.ip(), Ipv4Addr::new(10, 1, 0, 3));

        Ok(())
    });

    sim.node("tx1").build_client(async move {
        UdpSocket::bind("192.168.0.2:0")
            .await?
            .send_to(b"Hello from TX 1", "192.168.0.1:2000")
            .await?;
        Ok(())
    });

    sim.node("tx2").build_client(async move {
        sleep(Duration::from_secs(1)).await;
        UdpSocket::bind("10.1.0.3:0")
            .await?
            .send_to(b"Hello from TX 2", "10.1.0.1:2000")
            .await?;
        Ok(())
    });

    sim.run()
}

#[test]
fn udp_multi_interface_tx() -> Result {
    let mut sim = Builder::new().ip_subnets(double_ipv4_subnets()).build();
    sim.node("tx").build_client(async move {
        let sock = UdpSocket::bind("0.0.0.0:0").await?;
        sock.send_to(b"Hello to RX 1", "192.168.0.2:2000").await?;
        sock.send_to(b"Hello to RX 2", "10.1.0.3:2000").await?;
        Ok(())
    });
    sim.node("rx1")
        .build_client(rx_once("192.168.0.2:2000", "Hello to RX 1"));
    sim.node("rx2")
        .build_client(rx_once("10.1.0.3:2000", "Hello to RX 2"));

    sim.run()
}

async fn rx_once(bind: &str, expect: &str) -> Result<()> {
    let sock = UdpSocket::bind(bind).await?;
    let mut buf = [0; 128];
    let (n, _) = sock.recv_from(&mut buf).await?;
    assert_eq!(&buf[..n], expect.as_bytes());
    Ok(())
}

#[test]
#[should_panic]
fn udp_ip_version_not_supported() {
    let mut subnets = IpSubnets::new();
    subnets.add(IpSubnet::V4(Ipv4Subnet::new(Ipv4Addr::new(1, 2, 3, 0), 24)));
    let mut sim = Builder::new().ip_subnets(subnets).build();
    sim.node("alice").build_client(async {
        let _ = UdpSocket::bind(":::0").await?;

        Ok(())
    });
    sim.run().unwrap();
}

#[test]
fn tcp_connectivity() -> Result {
    let mut sim = Builder::new().build();
    sim.node("alice").build_client(async {
        let lis = TcpListener::bind("0.0.0.0:80").await?;
        let (mut stream, _) = lis.accept().await?;
        let mut s = String::new();
        stream.read_to_string(&mut s).await?;
        assert_eq!(s, "Hello world!");

        Ok(())
    });
    sim.node("bob").build_client(async {
        let mut stream = TcpStream::connect("192.168.0.1:80").await?;
        stream.write_all(b"Hello world!").await?;
        Ok(())
    });

    sim.run()
}

#[test]
fn tcp_interface_exclusivity() -> Result {
    let mut sim = Builder::new().build();
    sim.node("alice").build_client(async {
        let lis = TcpListener::bind(":::80").await?;
        let (mut stream, _) = lis.accept().await?;
        let mut s = [0; 128];
        let n = stream.read(&mut s).await?;
        assert_eq!(&s[..n], b"Hello world!");

        Ok(())
    });
    sim.node("bob").build_client(async {
        let mut stream = TcpStream::connect("fe80::1:80").await?;
        assert!(stream.local_addr()?.is_ipv6());
        stream.write_all(b"Hello world!").await?;

        Ok(())
    });

    sim.run()
}
