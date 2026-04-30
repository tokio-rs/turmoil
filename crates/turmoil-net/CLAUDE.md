# turmoil-net

## Determinism

This crate exists to make networking deterministic. Anything that
iterates must do so in a fixed order, run to run.

- Use `IndexMap` / `IndexSet`, not `HashMap` / `HashSet`, whenever
  the collection is iterated, drained, or its keys are visited in
  any order-sensitive way. `HashMap` is only acceptable for
  pure keyed lookup with no iteration — and even then, prefer
  `IndexMap` for consistency.
- `IndexMap::remove` is `swap_remove` (reorders). Use `shift_remove`
  to preserve insertion order.
- Never rely on `HashMap` iteration order even when "it works today"
  — the randomized hasher will bite a future test.

## Errors vs. panics

- `io::Error` is for conditions real code can observe and handle:
  bad user input, address conflicts, connection refused, etc.
- `panic!` / `assert!` / `.expect(...)` is for structural invariants
  that a bug — not a caller — would violate: an `Fd` that should
  exist, a socket `Type` the dispatch layer already narrowed. A
  panic here surfaces the bug; an `io::Error` hides it.

## Tokio mirror

`shim::tokio::net` mirrors `tokio::net` *exactly* — same type names,
same method signatures, same error kinds. The whole point is that
production code swaps the import behind a `#[cfg]` and compiles
unchanged. When adding a method, check the tokio source; match it
to the letter, including error semantics.
