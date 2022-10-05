use std::{io, rc::Rc, str::from_utf8, time::Duration};

use bytes::Bytes;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
    time::timeout,
};
use turmoil::{debug, hold, net, Builder};

/// Augments a stream with a simple ping/pong protocol.
struct Connection {
    stream: net::TcpStream,
}

impl Connection {
    fn new(stream: net::TcpStream) -> Self {
        Self { stream }
    }

    async fn send_ping(&mut self, how_many: u16) -> io::Result<()> {
        self.stream.write_u16(how_many).await
    }

    async fn send_pong(&mut self) -> io::Result<()> {
        self.stream.write_all_buf(&mut Bytes::from("pong")).await
    }

    async fn recv_ping(&mut self) -> io::Result<u16> {
        self.stream.read_u16().await
    }

    async fn recv_pong(&mut self) -> io::Result<()> {
        let mut buf = [0; 4];
        self.stream.read_exact(&mut buf).await?;
        let s = from_utf8(&buf).unwrap();
        assert_eq!("pong", s);
        Ok(())
    }
}

fn assert_error_kind<T>(res: io::Result<T>, kind: io::ErrorKind) {
    assert_eq!(res.err().map(|e| e.kind()), Some(kind));
}

#[test]
fn network_partitions() -> turmoil::Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = net::TcpListener::bind().await.unwrap();
        loop {
            let _ = listener.accept().await;
        }
    });

    sim.client("client", async {
        turmoil::partition("client", "server");

        assert_error_kind(
            net::TcpStream::connect("server").await,
            io::ErrorKind::ConnectionRefused,
        );

        turmoil::repair("client", "server");

        turmoil::hold("client", "server");

        assert!(
            timeout(Duration::from_secs(1), net::TcpStream::connect("server"))
                .await
                .is_err()
        );

        Ok(())
    });

    sim.run()
}

#[test]
fn hold_and_release_on_connect() -> turmoil::Result {
    let mut sim = Builder::new().build();

    let timeout_secs = 1;

    sim.client("server", async move {
        let listener = net::TcpListener::bind().await?;

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
            net::TcpStream::connect("server")
        )
        .await
        .is_err());

        turmoil::release("client", "server");

        Ok(())
    });

    sim.run()
}

#[test]
fn hold_and_release_once_connected() -> turmoil::Result {
    let mut sim = Builder::new().build();

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    sim.client("server", async move {
        let listener = net::TcpListener::bind().await?;
        let (s, _) = listener.accept().await?;
        let mut c = Connection::new(s);

        wait.notified().await;
        let _ = c.send_ping(1).await?;

        Ok(())
    });

    sim.client("client", async move {
        let s = net::TcpStream::connect("server").await?;
        let mut c = Connection::new(s);

        turmoil::hold("server", "client");

        notify.notify_one();

        assert!(timeout(Duration::from_secs(1), c.recv_ping())
            .await
            .is_err());

        turmoil::release("server", "client");

        assert!(c.recv_ping().await.is_ok());

        Ok(())
    });

    sim.run()
}

#[test]
fn accept_front_of_line_blocking() -> turmoil::Result {
    let wait = Rc::new(Notify::new());
    let notify = wait.clone();

    let mut sim = Builder::new().build();

    // We setup the simulation with hosts A, B, and C

    sim.host("B", || async {
        let listener = net::TcpListener::bind().await.unwrap();

        while let Ok((_, peer)) = listener.accept().await {
            debug!("peer {}", peer);
        }
    });

    // Hold all traffic from A:B
    sim.client("A", async move {
        hold("A", "B");

        assert!(
            timeout(Duration::from_secs(1), net::TcpStream::connect("B"))
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

        let _ = net::TcpStream::connect("B").await?;

        Ok(())
    });

    sim.run()
}

#[test]
fn send_upon_accept() -> turmoil::Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = net::TcpListener::bind().await.unwrap();

        while let Ok((s, _)) = listener.accept().await {
            let mut c = Connection::new(s);
            assert!(c.send_ping(1).await.is_ok());
        }
    });

    sim.client("client", async {
        let s = net::TcpStream::connect("server").await?;
        let mut c = Connection::new(s);

        assert!(c.recv_ping().await.is_ok());

        Ok(())
    });

    sim.run()
}

#[test]
fn n_responses() -> turmoil::Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = net::TcpListener::bind().await.unwrap();

        while let Ok((s, _)) = listener.accept().await {
            let mut c = Connection::new(s);

            tokio::spawn(async move {
                while let Ok(how_many) = c.recv_ping().await {
                    for _ in 0..how_many {
                        let _ = c.send_pong().await;
                    }
                }
            });
        }
    });

    sim.client("client", async {
        let s = net::TcpStream::connect("server").await?;
        let mut c = Connection::new(s);

        let how_many = 3;
        assert!(c.send_ping(how_many).await.is_ok());

        for _ in 0..how_many {
            assert!(c.recv_pong().await.is_ok());
        }

        Ok(())
    });

    sim.run()
}

#[test]
fn server_concurrency() -> turmoil::Result {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let listener = net::TcpListener::bind().await.unwrap();

        while let Ok((s, _)) = listener.accept().await {
            let mut c = Connection::new(s);

            tokio::spawn(async move {
                while let Ok(how_many) = c.recv_ping().await {
                    for _ in 0..how_many {
                        let _ = c.send_pong().await;
                    }
                }
            });
        }
    });

    let how_many = 3;

    for i in 0..how_many {
        sim.client(format!("client-{}", i), async move {
            let s = net::TcpStream::connect("server").await?;
            let mut c = Connection::new(s);

            assert!(c.send_ping(how_many).await.is_ok());

            for _ in 0..how_many {
                assert!(c.recv_pong().await.is_ok());
            }

            Ok(())
        });
    }

    sim.run()
}

#[test]
fn drop_listener() -> turmoil::Result {
    let how_many_conns = 3;

    let wait = Rc::new(Notify::new());
    let notify = wait.clone();

    let mut sim = Builder::new().build();

    sim.host("server", || {
        let notify = notify.clone();

        async move {
            let listener = net::TcpListener::bind().await.unwrap();

            for _ in 0..how_many_conns {
                let (s, _) = listener.accept().await.unwrap();
                let mut c = Connection::new(s);

                tokio::spawn(async move {
                    while let Ok(how_many) = c.recv_ping().await {
                        for _ in 0..how_many {
                            let _ = c.send_pong().await;
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
            let s = net::TcpStream::connect("server").await?;
            conns.push(Connection::new(s));
        }

        wait.notified().await;

        for mut c in conns {
            let how_many = 3;
            let _ = c.send_ping(how_many).await;

            for _ in 0..how_many {
                assert!(c.recv_pong().await.is_ok());
            }
        }

        assert_error_kind(
            net::TcpStream::connect("server").await,
            io::ErrorKind::ConnectionRefused,
        );

        Ok(())
    });

    sim.run()
}

#[test]
fn drop_listener_with_non_empty_queue() -> turmoil::Result {
    let how_many_conns = 3;

    let notify = Rc::new(Notify::new());
    let wait = notify.clone();

    let tick = Duration::from_millis(10);
    let mut sim = Builder::new().tick_duration(tick).build();

    sim.host("server", || {
        let wait = wait.clone();

        async move {
            let listener = net::TcpListener::bind().await.unwrap();
            wait.notified().await;
            drop(listener);
        }
    });

    sim.client("client", async move {
        let mut conns = vec![];

        for _ in 0..how_many_conns {
            conns.push(tokio::task::spawn_local(net::TcpStream::connect("server")));
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
