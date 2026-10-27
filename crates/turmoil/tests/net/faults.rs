//! How the simulated network's faults reach sockets: loss, crashes,
//! holds, and `turmoil_net` rules.

use std::{io::ErrorKind, rc::Rc, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
    time::timeout,
};
use turmoil::{
    net::{TcpListener, TcpStream, UdpSocket},
    Builder, Result, Transport,
};

const PORT: u16 = 1738;

#[test]
fn tcp_retransmits_across_random_loss() -> Result {
    const LEN: usize = 16 * 1024;
    let payload: Vec<u8> = (0..LEN).map(|i| i as u8).collect();
    let expected = payload.clone();

    let mut sim = Builder::new()
        .rng_seed(7)
        .fail_rate(0.05)
        .repair_rate(0.5)
        .simulation_duration(Duration::from_secs(120))
        .build();

    sim.host("server", move || {
        let payload = payload.clone();
        async move {
            let listener = TcpListener::bind(("0.0.0.0", PORT)).await?;
            loop {
                let (mut s, _) = listener.accept().await?;
                let mut request = vec![0; LEN];
                s.read_exact(&mut request).await?;
                assert_eq!(request, payload);
                s.write_all(&request).await?;
            }
        }
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        s.write_all(&expected).await?;
        let mut echoed = vec![0; LEN];
        s.read_exact(&mut echoed).await?;
        assert_eq!(echoed, expected);
        Ok(())
    });

    sim.run()
}

#[test]
fn crash_resets_connections_without_a_fin() -> Result {
    let mut sim = Builder::new().build();
    let accepted = Rc::new(Notify::new());

    sim.host("server", {
        let accepted = accepted.clone();
        move || {
            let accepted = accepted.clone();
            async move {
                let listener = TcpListener::bind(("0.0.0.0", PORT)).await?;
                let (_s, _) = listener.accept().await?;
                accepted.notify_one();
                std::future::pending().await
            }
        }
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;
        accepted.notified().await;
        // Let the test crash and bounce the server.
        tokio::time::sleep(Duration::from_secs(1)).await;

        // The crashed host sent nothing: no EOF ever arrived.
        assert_eq!(
            s.try_read(&mut [0; 1]).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
        // The rebooted host has no record of the connection.
        s.write_all(b"hello").await?;
        let err = s.read(&mut [0; 1]).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionReset);
        Ok(())
    });

    // Run until the server has accepted.
    while !sim.step()? && sim.elapsed() < Duration::from_millis(500) {}
    sim.crash("server");
    for _ in 0..100 {
        sim.step()?;
    }
    sim.bounce("server");
    sim.run()
}

#[test]
fn packets_to_a_crashed_host_are_lost() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let sock = UdpSocket::bind(("0.0.0.0", PORT)).await?;
        let mut buf = [0; 1];
        loop {
            let (_, from) = sock.recv_from(&mut buf).await?;
            sock.send_to(&buf, from).await?;
        }
    });
    sim.crash("server");

    sim.client("client", async {
        let sock = UdpSocket::bind(("0.0.0.0", 0)).await?;
        sock.send_to(&[1], ("server", PORT)).await?;
        let reply = timeout(Duration::from_secs(1), sock.recv_from(&mut [0; 1])).await;
        assert!(reply.is_err());
        Ok(())
    });

    sim.run()
}

#[test]
fn finished_client_closes_its_connections() -> Result {
    let mut sim = Builder::new().build();
    let eof = Rc::new(Notify::new());

    sim.client("server", {
        let eof = eof.clone();
        async move {
            let listener = TcpListener::bind(("0.0.0.0", PORT)).await?;
            let (mut s, _) = listener.accept().await?;
            assert_eq!(s.read(&mut [0; 1]).await?, 0);
            eof.notify_one();
            Ok(())
        }
    });

    // Returns with the stream open; dropping it sends a FIN.
    sim.client("client", async {
        let _s = TcpStream::connect(("server", PORT)).await?;
        Ok(())
    });

    sim.client("observer", async move {
        eof.notified().await;
        Ok(())
    });

    sim.run()
}

#[test]
fn turmoil_net_rules_filter_simulated_traffic() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        let sock = UdpSocket::bind(("0.0.0.0", PORT)).await?;
        let mut buf = [0; 1];
        sock.recv_from(&mut buf).await?;
        // Only the second datagram survives the rule.
        assert_eq!(buf, [2]);
        Ok(())
    });

    sim.client("client", async {
        let _drop_first = {
            let mut dropped = false;
            turmoil_net::rule(move |pkt: &turmoil::Packet| match &pkt.payload {
                Transport::Udp(_) if !dropped => {
                    dropped = true;
                    turmoil_net::Verdict::Drop
                }
                _ => turmoil_net::Verdict::Pass,
            })
        };
        let sock = UdpSocket::bind(("0.0.0.0", 0)).await?;
        sock.send_to(&[1], ("server", PORT)).await?;
        sock.send_to(&[2], ("server", PORT)).await?;
        // Packets leave the host after its tick; keep the rule installed
        // until then.
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(())
    });

    sim.run()
}

#[test]
fn links_expose_held_packets() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = TcpListener::bind(("0.0.0.0", PORT)).await?;
        loop {
            listener.accept().await?;
        }
    });
    sim.client("client", async {
        TcpStream::connect(("server", PORT)).await?;
        Ok(())
    });
    sim.hold("client", "server");
    sim.step()?;

    sim.links(|mut links| {
        let link = links.next().unwrap();
        let held: Vec<_> = link.collect();
        assert_eq!(held.len(), 1);
        let (src, dst) = held[0].pair();
        assert_eq!(dst.port(), PORT);
        assert_ne!(src.port(), 0);
        let Transport::Tcp(syn) = &held[0].packet().payload else {
            panic!("expected TCP");
        };
        assert!(syn.flags.syn && !syn.flags.ack);
    });

    sim.release("client", "server");
    sim.run()
}
