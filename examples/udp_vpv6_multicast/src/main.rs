use std::{net::Ipv6Addr, time::Duration};
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
        .ip_version(IpVersion::V6)
        .build();

    let multicast_port = 9000;
    let multicast_addr = "ff08::1".parse().unwrap();

    for host_index in 0..2 {
        sim.client(format!("server-{host_index}"), async move {
            let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, multicast_port)).await?;
            socket.join_multicast_v6(&multicast_addr, 0)?;

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
        let dst = (multicast_addr, multicast_port);
        let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await?;

        for _ in 0..N_STEPS {
            let _ = socket.send_to(&[1, 2, 3], dst).await?;
            info!("UDP packet has been sent");

            tokio::time::sleep(tick).await;
        }

        Ok(())
    });

    sim.run().unwrap();
}
