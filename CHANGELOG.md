# 0.5.8 (November 6, 2023)

### Fixed

- Fix subtraction overflow bug with latency ([#147]) 
- Fix ephemeral port leak upon tcp stream shutdown ([#145]) 

[#147]: https://github.com/tokio-rs/turmoil/pull/147
[#145]: https://github.com/tokio-rs/turmoil/pull/145

# 0.5.7 (October 20, 2023)

### Added

- Add reverse DNS resolution capabilities ([#141])

### Fixed

- Fix duplicate FIN in the drop glue ([#142])
- Extend loopback workaround ([#140])

[#142]: https://github.com/tokio-rs/turmoil/pull/142
[#141]: https://github.com/tokio-rs/turmoil/pull/141
[#140]: https://github.com/tokio-rs/turmoil/pull/140

# 0.5.6 (July 24, 2023)

### Added

- Return io::Error instead of panicking ([#130])
- Add network manipulation capabilities to Sim ([#129])
- Tracing improvments ([#124])

[#124]: https://github.com/tokio-rs/turmoil/pull/124
[#129]: https://github.com/tokio-rs/turmoil/pull/129
[#130]: https://github.com/tokio-rs/turmoil/pull/130

# 0.5.5 (June 6, 2023)

### Added

- Make socket buffer capacity configurable ([#121])

### Fixed

- Fix reverse dns lookups on static binds ([#120])

[#120]: https://github.com/tokio-rs/turmoil/pull/120
[#121]: https://github.com/tokio-rs/turmoil/pull/121

# 0.5.4 (May 23, 2023)

### Added

- Support ephemeral port assignments on bind ([#110])
- Add support for Ipv6 network/binds ([#113])
- Add support for loopback ([#118]) 

### Fixed

- Filter out equal IpAddrs for regex matching ([#116]) 
- Remove inconsistent address resolution ([#114])
- Fix bug in binding an in use port ([#117]) 

[#110]: https://github.com/tokio-rs/turmoil/pull/110
[#113]: https://github.com/tokio-rs/turmoil/pull/113
[#114]: https://github.com/tokio-rs/turmoil/pull/114
[#116]: https://github.com/tokio-rs/turmoil/pull/116
[#117]: https://github.com/tokio-rs/turmoil/pull/117
[#118]: https://github.com/tokio-rs/turmoil/pull/118

# 0.5.3 (Mar 22, 2023)

### Added

- Expose whether a host has software running ([#108])
- DNS improvements ([#109])

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
