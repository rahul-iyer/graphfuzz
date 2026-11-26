# Contributing

Issues and pull requests are welcome.

- **A wrong answer found with graphfuzz:** include the reduced query from the report, the rows each engine
  returned, and the engine versions. Check with `graphfuzz repro` that it reproduces on the default fixture.
  Report the bug to the engine too; graphfuzz's README lists the ones found so far.
- **A new engine:** an endpoint that speaks an existing dialect is a `config.yml` entry. A new protocol is
  one `Engine` implementation in `src/engine.rs`; a new dialect is a profile in `profiles/`.
- **Before a pull request:** `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.

Contributions are dual licensed under MIT OR Apache-2.0, as the project is.
