# 0.1.0

Initial release. The simulated filesystem was lifted out of `turmoil`'s
`unstable-fs` module into its own crate. `turmoil` continues to expose
the API as `turmoil::fs::*` (re-export) when the `unstable-fs` feature
is on, so existing consumers see no source-level changes.

### Added

- `Fs`, `FsConfig`, `FsContext`, `FsHandle`, `FsHandleGuard`,
  `FsCorruption`, `IoLatency`, `PageCacheConfig`, `PageCache` — the
  authoritative filesystem implementation: per-host namespace,
  inode/file table, pending-vs-synced durability state, crash recovery,
  O_DIRECT alignment rules.
- `shim::std::fs` / `shim::std::os::unix::fs` — drop-in replacements
  for `std::fs` and `std::os::unix::fs`.
- `shim::tokio::fs` — drop-in replacement for `tokio::fs`.
- `install_host_accessor` / `HostBorrow` / `HostBorrowFn` — embedding
  hooks for harnesses other than `turmoil`.
- `install_corruption_hook` — fired on every silent-corruption event,
  used by `turmoil`'s `unstable-barriers` integration.
- Public crash API: `Fs::crash(&mut self, rng)`. Used by
  `turmoil::Sim::crash`.
