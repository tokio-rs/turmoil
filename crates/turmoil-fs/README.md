# turmoil-fs

A deterministic filesystem substrate for testing async Rust.

> **Status: scaffold.** The authoritative filesystem implementation
> currently lives in `turmoil::fs` (behind the `unstable-fs` feature).
> This crate is being carved out along the same kernel/shim split used
> by `turmoil-net`, and the public surface below is what it will expose
> once the lift lands. Not yet on crates.io.

## What it will be

`turmoil-fs` is a simulated filesystem. Production code imports the standard types; tests flip the import to `turmoil_fs::shim` behind a `#[cfg]` and the same code runs against a simulated filesystem you control.

```rust,ignore
#[cfg(not(test))]
use std::fs::OpenOptions;
#[cfg(test)]
use turmoil_fs::shim::std::fs::OpenOptions;
```

## Why

Tests that touch the real filesystem are nondeterministic — `fsync` timing, page-cache flushes, and dirty-writeback scheduling all vary run to run. Crash-consistency failures (a crash between `write` and `fsync`, a torn write, a write that returned success but was never persisted) are hard to provoke on a real kernel.

A mocked `File` returns the bytes you prime it with; it doesn't distinguish "in the page cache" from "on stable storage", so it can't tell you what would survive a crash.

`turmoil-fs` models the split explicitly: writes land in per-host pending state, `fsync` promotes them to durable, and a simulated crash discards the pending state. With a seeded RNG driving the pending/sync interleaving, crash-consistency scenarios become deterministic and replayable.

## Structure

- `kernel` — the authoritative filesystem implementation: per-host namespace, inode/file table, pending-vs-synced durability state, crash recovery, O_DIRECT alignment rules.
- `shim::std` / `shim::tokio` — drop-in replacements for the corresponding standard-library and tokio APIs. Every type, method, and error kind mirrors the upstream signature exactly.

## Using it today

While the lift is in progress, the filesystem simulation is usable through the `turmoil` crate behind its `unstable-fs` feature:

```toml
[dev-dependencies]
turmoil = { version = "0.7", features = ["unstable-fs"] }
```

See the [`turmoil` README](../turmoil/README.md) for crash-consistency examples.
