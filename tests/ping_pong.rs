use std::{matches, rc::Rc, time::Duration};
use tokio::{sync::Semaphore, time::timeout};
use turmoil::{Builder, Io};

#[derive(Debug)]
enum Message {
    Ping,
    Pong,
}

impl turmoil::Message for Message {
    fn write_json(&self, _dst: &mut dyn std::io::Write) {
        unimplemented!()
    }
}

#[test]
fn ping_pong() {
    let mut sim = Builder::new().build();

    sim.register("server", |host| async move {
        loop {
            let (ping, src) = host.recv().await;
            assert!(matches!(ping, Message::Ping));

            host.send(src, Message::Pong);
        }
    });

    sim.client("client", |host| async move {
        host.send("server", Message::Ping);

        let (pong, _) = host.recv().await;
        assert!(matches!(pong, Message::Pong));
    });

    sim.run();
}

#[test]
fn network_partition() {
    let mut sim = Builder::new().build();

    sim.register("server", |host| async move {
        loop {
            let (ping, src) = host.recv().await;
            assert!(matches!(ping, Message::Ping));

            host.send(src, Message::Pong);
        }
    });

    sim.client("client", |host| async move {
        // introduce the partition
        turmoil::partition("client", "server");

        host.send("server", Message::Ping);

        let res = timeout(Duration::from_secs(1), host.recv()).await;
        assert!(matches!(res, Err(_)));
    });

    sim.run();
}

#[test]
fn multiple_clients_all_finish() {
    let how_many = 3;
    let tick_ms = 10;

    // N = how_many runs, each with a different client finishing immediately
    for run in 0..how_many {
        let mut sim = Builder::new()
            .tick_duration(Duration::from_millis(tick_ms))
            .build();

        let ct = Rc::new(Semaphore::new(how_many));

        for client in 0..how_many {
            let ct = ct.clone();

            sim.client(format!("client-{}", client), |_: Io<Message>| async move {
                let ms = if run == client { 0 } else { 2 * tick_ms };
                tokio::time::sleep(Duration::from_millis(ms)).await;

                let p = ct.acquire().await.unwrap();
                p.forget();
            });
        }

        sim.run();
        assert_eq!(0, ct.available_permits());
    }
}
