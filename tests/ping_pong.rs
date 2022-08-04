use std::matches;
use turmoil::Builder;

#[derive(Debug)]
enum Message {
    Ping,
    Pong,
}

#[test]
fn ping_pong() {
    let mut sim = Builder::new().build::<Message>();

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
