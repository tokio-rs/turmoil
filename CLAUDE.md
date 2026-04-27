# turmoil — Claude notes

Deterministic simulation-testing framework for distributed systems. See
`README.md` for user-facing docs and `CONTRIBUTING.md` for the full contributor
workflow.

## Workspace

- `crates/turmoil` — published core crate (`0.7.x`).
- `crates/turmoil-net` — scaffold; simulated socket layer. Not yet published.
- `crates/turmoil-fs` — scaffold; simulated filesystem. Not yet published.
- `examples/*` — non-published example crates that depend on `turmoil` via path.

## Commands

```sh
cargo test --workspace
cargo test --workspace --features regex
cargo fmt --check
cargo clippy -p turmoil -p turmoil-net -p turmoil-fs --all-targets -- --deny warnings
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
- `turmoil-net` and `turmoil-fs` are excluded (`release = false`) until their
  first manual publish to crates.io — release-plz can't create new crates.

To group changes into one release: leave the release PR open; it rolls in new
commits automatically on each push.

## Gotchas

- `examples/*` crates have `publish = false` — never add them to the release
  flow.
- The `unstable-fs` and `unstable-barriers` features in `turmoil` are not
  covered by semver; changes there don't need a major bump.
- README doctests run as part of `cargo test --workspace` via `doc-comment`;
  if you move `README.md`, fix the include path in
  `crates/turmoil/src/readme.rs`.
