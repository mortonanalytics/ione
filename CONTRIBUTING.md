# Contributing to IONe

Thanks for considering a contribution. IONe is **pre-alpha** and maintained by a small team — please read this before opening a PR.

## Before you file

- Open an issue first for non-trivial changes. The maintainers can tell you quickly whether the change fits the thesis (see [md/design/ione-v1.md](md/design/ione-v1.md)).
- Check [md/plans/ione-v1-plan.md](md/plans/ione-v1-plan.md) for the phased roadmap. Work that re-opens a closed phase's scope is likely to be deferred.

## Local dev

```bash
docker compose up -d postgres minio
cp .env.example .env
cargo sqlx database create
cargo sqlx migrate run
cargo run
```

Ollama running locally at `http://localhost:11434` is required for any live-LLM work. Pull the models IONe uses by default:

```bash
ollama pull llama3.2:latest qwen3:14b phi4-reasoning:14b qwen3:8b nomic-embed-text
```

## Code standards

- `cargo fmt` and `cargo clippy -- -D warnings` are CI gates. Run them locally before pushing.
- All new SQL lives under `migrations/NNNN_<slug>.sql`. After changing SQL, run `cargo sqlx prepare` and commit the `.sqlx/` metadata.
- Struct / column / field names come from the contract at [md/design/ione-v1-contract.md](md/design/ione-v1-contract.md). If you need a new name, add it to the contract first.
- DB uses snake_case. Rust fields use snake_case. JSON on the wire is camelCase (`#[serde(rename_all = "camelCase")]`).

## Tests

- Unit tests next to the code. Integration tests in `tests/phaseNN_*.rs`.
- All integration tests are `#[ignore]`-gated and run serially (`--test-threads=1`) against a shared live Postgres.
- Gate live external calls behind `IONE_SKIP_LIVE=1`; tests must pass with that env set in CI.
- Write tests **from the contract**, not from existing code. Contract-red first, then implement until green.

Run the full suite:

```bash
cargo test --test phase01_chat
DATABASE_URL=postgres://ione:ione@localhost:5433/ione \
  IONE_SKIP_LIVE=1 \
  cargo test -- --ignored --test-threads=1
```

## Commit messages

- Single-line subject, ≤72 chars.
- Body wraps at 80.
- No emoji.
- No co-author or "Generated with" trailers.

## PR checklist

- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy -- -D warnings` clean
- [ ] All prior phase tests green (regression)
- [ ] New feature ships with a `tests/phase*` addition OR an explicit note in the PR about why not
- [ ] Contract doc updated if any name or type changed
- [ ] CHANGELOG entry under an `## [Unreleased]` section

## Contributor License Agreement

Before a pull request can merge, you sign the [Contributor License Agreement](CLA.md) once, by commenting on the pull request as the CLA check instructs. The CLA lets Morton Analytics LLC license the project, your contribution included, under other terms in future without asking you again. You keep the copyright in your work.

The project itself is licensed under Apache 2.0.

## Contact

[morton@myma.us](mailto:morton@myma.us)
