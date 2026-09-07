# 0007 — Where contributor/agent-process docs live

## Status

Accepted — adopted immediately.

## Context

[0006](0006-code-and-documentation-locality.md) settled where code's own
documentation lives, and its Decision section states a rule this repo already
follows: "A decision referenced from more than one place is an ADR.
User-facing behaviour is `docs/`." `decisions/README.md` restates the same
line from the other side: "This is *not* user documentation (that's `docs/`)."
Between the two, this repo already has three top-level homes for non-user
content — `decisions/` (ADRs), `AGENTS.md`/`TODO.md`/`ROADMAP.md`/
`RELEASES.md`/`CHANGELOG.md` (contributor process and versioned history at the
root) — and none of them fit a fourth kind of content that showed up for the
first time in 0.27.0-dev: reference docs *for an AI coding agent to consult*,
not decision rationale and not versioned history. Running the
`setup-matt-pocock-skills` scaffolding skill produced exactly this — an issue
tracker convention, a triage label mapping, and domain-doc consumer rules —
and its own default location, `docs/agents/`, directly contradicts `0006`'s
rule: none of the three files describe user-facing behaviour.

Two ways to make room were considered and rejected before landing on a third.

## Decision

A new top-level `agents/` directory (plain, undotted — matching `decisions/`'s
own visible, discoverable style) holds agent-consulted reference docs:
`agents/issue-tracker.md`, `agents/domain.md`, `agents/triage-labels.md`
today. `docs/`'s scope is unchanged from `0006`; `decisions/` is unchanged.

## Alternatives considered

- **Broaden `docs/`'s own definition** to cover both user-facing and
  contributor reference content. Rejected: this would supersede `0006`'s rule
  1 for a one-time placement problem, and immediately reopens exactly the
  "which pages here are actually for a user" ambiguity `0006` closed —
  needing its own follow-up work to resolve, for no benefit over giving the
  new content its own home instead.
- **Move `decisions/` to `docs/adr/`**, matching `setup-matt-pocock-skills`'
  own seed-template default for where it expects ADRs to live. Rejected: no
  functional need — `agents/domain.md` already correctly points skills at
  `decisions/` instead of the tool's default, and that override works today.
  The cost is real and permanent: 20 existing links to `decisions/NNNN-*.md`
  across `CHANGELOG.md`/`RELEASES.md`/`ROADMAP.md`/`docs/*.md`, 6 of them in
  `CHANGELOG.md`, which is append-only and can never be edited — moving would
  break those 6 forever for a naming preference alone.
- **A dot-prefixed `.agents/` directory.** Rejected: this repo already has a
  project-local `.claude/` (currently `settings.json`/`settings.local.json`),
  which is genuinely Claude-Code-specific tool configuration. The new content
  is deliberately tool-agnostic — read by any AI coding agent, the same
  reason `CLAUDE.md` is a symlink to `AGENTS.md` rather than a second copy —
  so filing it under a dot-directory would misrepresent it as the same kind
  of thing `.claude/` already is, or as hidden config nobody's expected to
  read directly.
- **`.claude/` itself.** Rejected for the same reason: these docs aren't
  Claude-Code-specific, and putting tool-agnostic content there would tie it
  to one tool's directory by accident.

## Consequences

- `docs/agents/{issue-tracker,domain,triage-labels}.md` (as scaffolded by
  `setup-matt-pocock-skills` in this same change) move to
  `agents/{issue-tracker,domain,triage-labels}.md`; `AGENTS.md`'s `## Agent
  skills` section links there instead.
- Future content of this kind — more agent-process configuration, or output
  from other skills that read/write per-repo scaffolding — defaults to
  `agents/`, not `docs/`.
- This does not supersede `0006`; it depends on and is consistent with its
  rule 1. `0006` is why this content doesn't belong in `docs/` in the first
  place.
