use std::{matches, time::Duration};
use tokio::time::timeout;
use turmoil::Builder;

#[derive(Debug)]
enum Message {
    Ping,
    Pong,
}

#[test]
fn ping_pong() {
    let mut sim = Builder::new().build();

    let client = sim.client("client");

    sim.register("server", |host| async move {
        loop {
            let (ping, src) = host.recv().await;
            assert!(matches!(ping, Message::Ping));

            host.send(src, Message::Pong);
        }
    });

    sim.run_until(async {
        client.send("server", Message::Ping);

        let (pong, _) = client.recv().await;
        assert!(matches!(pong, Message::Pong));
    });
}

#[test]
fn network_partition() {
    let mut sim = Builder::new().build();

    let client = sim.client("client");

    sim.register("server", |host| async move {
        loop {
            let (ping, src) = host.recv().await;
            assert!(matches!(ping, Message::Ping));

            host.send(src, Message::Pong);
        }
    });

    sim.run_until(async {
        // introduce the partition
        sim.partition("client", "server");

        client.send("server", Message::Ping);

        let res = timeout(Duration::from_secs(1), client.recv()).await;
        assert!(matches!(res, Err(_)));
    });
}

impl turmoil::Message for Message {
    fn write_json(&self, _dst: &mut dyn std::io::Write) {
        unimplemented!()
    }
}
