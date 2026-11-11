# The `claims` checks

How this repo runs and configures the [`claims`](https://github.com/or1can/claims)
Claude Code plugin. `AGENTS.md`'s Tooling & CI section is the trigger: read
this before marking a finding checked, or when the hook blocks a commit. What
each check finds, and every setting it takes, is the plugin's own
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
8 names `tools/changelog-section.py` on the same line as the `gh` flags of
the release recovery steps, so its `--help` could only report that the
script lacks flags nobody claims it has. A denied command is still listed,
as skipped, rather than asked about.

## Marking a finding checked

**An advisory finding that was read and found true can be marked
checked**, in the committed `claims.checked.toml` beside `claims.toml`,
so the hook and CI stop reporting it until its sentence's wording, or
the content of a file it names, changes (the plugin's
[Checked marks](https://github.com/or1can/claims/blob/main/docs/checked-marks.md)
page). Write marks with `python3 -m claims.cli mark-checked`, over the
same `--diff-range` the findings were read over, with a note on each
saying what was checked against what, and `--by` naming who did the
reading: an agent's marks name the agent and whoever approved them, as
`--by "Claude Code agent, approved by Kevin Rogers"`. Deleting an entry
by hand lifts a mark early; `--prune` removes every orphaned one, as when
a reworded sentence is marked again. A `stale-claims` section or
`spliced-docs` doc comment read only in part is marked with `--partial`,
which the summary counts apart. Mark a finding only
after reading it against the code, file or section it names, never on
the strength of its own wording, and never mark a batch nobody read: a
mark records a reading, and one without a reading quiets a claim nobody
checked. A finding that was wrong is fixed, not marked. The summary
line's `quiet while checked` count says how many marks applied.
