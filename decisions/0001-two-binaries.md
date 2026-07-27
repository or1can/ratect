# 0001 — Two binaries (`ratect` and `ratect-compat`) on a shared core

**Status:** Accepted — shipped (the split landed in 0.20.0).

## Context

Ratect exists to replace the (now-unmaintained) JVM-based Batect. That creates
two goals that pull in opposite directions:

- **Strict compatibility** — a drop-in replacement has to match Batect's CLI and
  `batect.yml` format flag-for-flag and field-for-field, forever, or it isn't a
  replacement. Every deviation is a migration cost pushed onto users.
- **Forward-looking design** — the interesting work (a subcommand CLI, a native
  config format, better completions, modern-Rust-CLI conventions) requires the
  freedom to *diverge* from Batect's interface, which strict compatibility
  forbids.

One binary can't credibly be both. A single CLI that tried would either freeze
its interface to Batect's (no room to improve) or drift from it (no longer a
drop-in), and every feature would carry a "does this break compat?" tax.

## Decision

A **Cargo workspace with a shared core library and two thin binary crates**:

- **`ratect-core`** — all reusable logic: config parsing, the task engine, the
  `ContainerRuntime`/Docker integration, the UI layer. No CLI-specific code. This
  is what any binary depends on.
- **`ratect-compat`** — a strict, literal, flag-for-flag and field-for-field
  match for Batect's CLI and `batect.yml`. All [Batect Parity](../ROADMAP.md#batect-parity)
  work lands here, scoped by the tables in
  [Differences from Batect](../docs/differences-from-batect.md). Its only job is
  being a boring, reliable drop-in; it is *not* where new ideas go.
- **`ratect`** — the forward-looking CLI, free to diverge: subcommands
  (`ratect run <task>`, `ratect tasks list`), and — from 0.3.0 — a native config
  format ([ADR-0003](0003-ratect-native-config-format.md)). `ratect-compat` stays
  YAML-only, permanently, because that's what Batect compatibility requires.

Two consequences of the split were decided alongside it:

- **No binary is literally named `batect`** — that edges toward a trademark/naming
  concern and is confusing. Anyone who wants their existing `./batect` wrapper or
  `PATH` entry to keep working renames or symlinks `ratect-compat` themselves.
- **A migration path** from a `ratect-compat`-managed project to a `ratect`-managed
  one is a roadmap goal in its own right, not a side effect — enabled *because*
  both binaries lower to the same core types (see
  [ADR-0003](0003-ratect-native-config-format.md)'s `config convert`).

Both binaries are **versioned independently** (different maturity clocks) but
share a **release process** — a core fix ships for both at once, each bumping its
own patch version, so nobody runs a stale core. See
[Versioning & Releases](../ROADMAP.md#versioning--releases) for the mechanics.

## Alternatives considered

- **One binary evolving through phases, eventually deprecating compatibility.**
  Rejected: it makes Batect compatibility a temporary state to be sunset, when
  for many users it's the *entire* value proposition indefinitely. It also forces
  every user through a breaking transition on the tool's schedule, not theirs.
- **One binary with a `--compat` mode / flag.** Rejected: the two interfaces
  aren't a flag apart — they differ in argument *structure* (flat vs.
  subcommands), config *format*, and error *wording*. A mode switch would be two
  CLIs wearing one coat, with every code path branching on it.
- **A published, independently-versioned `ratect-core`.** Rejected: the core is
  an internal implementation detail, not something either binary's users interact
  with. Publishing it would invite external coupling to an interface we want to
  keep free to change.

## Consequences

- The parity/divergence tension is resolved *structurally* — a change is either
  `ratect-compat`'s (must match Batect) or `ratect`'s (free), and the crate it
  lives in says which, with no per-feature compat tax.
- Shared behaviour is proven once (in `ratect-core`'s fake-`ContainerRuntime`
  tests, and end-to-end via `ratect-compat`'s fixtures) rather than twice — which
  is why most fixtures live under `ratect-compat/` even though they exercise core
  engine behaviour, not the flat CLI. See
  [AGENTS.md / CLAUDE.md](../CLAUDE.md) "Where a fixture lives — by *layer*".
- The two binaries carry **independent version lines** (e.g. `ratect-compat 0.24`
  vs. `ratect 0.3`), which is why release tags are prefixed (`ratect/vX.Y.Z`,
  `ratect-compat/vX.Y.Z`) and the `version` label on created resources comes from
  the *binary*, not the core ([ADR-0002](0002-runtime-ownership-labels.md)).
- Every future "is this compat or forward-looking?" question has a home for its
  answer; this ADR is the one nearly every other decision leans on.
