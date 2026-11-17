---
name: settle-design
description: Find what a ticket's brief or ADR leaves open, and settle it as a design before any code is written. Use at the end of triage, before implementing a ticket, and when the user chooses it at `ready`'s design stop.
---

A ticket that leaves part of a mechanism open gets its design found one
review pass at a time, after the code exists. In 0.14.0 five tickets were
redesigned mid-implementation, and seven came from the Git include cache,
five of them changes to one lock protocol, each triaged as small
(ratect#902). This skill finds the open parts first and settles each as a
design, so a ticket that changes no mechanism pays only for step 1 and a
one-line record.

A **mechanism** here is anything with state that more than one actor can
act on: a lock, a cache, a lifecycle, a file two processes both write. Draw
one around whatever its states have to be named together for: a service
instance is one mechanism, not one per container. A process the ticket
runs counts too: what it inherits (the environment, config files, the
format of a path it's given) is an actor acting on it. A mechanism's
**design** is its states, the actors acting at once, and what each actor
does in each state.

## Steps

1. **Find what's open.** Read the ticket, its brief, each ADR they cite,
   and any earlier record on the issue (step 4).
   List each mechanism the ticket changes, and mark it open if any of
   these holds:
   - **Declared open**: the brief, an ADR or an earlier record leaves it
     unsettled ("Open, for #N to settle", "implementation detail", "Open
     point to settle first"), including when it hands it to the
     implementation to settle.
   - **Verified short**: the brief or ADR presents a decision about it as
     checked (verified, tested, probed), but the check left out the
     alternative it rejected, or ran a stand-in for the real case (one raw
     `docker create` for two concurrent `services up`).
   - **Omitted**: its design isn't written anywhere: the brief, an earlier
     record, the ADRs in `decisions/` (cited or not), or the `//!` doc
     comment of the module that owns it. Silence counts; a brief that reads
     as settled can still omit a mechanism.

   A design an earlier record settled counts as designed, unless new
   siblings (step 2) reopen it. Coming from `ready`'s design stop, the
   mechanism its passes keep finding problems in is open, wherever its
   design lives.

   *Done when* each mechanism is listed as open (and why) or as designed,
   with where its design lives. A ticket that changes no mechanism ends
   here: record "nothing open" (step 4) and stop. A ticket whose mechanisms all look designed still goes on to
   step 2: no one of those five lock tickets showed the problem alone.

2. **Find the siblings.** Search the tracker with
   `gh issue list --state all --limit 1000 --json number,title,state,stateReason,closedAt --search "<term>"`,
   once for the ticket's own number (catching "Follow-up to" and "found
   while" references), and once for each mechanism's names and files. A
   sibling is another issue changing the same mechanism: open, or closed
   as completed since the date of the second-newest dated release heading
   in `CHANGELOG.md` (`[Unreleased]` doesn't count).
   Two or more siblings that no earlier record decided on make the
   mechanism open, to settle in step 3 as one design across them all.
   *Done when* each mechanism has its sibling list and the searches that
   found it.

3. **Settle each open part as a design.** Make a new directory each time
   this skill runs, with `mktemp -d /tmp/settle-design-<issue>.XXXXXX`
   (with no issue, `/tmp/settle-design.XXXXXX`), and write every part's
   whole design into one `design.md` inside it, including any part an ADR
   already holds, as a table of state by actor where one fits. Find facts
   yourself; put each decision to the user as `mattpocock-skills:grilling`
   does, a round at a time with your recommendation.

   Then invoke the built-in `code-review` at `high` over that file, by its
   full path, briefed with `agents/review-brief.md` and told to
   **break the design**: each finding is a concrete interleaving of the
   listed actors, step by step, ending in a state the design doesn't
   handle or handles wrongly. Revise for what it confirms, then ask the
   user whether another pass is worth it: a pass nearly always finds
   something, and not everything it finds is worth fixing. If a pass finds
   more of the same kind of problem the last revision fixed, say so: the
   part is not yet understood.

   *Done when* each open part has its design in the file, the user has
   declined another review pass, and has confirmed each decision in it,
   or the part is left open: the user deferred it, or no one was there to
   decide.

4. **Record and report.** Post one comment on the issue, headed
   `## settle-design record`:
   - each mechanism examined, and where its design lives;
   - each sibling search and what it found;
   - what the user decided;
   - each settled design in full, from step 3's `design.md`.

   At triage it goes into the brief triage posts next instead, heading
   and all. A ticket that changes no mechanism gets one line saying so. An
   ADR that owns a settled mechanism is brought up to date from the record
   as part of the ticket's own work, so `/ready` reviews it with the rest
   (superseded instead, once its release has shipped: guideline 14).
   Any part left unsettled moves the issue to `ready-for-human`, with each
   such part under an "Open point to settle first" heading, and no code is
   written for it.
   With no issue (a ticketless change at `ready`'s design stop), the
   record goes to the user only.
   *Done when* the record is on the issue (at triage, in the brief once
   posted), and the user has the same summary.
