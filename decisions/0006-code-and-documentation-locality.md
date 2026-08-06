# 0006 — Where code and its documentation live

## Status

Accepted — adopted incrementally from 0.25.0-dev. Generalises what
[0002](0002-runtime-ownership-labels.md) and
[0005](0005-build-ssh-keyring-placement.md) each decided once for their own
module.

## Context

Ratect's information architecture is **layer-oriented**, and its code is
mostly **feature-oriented**. The two have drifted apart, and the gap is where
this cycle's defects came from.

`ratect-core` is already two-thirds feature-shaped. `cache`, `git_include`,
`interrupt`, `labels`, `proxy`, `schema`, `ssh_agent`, `user` and
`expressions` are each one nameable thing, 135–769 lines. The exceptions are
the three layer modules — `config.rs` (3,221 lines), `docker.rs` (3,091) and
`engine.rs` (2,379) — which carry every feature's share of parsing,
orchestration and Docker calls.

Two measurements make the cost concrete.

**A feature's code is scattered across the layers, its documentation across
the project.** `build_ssh` touched 18 files. Most are irreducible bookkeeping
(CHANGELOG, ROADMAP, an ADR, generated schemas, fixtures). The *code* was
four: `ssh_agent.rs` — the feature's own module — plus the three layer
modules. Its *prose* was five more: three `docs/` pages and three separate
places in `AGENTS.md`.

**That scatter produced defects, not just inconvenience.** Three review rounds
found: two adjacent `build_ssh` errors disagreeing about whether they named
the offending container, because attribution was added per-site rather than
once; `docs/ratect-config-reference.md` claiming every field "applies, with
the same meaning" while linking into a page that had just gained
`ratect-compat`-only behaviour; and `AGENTS.md`'s `ssh-key` feature list
drifting from `Cargo.toml`. Each is a fact recorded in one place and
contradicted in another.

**`AGENTS.md` is ~18,000 tokens, read in full every session** — 10,000 of
which is per-module Architecture prose relevant only when that module is
open.

One empirical detector fell out of the measurements, and it is what makes the
otherwise-subjective "this file is too big" testable: **every module with a
`//!` doc comment is under 800 lines.** The modules without one split cleanly
in two — trivially self-describing (`proxy`, `user`, `expressions`; 135–154
lines) or too large to summarise (`config`, `docker`, `engine`). Nothing in
between lacks one. A module doc comment does not get written for a file whose
responsibility cannot be stated.

## Decision

Three rules.

**1. Documentation lives with what it describes.** How a module works is a
`//!` on that module — read exactly when the module is opened, and unable to
drift far from the code it sits above. A decision referenced from more than
one place is an ADR. User-facing behaviour is `docs/`. `AGENTS.md` becomes an
*index* — what exists, where, and one line on why — not the repository.

**2. A module is one nameable thing.** Where the layer boundary allows, a
feature gets its own module that the layer modules call into, rather than a
share of each of them. This is not new: `cache.rs` was extracted from
`config`/`engine`, and `ssh_agent.rs` was written that way from the start.
It is the existing precedent, stated.

**3. The size trigger is the doc comment, not the line count.** Before
splitting, try to write the module's `//!`. If it can be written in a few
sentences, the file is fine at any length. If it can only be written by
enumerating unrelated responsibilities, that enumeration is the split.

## What this does not mean

Feature grouping stops at boundaries that exist for stated reasons, all of
which outrank it:

- **`docker.rs` must not depend on `config` types.** That boundary is what
  makes `ContainerRuntime` fakeable, which is what lets the engine be tested
  without a daemon. A `build_ssh` module spanning config → Docker would
  destroy it. Conversion stays in `engine.rs`.
- **`ssh_agent` and `dockerignore` stay extractable** ([0005](0005-build-ssh-keyring-placement.md)),
  so they may not absorb Ratect types in the name of grouping.
- **The two-binary split** ([0001](0001-two-binaries.md)) and the two config
  formats are hard boundaries; a feature is documented on both sides, not
  merged into one page.

So `config.rs`/`docker.rs`/`engine.rs` remain, and remain large. The rule is
that they shed feature logic as it grows, not that they disappear.

## Alternatives considered

- **Leave `AGENTS.md` as the single repository of module knowledge.**
  Rejected: it is read in full every session for detail that is almost always
  irrelevant, and it has already been observed drifting from the code in the
  same cycle it documented.
- **Move the detail to linked files under `docs/internals/`.** Rejected, and
  this is the near miss: a linked file only helps someone who knows to open
  it, and the value of these notes is being seen *without* looking for them.
  A `//!` is read because the module is open. Splitting the test modules out
  (0.25.0-dev) is what made this viable — the layer files are now 2–3k lines
  rather than 9k, so they get read.
- **Split files on a line-count threshold.** Rejected: it is arbitrary,
  invites arguing about the number, and measures the symptom. `git_include.rs`
  at 769 lines is fine; a 400-line file doing three things is not.
- **Reorganise the crate feature-first** (`build_ssh/`, `caches/`, …, each
  spanning config → engine → Docker). Rejected: it breaks the layer
  boundaries above, and the `ContainerRuntime` seam in particular is load-
  bearing for the entire test strategy.

## Consequences

- `AGENTS.md`'s Architecture section becomes a per-module index; its prose
  moves into each module's `//!`. Expected to remove ~10,000 tokens from
  every session's fixed cost, and to put each note where it can be checked
  against the code it describes.
- A new feature defaults to its own module. Adding a third feature's worth of
  logic to `config.rs`'s resolution pass is the smell this exists to catch.
- **rustdoc becomes load-bearing documentation.** Module docs are no longer
  optional colour; a module without a `//!` is either trivial or overdue for
  a split, and reviewers can ask which.
- The three layer modules stay on the exceptions list until their feature
  logic has moved out. That is a direction, not a scheduled task — none of it
  is worth doing as a standalone churn commit, and all of it is cheap when
  the next change touches that code anyway.
