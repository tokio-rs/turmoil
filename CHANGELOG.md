# 0.5.3 (Mar 22, 2023)

### Added

- Expose whether a host has software running ([#108])
- DNS improvments ([#109])

### Fixed

- Fix AsyncRead impl for TcpStream ([#107])

[#107]: https://github.com/tokio-rs/turmoil/pull/107
[#108]: https://github.com/tokio-rs/turmoil/pull/108
[#109]: https://github.com/tokio-rs/turmoil/pull/109

# 0.5.2 (Mar 13, 2023)

### Added

- Add more support for UDP([#100], [#101], [#102])

[#100]: https://github.com/tokio-rs/turmoil/pull/100
[#101]: https://github.com/tokio-rs/turmoil/pull/101
[#102]: https://github.com/tokio-rs/turmoil/pull/102

# 0.5.1 (Mar 1, 2023)

### Added

- gRPC example ([#81])
- axum example ([#91])

### Fixed

- Handle task cancellation due to crashing a host ([#85])
- Remove runtime after host crashes ([#89])

[#81]: https://github.com/tokio-rs/turmoil/pull/81
[#85]: https://github.com/tokio-rs/turmoil/pull/85
[#89]: https://github.com/tokio-rs/turmoil/pull/89
[#91]: https://github.com/tokio-rs/turmoil/pull/91

# 0.5 (Feb 8, 2023)

### Added

- Expose a mechanism to manually drive the Sim ([#76])
- Add option to query hosts via regex ([#77])
- Add network topology introspection ([#78])

[#76]: https://github.com/tokio-rs/turmoil/pull/76
[#77]: https://github.com/tokio-rs/turmoil/pull/77
[#78]: https://github.com/tokio-rs/turmoil/pull/78

### Changed

- The following methods use a new trait (`ToIpAddrs`) for host lookup which
  includes the same implementations as `ToIpAddr`.
  - `Sim#bounce`
  - `Sim#crash`
  - `Sim#set_link_fail_rate`
  - `Sim#set_max_message_latency`
  - `repair`
  - `partition`
  - `release`
  - `hold`

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