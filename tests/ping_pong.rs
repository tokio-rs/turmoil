use std::matches;
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

    sim.run_until(async move {
        client.send("server", Message::Ping);

        let (pong, _) = client.recv().await;
        assert!(matches!(pong, Message::Pong));
    });
}

impl turmoil::Message for Message {
    fn write_json(&self, _dst: &mut dyn std::io::Write) {
        unimplemented!()
    }
}
