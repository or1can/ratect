# 0013 — When `ratect-compat` copies Batect, and when it diverges

**Status:** Accepted — adopted during the triage of the spec-level parity sweep
([#284](https://github.com/or1can/ratect/issues/284)). Exit codes are decided
separately
([#540](https://github.com/or1can/ratect/issues/540)).

## Context

`ratect-compat` is a drop-in replacement for Batect ([0001](0001-two-binaries.md)):
a `batect.yml` and a command line that work under Batect should work the same
under it. The sweep in [#284](https://github.com/or1can/ratect/issues/284) mined
Batect's unit specs for behavioural rules and filed around 240 issues where
`ratect-compat` behaves differently, from error wording to which configs load.

Deciding each one on its own merits produced inconsistent answers — the same
behaviour, validating image tags before a build, was classified as a gap from
one Batect spec and as a deliberate divergence from another
([#302](https://github.com/or1can/ratect/issues/302)) — and decisions kept
reopening the same argument. Batect is also
unmaintained, so "what Batect does" includes its bugs: crashes on valid input,
output it silently drops, cleanup that stalls. Copying everything would mean
copying those too.

The question that kept deciding cases was not "does it differ?" but "does the
difference change what happens?". Answering it narrows what
[0001](0001-two-binaries.md) calls a "strict, literal" match: strict about
outcomes, not about wording, and not about Batect's bugs.

## Decision

A difference from Batect is either a **parity gap** (fixed) or a
**divergence** (kept, and recorded in
[`docs/differences-from-batect.md`](../docs/differences-from-batect.md)),
by these rules, in order:

1. **Copy Batect where the outcome differs.** If a config or command line that
   Batect accepts would load, run or fail differently under `ratect-compat`,
   that's a gap. So is `ratect-compat` accepting something Batect rejects, when
   accepting it changes what runs — a typo that silently becomes a literal
   value, an expression expanded where Batect keeps it literal, an empty
   option value that fails later with an unrelated error, a command line with
   no task name that exits 0.
2. **Keep harmless leniency, as a divergence.** Where `ratect-compat` accepts
   something Batect rejects and the accepted input still does what the user
   evidently meant — a port written `+80`, a dependency listed twice, a field
   left as YAML null meaning "omitted", combined short flags like `-Tf` — it
   stays accepted and is recorded. A lenient input that is usually a slip (a
   duplicate entry) can still earn a diagnostic in `ratect doctor` /
   `ratect config validate`.
3. **Don't copy Batect's bugs.** Where Batect crashes, fails on valid input,
   loses output, or leaves more behind than it should, `ratect-compat` keeps
   its own behaviour, recorded as a divergence: chowning a read-only cache
   mount, following a symlinked cache entry outside the project, dropping a
   container's unterminated last line, blanking every line on a zero-width
   terminal, stalling cleanup after one failed stop.
4. **Wording and format are `ratect-compat`'s own** — messages, help text,
   `--version`, the task-list header, the argument-error format — provided the
   accept/reject outcome and the point of failure match. Own words must not be
   worse words: an error names what the user wrote
   ([AGENTS.md](../AGENTS.md) guideline 15) and, where it can, the likely cause.
   A message that buries the reason under library internals is a gap even when
   wording is a divergence.
5. **Nothing claims parity it doesn't have.** Every divergence is recorded on
   the differences page, and a doc page or code comment that says "matching
   Batect" or "exactly like Batect" about a divergence is a defect.

The native `ratect` binary is outside this record: it is free to diverge
([0001](0001-two-binaries.md)), and a decision here only binds it where the
code is shared and the decision says so.

## Alternatives considered

- **Exact parity everywhere.** Rejected: it means reproducing Batect's crashes,
  lost output and stalled cleanup, and refusing inputs that harm no one, all for
  a tool nobody maintains. Parity is the means; a working drop-in is the end.
- **Case by case, with no rule.** What the sweep started with. Rejected: it gave
  contradictory answers to the same shape of difference, and every review
  re-argued the trade-off.
- **Lenient everywhere** (accept anything Batect accepts, plus anything else
  that parses). Rejected: a silent change of meaning — a typo that runs, a value
  read differently — is the worst failure a drop-in can have, because nothing
  tells the user their config means something else now.

## Consequences

- The differences page grows, and becomes the place a user switching from
  Batect reads; sweeps of stale "matching Batect" claims are part of recording
  a divergence, not an afterthought (sweeps on #541 found sixteen stale claims,
  and #544's found seven).
- Fixes under rule 1 can reject inputs that `ratect-compat` used to accept.
  None of them was a valid Batect input, so no Batect user is affected, but a
  config written for `ratect-compat` first can break; those land as breaking
  changes in `CHANGELOG.md`.
- "Harmless" is a judgement. The test that settled the cases so far: does the
  run do something the user didn't ask for, or report something untrue? If
  either, it's rule 1.
- Wording being a divergence doesn't excuse poor messages; message quality is
  reviewed against rule 4 like any other change.
