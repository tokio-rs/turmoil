//! Run the real-socket tests in an isolated Linux network namespace.

use std::env;
use std::fs;
use std::process::Command;

fn checked(command: &mut Command) {
    let status = command.status().unwrap_or_else(|err| {
        panic!("failed to run {command:?}: {err}");
    });
    assert!(status.success(), "{command:?} failed: {status}");
}

#[test]
#[ignore = "requires unprivileged user/network namespaces and iproute2"]
fn udp_cast() {
    let binary = env::current_exe().unwrap();
    let namespace = fs::read_link("/proc/self/ns/net").unwrap();
    let Ok(parent_namespace) = env::var("TURMOIL_NET_PARENT_NETNS") else {
        checked(
            Command::new("unshare")
                .args(["--user", "--map-root-user", "--net"])
                .arg(&binary)
                .args(["--ignored", "--exact", "linux::udp_cast", "--nocapture"])
                .env("TURMOIL_NET_PARENT_NETNS", namespace),
        );
        return;
    };

    // Verify isolation before configuring interfaces or running socket tests.
    assert_ne!(namespace.to_str().unwrap(), parent_namespace);
    for args in [
        "link set lo up",
        "link add sim0 type dummy",
        "link set sim0 up multicast on",
        "addr add 10.0.0.1/24 dev sim0",
        "addr add 10.0.0.2/24 dev sim0",
        "-6 addr add fd00::1/64 dev sim0 nodad",
        "-6 addr add fe80::1/64 dev sim0 nodad",
        "route add 224.0.0.0/4 dev sim0",
        "-6 route add ff00::/8 dev sim0",
    ] {
        checked(Command::new("ip").args(args.split_whitespace()));
    }
    // Host-wide memberships require serial execution. Exclude this runner.
    checked(
        Command::new(binary)
            .args(["--ignored", "--skip", "linux::", "--test-threads=1"])
            .env("TURMOIL_NET_PRIVATE_NETNS", "1"),
    );
}
