# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root — the domain glossary for Ratect's configuration model (`ratect`/`ratect-compat`/`ratect-core`). This is a single-context repo for that domain, despite being a multi-crate Cargo workspace, so there's one `CONTEXT.md`, not one per crate. The exception is `dockerignore`: a from-scratch port of Docker's own `.dockerignore` matching, deliberately kept free of any Ratect-specific type so it could be extracted and published independently — it isn't part of this glossary's domain, and a change scoped to it has no `CONTEXT.md` term to consult.
- **`decisions/`** at the repo root — this repo's ADRs. Not `docs/adr/`: `decisions/README.md` explains the convention, and `AGENTS.md` explains why the location is deliberate (`docs/` is the user-facing tree; ADRs are for contributors, and moving them would break append-only `CHANGELOG.md` links into already-released sections). Read ADRs that touch the area you're about to work in.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates them lazily when terms or decisions actually get resolved.

## File structure

Single-context (this repo, despite the multi-crate workspace):

```
/
├── CONTEXT.md
├── decisions/
│   ├── README.md
│   ├── 0001-two-binaries.md
│   └── ...
├── ratect/src/
├── ratect-compat/src/
├── ratect-core/src/
└── dockerignore/src/
```

There is no `CONTEXT-MAP.md` here and no per-crate `CONTEXT.md`/ADR directories — `ratect`/`ratect-compat`/`ratect-core` implement one domain (a task execution engine and its two config formats) and share the one `CONTEXT.md`. `dockerignore` sits outside that domain entirely (see above) rather than needing a context of its own.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids (e.g. a *grant* vs. an *effective boundary* are different things, on purpose).

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR in `decisions/`, surface it explicitly rather than silently overriding:

> _Contradicts decisions/0004 (Git-include host-path trust) — but worth reopening because…_

Also check whether the decision is version-scoped rather than cross-cutting: some decisions live inline in a `RELEASES.md` entry instead of `decisions/` (see `AGENTS.md` guideline 14 for which is which).
