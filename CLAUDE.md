# turmoil — Claude notes

Deterministic simulation-testing framework for distributed systems. See
`README.md` for user-facing docs and `CONTRIBUTING.md` for the full contributor
workflow.

## Workspace

- `crates/turmoil` — published umbrella crate (`0.7.x`); re-exports the others.
- `crates/turmoil-net` — published simulated socket layer (`0.1.x`).
- `crates/turmoil-fs` — published simulated filesystem (`0.1.x`).
- `crates/turmoil-io_uring` — published simulated io_uring (`0.1.x`); optional `fs` feature.
- `examples/*` — non-published example crates that depend on `turmoil` via path.

## Commands

```sh
cargo test --workspace
cargo test --workspace --features regex
cargo test -p turmoil --features unstable-fs --test fs
cargo test -p turmoil --features unstable-io_uring --test fs --test io_uring_conformance
cargo fmt --check
cargo clippy -p turmoil -p turmoil-net -p turmoil-fs -p turmoil-io_uring --all-targets -- --deny warnings
```

Public-API drift on `turmoil` is gated by `cargo-check-external-types` (nightly
toolchain pinned in `.github/workflows/rust.yml`). Allowed external types are
listed in `crates/turmoil/Cargo.toml` under
`[package.metadata.cargo_check_external_types]`.

## Commit style

Conventional Commits. `release-plz` parses these to pick version bumps and
generate the changelog, so this is load-bearing — not cosmetic.

- `feat:` → minor bump, `fix:`/`perf:` → patch bump.
- `docs:`, `refactor:`, `test:`, `chore:`, `build:` → no bump.
- Breaking: `feat!:` or `BREAKING CHANGE:` footer.
- Scope when localized: `feat(net):`, `fix(fs):`, `fix(turmoil):`.

See `CONTRIBUTING.md` for the full table and examples.

## Releases

Automated by release-plz (`.github/workflows/release-plz.yml`,
`release-plz.toml`):

- Every push to `main` opens/updates a "release PR" with version bumps +
  `CHANGELOG.md`.
- Merging that PR publishes to crates.io, tags, and cuts a GitHub Release.
- All four crates participate in the release flow.

To group changes into one release: leave the release PR open; it rolls in new
commits automatically on each push.

## Gotchas

- `examples/*` crates have `publish = false` — never add them to the release
  flow.
- The `unstable-fs`, `unstable-io_uring`, and `unstable-barriers` features
  in `turmoil` are not covered by semver; changes to the lifted crates
  (`turmoil-fs`, `turmoil-io_uring`) don't need a major bump.
- `turmoil` widens some `Fs` fields/methods to `pub` so `turmoil-io_uring`
  (a sister crate) can call them with `&mut Fs`. This is the
  published-but-unstable surface — do not treat it as stable for end users.
