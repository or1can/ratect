# 0009 — Adopting the `claims` plugin for documentation checks

## Status

Accepted — adopted immediately.

## Context

This repo carried four hand-rolled doc-integrity scripts under `tools/` —
`echoed-claims.py`, `spliced-docs.py`, `stale-claims.py`, `verify-docs.py`
— each written to catch one specific way prose in this repo had gone wrong
before (a stale claim, a spliced doc comment, a corrected sentence still
said elsewhere, an example whose real output had drifted). All four were
candidate lists or gates a contributor had to remember to run by hand before
a release; nothing enforced them on an ordinary commit.

Separately, the same author built [`claims`](https://github.com/or1can/claims),
a Claude Code plugin generalizing this exact problem — checking documentation
and agent-instruction claims against the code they describe — specifically so
the functionality could be shared across projects instead of re-implemented
per repo. By 0.2.2 it was a strict superset of what this repo's four scripts
did (each ported and generalized as one of its checks), plus three checks
with no local equivalent (`check-links`, `check-citations`, `claim-words`,
and a judgment-shaped `judgment-agent`), and it runs automatically on every
`git commit` via a hook rather than only when invoked by hand.

A first comparison run against this repo's own tree found and fixed 3 real
dead links via `check-links` alone — a defect class none of the four old
scripts could ever have caught.

## Decision

Adopt the `claims` plugin as this repo's doc-integrity tooling in place of
the four `tools/` scripts:

- Enable it via `.claude/settings.json` (`extraKnownMarketplaces`/
  `enabledPlugins`).
- Configure it for this repo via a new root `claims.toml`: `restatement`'s
  `extensions` gains `.rs` (this repo's claims live mostly in Rust doc
  comments, not just Markdown), and `claim-words` is opted into this repo's
  record-like docs (`AGENTS.md`, `CONTEXT.md`, `ROADMAP.md`, `RELEASES.md`,
  `TODO.md`, `decisions/*.md`, `docs/**/*.md`).
- Delete `tools/echoed-claims.py`, `tools/spliced-docs.py`,
  `tools/stale-claims.py`, `tools/verify-docs.py`, and their test files.
  `tools/changelog-section.py` (release-pipeline tooling, unrelated to doc
  integrity) stays.
- Simplify CI's `Tools Tests` job accordingly — it now only runs
  `changelog-section.py`'s own tests, so the full-history checkout and
  verbosity rationale written for `verify-docs.py`'s regression tests no
  longer apply.
- Rewrite `AGENTS.md`'s "Tooling & CI" section, and its guideline 15/16
  cross-references naming the old scripts, to describe the plugin's checks
  instead.

## Alternatives considered

- **Keep maintaining the four scripts independently.** Rejected: this is
  exactly the duplicate-maintenance problem the plugin exists to solve —
  the same author would otherwise re-fix the same class of bug in every
  project's own copy, as already happened once (three of `echoed-claims.py`'s
  own bugs were found and fixed after ratect's copy already existed).
- **Fork/vendor the plugin's checks into `tools/` instead of taking it as an
  external plugin.** Rejected: reintroduces the per-project duplication the
  plugin exists to avoid, and this repo would own a copy that drifts instead
  of sharing fixes across every project using the plugin.
- **Wait for the two noise issues found during adoption
  ([or1can/claims#9](https://github.com/or1can/claims/issues/9),
  [#10](https://github.com/or1can/claims/issues/10)) to be resolved
  upstream first.** Rejected: both are advisory-only checks that can never
  block a commit, so there is no correctness or gate regression to wait
  on — only more advisory noise than the old scripts produced in two
  specific checks, which is a review-time cost, not a safety one.

## Consequences

- This repo's doc-integrity checking now depends on an external,
  git-URL-sourced Claude Code plugin rather than in-repo scripts — pinned
  at install rather than always-latest (the plugin's own distribution
  model), so an upstream change can't silently alter what gates a commit
  here without a deliberate re-pin.
- Four noise/timeout findings from comparing 0.2.2 against the scripts it
  replaced were all filed upstream rather than worked around locally, and
  all four shipped fixes within days: `spliced-docs`'s weaker `unknown`
  evidence mode is now opt-in
  ([#9](https://github.com/or1can/claims/issues/9)); `restatement` no
  longer fires on text duplicated across many files by design, like a
  shared license header ([#12](https://github.com/or1can/claims/issues/12));
  `executable-claims`'s timeout is now advisory and configurable
  ([#8](https://github.com/or1can/claims/issues/8)); and `stale-claims`'s
  bare-name module matching gained the project-configurable scope this
  repo's own old tool had, via `claims.toml`'s `module_reference_scope`
  ([#10](https://github.com/or1can/claims/issues/10)) — evidence the "file
  it, don't route around it" alternative above was the right call.
- `check-citations` has no Rust support (Markdown and Swift comments only),
  so a dead citation inside a Rust doc comment still isn't caught — the same
  gap that existed before adoption, not a regression from it.
- Doc-integrity enforcement moves from "manual, run before a release" to
  "automatic, every commit" **inside a Claude Code session with the plugin
  enabled**: a gate finding (`check-links`, `check-citations`,
  `executable-claims`) blocks that commit outright via the `PreToolUse`
  hook. This has no reach into a `git commit` from a plain shell, another
  editor's Git integration, or a bot account (Renovate's PRs, routine here
  per guideline 12) — and, unlike the old `verify-docs.py`, none of these
  checks run in `.github/workflows/ci.yml` either, so a PR from any of those
  paths merges without them regardless. `verify-docs.py` itself was already
  no stronger than this pre-adoption (AGENTS.md's own prior text: "the
  checks themselves are not in CI... though their tests are"), so this
  isn't a new gap for that one check — but `check-links`/`check-citations`
  are introduced as "gate" severity with no prior local-only equivalent,
  so their real-world strength is now weaker than their label implies for
  anyone not running Claude Code. Wiring `claims` into CI directly would
  close this, but the plugin has no tagged releases yet to pin a CI checkout
  against — tracked in `TODO.md` rather than solved here.
- The CI `Tools Tests` job's own scope shrank from testing all four retired
  scripts to testing only `tools/changelog-section.py` (unrelated
  release-pipeline tooling, ticket #30) — it still passes, but no longer
  proves anything about doc-integrity logic; that proof now lives entirely
  in the `claims` repo's own CI, which this repo's contributors never see
  run against their own PRs.
