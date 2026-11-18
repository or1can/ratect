# Issue tracker: GitHub

Issues and specs for this repo live as GitHub issues. Use the `gh` CLI for all operations.

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --comments`, filtering comments by `jq` and also fetching labels, plus its design record with `uv run tools/design-record.py get <number>` (see "The design record and the agent brief").
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --comment "..."`

Infer the repo from `git remote -v` — `gh` does this automatically when run inside a clone.

## Rejected feature requests: `.out-of-scope/`

`/triage` closing an enhancement as `wontfix` (not "already implemented" —
that's a built feature, points to where it lives instead) records the
decision in `.out-of-scope/<concept>.md`: what was rejected, why, and every
issue that requested it. One file per concept, not per issue — a repeat
request gets appended to the existing file's list rather than re-litigated.
Triage reads this directory first so a matching new issue can be closed
against the existing rationale instead of a fresh one being written. See the
`triage` skill's own `OUT-OF-SCOPE.md` for the format and full flow.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

When set to `yes`, PRs run through the same labels and states as issues, using the `gh pr` equivalents:

- **Read a PR**: `gh pr view <number> --comments` and `gh pr diff <number>` for the diff.
- **List external PRs for triage**: `gh pr list --state open --json number,title,body,labels,author,authorAssociation,comments` then keep only `authorAssociation` of `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, or `NONE` (drop `OWNER`/`MEMBER`/`COLLABORATOR`).
- **Comment / label / close**: `gh pr comment`, `gh pr edit --add-label`/`--remove-label`, `gh pr close`.

GitHub shares one number space across issues and PRs, so a bare `#42` may be either — resolve with `gh pr view 42` and fall back to `gh issue view 42`.

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> --comments`, and read its design record with
`uv run tools/design-record.py get <number>` (below).

## The design record and the agent brief

A ticket can carry two specifications: the agent brief the `triage` skill
posts as it applies its outcome, and the design record `settle-design`
writes just before that, at the end of triage
([decisions/0028](../decisions/0028-design-and-implementation-review.md)).
They don't share authority. **The design record outranks the brief**, its
acceptance criteria included, whatever the brief's own template says
about being the contract. `uv run tools/design-record.py get <number>`
returns the record: it knows which comment holds it and whose comments
count. When it exits 1 (two records, or legacy ones not yet merged), stop:
merging them is `settle-design`'s, with the maintainer confirming, and no
reader picks one. Until ratect#927 lands, `settle-design` still writes its
record as a `## settle-design record` section inside the brief, with no
decision ids; `get` reads that section, up to the next `##` heading, as
the record (so a `##` heading inside it cuts it short), and the rules
below apply to it as far as it goes.

- **Reading a ticket** (`implement`, `implement-spec`, `code-review`):
  read the brief and the record together, and for each pointer `get`
  returns ("governed by #N"), read that issue's record with `get N` too:
  a mechanism's decisions live only in the record it points to. A brief
  criterion that disagrees with a record decision is a design flaw: raise
  it with the maintainer before building to either one; with the
  maintainer absent, stop and post it on the issue. These skills don't
  pass this doc to their subagents, so pass it on yourself, rule and
  record together: `code-review` puts the records into its Spec
  sub-agent's prompt beside the acceptance criteria, as the spec it checks
  against, and `implement-spec` lists them among its implementers' context
  pointers; each is told that the record outranks the brief, and that a
  disagreement between them is reported, not resolved. A brief's D-id
  citations need no refreshing when a decision is amended later: a reader
  reads the decision's current text, and an amendment that leaves a
  criterion disagreeing with it is that same design flaw.
- **Writing a brief** (`triage`): take its acceptance criteria from the
  record, citing the decision ids (D1, D2…) each one rests on, and don't
  restate the decisions themselves. A decision reached while grilling
  goes into the record through `settle-design`, not into the brief alone.
  A quick state override to `ready-for-agent` still runs `settle-design`
  first (AGENTS.md, "Settling design"), so there is a record to cite.
  Checking that a brief written before its record still agrees with it
  is `check-design`'s job once ratect#930 adds it; until then, the agent
  implementing the ticket checks it.
- **Resuming triage**: read the record too; a decision in it is settled.
  Don't ask it again; a reason to reopen one goes to the maintainer as
  "reopen Dn?".
- **Tickets from `to-tickets` or `to-spec`**: label each one
  `needs-triage`, not the `ready-for-agent` those skills apply by default,
  and write no `## Design record` section into its body. Only
  `settle-design` writes a record's decisions, so triage runs it on each
  ticket; where a parent's record already governs every mechanism the
  ticket changes, the record it writes is a short "governed by #N's
  design record" one.

## When a ticket declares a Parent

Use the real GitHub sub-issue relationship instead of `/to-tickets`' default plain-text `## Parent` section — the native link is queryable (`gh issue view <n> --json parent`, returning number/title/state/url), shown in GitHub's own UI, and puts a live "N of M sub-issues done" progress bar on the spec itself; the prose adds nothing on top of that, so omit that section from the ticket body entirely rather than keep both.

Fold it into ticket creation itself: `gh issue create --parent <spec> --title "..." --body "..."` — one command, plain issue numbers, no separate linking step and no database-id lookup (contrast the `dependencies/blocked_by` API below, which does need one). If a ticket already exists without the link, `gh issue edit <parent> --add-sub-issue <ticket>` adds it after the fact.

This isn't modifying the parent in the sense `/to-tickets`' "do not close or modify any parent issue" warns against — that's about the parent's own content/state; declaring a sub-issue relationship from the child side doesn't touch either.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (`gh api` on the sub-issues endpoint). Where sub-issues aren't enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: GitHub's **native issue dependencies** — the canonical, UI-visible representation. Add an edge with `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`, _not_ the `#number` or `node_id`). GitHub reports `issue_dependencies_summary.blocked_by` (open blockers only — the live gate). Where dependencies aren't available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (`issue_dependencies_summary.blocked_by > 0`, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me` — the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.
