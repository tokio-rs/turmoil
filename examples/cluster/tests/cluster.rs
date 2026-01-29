use std::time::Duration;

use cluster::{echo_client_cycle, machine_attrition, Cluster};
use tracing_subscriber::EnvFilter;

#[test]
fn retries() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or(EnvFilter::from("info")))
        .with_test_writer()
        .try_init();

    let test_duration = Duration::from_secs(10);

    let mut cluster = Cluster::new(50, Some(8877673959163467592));

    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(360))
        .build();

    cluster.setup(&mut sim);

    sim.client(
        "echo_client_cycle",
        echo_client_cycle(cluster.clone(), 50, test_duration),
    );

    sim.client(
        "attrition",
        machine_attrition(cluster.clone(), test_duration, 40, 10),
    );

    cluster.run(&mut sim).unwrap();
}
