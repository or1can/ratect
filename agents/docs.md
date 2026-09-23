# User documentation

How to write a page under `docs/`. `AGENTS.md`'s guideline 10 is the trigger
(a change to user-visible behaviour updates `docs/` in the same change); this
file is what to follow once you are there.

## What `docs/` is

The `docs/` directory is user-facing documentation (installation, getting started, architecture, CLI reference, config reference, differences from Batect) — **not** `ROADMAP.md`, `RELEASES.md`, `AGENTS.md`, `CHANGELOG.md`, or `decisions/`, which are project-management/contributor docs. `docs/` deliberately does not assume familiarity with Batect's own documentation, since Ratect's behavior is a subset of and sometimes diverges from it.

## The two config references

**`docs/ratect-config-reference.md` defers to `docs/ratect-compat-config-reference.md` for most
field semantics, and that deferral is only safe while the differences are about
*shape*.** It says every field "applies, with the same meaning" and then links
into the `batect.yml` reference from ~30 places, so any behaviour that is
`ratect-compat`-only silently falsifies it for native readers who followed a
link. The first semantic divergence (0.25.0's image-source validation, which
`extends` requires the native format not to have) is why there is now a **Where
the semantics differ** table in the native reference: a new behavioural
divergence needs a row there *and* a marker at the compat end of the link, not
just a sentence wherever it was implemented.

## Examples and captured output

A `docs/` page shows one of three kinds of
example, chosen by what the real thing is:

- **When the real file *is* the example, `{{#include}}` it verbatim** — a
  config under [`examples/`](../examples/), anchored (`ANCHOR`/`ANCHOR_END`) when
  only a slice is wanted, as `docs/worked-examples.md` and the homepage do. A
  hand-copied excerpt is the one thing the `worked-examples` CI job can't
  catch drifting.
- **When it is the binary's output, it is a capture** — the six rules below.
- **Invented only when no real project has the shape**, and then with a
  pointer to the nearest real one, so a reader knows it is illustrative and
  the next editor knows what to capture instead. The exception, not a third
  equal option.

The captured-output rules (settled on #158; the tooling landed in #161):

1. **Captured or absent.** A block showing the binary's output comes from a
   real run against a real, checked-in project — `examples/*`, or a
   `tests/fixtures/*` file when the point is a state no example has (a flat
   task list; a `doctor` finding). Not typed, not edited. Each block carries
   a one-line provenance: the command and the project.
2. **Animated only where the output changes in place.** `fancy` mode, a
   pull/build progress line, the `Cleaning up:` countdown — those are
   asciinema recordings (`docs/demo.cast`, played by the self-hosted player
   `book.toml` loads; the homepage and the `fancy` section share the one
   recording). Everything else is static: `simple`, `quiet`, `all`,
   listings, `doctor`, `caches`/`includes`/`resources`, errors.
3. **Colour is captured, not added.** `tools/capture-output.py <name> --
   <command...>` runs the command on a pseudo-terminal
   (`TERM=xterm-256color`, 80×24, stdin included — so a task container gets
   its TTY, as it would in a shell) and writes the raw bytes to
   `docs/captures/<name>.ansi`: the green/red exit code, `all`'s
   per-container prefix colours, whatever a container's own program
   coloured. The page includes it as

   ````markdown
   ```ansi
   {{#include captures/<name>.ansi}}
   ```
   ````

   and `tools/mdbook-ansi.py` (`book.toml`'s `[preprocessor.ansi]`) renders
   that at build time as a `<pre>` of spans, styled for mdBook's light and
   dark themes by `tools/mdbook-ansi.css` — no generated HTML is committed.
   It is a small terminal emulator, not a colour-code converter, because a
   TTY capture holds in-place drawing too (npm's spinner; a `\r`-overwritten
   progress line), and what the reader should see is the terminal's final
   state. On GitHub's own rendering of the `.md` file the block shows the
   include marker instead — accepted; the site is the published form. A
   `<!-- verify: -->` marker keeps a plain listing honest exactly as before;
   the colour capture is the same command's TTY run.
4. **Show, then clarify.** Where a capture can show it, prose doesn't
   describe it. Prose says what the capture can't: that `all` allocates no
   TTY; that `simple` hides the task container's own readiness milestones,
   and why; that `db` takes longer because it seeds a million rows.
5. **One project per comparison.** Blocks meant to be compared (the four
   output styles; grouped vs quiet listings) come from the same project, so
   the reader compares styles rather than projects. Task listings can't all
   share one: a flat listing needs a project with no `group`, and
   `ratect-compat`'s listings can't come from a `ratect.toml` project. So
   the rule holds *within* a page's comparison, and each page's listing uses
   its own binary's nearest real project rather than forcing one across.
6. **The tutorial runs on a real project.** `docs/getting-started.md`'s
   config is [`examples/getting-started/`](../examples/getting-started/),
   `{{#include}}`d step by step by anchor, so its transcripts are captures
   and the `worked-examples` job proves the tutorial still runs.

Captured so far (#161, then #166's sweep): `simple`/`quiet`/`all` on
`docs/ratect-compat-cli.md` (`fancy` is the recording, per rule 2; each
style is one subsection — clarifying prose beside its capture, not a
description of every style followed by a gallery of them), Getting Started's first run, `docs/ratect-cli.md`'s
`caches`/`includes`/`resources`/`doctor` blocks, and
`docs/migrating-batect-project.md`'s `config convert`. Task listings are the
plain `<!-- verify: -->` form instead — the binary prints them uncoloured, so
a capture would add nothing the marker's re-run doesn't already prove.

## Ownership

Which page owns a fact, and which format and binary a page speaks in.
Six rules, each with its purpose and the page that already models
it; the evidence for each — the pages found breaking it — is in the
[second-pass audit on #158](https://github.com/or1can/ratect/issues/158#issuecomment-5784985911),
not restated here:

1. **Docs mirror the code split.** A per-binary reference documents that
   binary's interface only. Behaviour both binaries share is written once,
   in a shared page under Using Ratect, and each reference's row for a
   shared flag is one line plus a link. The same holds for the two config
   references: a section with no config field of its own is a concept, not
   a reference entry. Purpose: `ratect-core` is shared and the binaries are
   thin ([decisions/0001](../decisions/0001-two-binaries.md)), and a
   paraphrase on the second page drifts from the first. Model: the
   "Semantics" column of
   [`docs/ratect-config-reference.md`'s field reference](../docs/ratect-config-reference.md#field-reference)
   is a link, with at most a native-only qualifier beside it, never a
   paraphrase of the linked section.
2. **Concept pages are format-neutral.** Native spelling first, the
   `batect.yml` spelling only where the syntax differs, and `ratect run` as
   the command. Purpose: Getting Started is native-first, so a concept page
   it links to must not land the reader in the other format and the other
   binary. Model: `docs/includes.md`, which states this rule for itself,
   and `docs/getting-started.md` for the command form.
3. **Reference prose is present tense.** Version numbers and "yet", "used
   to", "this first version" belong in `CHANGELOG.md`; a reference
   describes what the binary does today. Purpose: there is no version
   picker, so a reader can't check "since 0.21.0" against the binary they
   have, and a "yet" rots silently once the thing lands. Model:
   `docs/faq.md` and `docs/reusable-building-blocks.md`, which carry no
   version or temporal wording at all.
4. **A user page links out of `docs/` for reasoning, never for the fact.**
   An ADR link for the why is fine; a `ROADMAP.md`/`RELEASES.md` link for
   what the tool is, is not. Purpose: `docs/` is self-contained, and
   `ROADMAP.md` is freely rewritten (`AGENTS.md`'s guideline 9), so an
   anchor into it can vanish from under a user page. Model: the
   `decisions/` links already in `docs/` — `docs/installation.md`'s to
   [0010](../decisions/0010-release-binary-distribution.md),
   `docs/includes.md`'s to
   [0004](../decisions/0004-git-include-host-path-trust.md) — each a why
   beside a fact the page states itself.
5. **Batect-relative framing only on the Coming-from-Batect pages.** No
   page outside that section opens by comparing itself to a Batect page,
   and no section heading outside it names Batect; an inline "matching
   Batect" aside is fine, and so is a closing "Coming from Batect?"
   paragraph, after the page has said its own piece (`docs/comparison.md`,
   `docs/task-lifecycle.md`). Purpose: `docs/` does not assume familiarity with
   Batect's documentation (above), and a New-to-Ratect reader has never
   seen the page an opening compares itself to. Model:
   `docs/dependency-readiness.md` — no Batect opening, no Batect heading,
   and a "matching Batect" aside inline wherever a detail does.
6. **Every page in a path section hands off.** Each page ends with where
   to go next, and the section's last page hands to the next section.
   Purpose: mdBook's previous/next buttons make `docs/SUMMARY.md`'s order a
   reading order, so a page without a hand-off strands the reader at a
   button. Model: `docs/getting-started.md`'s "Next steps" section; no section's
   last page hands off yet, so the second clause has no model.

## Around `docs/`

The [`decisions/`](../decisions/) directory holds Architecture Decision Records — the **cross-cutting** decisions that get referenced from more than one place (the two-binary split, the runtime-ownership labels, the native config format, trusting a Git include's host paths). Its [`README.md`](../decisions/README.md) states the convention; see `AGENTS.md`'s guideline 14 for when to write one.

[`CONTEXT.md`](../CONTEXT.md) at the root is the **glossary**: what each term in
the configuration model denotes, and nothing else — no implementation detail,
no decisions, no behaviour. It exists because several of this project's bugs
have been one word covering two concepts (a *grant* is written on an include
entry, an *effective boundary* is what a file ends up with; `ConfigFormat` is a
*project's* dialect, not a *file's* syntax). Add a term when settling one
resolves an ambiguity, not to catalogue vocabulary that was never in doubt.

So: glossary at the root, cross-cutting rationale in `decisions/`, user-facing
behaviour in `docs/`, contributor process in `AGENTS.md`. `decisions/` deliberately is
*not* `docs/adr/` — `docs/` is the user-facing tree, and ADRs are for
contributors. Moving them would also break links from already-released
CHANGELOG sections, which are append-only.
