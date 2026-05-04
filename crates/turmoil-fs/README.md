# turmoil-fs

A deterministic filesystem substrate for testing async Rust.

> **Status: scaffold.** The authoritative filesystem implementation
> currently lives in `turmoil::fs` (behind the `unstable-fs` feature).
> This crate is being carved out along the same kernel/shim split used
> by `turmoil-net`, and the public surface below is what it will expose
> once the lift lands. Not yet on crates.io.

## What it will be

`turmoil-fs` is a simulated filesystem — a POSIX-shaped kernel behind shims that mirror `std::fs`, `std::os::unix::fs`, and `tokio::fs`. Production code imports the standard types; tests flip the import to `turmoil_fs::shim` behind a `#[cfg]` and the same code runs against a simulated filesystem you control.

```rust,ignore
#[cfg(not(test))]
use std::fs::OpenOptions;
#[cfg(test)]
use turmoil_fs::shim::std::fs::OpenOptions;
```

No wrapper types, no trait objects, no conditional method calls in the code under test.

It is not a full simulation runtime — it doesn't own your scheduler or intercept time. It's the filesystem piece. If you want the rest, the `turmoil` crate (one level up in this repo) layers on top once those stories are solid.

## Why

Tests that touch the real filesystem are nondeterministic: `fsync` timing, page-cache flushes, kernel scheduling of dirty writebacks, and even filesystem-specific durability semantics all vary run to run. The failure modes that matter for a durable system — a crash between `write` and `fsync`, a torn write, a successful `write` that was never actually persisted — are painful-to-impossible to provoke on a real kernel.

Mocks stop one layer too shallow. A mocked `File` returns the bytes you prime it with — it doesn't model the split between "in the page cache" and "on stable storage", or what survives a crash. Those are the bugs that matter, and mocks paper right over them.

`turmoil-fs` models the split explicitly: writes land in a per-host pending state, `fsync` promotes them to durable, and a simulated crash discards the pending state. Combined with a seeded RNG, this turns crash-consistency testing from "run fio in a loop" into a single deterministic test you can shrink and replay.

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
