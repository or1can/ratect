# 0028 — Design and implementation review: three tools around one design record

**Status:** Accepted — planned (ratect 0.14.0):
[#924](https://github.com/or1can/ratect/issues/924) and its sub-issues,
[#925](https://github.com/or1can/ratect/issues/925) (this record, the
shared brief and the glossary terms) through
[#930](https://github.com/or1can/ratect/issues/930) (`check-design`). The
decisions below are D1, D13–D17, D19, D20, D22–D24 and D27 of
[#924's design record](https://github.com/or1can/ratect/issues/924#issuecomment-6954635807),
confirmed by the maintainer on 2026-11-17 (D13 and D22 as amended, and
D24 and D27, on 2026-11-20), with Decision 2 summarising
D5–D9, whose detail is #926's. Each tool's own procedure stays in its
skill. Amended while implementing #926: Decision 2 named `settle-design`
as the author of the acceptance criteria, when the `triage` skill writes
them in its brief, after `settle-design` has run, so Decision 1 also no
longer has `settle-design` read a brief at triage; #924's D9 and D13 are
amended to match, and ratect#927 brings `settle-design`'s own step 1
into line. Amended by [#938](https://github.com/or1can/ratect/issues/938),
which puts the simplest working design before `settle-design`'s first pass
and asks after every pass whether its findings point at a simpler design
(#924's D13 and D22 amended, D24, D25 and D27 added; D25's design order
stays in the skill): Decision 1, Decision 2's record contents, Decision
3's routing of a design gap, the "Two design passes, then a decision"
consequence, and the end of the interim review rule.

## Context

Two skills under `.claude/skills/` split review between them:
`settle-design`, which settles a ticket's open mechanisms before code is
written, and `ready`, which runs the bug review and the two-axis review over
a change before it is committed. The line between them had worn away, and
three things showed it.

**Review passes almost never came back clean.** Of the 98 bug reviews
[#911](https://github.com/or1can/ratect/issues/911) measured, one returned
no findings. Later rounds mostly found prose the previous round's fix had
made false.

**Recorded decisions were argued again.** In #911's own implementation
session, `ready`'s design stop sent the work back into `settle-design` on a
mechanism #911 had already settled. Each pass after that found 9 or 10 more
issues, many of them recorded choices disputed rather than bugs. The
meaning of "present" flipped three times, because the settled design and
the acceptance criteria disagreed and nothing said which one won.

**Using `ready` on `ready` is recursive.** When the change is a skill or an
ADR, the implementation *is* the design, so `settle-design`'s review and
`ready`'s review read the same text and find the same things.

Underneath all three: both reviews were briefed with the same whole list
of rules, and asked to judge the design and the code alike, at every stage. Nothing told a reviewer what had
already been decided, so a pass could not tell new evidence from an old
argument.

## Decision

### 1. Three tools, each given only the context its job needs

| Tool | When | Its job | It does not |
| --- | --- | --- | --- |
| **`settle-design`** | at triage only | Makes the design clear, actionable and achievable: the thorough, adversarial critique happens here, and it writes the design record | run at implementation, or at `ready`'s design stop: a design flaw comes back to it as a fresh run on that mechanism |
| **`check-design`** (new) | before implementation starts | A light pass, close to triage: given what is known now, does each decision still hold, is a case missing, and do the acceptance criteria still agree with the record? | critique the design, run a reviewer, or loop |
| **`ready`** | before each commit ([AGENTS.md](../AGENTS.md) guideline 15) | Is the implementation good, does it meet the record, the spec and this repo's standards, and does it have bugs? | change a recorded decision |

`settle-design` reads the ticket, what triage has established so far (or
its brief, on a re-run after one is posted), the ADRs they cite, its
siblings' records and the `//!` doc of each module that owns a mechanism,
and, for a ticket that changes a mechanism, frames the problem apart from
any solution the ticket proposes. Before its first pass, it puts the
simplest design that works for each open mechanism to the maintainer as a
decision, beside each mechanism's deletion test and seam check. It
runs at most two break-the-design passes, each a `code-review` at `high
--max-findings all` over the whole current design, briefed with the
record's decisions and rejected alternatives and with no earlier
findings; after each, before revising, it asks whether the findings point
at a simpler design. `check-design` reads the
record, the current code of the modules it names, and any issue closed
since it was written that touches its mechanisms. `ready` briefs every pass
with the record, and its two-axis review's Spec axis reads the record and
the acceptance criteria, not the issue body.

### 2. The design record is the seam between them

One design record per issue holds its problem, stated apart from any
solution, where its `settle-design` run made a decision; its decisions,
each with an id (D1, D2…), what was decided and why, the alternatives
rejected and why, and its accepted cost; the mechanisms it covers; its
open points; and an amendment log. `settle-design` writes it, and the
maintainer confirms each decision. `check-design` and `ready` read it,
and may append a design gap; neither changes a decision. Changing one goes
back to `settle-design`, through the maintainer.

The record outranks the agent brief, acceptance criteria included. The
`triage` skill writes the brief after `settle-design` has written the
record, so it takes the criteria from the record, citing its decision ids,
and restates no decision; `check-design` checks they still agree, and a
disagreement `ready` finds is a design flaw, not a choice for `ready` to
make by picking a side. The skills this repo can't edit learn this from
[`agents/issue-tracker.md`](../agents/issue-tracker.md), the tracker doc
they consult for fetching and writing tickets, which also says what to
hand their subagents, who don't read it. The record's format, write rules
and tooling are #926's.

### 3. One classification, shared by `settle-design` and `ready`

What a reviewer raises is read against the design record and classed as a
**bug** (`ready` only: the code departs from the record or the spec), a **design gap**
(the record is silent, so it is settled in flow and appended; in
`settle-design`, only once the pass's findings have been checked for a
simpler design, and after any decision to remove or reduce the mechanism
that leads to), a **design
flaw** (the record is wrong, or the fix would change a decision: the design
stop, back to `settle-design` through the maintainer) or a **dispute** (an
argument against a recorded decision with no new evidence: not a finding,
but a question for the maintainer, once per decision). Each class sends
the finding somewhere different, which is why the class, and not the
finding's severity, decides what happens next.

The reviewer tags each finding, and the author confirms the tag. `ready`'s
design stop fires only on a confirmed design flaw. A finding that recurs is
a candidate, checked against the record: one that argues against a decision
is a dispute, one that shows the record wrong or silent a design flaw or a
design gap.

The classification's working definitions, the dispute rule and the checks
a reviewer applies are written once, in [`agents/review-brief.md`](../agents/review-brief.md),
which both tools brief their reviewers with. The habits an author follows
while writing a fix are not in it: a reviewer can't act on them, so they
stay in `ready`'s skill.

### 4. The bootstrap rule

When a change's implementation *is* its design — a skill, or an ADR —
`ready` checks it for conformance to the record and for bugs, and never
re-critiques a recorded decision. A finding that argues with one, bringing
no new evidence, is a dispute; one with evidence that the record is wrong
is a design flaw. This is
what stops `ready` reviewing a change to `ready` from re-running
`settle-design`'s critique.

### 5. Effort

`settle-design`'s design passes and every `ready` pass run at `high`.
Over #911's data, `ready`'s fix round 2 found 28 behaviour findings in 18
passes; `medium` was weighed against that and rejected. `check-design` runs
no reviewer, so has no level. `max` and `ultra` are invoked by the user
only.

### 6. The terms

The terms — design record, decision, accepted cost, open point,
design gap, design flaw, dispute, bug, design stop, ledger and review
coverage —
are defined in [CONTEXT.md](../CONTEXT.md)'s "Design review" section.

## Alternatives considered

- **One skill with modes.** Rejected: a mode switch inside one skill still
  hands each stage the whole procedure, and every stage the authority to
  judge the design. That is the shape that let `ready`'s design stop send
  #911 back into a full re-critique. Three tools let each be briefed with
  only what its job needs, and let the one that must not critique simply
  not have the instructions to.
- **Two tools: drop `check-design`.** Rejected; see Consequences, which
  carry its case.
- **No ADR, since this is process rather than product.** Rejected: the
  classification is cited by both reviewing tools, by the review brief and
  by guidelines 15 and 16, and the bootstrap rule governs every one of
  #924's sub-issues, which is guideline 14's trigger; and
  [0007](0007-where-agent-process-docs-live.md)–[0009](0009-adopt-claims-plugin.md)
  are process records too.
- **A design stop on any recurring finding.** Rejected: in #911's session
  it fired on disputes. Recurrence makes a finding a candidate, not a
  flaw.
- **The classification restated in each skill.** Rejected: two copies of a
  definition drift, and these two tools must agree on it to hand findings
  between them.
- **`medium` effort for `ready`'s passes.** Rejected for #911's reason
  under Decision 5.

## Consequences

- **`check-design` is shallow, and stays.** Its interface is almost as big
  as what it does. It stays because deleting it brings back "run
  `settle-design` again before writing code", which is how #911's
  re-critique happened: its value is mostly what it refuses to do. It
  borrows three steps from triage, cut down — *already done?* (has a
  sibling closed since the record delivered the change), *premise still
  true?* (do the files, functions and flags the brief names still exist,
  and does the bug still reproduce), and *recommend, then wait* (a label
  the maintainer applies) — and explicitly does **not** grill, categorise,
  consult `.out-of-scope/`, or write a brief.
- **Two design passes, then a decision.** `settle-design` stops after two
  break-the-design passes, each at `--max-findings all`, so a cluster of
  findings isn't cut at the default 10; a second that finds more of what
  the first revision fixed is reported as "not yet understood", after its
  simpler-design question, rather than buying a third. Some holes a third
  pass would find are settled by decision, with its accepted cost, or left
  open instead. A mechanism reduced after the last pass reaches the record
  unreviewed by any pass.
- **Disputes cost the maintainer's attention**, once per decision, in place
  of a fix loop. With the maintainer absent, `ready` ends blocked on a
  design flaw rather than resolving it.
- **Process terms enter a product glossary.** [`agents/domain.md`](../agents/domain.md)
  makes CONTEXT.md the single glossary, so they go there, named so that
  "gap" never collides with **Parity gap**.
- **The tools land one sub-issue at a time.** Until #930 lands,
  `check-design` doesn't exist, and until #929 does, `ready` finds what's
  unreviewed as it did before #928;
  [AGENTS.md](../AGENTS.md)'s "Settling design" section says how the
  interim works now that #927 has made `settle-design` triage-only. Work
  on #924 itself is reviewed under its interim rule until the new tools
  exist: once both #929 and #930 have landed.
