---
name: settle-design
description: Find what a ticket leaves open, settle it as a design, and write the issue's design record. Use at the end of triage, before the issue moves to `ready-for-agent` or `ready-for-human`.
---

A ticket that leaves part of a mechanism open gets its design found one
review pass at a time, after the code exists. In 0.14.0 five tickets were
redesigned mid-implementation, and seven came from the Git include cache,
five of them changes to one lock protocol, each triaged as small
(ratect#902). This skill finds the open parts first, settles each as a
design, and writes the issue's **design record**, so a ticket that changes
no mechanism pays only for step 1 and a short record saying so.

It is the first of the three tools in
[decisions/0028](../../../decisions/0028-design-and-implementation-review.md),
and it runs at the end of triage only, before the `triage` skill posts its
agent brief. A design flaw found later, by `check-design` or by `ready`,
comes back here as a fresh run on that mechanism: the maintainer sends the
issue back to `needs-triage`, and triage runs this skill again.

A **mechanism** here is anything with state that more than one actor can
act on: a lock, a cache, a lifecycle, a file two processes both write. Draw
one around whatever its states have to be named together for: a service
instance is one mechanism, not one per container. A process the ticket
runs counts too: what it inherits (the environment, config files, the
format of a path it's given) is an actor acting on it. A mechanism's
**design** is its states, the actors acting at once, and what each actor
does in each state.

Every read and write of a design record goes through
`uv run tools/design-record.py`; its `--help` says which comment is the
record, what a record holds, and who may write what.

**With the user absent**, the run waits at the first decision it must put
to them, in any step (#924's D13). Nothing is posted as an open point for
want of someone to decide; if the run ends while waiting, nothing is
posted and the issue stays `needs-triage`.

## Steps

1. **Find what's open.** Read the ticket (`gh issue view <number>
   --comments`), what triage has established so far (or its brief, on a
   re-run after one is posted), each ADR they cite, the issue's record
   (`uv run tools/design-record.py get <number>`), the record of each
   issue its `pointers` name, and the `//!` doc comment of each module that
   owns a mechanism the ticket changes. Only what `get` returns is a
   record: a comment shaped like one that `get` leaves out, such as one
   from someone who can't write to the repository, is not. Decisions the
   user reached in
   triage's grilling are decisions for this record, not for the brief
   alone.

   When `get` exits 1, the issue holds more than one record (#924's D12):
   several `## settle-design record` comments, one posted after the design
   record, or two design records. Read them all with `get <number>
   --all`, and merge into one record what the design record (if any) hasn't
   already absorbed: the issue body's section, and the legacy records, or
   with a design record only those posted after it and not named in its
   log. Put each conflict between them to the user; step 4 writes the
   merge. With two design records, the maintainer deletes all but one only
   after you have read both.

   List each mechanism the ticket changes, and mark it open if any of
   these holds:
   - **Declared open**: the ticket, an ADR or a record leaves it
     unsettled ("Open, for #N to settle", "implementation detail", an open
     point), including when it hands it to the implementation to settle.
   - **Verified short**: the ticket or an ADR presents a decision about it
     as checked (verified, tested, probed), but the check left out the
     alternative it rejected, or ran a stand-in for the real case (one raw
     `docker create` for two concurrent `services up`).
   - **Omitted**: its design isn't written anywhere: the ticket, a record,
     the ADRs in `decisions/` (cited or not), or the `//!` doc comment of
     the module that owns it. Silence counts; a ticket that reads as
     settled can still omit a mechanism.

   A design a record settles counts as designed, unless new siblings
   (step 2) reopen it. On a fresh run for a design flaw, the mechanism the
   flaw is in is open, and so is each decision it cites.

   **Then frame the Problem** (#924's D24), for a ticket that changes a
   mechanism, apart from any solution the ticket proposes: the constraints
   any design must meet, as the first bullet of
   `mattpocock-skills:codebase-design`'s DESIGN-IT-TWICE step 1 frames
   them (that bullet only: don't go on to its sub-agents); what must hold,
   and for whom; the cost being saved, and how it was measured (the method
   and inputs, not a path to a scratch file); and what happens if nothing
   is built. A problem stated only as a solution, or with its cost
   unmeasured where cost is the motive, is the first thing you put to the
   user. On a re-run, keep the
   recorded Problem: changing it is a decision for the user. A record from
   before #938 has none, and isn't back-filled (#924's D12): a re-run on
   it frames the Problem like any run, and step 4 records it only if the
   run makes a decision (#924's G5).

   *Done when* each mechanism is listed as open (and why) or as designed,
   with where its design lives, and the Problem is framed. A ticket that
   changes no mechanism ends here, with no Problem: record "nothing open"
   (step 4) and stop. A ticket whose mechanisms all look designed still
   goes on to step 2: no one of those five lock tickets showed the problem
   alone.

2. **Find the siblings.** Search the tracker with
   `gh issue list --state all --limit 1000 --json number,title,state,stateReason,closedAt --search "<term>"`,
   once for the ticket's own number (catching "Follow-up to" and "found
   while" references), and once for each mechanism's names and files. A
   sibling is another issue changing the same mechanism: open, or closed
   as completed since the date of the second-newest dated release heading
   in `CHANGELOG.md` (`[Unreleased]` doesn't count). Read each sibling's
   record with `get`; where it exits 1, merge the sibling's records as
   step 1 says before deciding who owns the mechanism.

   A mechanism shared with siblings has one **owning record**, which holds
   all its decisions (#924's D6). If a sibling's record already covers the
   mechanism, that issue owns it: this issue's record says "<mechanism>:
   governed by #N's design record" and holds no decision on it, and a
   decision this run makes on it is written into #N's record. Otherwise
   this issue owns it. Two or more siblings that no record decides on make
   the mechanism open, to settle in step 3 as one design across them all.

   *Done when* each mechanism has its sibling list, the searches that
   found it, and its owning record.

3. **Settle each open part as a design.** Make a new directory each time
   this skill runs, with `mktemp -d /tmp/settle-design-<issue>.XXXXXX`,
   and write every part's whole design into one `design.md` inside it,
   including any part an ADR already holds, as a table of state by actor
   where one fits. Find facts yourself; put each decision to the user as
   `mattpocock-skills:grilling` does, a round at a time with your
   recommendation.

   **Design in this order** (#924's D25): correctness; then the simplest
   design that works; then optimisation, which may wait where the cost of
   not optimising is stated as an accepted cost, with what would justify
   it later; then DRY. The simplest design doesn't win at all costs: a
   heavier or optimised one wins when the user picks it, its cost stated.

   **Test every mechanism's shape, and put the simplest design first.**
   For each mechanism step 1 listed, designed or open, apply
   `mattpocock-skills:codebase-design`'s **deletion test** (imagine
   deleting it: does its complexity vanish, or reappear across its
   actors?) and its **seam check** (where does its interface live, and
   does anything vary across it? One adapter is a hypothetical seam). A
   result that shows a designed mechanism's design wrong makes it open.
   For each open mechanism, write down the simplest design that works,
   with its cost: deleting the mechanism, or doing the expensive simple
   thing, are candidates. Before the first pass, put all of it to the user
   as one decision (#924's D22): the results for every mechanism, and for
   each open one, take the simplest design or keep the one the ticket
   proposes, or this run's where the ticket proposes none (#924's G8). The
   chosen design is a decision, and becomes the part's design in
   `design.md`; the other is its rejected alternative, with why, and the
   shape results go into that decision's why rather than staying notes.
   Results that choose nothing, every mechanism being designed (here or in
   its owning record), are reported, not recorded as a decision (#924's
   G6 and G9).

   **Pick the other instruments per mechanism**, each by its
   `mattpocock-skills:` name: `research` for a fact to find in primary
   sources, `domain-modeling` for a term or an ADR, `codebase-design` for a
   module's interface, `prototype` for a state model to try out,
   `diagnosing-bugs` for a cause the design depends on. A skill whose
   `SKILL.md` sets `disable-model-invocation` (such as
   `improve-codebase-architecture` or `wayfinder`) can't be invoked from
   here: name it, write a ready-to-paste prompt for it into `design.md`,
   and leave the part as an open point, "awaiting <skill>", until the
   maintainer brings back its result. Never skip it silently.

   **Then run at most two break-the-design passes** (#924's D13). Invoke
   the built-in `code-review` at `high --max-findings all`, always giving
   the flag, over the whole current `design.md`, by its full path, briefed
   with `agents/review-brief.md` and with the record's decisions and their
   rejected alternatives (those recorded already, and those settled in
   this run), and told to **break the design**: each finding is a concrete
   interleaving of the listed actors, step by step, ending in a state the
   design doesn't handle or handles wrongly. Give it no earlier pass's
   findings and no hypothesis: brief a decision settled after a pass by
   what it decided and what it rejected, leaving out the findings it
   answers (#924's G7). A pass that returns exactly 32 findings, `all`'s
   ceiling where ReportFindings is in use (#924's D13), was cut off: say
   so when you report it.

   **After every pass, before any revision** (#924's D27), confirm each
   finding's class: the reviewer classes it as a design gap, a design flaw
   or a dispute, as the brief defines them. Then, for each mechanism the
   pass reviewed, findings or none, write one line, "pass N: simpler
   design? ...", into a `passes.md` beside `design.md`, never into
   `design.md` itself, which the next pass reads whole; step 4 puts each
   line beside its mechanism in this issue's record, on its pointer line
   for a mechanism another record owns (#924's G7). It answers whether
   these findings, clustered or not, point at a simpler design, and which
   of them it would remove. If it names one, put those findings, design
   gaps included, to the user as one decision, "remove or reduce the
   mechanism" with its cost, beside the fixes. Fix design gaps in flow
   only after that decision, or straight away when the line names nothing.
   Put each design flaw to the user, since its fix changes a decision. A
   dispute changes nothing: put it to the user once per decision, as
   "reopen D*n*?", and don't count it as something the pass found.

   A revision that adds an actor, or a writer of state another tool also
   writes, adds a mechanism: run steps 1 and 2 over it, listing and
   marking that mechanism only and keeping this run's Problem. A mechanism
   a revision adds, reduces or replaces gets its shape tested and its
   simplest design put to the user, as above, before the next pass, or
   before step 4 when no pass is left.

   If the first pass found something, revise and ask the user whether the
   second is worth it. If the second finds more of the same kind of
   problem the first revision fixed, report the part as **not yet
   understood**, after its simpler-design line; that doesn't buy a third
   pass. After the last pass, the user settles whatever remains by a
   decision, with its accepted cost, or leaves it as an open point.

   *Done when* each open part has its design in the file, no pass is left
   to run, and the user has confirmed each decision, or the part is left as
   an open point: the user deferred it, or it awaits a skill.

4. **Record and report.** Write the record in the format `design-record.py
   --help` gives under "What a record holds", with #924's record as the
   worked example, and #938's for where a Problem goes, which #924's
   predates. Beyond what that format asks for, it holds each
   mechanism's sibling searches and what they found, each settled design
   in full from step 3's `design.md`, the decisions the user reached in
   triage's grilling, and the prompt for any part awaiting a skill. A
   ticket that changes no mechanism gets a record saying so, still with
   its `## Design record` heading and `### Amendment log`.

   The Problem step 1 framed goes in a `### Problem` section, first in the
   record, in the record of this issue when this run makes at least one
   decision (#924's D24). A decision written into another issue's owning
   record cites this issue, whose record holds the Problem, even when all
   of this record's mechanisms are pointers, as #938's are. A record whose
   mechanisms are all pointers and whose run made no decision has no
   Problem, and cites its parent's. Each simpler-design line from
   `passes.md` goes beside its mechanism, as step 3 says. Adding a Problem
   to a record that has none is a `rewrite`, even when the run changes
   only one decision, since `amend` can't add a section (#924's G6).

   Write it with `--by settle-design`, choosing by what step 1 found:
   - `source` `none`, `body` or `legacy`, or several legacy records:
     `create`, carrying over what the body's section and the legacy records
     held, so no earlier decision drops out of what later readers get;
   - `source` `comment`: `amend DN --why` to change one decision, and
     `rewrite --why` for anything else, including merging a legacy record
     posted after it, or a second design record once the maintainer has
     deleted it.

   A decision on a mechanism a sibling owns is written into that sibling's
   record the same way. The record is posted as its own comment, before
   the `triage` skill posts its brief, and never as a section inside the
   brief, which cites the record's decision ids rather than restating them
   (`agents/issue-tracker.md`). Its text, on `create` and on every
   `rewrite`, starts with the `triage` skill's AI disclaimer. For each
   decision this run changed, list each posted brief's criterion that
   rests on it, this issue's or a sibling's, so triage replaces them in a
   new brief.

   An ADR that owns a settled mechanism is brought up to date from the
   record as part of the ticket's own work, so `/ready` reviews it with the
   rest (superseded instead, once its release has shipped: guideline 14).
   Any open point moves the issue to `ready-for-human`, and no code is
   written for it. A design with no issue has no record to go in: design
   work that needs a record needs a ticket (#924's D11).

   *Done when* the record is on the issue, and the user has the same
   summary.
