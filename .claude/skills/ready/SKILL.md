---
name: ready
description: Review a change until both reviews, the bug hunt and the two-axis conformance review, cover it. Use before each commit, and before reporting work as done or opening its PR.
---

Two reviews find different things here, and neither replaces the other. The
**bug review** is the built-in `code-review` skill, which hunts for behaviour
that's wrong. The **two-axis review** is `mattpocock-skills:code-review`,
which checks the change against this repo's standards and against its spec.
Both answer to the bare name `code-review`, so always invoke them by these
names. Over the 0.13.0 milestone the bug review found what the two-axis
review missed on six PRs, and only ran when the maintainer asked (ratect#813).

This is the third of the three tools in
[decisions/0028](../../../decisions/0028-design-and-implementation-review.md):
it reviews the implementation against the issue's **design record**, and
never changes a recorded decision. When the change is a skill or an ADR,
whose implementation *is* its design, it checks conformance to the record
and hunts bugs, and does not critique a recorded decision again (0028's
Decision 4, the bootstrap rule). Every read and write of a record goes through
`uv run tools/design-record.py`; its `--help` says which comment is the
record and what `ready` may write to it.

A `/ready` call **ends done** or **ends blocked**. Done means the change
may be committed and reported as done. Blocked means neither: don't commit,
don't report the work as done, and skip straight to step 7 to report
what blocked it. The maintainer is **present** in an interactive session:
ask, and wait for the answer. They are **absent** in a non-interactive
session or a subagent (inside `mattpocock-skills:implement`, say), where
nothing asked gets answered. A fix **changes code** if its diff changes
anything but prose. A comment or doc comment in a source file is prose; a
test is code, and so is a doc comment's code block (a doctest), a command
a doc gives to run (a code block, a `<!-- verify: -->` marker), and an
instruction in a skill or agent doc, which an agent carries out. A call's
round and **ledger** live in the session only; what compaction loses
counts the safe way: the ledger as never recorded, and the round as
`fix 3`.

## Steps

1. **Find what's unreviewed.** The change is the branch since
   `git merge-base origin/main HEAD`, plus anything uncommitted. For each
   review, find the last time it ran in this session, including inside
   `mattpocock-skills:implement`: a run over the change that ranked bugs was the bug review, one reporting
   Standards and Spec was the two-axis review. A review **covers** the change
   only if nothing has changed since it ran. Otherwise its gap is the diff
   from the commit it reviewed to now, and an edit made in response to a
   review is part of that gap. Which runs count across calls, commits and
   sessions is ratect#929's to settle; until it does, a run counts as
   this step says.
   *Done when* each review is marked covered, or has a gap named as a diff.

2. **Read the record.** For a change with an issue, run
   `uv run tools/design-record.py get <number>`, and `get` again for each
   issue its `pointers` name: a mechanism's decisions live only in the
   record a pointer names. Read, too, any other record one of them names
   as settling part of the change, such as one linked by URL. Find the
   acceptance criteria: the agent brief's, or the issue body's own section
   where no brief was posted. A change with no issue is reviewed against an
   empty record (#924's D11), and has no criteria. If `get` exits 1, the
   issue holds more than one record, which `settle-design` and the
   maintainer resolve, not `ready`; if it exits 2, the record couldn't be
   read. Either way the call ends blocked, since a review with no record
   to read isn't one against the record.
   *Done when* you hold every record that governs the change, or know it
   has none, and its acceptance criteria, if any.

3. **Bug review, then its fix loop.** If step 1 marked the bug review
   covered, go to step 4. Otherwise invoke the built-in `code-review` at
   `high` over its gap. Brief it, and every bug pass after it, with
   `agents/review-brief.md` and the records from step 2, and tell it to tag
   each finding with one of the brief's four classes. For a change to a
   skill or an ADR, tell it too that the change is checked for conformance
   to the record and for bugs, and that a recorded decision isn't critiqued
   again (0028's Decision 4). For a change that
   drives the Docker daemon, brief it to read the daemon's own sources
   (moby and runc in the Go module cache) for side effects, and probe each
   finding against a real daemon before ranking it: `agents/probing.md`
   says how, for each daemon class. This pass is round `initial`. Route
   every pass's findings, this one's and each fix round's, as
   [Routing findings](#routing-findings) says, before fixing any of them;
   then fix what is to be fixed, test-first for behaviour.

   Every fix a pass in this loop asks for is one of its round's fixes:
   docs, skill text and a fix to prose a finding showed false included. A
   docs-only change still needs the bug review: the ratect#877 note's review
   found the lock hazards that became ratect#880. A finding listed with why
   not (rejected, deferred, or a dispute) counts as no finding. After each
   round's fixes, sweep (below), then:

   | After round | If | Then |
   | --- | --- | --- |
   | any | the round fixed nothing | end the loop |
   | `initial`, `fix 1` | otherwise | bug pass over this round's fixes alone; next round |
   | `fix 2` | its fixes changed code | bug pass over them; round `fix 3` |
   | `fix 2` | its fixes changed only prose | end the loop |
   | `fix 3` | — | end the loop (the cap) |

   Read "changed code" off fix 2's diff, not off how its findings
   were classified. The fixes of the round that ends the loop aren't
   read by any bug pass, only by step 4's two-axis review, and step 7
   lists them. When the cap ends the loop, that is its accepted cost:
   over November's reviews, fix round 3 and later found no new mechanism,
   and their 5 behaviour findings all recurred on mechanisms earlier
   rounds had raised (ratect#911).

   Every pass is at `high`, fix rounds and docs-only changes included
   (0028's Decision 5). Brief every pass after `initial` with the
   **ledger**: each of this call's earlier findings, its confirmed class,
   and what was done with it (fixed, and how; appended as a design gap;
   rejected or deferred, and why; or put to the maintainer as a dispute).
   Tell it to hunt its gap as before, not only the ledger's items; not to
   re-report a rejected finding or a dispute without new evidence; and to
   tag a finding that recurs on a mechanism in the ledger.
   *Done when* the loop has ended with each finding fixed or listed with
   why not, or the call has ended blocked at the design stop.

4. **Two-axis review, once,** after the loop ends: work out its gap
   again on the final tree, and if it isn't empty, invoke
   `mattpocock-skills:code-review` over it, so spec and standards are
   checked on what ships. Give it the gap as its diff, untracked files
   included, in place of its own `<fixed-point>...HEAD`, which reads
   committed history only, while `ready` runs before a commit
   (guideline 15). Brief both its sub-agents as step 3 briefs a bug pass,
   and ask for cosmetic findings separately. Its
   Spec sub-agent's spec is the records from step 2 and the acceptance
   criteria, not the issue body. Tell it that the record outranks the
   brief, acceptance criteria included, and that a disagreement between
   them is reported, not resolved by picking one side
   (`agents/issue-tracker.md`, "Reading a ticket"). With no issue, there
   is no spec to give it: its sub-agent skips, and the report says so.

   It runs once per call, never inside the fix loop: before the loop, the
   loop's fixes could break the spec unchecked, and in November half its
   findings were cosmetic (ratect#911). Route its findings as below, then
   fix what is to be fixed; these fixes don't go back into the loop. A fix
   that changes no code is swept, not reviewed. If any fix changes code,
   run **one** bug pass at `high` over those fixes, briefed with the
   ledger and outside the cap. Route its findings as below too; sweep its
   fixes and list them as unreviewed, with no further loop.
   *Done when* each finding is fixed or listed with why not, and any
   bounded bug pass has run; or the call has ended blocked at the design
   stop.

5. **Check the Windows build,** last, with
   `uv run tools/check-windows-build.py`, which runs CI's Windows Check
   locally. Neither review compiles the `cfg(not(unix))` side of a gate,
   and Windows Check isn't a required status, so a break there is found
   after the PR opens unless it's found here (ratect#904). A change to
   nothing cargo reads (Rust sources, manifests, `Cargo.lock`, `.cargo/`
   config, a file a crate includes) and not to the check itself or CI's
   `windows-check` job skips it and says so. If it fails, fix it, list
   the fix as unreviewed, and re-run once. If it still doesn't exit 0, for
   whatever reason, including one the change didn't cause, the call ends
   blocked.
   *Done when* it exits 0 over the final tree or was skipped, and the call
   ends done; or the call ends blocked.

6. **Size the claims work,** before reporting the work as done or opening
   its PR, once the change is committed (before a commit, skip to step 7
   and say the count is pending). Run `git fetch origin main`, then
   `pre-commit run --hook-stage pre-push claims-unmarked > "/tmp/claims-$(git rev-parse --short HEAD).log" 2>&1`
   and read the whole log. Its unmarked count is the one to quote: the commit
   hook's list covers only the last commit.
   *Done when* the count comes from a run over the current `HEAD`, or is
   reported as pending.

7. **Report**:
   - whether the call ended done or blocked, and if blocked, the design
     flaw, the record `get` couldn't give, or the failing check;
   - which commit each review covered, and, for a change with no issue,
     that the Spec sub-agent skipped;
   - what each review found, the class you confirmed for each, what was
     fixed, and what wasn't and why;
   - each design gap appended to a record, and each dispute put to the
     maintainer;
   - what each sweep re-read and fixed, and any grep it ran;
   - the fixes no bug pass read: the last loop round's, which only the
     two-axis review read, and the two-axis review's prose fixes, the
     bounded pass's and the Windows check's, which no review read;
   - the Windows check's result, or that it was skipped;
   - the unmarked claims count.

## Routing findings

A reviewer tags each finding with one of `agents/review-brief.md`'s four
classes, read against the record; you confirm the tag, reclassing it where
the record says otherwise. The brief defines the classes; the confirmed
class decides the route (0028's Decision 3):

- **Bug**: fix it if it's worth fixing, or list it with why not.
- **Design gap**: fix it in flow, then append the case and how you settled
  it with `uv run tools/design-record.py append <number> gap --by ready`,
  which marks it as added at implementation, to the record that owns the
  mechanism: the issue a pointer names, where the change's own record
  points there for it (#924's D6). The report lists it, so the maintainer
  can reopen it. A change with no issue has no record to append to: the
  report lists the gap alone.
- **Design flaw**: don't fix it; it is the design stop (below). A
  disagreement between the record and the acceptance criteria is one
  (#924's D9), whichever side the code follows.
- **Dispute**: fix nothing for it. Put it to the maintainer as "reopen
  D*n*?" once per decision, however many passes or calls raise it: with
  an issue, post the question on it, unless the issue already carries it
  for that decision, and with the maintainer present, ask it too. With no
  issue, the question is "reopen?", about whatever the finding would
  change, and the report carries it.

**A recurring finding is a candidate design flaw, not a design stop.** A
finding the reviewer tags as recurring on a mechanism in the ledger, or one
you see repeat what an earlier round fixed, is checked against the record:
one that argues against a decision is a dispute; one that shows the record
wrong is a design flaw; one that shows it silent is a design gap; and one
the record already answers, which the code doesn't yet meet, is a bug. In
#911's session a design stop on any recurrence fired on disputes.

### The design stop

It fires on a confirmed design flaw, and on nothing else. The code stays
as it is, and the flaw goes back to `settle-design` through the
maintainer, as a fresh run on that mechanism at triage:

- **Maintainer present:** put the flaw to them, with the decision it
  cites and the finding's evidence. If they confirm it, the call ends
  blocked, and they send the issue back to `needs-triage`, where
  `settle-design` runs on that mechanism. If they class it otherwise, route
  it by the class they give.
- **Maintainer absent:** the call ends blocked. Post the flaw on the
  issue, under an "Open point to settle first" heading, with the decision
  it cites and the evidence, and move the issue to `ready-for-human`
  (ratect#911's blocked end), so no agent picks it up again before the
  maintainer sends it back.

A change with no issue that reaches the design stop gets an issue first,
since design work that needs a record needs one (#924's D11).

## The sweep

Fixes make the next round's findings: most prose-false findings from fix
round 2 on were made false by the round before (ratect#911). So before
every bug pass after `initial` and before the call ends, re-read every
comment, doc sentence, error and log string that names what the round's
fixes changed, and fix what they made false. Every round sweeps. Sweep
edits belong to the round they were made in.

If the fixes renamed an identifier or term, or changed what one means,
also run `git grep --untracked` over the whole checkout, not the diff, for
the old name and the old defining phrases. The reviews read only the diff,
so they find leftovers elsewhere only a few per pass (ratect#906). Fix
each hit or say why it stands; a shipped `CHANGELOG.md` or `RELEASES.md`
entry stands (guideline 9). Grep matches spelling, not meaning, so a
sentence asserting the old meaning in other words needs reading around the
hits, not another grep. The `claims` `restatement` check doesn't replace
this: it runs at commit or through `check-claims`, keyed on text you
deleted, and guideline 16 says what it misses.

## The bound

So a call runs at most five bug passes (four in the loop, one after the
two-axis review) and one two-axis pass, and the Windows check at most
twice. Only a present maintainer's answer can take unbounded time, and
that wait is theirs.

## Author habits

These are for you, while writing a fix; a reviewer can't act on them, so
they stay out of the reviewers' brief, `agents/review-brief.md`, which holds
the checks a reviewer applies. Each of these has caught something a review
missed:

- **Anchor an inserted item on the preceding item's closing brace, never on
  the new one's attributes.** A Rust item's doc comment sits *above* its
  `#[test]`/`#[derive]` attributes, so anchoring there splices the new item
  into the previous one's documentation — silently, and the compiler is
  happy. The `claims` plugin's `spliced-docs` check finds the ones that
  get through; it's advisory (never blocks a commit), so read each
  candidate rather than treating a report as a defect.
- **Re-read every string you added, in its final control-flow position.**
  Nothing type-checks a log or error message, so they stay as written while
  the code around them moves. An error should name something the user
  actually wrote (the reviewers' brief has why).
- **When a change alters observable behaviour, re-read that behaviour's
  whole doc section and *run* each claim against the binary.** Not grep —
  execute. Every claim: the example output, the flag descriptions, the "this
  does X" sentences. Capture real output and `diff` it rather than editing
  what looks wrong, since what looks wrong is exactly the set you already
  believe. Grep is the fallback for claims nothing can be run against (a
  roadmap entry, a design note); it is not the check (this rule was once
  grep, and reported clean while missing three).

  To find which claims to re-read, the `claims` plugin's `stale-claims`
  check ranks prose by how much the code it names has moved since the
  claim was last touched. Treat a hit as "re-read this", never as "this
  is wrong" — it measures churn, so a true claim about a hot file looks
  suspicious. Don't record its candidate count anywhere: the number rises
  when code moves and falls when a doc is fixed, so it measures neither.
- **Verify a claim before writing it, not after a reviewer questions it** —
  the review-time half of the change loop's write-prose step.
- **A claim copied from the handoff is unverified.** The handoff is a
  *specification* — what to build, and why it is worth building. It is not
  a source for what exists. So every name in it that you carry into a doc
  comment, an error, or a page under `docs/` gets run first: a flag against
  `--help`, a mode against the code that would implement it, a path against
  the filesystem. This is the change loop's "the code includes whatever the
  prose *names*", applied to the one input that most looks like it has
  already been checked (`RELEASES.md`'s 0.26.0 entry corrects one that
  wasn't). Correct the handoff where it is still live;
  where it is already struck through, record the correction in its
  done-summary rather than editing history.
- **Run a self-cleaning test twice, asserting external state (`docker volume
  ls`) rather than the exit code.** One run passes whether or not the
  cleanup matched anything — the first run starts clean by definition.
