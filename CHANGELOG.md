# 0.4 (Jan 10, 2023)

### Added

- Add more type conversions for ToSocketAddrs ([#71])
- Add host error support ([#67])

### Changed

- Make tokio unstable opt in ([#73])
- Rename ToSocketAddr to ToSocketAddrs ([#60])

[#73]: https://github.com/tokio-rs/turmoil/pull/73
[#71]: https://github.com/tokio-rs/turmoil/pull/71
[#67]: https://github.com/tokio-rs/turmoil/pull/67
[#60]: https://github.com/tokio-rs/turmoil/pull/60

# 0.3.3 (Dec 7, 2022)

### Fixed

- Fix host elapsed time across software restarts ([#65])

[#65]: https://github.com/tokio-rs/turmoil/pull/65

# 0.3.2 (Nov 14, 2022)

### Added

- Expose the sim's epoch and elapsed duration ([#54])

[#54]: https://github.com/tokio-rs/turmoil/pull/54

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