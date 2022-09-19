use std::{
    matches,
    rc::Rc,
    sync::{atomic::AtomicUsize, atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{sync::Semaphore, time::timeout};
use turmoil::{io, Builder};

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

    sim.host("server", || async {
        loop {
            let (ping, src) = io::recv().await;
            assert!(matches!(ping, Message::Ping));

            io::send(src, Message::Pong);
        }
    });

    sim.client("client", async {
        io::send("server", Message::Ping);

        let (pong, _) = io::recv().await;
        assert!(matches!(pong, Message::Pong));
    });

    sim.run();
}

#[test]
fn network_partition() {
    let mut sim = Builder::new().build();

    sim.host("server", || async {
        loop {
            let (ping, src) = io::recv().await;
            assert!(matches!(ping, Message::Ping));

            io::send(src, Message::Pong);
        }
    });

    sim.client("client", async {
        // introduce the partition
        turmoil::partition("client", "server");

        io::send("server", Message::Ping);

        let res = timeout(Duration::from_secs(1), io::recv::<Message>()).await;
        assert!(matches!(res, Err(_)));
    });

    sim.run();
}

#[test]
fn bounce() {
    let mut sim = Builder::new().build();

    // The server publishes the number of requests it thinks it processed into
    // this usize. Importantly, it resets when the server is rebooted.
    let reqs = Arc::new(AtomicUsize::new(0));
    let publish = reqs.clone();

    sim.host("server", move || {
        let publish = publish.clone();
        let mut reqs = 0;
        async move {
            loop {
                let (ping, src) = io::recv().await;
                assert!(matches!(ping, Message::Ping));
                reqs += 1;
                publish.store(reqs, Ordering::SeqCst);

                io::send(src, Message::Pong);
            }
        }
    });

    for i in 0..3 {
        sim.client(format!("client-{}", i), async {
            io::send("server", Message::Ping);

            let (pong, _) = io::recv().await;
            assert!(matches!(pong, Message::Pong));
        });

        sim.run();

        // The server always thinks it has only server 1 request.
        assert_eq!(1, reqs.clone().load(Ordering::SeqCst));
        sim.bounce("server");
    }
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

            sim.client(format!("client-{}", client), async move {
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
