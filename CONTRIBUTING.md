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

Documentation is checked on every commit by the
[`claims`](https://github.com/or1can/claims) hook in
`.pre-commit-config.yaml`. Turn it on once per clone, with
[pre-commit](https://pre-commit.com):

```bash
pre-commit install
```

pre-commit brings the hook its own environment, built from the Python
pre-commit itself runs on, so that Python, not your `python3`, is the one
that needs to meet `claims`' minimum of 3.11.
`uv tool install --python '>=3.11' pre-commit` installs it that way.

That installs a `pre-push` hook too, which runs CI's claims checks over
the branch and refuses a push with an unmarked finding, as CI's `claims`
job would. A clone with only the `pre-commit` hook installed needs
`pre-commit install` run again to add it.

The commit hook itself refuses a commit that brings in an unmarked
`claim-words`, `temporal-words` or `restatement` finding, or leaves an
orphaned mark for one, such as a marked sentence's old mark after it is
reworded (the list is `claims.toml`'s `[hook] fail_on_unmarked`): fix or
mark the finding, as [AGENTS.md](AGENTS.md) describes, and prune the
orphan.

## The `tools/` scripts

The Python scripts under `tools/` (release notes extraction, the docs'
captures and their `ansi` preprocessor, the README check) need Python 3.11
or later and nothing outside the standard library. Run them with
[uv](https://docs.astral.sh/uv/), which fetches a matching Python when
yours is older:

```bash
uv run tools/check-readme-docs-list.py
uv run --python '>=3.11' -m unittest discover -s tools -p 'test_*.py'
```

Plain `python3 tools/<script>.py` works too on 3.11 or later; an older
`python3`, from 3.9, stops with a message naming the minimum. `mdbook build` runs the
docs' preprocessor through `uv`, so it needs `uv` on your PATH. CI tests at
the minimum and at the newest stable Python. The minimum is the oldest
Python still supported upstream, and rises when that version reaches end of
life; checking for that is part of cutting a release (AGENTS.md's Version
Lifecycle guideline).

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

## Developer Certificate of Origin

Every commit must be **signed off**:

```bash
git commit -s
```

This appends a `Signed-off-by: Your Name <you@example.com>` trailer to the commit
message. It is not a signature or an identity check — it's your affirmation of the
[Developer Certificate of Origin](https://developercertificate.org): that you wrote
the change (or otherwise have the right to submit it), and that you're submitting it
under the project's license. Worth knowing before you attest to it, since the
trailer is easy to add without realizing it carries that meaning.

CI checks every pull-request commit for a sign-off matching its author. If you
forgot one, fix up your branch with `git rebase --signoff` and force-push.

## License

Ratect is licensed under the [Apache License 2.0](LICENSE), and contributions are
accepted under the same license (inbound = outbound). There is no CLA — the DCO
sign-off above is all that's asked.
