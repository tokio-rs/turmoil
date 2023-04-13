use std::{collections::VecDeque, net::SocketAddr, time::Duration};

use rand::seq::SliceRandom;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use turmoil::{
    net::{TcpListener, TcpStream},
    Builder, LinkIter, Result, SentRef,
};

#[test]
fn replay_012() -> Result {
    run([
        ("client-0", "server"),
        ("client-1", "server"),
        ("client-2", "server"),
    ])
}

#[test]
fn replay_021() -> Result {
    run([
        ("client-0", "server"),
        ("client-2", "server"),
        ("client-1", "server"),
    ])
}

#[test]
fn replay_120() -> Result {
    run([
        ("client-1", "server"),
        ("client-2", "server"),
        ("client-0", "server"),
    ])
}

fn run(trace: impl Into<VecDeque<(&'static str, &'static str)>>) -> Result {
    let mut sim = Builder::new()
        // HACK: We should be able to use turmoil::hold here, but the ergonomics
        // are a bit clunky. Choosing a high message latency ensures we always
        // enqueue every packet on each step.
        .tick_duration(Duration::from_millis(10))
        .min_message_latency(Duration::from_millis(100))
        .min_message_latency(Duration::from_millis(100))
        .build();

    let how_many = 3;

    sim.client("server", server(1234, how_many));

    let addr = sim.lookup("server");
    for i in 0..how_many {
        sim.client(
            format!("client-{i}"),
            client((addr, 1234), format!("hello, world from {i}")),
        );
    }

    // step once to enqueue all the SYNs, let these deliver normally
    sim.step()?;
    sim.links(|l| {
        for link in l {
            link.deliver_all();
        }
    });

    // step and inspect the in flight packets, choosing a non-empty link at random
    // for delivery each iteration

    let mut trace = trace.into();

    loop {
        // enqueue packets
        sim.step()?;

        if trace.is_empty() {
            break;
        }
        let (a, b) = trace.pop_front().unwrap();
        let ip_1 = sim.lookup(a);
        let ip_2 = sim.lookup(b);

        sim.links(|l| {
            let mut links: Vec<LinkIter> = l.collect();
            links.shuffle(&mut rand::thread_rng());
            let mut links = links.into_iter();

            while let Some(link) = links.next() {
                let (from, to) = link.pair();

                let enqueued: Vec<SentRef> = link.collect();
                if enqueued.is_empty() {
                    continue;
                }

                if (ip_1 != from || ip_2 != to) && (ip_1 != to || ip_2 != from) {
                    continue;
                }

                for s in enqueued {
                    s.deliver();
                }

                return;
            }
        });
    }

    Ok(())
}

async fn client(addr: impl Into<SocketAddr>, msg: String) -> Result {
    let mut s = TcpStream::connect(addr.into()).await?;
    s.write_all(msg.as_bytes()).await?;

    Ok(())
}

async fn server(port: u16, how_many: usize) -> Result {
    let l = TcpListener::bind(("0.0.0.0", port)).await?;
    let (tx, mut rx) = mpsc::unbounded_channel();

    let mut accepted = 0;
    loop {
        let (mut s, _) = l.accept().await?;
        let tx = tx.clone();
        accepted += 1;

        tokio::spawn(async move {
            let mut msg = String::new();
            s.read_to_string(&mut msg)
                .await
                .expect("unable to recv msg");

            _ = tx.send(msg);
        });

        if accepted == how_many {
            break;
        }
    }

    drop(tx); // drop to start consuming messages

    println!("msg log:");
    while let Some(msg) = rx.recv().await {
        println!("{msg}");
    }

    Ok(())
}
