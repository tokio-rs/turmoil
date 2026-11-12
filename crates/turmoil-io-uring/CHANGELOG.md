# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/tokio-rs/turmoil/compare/turmoil-io-uring-v0.1.0...turmoil-io-uring-v0.1.1) - 2026-11-12

### Other

- updated the following local packages: turmoil-fs
# 0.1.0

Initial release. The simulated `io_uring` was lifted out of `turmoil`'s
`unstable-io_uring` module into its own crate. `turmoil` continues to
expose the API as `turmoil::io_uring::*` (re-export) when the
`unstable-io_uring` feature is on, so existing consumers see no
source-level changes.

### Added

- `IoUring`, `Builder`, `Parameters` — the runtime ring instance. Mirrors
  the `io-uring` 0.7 crate's public API closely enough to swap imports
  behind a `#[cfg]`.
- `Submitter`, `SubmissionQueue`, `CompletionQueue`, `AsyncFd`,
  `AsyncFdReadyGuard`.
- `opcode::Read`, `opcode::Write`, `opcode::Fsync`, `opcode::AsyncCancel`.
- `squeue::{Entry, Flags, EntryMarker, PushError}` and
  `cqueue::{Entry, EntryMarker}` — data shapes available without the
  `fs` feature, useful for parity tests against the real `io-uring`
  crate.
- `host::IoUringHostState` with public `crash()` for the embedder's
  crash path; `install_host_accessor` and `install_combined_accessor`
  for embedding hooks.

### Features

- `fs` (optional) — wires the runtime to `turmoil-fs`. Without `fs`,
  only the data-shape types compile (no runtime).

### Behavior

- Submitting `IORING_SETUP_SQPOLL`, `IORING_SETUP_IOPOLL`, linked SQEs,
  fixed files / registered buffers / BufRing, multi-shot ops, or any
  opcode other than the four listed above surfaces as `EINVAL` —
  exactly as the real kernel would.
- `CompletionQueue::sync()` is required before iterating; without it,
  `next()` returns `None` even when CQEs are matured. Mirrors real
  Linux semantics so a consumer who forgets to sync sees the bug under
  the simulation instead of silently on Linux.
