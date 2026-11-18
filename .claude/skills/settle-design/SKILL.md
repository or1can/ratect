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

   *Done when* each mechanism is listed as open (and why) or as designed,
   with where its design lives. A ticket that changes no mechanism ends
   here: record "nothing open" (step 4) and stop. A ticket whose
   mechanisms all look designed still goes on to step 2: no one of those
   five lock tickets showed the problem alone.

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

   **Test every mechanism's shape.** For each mechanism step 1 listed,
   designed or open, apply `mattpocock-skills:codebase-design`'s
   **deletion test** (imagine deleting it: does its complexity vanish, or
   reappear across its actors?) and its **seam check** (where does its
   interface live, and does anything vary across it? One adapter is a
   hypothetical seam). Write each result into `design.md`. One that shows
   a designed mechanism's design wrong makes it open.

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

   **Then run at most two break-the-design passes.** Invoke the built-in
   `code-review` at `high` over `design.md`, by its full path, briefed with
   `agents/review-brief.md` and with the record's decisions and their
   rejected alternatives (those recorded already, and those settled in this
   run), and told to **break the design**: each finding is a concrete
   interleaving of the listed actors, step by step, ending in a state the
   design doesn't handle or handles wrongly. The reviewer classes each
   finding as a design gap, a design flaw or a dispute, as the brief
   defines them, and you confirm the class. Revise for each confirmed gap,
   and put each flaw to the user, since its fix changes a decision. A
   dispute changes nothing: put it to the user once per decision, as
   "reopen D*n*?", and don't count it as something the pass found.

   A revision that adds an actor, or a writer of state another tool also
   writes, adds a mechanism: run steps 1 and 2 over it, and test its shape,
   before the next pass, or before step 4 when no pass is left.

   If the first pass found something, revise and ask the user whether the
   second is worth it. If the second finds more of the same kind of
   problem the first revision fixed, report the part as **not yet
   understood**; that doesn't buy a third pass. After the last pass, the
   user settles whatever remains by a decision, with its accepted cost, or
   leaves it as an open point.

   *Done when* each open part has its design in the file, no pass is left
   to run, and the user has confirmed each decision, or the part is left as
   an open point: the user deferred it, it awaits a skill, or no one was
   there to decide.

4. **Record and report.** Write the record in the format `design-record.py
   --help` gives under "What a record holds", with #924's record as the
   worked example. Beyond what that format asks for, it holds each
   mechanism's sibling searches and what they found, each settled design
   in full from step 3's `design.md`, the decisions the user reached in
   triage's grilling, and the prompt for any part awaiting a skill. A
   ticket that changes no mechanism gets a record saying so, still with
   its `## Design record` heading and `### Amendment log`.

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
