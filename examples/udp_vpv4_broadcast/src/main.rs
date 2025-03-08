use std::{net::Ipv4Addr, time::Duration};
use tracing::info;
use turmoil::{IpVersion, net::UdpSocket};

const N_STEPS: usize = 3;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let tick = Duration::from_millis(100);
    let mut sim = turmoil::Builder::new()
        .tick_duration(tick)
        .ip_version(IpVersion::V4)
        .build();

    let broadcast_port = 9000;

    for server_index in 0..2 {
        sim.client(format!("server-{server_index}"), async move {
            let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, broadcast_port)).await?;

            let mut buf = [0; 1024];
            for _ in 0..N_STEPS {
                let (n, addr) = socket.recv_from(&mut buf).await?;
                let data = &buf[0..n];

                info!("UDP packet from {} has been received: {:?}", addr, data);
            }
            Ok(())
        });
    }

    sim.client("client", async move {
        let dst = (Ipv4Addr::BROADCAST, broadcast_port);
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        socket.set_broadcast(true)?;

        for _ in 0..N_STEPS {
            let _ = socket.send_to(&[1, 2, 3], dst).await?;
            info!("UDP packet has been sent");

            tokio::time::sleep(tick).await;
        }

        Ok(())
    });

    sim.run().unwrap();
}
