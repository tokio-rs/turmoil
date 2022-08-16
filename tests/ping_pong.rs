use std::{matches, time::Duration};
use tokio::time::timeout;
use turmoil::Builder;

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
