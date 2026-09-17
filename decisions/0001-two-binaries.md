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
own patch version, so nobody runs a stale core. See this ADR's own Consequences
below for why, and [AGENTS.md](../AGENTS.md)'s Version Lifecycle guideline for
the actual release-cutting mechanics.

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
  vs. `ratect 0.3`) because forcing one number to serve both meanings breaks the
  moment they diverge — which they will, since `ratect-compat` has a head start.
  The core crate itself isn't published or meaningfully versioned on its own;
  it's an internal implementation detail, not something either binary's users
  interact with directly.
- **Tags are prefixed with the binary they release** — `ratect/vX.Y.Z`,
  `ratect-compat/vX.Y.Z` — because the two version lines would otherwise
  collide: `v0.2.0` was already taken, by `ratect-compat`'s own 0.2.0 back when
  it was the only binary. Bare `vX.Y.Z` tags (`v0.1.0` through `v0.21.0`) are
  that pre-split history and stay exactly as they are, and the `version` label
  on a created resource comes from the *binary*, not the core
  ([ADR-0002](0002-runtime-ownership-labels.md)).
- **One shared `CHANGELOG.md`, not one per binary.** Most substantive work is in
  `ratect-core` and so reaches both binaries — the anonymous-volume fix
  ([0.21.1](../RELEASES.md#ratect-compat)) is the pattern, not the exception —
  so two files would be largely the same prose under different headings,
  drifting apart on every core change. (That's the opposite of the CLI
  reference docs, which *are* split per binary: those overlap by almost
  nothing, since they document different flags. Split where the content
  differs, share where it doesn't.) An entry with no binary named applies to
  both; one that doesn't says `(ratect only)`/`(ratect-compat only)`, so the
  annotation cost falls on the rarer case. Revisit only if `ratect` diverges
  far enough that shared-core changes stop being the bulk of the work — 0.3.0's
  own config format is a step that way — since cutting one file in two later
  is easy, and merging two back into one isn't.
- **A release cycle bumps only the crates it actually changes.** A
  `ratect`-only cycle still moves `ratect-core` (the same shared crate, whose
  number has always run with the release cadence rather than standing still)
  and still leaves `ratect-compat` on a `-dev` of its own — a patch bump if
  nothing but the shared core moved underneath it, a minor one if it gained
  anything itself. Which of the two it turns out to be is decided at release
  time; the `-dev` number in between is a statement of intent, not a
  commitment.
- Every future "is this compat or forward-looking?" question has a home for its
  answer; this ADR is the one nearly every other decision leans on.
