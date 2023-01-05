use std::{
    io,
    net::{IpAddr, Ipv4Addr},
    rc::Rc,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
    time::timeout,
};
use turmoil::{
    net::{TcpListener, TcpStream},
    Builder, Result,
};

const PORT: u16 = 1738;

fn assert_error_kind<T>(res: io::Result<T>, kind: io::ErrorKind) {
    assert_eq!(res.err().map(|e| e.kind()), Some(kind));
}

async fn bind() -> TcpListener {
    TcpListener::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), PORT))
        .await
        .unwrap()
}

#[test]
fn network_partitions_during_connect() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await;
        loop {
            let _ = listener.accept().await;
        }
    });

    sim.client("client", async {
        turmoil::partition("client", "server");

        assert_error_kind(
            TcpStream::connect(("server", PORT)).await,
            io::ErrorKind::ConnectionRefused,
        );

        turmoil::repair("client", "server");
        turmoil::hold("client", "server");

        assert!(
            timeout(Duration::from_secs(1), TcpStream::connect(("server", PORT)))
                .await
                .is_err()
        );

        Ok(())
    });

    sim.run()
}

#[test]
fn client_hangup_on_connect() -> Result {
    let mut sim = Builder::new().build();

    let timeout_secs = 1;

    sim.client("server", async move {
        let listener = bind().await;

        assert!(
            timeout(Duration::from_secs(timeout_secs * 2), listener.accept())
                .await
                .is_err()
        );

        Ok(())
    });

    sim.client("client", async move {
        turmoil::hold("client", "server");

        assert!(timeout(
            Duration::from_secs(timeout_secs),
            TcpStream::connect(("server", PORT))
        )
        .await
        .is_err());

        turmoil::release("client", "server");

        Ok(())
    });

    sim.run()
}

#[test]
fn hold_and_release_once_connected() -> Result {
    let mut sim = Builder::new().build();

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    sim.client("server", async move {
        let listener = bind().await;
        let (mut s, _) = listener.accept().await?;

        wait.notified().await;
        s.write_u8(1).await?;

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        turmoil::hold("server", "client");

        notify.notify_one();

        assert!(timeout(Duration::from_secs(1), s.read_u8()).await.is_err());

        turmoil::release("server", "client");

        assert_eq!(1, s.read_u8().await?);

        Ok(())
    });

    sim.run()
}

#[test]
fn network_partition_once_connected() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await;
        let (mut s, _) = listener.accept().await?;

        assert!(timeout(Duration::from_secs(1), s.read_u8()).await.is_err());

        s.write_u8(1).await?;

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        turmoil::partition("server", "client");

        s.write_u8(1).await?;

        assert!(timeout(Duration::from_secs(1), s.read_u8()).await.is_err());

        Ok(())
    });

    sim.run()
}

#[test]
fn accept_front_of_line_blocking() -> Result {
    let wait = Rc::new(Notify::new());
    let notify = wait.clone();

    let mut sim = Builder::new().build();

    // We setup the simulation with hosts A, B, and C

    sim.host("B", || async {
        let listener = bind().await;

        while let Ok((_, peer)) = listener.accept().await {
            tracing::debug!("peer {}", peer);
        }
    });

    // Hold all traffic from A:B
    sim.client("A", async move {
        turmoil::hold("A", "B");

        assert!(
            timeout(Duration::from_secs(1), TcpStream::connect(("B", PORT)))
                .await
                .is_err()
        );
        notify.notify_one();

        Ok(())
    });

    // C:B should succeed, and should not be blocked behind the A:B, which is
    // not eligible for delivery
    sim.client("C", async move {
        wait.notified().await;

        let _ = TcpStream::connect(("B", PORT)).await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn send_upon_accept() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await;

        while let Ok((mut s, _)) = listener.accept().await {
            assert!(s.write_u8(9).await.is_ok());
        }
    });

    sim.client("client", async {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        assert_eq!(9, s.read_u8().await?);

        Ok(())
    });

    sim.run()
}

#[test]
fn n_responses() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await;

        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                while let Ok(how_many) = s.read_u8().await {
                    for i in 0..how_many {
                        let _ = s.write_u8(i).await;
                    }
                }
            });
        }
    });

    sim.client("client", async {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        let how_many = 3;
        s.write_u8(how_many).await?;

        for i in 0..how_many {
            assert_eq!(i, s.read_u8().await?);
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn server_concurrency() -> Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = bind().await;

        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                while let Ok(how_many) = s.read_u8().await {
                    for i in 0..how_many {
                        let _ = s.write_u8(i).await;
                    }
                }
            });
        }
    });

    let how_many = 3;

    for i in 0..how_many {
        sim.client(format!("client-{}", i), async move {
            let mut s = TcpStream::connect(("server", PORT)).await?;

            s.write_u8(how_many).await?;

            for i in 0..how_many {
                assert_eq!(i, s.read_u8().await?);
            }

            Ok(())
        });
    }

    sim.run()
}

#[test]
fn drop_listener() -> Result {
    let how_many_conns = 3;

    let wait = Rc::new(Notify::new());
    let notify = wait.clone();

    let mut sim = Builder::new().build();

    sim.host("server", || {
        let notify = notify.clone();

        async move {
            let listener = bind().await;

            for _ in 0..how_many_conns {
                let (mut s, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    while let Ok(how_many) = s.read_u8().await {
                        for i in 0..how_many {
                            let _ = s.write_u8(i).await;
                        }
                    }
                });
            }

            drop(listener);
            notify.notify_one();
        }
    });

    sim.client("client", async move {
        let mut conns = vec![];

        for _ in 0..how_many_conns {
            let s = TcpStream::connect(("server", 1738)).await?;
            conns.push(s);
        }

        wait.notified().await;

        for mut s in conns {
            let how_many = 3;
            let _ = s.write_u8(how_many).await;

            for i in 0..how_many {
                assert_eq!(i, s.read_u8().await?);
            }
        }

        assert_error_kind(
            TcpStream::connect(("server", 1738)).await,
            io::ErrorKind::ConnectionRefused,
        );

        Ok(())
    });

    sim.run()
}

#[test]
fn drop_listener_with_non_empty_queue() -> Result {
    let how_many_conns = 3;

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    let tick = Duration::from_millis(10);
    let mut sim = Builder::new().tick_duration(tick).build();

    sim.host("server", || {
        let wait = wait.clone();

        async move {
            let listener = bind().await;
            wait.notified().await;
            drop(listener);
        }
    });

    sim.client("client", async move {
        let mut conns = vec![];

        for _ in 0..how_many_conns {
            conns.push(tokio::task::spawn_local(TcpStream::connect((
                "server", PORT,
            ))));
        }

        // sleep for one iteration to land syns in the listener
        tokio::time::sleep(tick).await;
        notify.notify_one();

        for fut in conns {
            assert_error_kind(fut.await?, io::ErrorKind::ConnectionRefused);
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn recycle_server_socket_bind() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async {
        for _ in 0..3 {
            let _ = TcpListener::bind((IpAddr::from(Ipv4Addr::UNSPECIFIED), 1234)).await?;
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn hangup() -> Result {
    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await;
        let (mut s, _) = listener.accept().await?;

        let mut buf = [0; 8];
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        // Read again to ensure EOF
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        // Write until RST arrives
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;

            if let Err(e) = s.write_u8(1).await {
                assert_eq!(io::ErrorKind::BrokenPipe, e.kind());
                break;
            }
        }
        notify.notify_one();

        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;

        drop(s);
        wait.notified().await;

        Ok(())
    });

    sim.run()
}

#[test]
fn shutdown_write() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await;
        let (mut s, _) = listener.accept().await?;

        for i in 1..=3 {
            assert_eq!(i, s.read_u8().await?);
        }

        let mut buf = [0; 8];
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        for i in 0..3 {
            s.write_u8(i).await?;
        }

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        for i in 1..=3 {
            s.write_u8(i).await?;
        }

        s.shutdown().await?;

        assert_error_kind(s.shutdown().await, io::ErrorKind::NotConnected);
        assert_error_kind(s.write_u8(1).await, io::ErrorKind::BrokenPipe);

        // Ensure the other half is still open
        for i in 0..3 {
            assert_eq!(i, s.read_u8().await?);
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn read_with_empty_buffer() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await;
        let _ = listener.accept().await?;

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // no-op
        let mut buf = [0; 0];
        let n = s.read(&mut buf).await?;
        assert_eq!(0, n);

        Ok(())
    });

    sim.run()
}

#[test]
fn write_zero_bytes() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await;
        let (mut s, _) = listener.accept().await?;

        assert_eq!(1, s.read_u8().await?);

        Ok(())
    });

    sim.client("client", async move {
        let mut s = TcpStream::connect(("server", PORT)).await?;

        // no-op
        let buf = [0; 0];
        let _ = s.write(&buf).await?;

        // actual write to ensure server:read_u8 is not EOF
        s.write_u8(1).await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn split() -> Result {
    let mut sim = Builder::new().build();

    sim.client("server", async move {
        let listener = bind().await;
        let (mut s, _) = listener.accept().await?;

        assert_eq!(1, s.read_u8().await?);

        let mut buf = [0; 8];
        assert!(matches!(s.read(&mut buf).await, Ok(0)));

        s.write_u8(1).await?;

        // accept to establish s1, s2, s3 below
        listener.accept().await?;
        listener.accept().await?;
        listener.accept().await?;

        Ok(())
    });

    sim.client("client", async move {
        let s = TcpStream::connect(("server", PORT)).await?;

        let (mut r, mut w) = s.into_split();

        let h = tokio::spawn(async move { w.write_u8(1).await });

        assert_eq!(1, r.read_u8().await?);
        h.await??;

        let s1 = TcpStream::connect(("server", PORT)).await?;
        let s2 = TcpStream::connect(("server", PORT)).await?;
        let s3 = TcpStream::connect(("server", PORT)).await?;

        let (r1, w1) = s1.into_split();
        let (r2, w2) = s2.into_split();

        assert!(r1.reunite(w2).is_err());
        assert!(w1.reunite(r2).is_err());

        let (r3, w3) = s3.into_split();
        r3.reunite(w3)?;

        Ok(())
    });

    sim.run()
}
