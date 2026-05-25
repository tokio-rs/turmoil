# turmoil-io_uring

Simulated [`io_uring`](https://kernel.dk/io_uring.pdf) for deterministic testing in Rust.

## What it is

`turmoil-io_uring` mirrors the [`io-uring` 0.7](https://docs.rs/io-uring/0.7) crate's API closely enough that consumers can flip a feature flag and swap their `use io_uring::*` imports for `use turmoil_io_uring::*`, then run the same code on macOS or Windows under a turmoil simulation.

It is **not** a real `io_uring` — there is no kernel ring, no SQ/CQ mmap, no `io_uring_enter` syscall. SQEs are queued in per-host Rust state, and CQEs become observable as simulated time advances past per-op completion timestamps.

## Features

- `fs` (optional) — wires the io_uring runtime to `turmoil-fs` so submitted ops actually execute against a simulated filesystem. Without `fs`, only the data-shape types (`Entry`, `Flags`, `opcode::*`) compile — useful for parity tests against the real `io-uring` crate, but not runnable.

## Usage

Normally consumed through the `turmoil` umbrella crate's `unstable-io_uring` feature. See [`turmoil`](https://crates.io/crates/turmoil) for the integration shape.
