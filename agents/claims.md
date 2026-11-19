# The `claims` checks

How this repo runs and configures the [`claims`](https://github.com/or1can/claims)
Claude Code plugin. `AGENTS.md` names the trigger, in Tooling & CI and in
Agent skills: read this before marking a finding checked, or when the hook
blocks a commit. What each check finds, and every setting it takes, is the plugin's own
documentation ([its checks](https://github.com/or1can/claims/tree/main/docs/checks),
[configuring](https://github.com/or1can/claims/blob/main/docs/configuring.md));
this page is what this repo does with them. The plugin replaced four
hand-rolled `tools/` scripts, and
[decisions/0009](../decisions/0009-adopt-claims-plugin.md) has the migration.

## Where it runs

- **On every commit**, as a git `pre-commit` hook declared in
  `.pre-commit-config.yaml` (pinned to a `claims` release tag, which
  Renovate bumps) — a gate finding blocks the commit, an advisory finding
  is printed alongside it, and an unmarked one from a check `claims.toml`'s
  `[hook] fail_on_unmarked` lists blocks it too.
- **On demand**, via the `check-claims` skill (ask an agent to check
  claims, any time mid-task, not only at commit time).
- **Before each push**: a `pre-push` hook runs the same checks over the
  branch from its merge-base with `origin/main`, with your own grants, so
  its unmarked count is the one to quote when sizing a PR's claims work.
- **On every pull request**, as the `claims` job in
  `.github/workflows/ci.yml`, so a PR from a clone without the hook, or
  from a bot like Renovate, is checked too. It checks out the `claims`
  release `.pre-commit-config.yaml` pins, read from that file at run time,
  so a bump of the hook moves CI with it; runs the checks against the PR's
  base branch, which covers the PR's whole change; and fails on a gate
  finding, or on an advisory finding that is neither fixed nor marked
  checked (`--fail-on-unmarked`; of advisory findings, the commit hook
  blocks only on those, and on an orphaned mark, from the checks `[hook]
  fail_on_unmarked` lists). So a PR resolves the findings it brings in, and
  whoever batches Renovate's changes marks what those bring in. It writes
  its own `claims.local.toml` with `AGENTS.md`'s grant list. The job is
  not a required status check.

## The checks here

Gates: `executable-claims` (`<!-- verify: ... -->` markers), `check-links`,
`check-file-refs` and `check-citations` (Markdown/Swift only — no Rust
support). Advisory: `stale-claims`, `spliced-docs`, `restatement`,
`check-cli-flags`, `temporal-words`, `claim-words` and `judgment-agent`
(every backticked Rust/Swift name a diff added or removed, in any tracked
Markdown file, as candidates for architecture-claim review; not opt-in).
`check-config-defaults` and `check-env-vars` are registered too, but find
nothing here: there is no `[check-config-defaults]` mapping and no
`.env.example`.

`claims.toml` configures them for this repo, each setting under its own
comment saying why:

- `claim-words` (totalising language) is opt-in, and this repo opts in
  `AGENTS.md`, `agents/*.md`, `CONTEXT.md`, `ROADMAP.md`, `RELEASES.md`,
  `TODO.md`, `decisions/*.md`, `docs/*.md`.
- `temporal-words` (version-dated wording added to reference prose) reads
  `docs/*.md`, `CONTEXT.md`, `agents/*.md` and `README.md` — leaving out
  the files that are dated or forward-looking by design (`AGENTS.md`,
  `ROADMAP.md`, `TODO.md`, `RELEASES.md`, `decisions/*.md`), all of which
  `claim-words` does read.
- `restatement`'s default `extensions` widens to add `.rs`.
- `stale-claims`'s bare-name module matching is scoped to
  `decisions/`/`AGENTS.md`, and it doesn't rank an ADR's Context and
  Alternatives considered sections.
- `executable-claims`'s timeout is raised past this repo's own cold-build
  time.

`executable-claims` also refuses to run a `<!-- verify: -->` marker
containing shell chaining, redirection, substitution, or `sed`/`awk`/`grep`
— a fixed, no-config-needed gate, not a per-project knob.

## Grants

A marker's command with no exact-string entry in `claims.local.toml`'s
`[executable-claims]` section is a **gate finding** naming the command and
the exact TOML to add, regardless of whether it's actually safe (the
plugin's own
[deny-by-default ADR](https://github.com/or1can/claims/blob/main/decisions/0001-executable-claims-deny-by-default.md)).
`AGENTS.md` lists this repo's grants.

`check-cli-flags` asks for the same kind of grant before running a script
it finds named next to a flag. Its one candidate here is denied: guideline
8 names `tools/changelog-section.py` on the same line as the
`--prerelease` flag, so its `--help` could only report that the
script lacks flags nobody claims it has. A denied command is still listed,
as skipped, rather than asked about.

## Marking a finding checked

**An advisory finding that was read and found true can be marked
checked**, in the committed `claims.checked.toml` beside `claims.toml`,
so the hook and CI stop reporting it until its sentence's wording, or
the content of a file it names, changes (the plugin's [Checked
marks](https://github.com/or1can/claims/blob/main/docs/checked-marks.md)
page). Write marks from the repository's root with `uv run
tools/claims.py mark-checked`, which runs the `claims` release
`.pre-commit-config.yaml` pins (a plain `python3 -m claims.cli` fails
outside the hook's own environment), over the same `--diff-range` the
findings were read over, with a note on each saying what was checked
against what, and `--by` naming who did the reading. Deleting an entry
by hand lifts a mark early; `--prune` removes every orphaned one, as
when a reworded sentence is marked again. A `stale-claims` section or
`spliced-docs` doc comment read only in part is marked with `--partial`,
which the summary counts apart. Mark a finding only after reading it
against the code, file or section it names, never on the strength of its
own wording, and never mark a batch nobody read: a mark records a
reading, and one without a reading quiets a claim nobody checked. A
finding that was wrong is fixed, not marked. The summary line's `quiet
while checked` count says how many marks applied.

## An agent's marks

An agent doesn't wait for the maintainer, to mark or to carry on. It
marks what its own pre-commit run shows, whether the hook blocks on the
finding or only prints it, in that same commit, not in a marks commit at
the end of a branch: `uv run tools/claims.py mark-checked --pending --by
"Claude Code agent"`, staged with the change that raises the finding. A
pending mark keeps its finding quiet like any other, and is written with
`review = "pending"`. A later commit that changes a marked subject
re-marks it pending in that commit. Re-marking a pending finding without
`--pending` would record it as reviewed, so an agent re-marks with
`--pending` too.

A finding raised after its commit, by the pre-push hook's branch range
or by CI, still belongs to the commit that raised it: the last commit
that changed any of the mark's subjects (for `stale-claims`, the section
or a file it names), since the mark records them as they are at the tip.
Its pending mark goes in a `fixup!` commit whose subject is `fixup!
<that commit's full SHA>`, which the approval fold below folds in. That
SHA names the commit only until the branch is rewritten, so a rebase of
a branch holding a `fixup!` folds it in first, with
`GIT_SEQUENCE_EDITOR=true git rebase -i --autosquash <onto>` (without
`-i`, git before 2.44 ignores `--autosquash`), and if that rebase stops,
it is aborted with `git rebase --abort` and the agent asks. A branch
holding pending marks or a `fixup!` is updated only that way: never with
GitHub's "Update branch", whose rebase leaves the `fixup!` naming
nothing and whose merge commit the fold refuses, and never with `git
merge`.

The pull request's body lists, for each commit in it, the marks that
commit added or changed, as `uv run tools/claims.py pending-marks --at
<sha> --own` prints them, under its full SHA. A `fixup!` commit's marks
are listed under the commit it folds into, below that commit's own,
since the fold approves them only by that commit's SHA. The list is
replaced whenever the head moves, and reads "No marks pending" once none
is. A subagent's report names its branch, its worktree's path, its tip's
full SHA and that list, and the session that started it puts the list to
the maintainer.

Only the maintainer approves or rejects a mark: by a reply in the
session, or by a comment on the pull request from their own account. The
repository is public, so anyone else's comment is ignored. They approve
the marks of some or all of the listed commits, a commit's marks all
together, or reject marks. A commit holding a mark they haven't read
stays pending until they read it. An approval names the SHAs listed when
it was given, so after a rewrite the marks are listed again and approved
again. A rejection is of the versions listed, and each commit whose
listed version was rejected gets a `fixup!` commit of its own, holding
the fix, the deletion of the rejected entry, and a `--pending` re-mark
only if a second reading found it true, with a note on that entry saying
what the reading found; the commit can then be approved, and the deleted
entry, no longer in the tree, isn't. Merging is not approval.

Approving is the approval fold, `claims`' `fold-approvals` ([Folding
approvals into their
commits](https://github.com/or1can/claims/blob/main/docs/checked-marks.md#folding-approvals-into-their-commits)
says how it works and what it refuses). It approves each mark inside the
commit that wrote it, as written there, so a mark re-marked in three
commits is three approvals, one in each. It runs only where the
maintainer can answer the prompts for it and for the force-push, both of
which `.claude/settings.json` asks before running, as typed; an approval
given in a pull request comment is carried out in the maintainer's next
session. It runs from a clean tree, after `git fetch`, and, where
`origin/<branch>` exists, only if it is an ancestor of `HEAD`, so
nothing pushed from elsewhere is overwritten; otherwise stop and ask.
The push uses the lease below, not the one the fold prints, which names
the old local tip:

```sh
uv run tools/claims.py fold-approvals "$(git merge-base origin/main HEAD)" --by "<maintainer>" --approve <sha> ...
uv run tools/claims.py fold-approvals "$(git merge-base origin/main HEAD)" --by "<maintainer>" --approve <sha> ... --apply
git push --force-with-lease=<branch>:<origin/<branch>'s SHA>
```

The first run only previews: each commit with its own pending marks and
the `fixup!` commits that fold into it. `--approve` names each commit
whose marks the maintainer approved, by the SHA they were listed under,
and covers the `fixup!` commits listed under it too, a rejected mark's
`--pending` re-mark included, so those are read in the preview first.
`--apply` folds every `fixup!` and writes the maintainer's bare name as
each approved entry's `by`: there is no approval commit and no message
line, and `by` is the record of who approved. A commit not named keeps
its marks pending. If the fold's rebase stops, it puts the branch back;
the agent asks the maintainer rather than reworking the `fixup!`
unasked. A branch never pushed is pushed with `git push -u origin
<branch>`, with no force, and a rejected lease means stop and fetch, not
retry. Approving at commit time is the same fold over one commit.
Approving by re-marking instead would record the subjects as they are on
disk now, which nobody may have read.

A rewrite for any other reason lists the pending marks again, and leaves
an approved mark approved if every key is unchanged: an approval holds
for a mark's content, not for a commit (`claims`' [ADR
0015](https://github.com/or1can/claims/blob/main/decisions/0015-an-approval-holds-for-content-not-for-a-commit.md)).

CI's Claims Marks job is a required status. It runs `pending-marks`,
which fails while any mark in `claims.checked.toml` is pending, so a
pending mark can't merge, and on a pull request it also fails while a
commit in it has a subject starting `fixup!`, so a correction nobody
folded can't merge either. `fold-approvals` refuses a range with nothing
pending; that range's `fixup!` commits are folded by hand with
`GIT_SEQUENCE_EDITOR=true git rebase -i --autosquash "$(git merge-base
origin/main HEAD)"`, under the fold's preconditions: `HEAD`'s SHA is
noted first, `git diff <that SHA> HEAD` must be empty afterwards, and
the result is pushed as above. If the rebase stops, it is aborted with
`git rebase --abort`; if the diff isn't empty, the branch goes back with
`git reset --hard <that SHA>`; either way the agent asks. The job checks
the whole file, not the pull request's change. A pending mark that
reaches `main` anyway can't be folded, since `main` can't be rewritten:
it is approved by a commit of its own, in a pull request of its own,
made with `uv run tools/claims.py approve-marks <sha> --by
"<maintainer>"`. A branch whose base holds it runs no fold until that
pull request merges and the branch rebases onto `main`, and
`fold-approvals` refuses a base holding a pending mark.
