# Architecture Decision Records

Contributor-facing records of the **cross-cutting** decisions that shape Ratect —
the ones referenced from more than one place, that don't belong to any single
version's [`ROADMAP.md`](../ROADMAP.md) entry. This is *not* user documentation
(that's [`docs/`](../docs/)) and not a changelog (that's
[`CHANGELOG.md`](../CHANGELOG.md)); it's the "why" behind the load-bearing
choices, kept where they can be linked to instead of re-explained.

## What earns an ADR (and what doesn't)

The line is **cross-cutting vs. version-scoped**:

- **Cross-cutting → an ADR here.** A decision that transcends one release and
  gets cross-referenced from several places — the two-binary split (every entry
  leans on it), the runtime-ownership labels (`resources`, `schema`, `doctor`,
  the docs domain all touch it), the native config format (config + includes +
  the migration verb). Filing these under one version's bullet buries them; a
  linkable record doesn't.
- **Version-scoped → stays inline in `ROADMAP.md`.** A decision that belongs to
  one release reads fine in that release's entry, using the roadmap's existing
  **"Scope, settled before building:" / "As built:"** subsection pattern (see the
  git-includes and orphaned-resource entries). Don't extract these — it buys
  nothing and scatters the reasoning away from the plan it explains.

Practical trigger: a decision earns an ADR the moment it's about to be
referenced from a *second* place. Most decisions never cross that line.

We deliberately **do not backfill exhaustively** — an ADR is point-in-time by
nature, so a mix of "recorded here" and "still inline in the roadmap" is the
expected state, not debt. The seed set below is only the genuinely foundational,
multiply-referenced decisions.

## Format

Each record is `NNNN-short-slug.md`, numbered in logical/foundational order (not
strictly creation order, since the seed set is curated). Keep the roadmap's dense,
reasoning-rich voice — an ADR has room for headers, so it should read *more*
clearly than the bullet it replaces, not less. Sections:

- **Status** — `Accepted — shipped (<version>)`, `Accepted — planned (<version>)`,
  `Superseded by [NNNN]`, etc.
- **Context** — the forces and constraints that made a decision necessary.
- **Decision** — what was chosen, in enough detail to implement against.
- **Alternatives considered** — what was rejected, and *why* — the part that
  stops the question being re-litigated later.
- **Consequences** — what this commits us to, including the awkward parts.

When a decision is superseded, mark the old record's status and link forward;
never delete one (same append-only spirit as the roadmap's versioned lists).

## Index

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-two-binaries.md) | Two binaries (`ratect` + `ratect-compat`) on a shared core | Accepted — shipped (0.20.0) |
| [0002](0002-runtime-ownership-labels.md) | Runtime-ownership labels (`eu.orican.ratect.*`) | Accepted — shipped (ratect-compat 0.21.1 · ratect 0.2.0) |
| [0003](0003-ratect-native-config-format.md) | `ratect`-native config format (TOML) | Accepted — shipped (ratect 0.3.0) |
| [0004](0004-git-include-host-path-trust.md) | Trusting a Git include's host paths (`allow_host_paths`) | Accepted — implemented |
| [0005](0005-build-ssh-keyring-placement.md) | Where `build_ssh`'s ssh-agent keyring lives | Accepted — implemented (ratect-compat 0.25.0) |
| [0006](0006-code-and-documentation-locality.md) | Where code and its documentation live | Accepted — adopted incrementally |
