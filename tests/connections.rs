use std::sync::Arc;

use tokio::sync::Notify;
use turmoil::{Builder, ConnectionIo};

#[derive(Debug, serde::Serialize)]
enum Message {
    Ping { how_many: u16 },
    Pong,
}

impl turmoil::Message for Message {
    fn write_json(&self, dst: &mut dyn std::io::Write) {
        serde_json::to_writer_pretty(dst, self).unwrap()
    }
}

#[test]
#[should_panic]
fn unknown_host() {
    let mut sim = Builder::new().build();

    sim.client("client", async {
        let io: ConnectionIo<Message> = ConnectionIo::new();
        let _ = io.connect("foo").await;
    });

    sim.run();
}

#[test]
fn n_responses() {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let mut io: ConnectionIo<Message> = ConnectionIo::new();

        while let Some(mut c) = io.accept().await {
            tokio::spawn(async move {
                while let Ok(Message::Ping { how_many }) = c.recv().await {
                    for _ in 0..how_many {
                        let _ = c.send(Message::Pong).await;
                    }
                }
            });
        }
    });

    sim.client("client", async {
        let io: ConnectionIo<Message> = ConnectionIo::new();
        let mut c = io.connect("server").await.unwrap();

        let how_many = 3;
        let _ = c.send(Message::Ping { how_many }).await;

        for _ in 0..how_many {
            let m = c.recv().await;
            assert!(matches!(m, Ok(Message::Pong)));
        }
    });

    sim.run();
}

#[test]
fn server_concurrency() {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let mut io: ConnectionIo<Message> = ConnectionIo::new();

        while let Some(mut c) = io.accept().await {
            tokio::spawn(async move {
                while let Ok(Message::Ping { how_many }) = c.recv().await {
                    for _ in 0..how_many {
                        let _ = c.send(Message::Pong).await;
                    }
                }
            });
        }
    });

    let how_many = 3;

    for i in 0..how_many {
        sim.client(format!("client-{}", i), async move {
            let io: ConnectionIo<Message> = ConnectionIo::new();
            let mut c = io.connect("server").await.unwrap();

            let _ = c.send(Message::Ping { how_many }).await;

            for _ in 0..how_many {
                let m = c.recv().await;
                assert!(matches!(m, Ok(Message::Pong)));
            }
        });
    }

    sim.run();
}

#[test]
fn client_hangup() {
    let mut sim = Builder::new().build();

    let wait = Arc::new(Notify::new());
    let notify = wait.clone();

    sim.client("bob", async move {
        let mut io: ConnectionIo<Message> = ConnectionIo::new();
        let mut c = io.accept().await.unwrap();

        assert!(c.recv().await.is_err());
        assert!(c.send(Message::Ping { how_many: 1 }).await.is_err());

        notify.notify_one();
    });

    sim.client("sally", async move {
        let io: ConnectionIo<Message> = ConnectionIo::new();
        let _ = io.connect("bob").await;

        wait.notified().await;
    });

    sim.run();
}

#[test]
fn server_hangup() {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        let mut io: ConnectionIo<Message> = ConnectionIo::new();

        while let Some(c) = io.accept().await {
            drop(c);
        }
    });

    sim.client("client", async {
        let io: ConnectionIo<Message> = ConnectionIo::new();

        let mut c = io.connect("server").await.unwrap();

        assert!(c.recv().await.is_err());
        assert!(c.send(Message::Ping { how_many: 1 }).await.is_err());
    });

    sim.run();
}

#[test]
fn drop_io() {
    let mut sim = Builder::new().build();

    let how_many = 3;

    let wait = Arc::new(Notify::new());

    sim.host("server", || {
        let notify = wait.clone();

        async move {
            let mut io: ConnectionIo<Message> = ConnectionIo::new();

            for _ in 0..how_many {
                let mut c = io.accept().await.unwrap();

                tokio::spawn(async move {
                    let _ = c.recv().await;
                });
            }

            drop(io);
            notify.notify_one();
        }
    });

    sim.client("client", async move {
        let io: ConnectionIo<Message> = ConnectionIo::new();

        let mut conns = vec![];

        for _ in 0..how_many {
            conns.push(io.connect("server").await.unwrap());
        }

        wait.notified().await;

        for mut c in conns {
            assert!(c.recv().await.is_err());
            assert!(c.send(Message::Ping { how_many: 1 }).await.is_err());
        }
    });

    sim.run();
}
