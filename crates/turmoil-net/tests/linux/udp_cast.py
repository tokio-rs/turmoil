#!/usr/bin/env python3
"""Run udp_cast_conformance against real Tokio in a private Linux namespace.

Requires Linux unprivileged user/network namespaces, iproute2, Python, and Cargo.
Run from the workspace root:
    python3 crates/turmoil-net/tests/linux/udp_cast.py
"""

import json
import os
from pathlib import Path
import subprocess
import sys


def ip(*args):
    subprocess.run(["ip", *args], check=True)


if len(sys.argv) > 1 and sys.argv[1] == "--inside":
    assert os.readlink("/proc/self/ns/net") != sys.argv[2]
    ip("link", "set", "lo", "up")
    ip("link", "add", "sim0", "type", "dummy")
    ip("link", "set", "sim0", "up", "multicast", "on")
    for addr in ["10.0.0.1/24", "10.0.0.2/24"]:
        ip("addr", "add", addr, "dev", "sim0")
    ip("-6", "addr", "add", "fd00::1/64", "dev", "sim0", "nodad")
    ip("-6", "addr", "add", "fe80::1/64", "dev", "sim0", "nodad")
    ip("route", "add", "224.0.0.0/4", "dev", "sim0")
    ip("-6", "route", "add", "ff00::/8", "dev", "sim0")
    env = dict(os.environ, TURMOIL_NET_PRIVATE_NETNS="1")
    # The tests share host-wide memberships. Serial execution is essential.
    sys.exit(subprocess.call([sys.argv[3], "--ignored", "--test-threads=1"], env=env))

env = dict(os.environ)
env["RUSTFLAGS"] = env.get("RUSTFLAGS", "") + " --cfg parity_tokio"
build = subprocess.run(
    ["cargo", "test", "-p", "turmoil-net", "--test", "udp_cast_conformance",
     "--offline", "--no-run", "--message-format=json"],
    env=env, text=True, stdout=subprocess.PIPE, check=True,
)
artifacts = [json.loads(line) for line in build.stdout.splitlines()]
binary = next(a["executable"] for a in artifacts
              if a.get("reason") == "compiler-artifact" and a.get("executable"))
sys.exit(subprocess.call([
    "unshare", "--user", "--map-root-user", "--net",
    sys.executable, str(Path(__file__).resolve()), "--inside",
    os.readlink("/proc/self/ns/net"), binary,
]))
