# Contributing to turmoil

Thanks for your interest in contributing. This document covers the workflow for
landing changes in this repo.

## Workspace layout

```
crates/
  turmoil/       # published core crate
  turmoil-net/   # scaffold, not yet published
  turmoil-fs/    # scaffold, not yet published
examples/        # non-published example crates
```

## Local checks

Before opening a PR, run:

```sh
cargo fmt --check
cargo clippy -p turmoil -p turmoil-net -p turmoil-fs --all-targets -- --deny warnings
cargo test --workspace
cargo test --workspace --features regex
```

Public-API drift on `turmoil` is gated by `cargo-check-external-types` in CI.

## Commit style

This repo uses [Conventional Commits](https://www.conventionalcommits.org/).
`release-plz` parses commit messages to decide version bumps and to generate
the changelog, so following the convention matters.

Format:

```
<type>(<optional scope>): <short summary>

<optional body>

<optional footers>
```

Common types:

| Type       | Use for                                        | Bump  |
|------------|------------------------------------------------|-------|
| `feat`     | new user-visible functionality                 | minor |
| `fix`      | bug fix                                        | patch |
| `perf`     | performance improvement                        | patch |
| `docs`     | documentation only                             | none  |
| `refactor` | internal change, no behavior change            | none  |
| `test`     | tests only                                     | none  |
| `chore`    | tooling, CI, deps, etc.                        | none  |
| `build`    | build system / packaging                       | none  |

Breaking changes: append `!` after the type (e.g. `feat!: ...`) or add a
`BREAKING CHANGE:` footer. These produce a major bump (or a minor bump while
`0.x`).

Scopes are optional but useful when a change is localized to one crate:
`feat(net): ...`, `fix(fs): ...`, `fix(turmoil): ...`.

Examples:

```
feat(net): add UdpSocket::peek
fix: avoid panic when host crashes during send
refactor(turmoil): extract rng helpers
feat!: replace Builder::fs_sync with fs_sync_probability
```

## Releases

Releases are automated via [release-plz](https://release-plz.dev/).

1. Merge conventional-commit PRs into `main`.
2. release-plz opens (or updates) a **release PR** with version bumps and a
   generated `CHANGELOG.md`. Review it as you would any PR.
3. Merging the release PR publishes to crates.io, creates the git tag, and
   cuts a GitHub Release.

To group multiple changes into one release, just leave the release PR open;
it updates on each push to `main` and rolls in new commits automatically.

`turmoil-net` and `turmoil-fs` are not yet published — they are excluded from
the release flow in `release-plz.toml` until their first manual publish.

## License

By contributing, you agree that your contributions will be licensed under the
MIT license, as stated in the README.
