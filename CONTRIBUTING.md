# Contributing to Ratect

Thanks for your interest! A few things to know before diving in.

## Project stage

Ratect is pre-1.0 and evolving quickly, with a deliberate release plan — see
[ROADMAP.md](ROADMAP.md). **Please open an issue to discuss before starting
non-trivial work**: a feature may already be scheduled for a specific release, or
deliberately deferred, and it's better to find that out before writing code.

## Development setup

Everything is standard Cargo:

```bash
cargo build --workspace          # build
cargo test --workspace           # unit tests (no Docker needed)
cargo test --workspace --test cli -- --ignored   # integration tests (real Docker daemon required)
```

Before submitting, make sure these pass — CI enforces all three:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Conventions

- **Commits** follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `chore:`, …), with concise summaries; a body only when
  it explains non-obvious *why*.
- **Compatibility**: config parsing (`batect.yml`) should stay compatible with
  Batect's format — [Differences from Batect](docs/differences-from-batect.md) is
  the source of truth for current status, and changes to user-visible behavior must
  update the relevant `docs/` pages and `CHANGELOG.md` in the same change.
- **Repo conventions in depth** live in [AGENTS.md](AGENTS.md) — architecture
  notes, dependency rationale, and the guidelines both human and AI contributors
  follow here.

## License

By contributing, you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE), the project's license (inbound = outbound). No CLA.
