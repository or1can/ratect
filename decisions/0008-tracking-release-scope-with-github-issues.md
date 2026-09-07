# 0008 — Tracking release scope with GitHub Issues, not RELEASES.md

## Status

Accepted — adopted immediately for the in-progress `ratect-compat 0.27.0` /
`ratect 0.6.0` release's *remaining* scope. A GitHub Milestone named
`ratect-compat 0.27.0 · ratect 0.6.0` exists, holding the five issues (#2-#6)
this decision's own originating work produced.

`RELEASES.md`'s existing `0.6.0` entry is not rewritten by this decision —
it still holds the live scope-then-strikethrough record of the first six
candidates, landed under the old convention before this decision was made.
The **append-only guarantee protects a version once it ships** (tagged,
bumped off `-dev`) — not while it's still being written. So that entry is
expected to be rewritten *once*, wholesale, into this decision's short
retrospective form when `0.6.0` actually ships (covering both candidate
batches, old and new), rather than gaining more hand-written strikethrough
bullets for issues #2-#6 as they land. No action is needed on it now.

## Context

`RELEASES.md` is the append-only, per-binary record of what each version
scoped before it was built and what it turned into — 1,799 lines at the time
of this decision, with no mechanism to ever shrink, since every entry stays
forever by design. Its own maintenance rule (`AGENTS.md` guideline 9) is a
two-phase, hand-edited document per version: write the scope, then come back
after shipping and strikethrough each completed item with a done-summary — a
`~~strikethrough~~` that must not cross a blank line, a rendering hazard
worth its own caveat in that guideline.

Separately, [decisions/0007](0007-where-agent-process-docs-live.md) settled
that `agents/issue-tracker.md` documents GitHub Issues as this repo's tracker
for the mattpocock-skills engineering flow (`/to-spec`, `/to-tickets`,
`/triage`, `/wayfinder`). That flow was used for real for the first time
producing this decision's own five issues — a `/improve-codebase-architecture`
pass's candidates, each grilled and turned into a GitHub issue with a full
spec via `/to-spec`. Having done that, there was no reason left to keep a
release's scope tracked twice: once live, informally, in conversation and
commits, and again by hand in `RELEASES.md`'s two-phase document — when
GitHub's own issue open/closed state already tracks exactly that, live, for
free, immune to the strikethrough rendering hazard entirely.

`CHANGELOG.md`'s own growth (675 lines, also append-only by the Keep a
Changelog convention it follows) surfaced in the same conversation, but is a
separate problem with a separate, non-ADR-worthy fix (`AGENTS.md` guideline
7): its entries had drifted toward verbose implementation rationale instead
of the terse "what changed" Keep a Changelog itself already asks for. That is
a correction back to an existing documented standard, not a new decision with
real alternatives, and is not this ADR's subject.

## Decision

- **One GitHub Issue per feature/candidate**, published via `/to-spec` with
  the `ready-for-agent` label — already `agents/issue-tracker.md`'s
  convention, now the release-scope-tracking unit too.
- **One GitHub Milestone per release version**, named to match that release's
  eventual `CHANGELOG.md` heading exactly (e.g. `ratect-compat 0.27.0 ·
  ratect 0.6.0`), holding every issue that ships in it.
- **The version-bump/tag/GitHub-Release sequence** (`AGENTS.md` guideline 8)
  only starts once every issue in the release's Milestone is closed — the
  Milestone's own state is the readiness gate.
- **`RELEASES.md`'s per-version entry becomes a short, retrospective
  pointer** — the release's theme in a sentence or two, and a link to its
  closed Milestone — written once, after the release ships. The
  scope-then-strikethrough two-phase convention is retired for new entries.
  Existing entries (everything before this decision) are untouched, per the
  append-only rule; `AGENTS.md` guideline 9 now documents both forms.
- **A version-scoped implementation decision** (`AGENTS.md` guideline 14's
  "belongs to one release" case) now lives inline in that release's own
  GitHub issue — its "Implementation Decisions" section, the shape `/to-spec`
  already produces — rather than in a `RELEASES.md` "Scope, settled before
  building:"/"As built:" subsection. That subsection pattern is retired
  alongside the strikethrough convention it was written to sit next to.
  Cross-cutting decisions still become ADRs, unchanged.

## Alternatives considered

- **Eliminate `RELEASES.md` entirely**, relying on GitHub's own Milestone/
  issue UI as the sole release history. Rejected: this repo's own pattern —
  extracting ADRs out of `ROADMAP.md` into `decisions/` rather than deleting
  them, splitting `ROADMAP.md`/`RELEASES.md` rather than dropping either —
  is consistently *extract and link*, never *delete and depend on an external
  system*. A clone of this repo should still show what every release was
  without GitHub being reachable.
- **Move `decisions/` to `docs/adr/`** to align fully with
  `setup-matt-pocock-skills`' own conventions. Out of scope here — already
  decided, and rejected, in [decisions/0007](0007-where-agent-process-docs-live.md).
- **Keep the live scope-then-strikethrough convention, just write shorter
  entries.** Rejected: the two-phase editing this asks a contributor to do by
  hand is exactly what GitHub's issue/Milestone open-closed state already
  does, live, with no risk of the inline-markdown-across-a-blank-line
  rendering hazard guideline 9 previously had to warn about.
- **Fix `CHANGELOG.md`'s verbosity via the same mechanism.** Considered and
  rejected as the wrong remedy for a shared symptom: `CHANGELOG.md` is
  user-facing prose reused as a GitHub Release body, not contributor-facing
  scope tracking — replacing verbose entries with issue links would degrade
  what an upgrading user actually reads. Addressed separately, directly in
  `AGENTS.md` guideline 7, with no ADR (a correction to an existing
  documented standard, not a new decision).

## Consequences

- `RELEASES.md`'s growth rate drops sharply going forward: one short
  paragraph per release instead of a multi-paragraph scope-and-done-summary.
- `AGENTS.md` guideline 8 gains a precondition: a release's Milestone must be
  fully closed before the version-bump/tag/publish sequence starts.
- `AGENTS.md` guideline 9 documents two eras of `RELEASES.md` convention in
  one file — expected, not a defect, per the append-only rule.
- `AGENTS.md` guideline 14 changes where a version-scoped decision's
  rationale lives: the release's own GitHub issue, not a `RELEASES.md`
  subsection.
- A release's readiness is now visible natively in GitHub (Milestone progress
  bar: N of M issues closed) rather than only inferable from `RELEASES.md`'s
  own prose.
