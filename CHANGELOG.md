# 0.3.1 (Nov 9, 2022)

### Added

- Add local/peer addrs to tcp types
- Expose host elapsed time

### Changed

- Use tracing levels for different network events ([#53])

[#53]: https://github.com/tokio-rs/turmoil/pull/53

### Fixed

- Fix host crash behavior ([#52])

[#52]: https://github.com/tokio-rs/turmoil/pull/52

# 0.3.0 (Oct 28, 2022)

### Added

- Bind to multiple ports per host
- Simulated networking (UDP and TCP) that mirror tokio::net
- Client host error handling

### Changed

- Logging uses `tracing` for writing events

# 0.2.0 (Aug 11, 2022)

- Initial release