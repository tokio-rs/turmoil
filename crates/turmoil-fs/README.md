# turmoil-fs

A deterministic filesystem substrate for testing async Rust.

## What it is

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

- The crate root holds the authoritative filesystem (`Fs`, `FsConfig`, `FsContext`, `FsHandle`): per-host namespace, inode/file table, pending-vs-synced durability state, crash recovery, O_DIRECT alignment rules.
- `shim::std` / `shim::tokio` — drop-in replacements for the corresponding standard-library and tokio APIs. Every type, method, and error kind mirrors the upstream signature exactly.

## Embedding

`turmoil-fs` is normally consumed via the `turmoil` umbrella crate behind its `unstable-fs` feature:

```toml
[dev-dependencies]
turmoil = { version = "0.7", features = ["unstable-fs"] }
```

If you are building a custom harness, call `install_host_accessor` once per simulation to plug a callback that hands `FsContext::current` (and `FsContext::current_if_set`) the current host's `Arc<Mutex<Fs>>`, simulated time, and RNG.

See the [`turmoil` README](../turmoil/README.md) for crash-consistency examples.
