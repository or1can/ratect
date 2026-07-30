# 0004 — Trusting a Git include's host paths

**Status:** Accepted — planned. The `allow_host_paths` opt-in below is not yet
implemented; the containment it relaxes shipped in 0.10.0.

## Context

A container defined inside a Git-included bundle must resolve its `volumes`
host paths, `build_directory`, and `build_secrets` `path` to somewhere inside
either **that bundle's own clone directory** or **the project directory**.
Anything else is rejected. This containment shipped in 0.10.0
(`container_git_boundaries`) and is a **deliberate divergence** — Batect has no
equivalent check, and the divergence is documented in
[Differences from Batect](../docs/differences-from-batect.md) and
[the config reference](../docs/config-reference.md#git-includes).

It exists because a bundle defines both the container *and* the command it
runs. Without containment, a third-party bundle could mount `~/.ssh`, `~/.aws`
or `~/.gnupg` into a container whose command it also controls — straightforward
credential exfiltration. That is a real escalation beyond the trust a bundle
already has (running code in a container with the project directory mounted),
which is why the hardening was worth diverging for.

**What forced this decision.** Expanding a leading `~` to the home directory
(matching Batect's `PathResolver.resolveHomeDir`) exposed a conflict that had
been masked. A common, legitimate bundle pattern is a machine-wide tool cache:

```yaml
containers:
  trivy:
    volumes:
      - local: ~/.cache/trivy          # shared across every project
        container: /home/container-user/.cache/trivy
```

Before `~` expansion this silently resolved to a literal `~` directory *inside
the clone* — wrong, but it satisfied containment, so the bundle "worked" while
its cache went nowhere useful. With expansion it resolves correctly, and
containment now (correctly, by its own rules) rejects it. The same bundle runs
fine under Batect.

**And there is no in-config workaround**, which is what makes this blocking
rather than merely annoying:

- `customise` cannot add volumes — it carries only `environment`, `ports` and
  `working_directory` (matching Batect).
- A project cannot redefine a container that came from an include: that's a
  hard error (*"The container 'x' is defined in multiple files"*).
- Switching the bundle to a `type: cache` volume requires *owning the bundle*,
  and isn't equivalent anyway: caches are per-project
  (`batect-cache-<project-key>-<name>`), so the tool re-downloads its data for
  every project — the opposite of what a shared home cache is for.

For a bundle the project owner doesn't control — the normal case, e.g. an
org-wide bundle or a published one — the only remaining option is to fork it.

**How common is this?** A survey of nine real bundles (`bundle-dev-bundle`, and
`hadolint-bundle`/`shellcheck-bundle` at four refs each) found **none using a
host path**: linter bundles mount the project directory and nothing else. Host
paths show up in the tool-cache pattern above, in bundles that projects include
*directly*. The one bundle that nests further Git includes pulls in two that
need no host paths. So the direct case is the one to solve well, and the nested
case (below) can wait for evidence that it happens.

## Decision

Add a per-include opt-in on a `type: git` include entry:

```yaml
include:
  - type: git
    repo: https://example.com/infra-bundle.git
    ref: 1.2.3
    allow_host_paths: true
```

`allow_host_paths: true` relaxes the containment check for containers defined
in *that* bundle, letting their host paths resolve anywhere. Five properties
define it:

1. **Explicit per include, never recursive.** Trusting a bundle says nothing
   about bundles *it* includes. (The bundle that prompted this itself nests two
   more Git includes — recursion would have silently extended trust to repos
   the project owner never chose, and which a moving `ref` could later change.)
2. **Honoured only in files the project owner controls** — the root config, or
   a local include of it. A `allow_host_paths` appearing *inside* a Git-included
   file is ignored. Without this the flag would be self-granting, and therefore
   worthless as a control.
3. **Named for what it does**, leaving the separately-planned
   `allow_nested_git_includes` (see
   [ROADMAP](../ROADMAP.md#future-vision)) as its own distinct control. One
   `trusted: true` covering both would blur two unrelated permissions.
4. **Boolean now, forward-compatible with an allowlist.** A later
   `allow_host_paths: ["~/.cache/*"]` is the same permission with values, the
   way Deno's `--allow-read` is bare-or-valued — so shipping the boolean
   forecloses nothing.
5. **Both binaries.** The containment applies to both today, and
   `ratect-compat` is where this is actually hit, since it's the binary
   consuming existing Batect bundles.

Safe by default is preserved: containment stays on for every include that
hasn't been explicitly vouched for.

### How this composes with the other two tracks

This decision is deliberately one of three, each solving a different part of
the problem — worth stating together so none is mistaken for the whole answer:

- **`allow_host_paths` (this ADR) — make existing bundles work now.** The only
  option that can help `ratect-compat`, which must run today's bundles
  *unmodified*.
- **A shared cache — solve the underlying need properly.** What `~/.cache/tool`
  is really asking for is "a cache that persists and is shared across
  projects"; it's spelled as a host path only because Batect offers no other
  way. A first-class shared cache (`type: cache` with cross-project scope)
  serves that exactly, grants no host filesystem access at all, and keeps the
  location under Ratect's control. It needs a new config field, so it can only
  ever be the answer for `ratect`-native configs — never for compat.
- **An allowlist — make the boolean better.** Narrowing "any host path" to
  "these host paths" is how a bundle that *can't* migrate to the native format
  (legacy, third-party, or simply not converting) gets supported at a security
  level closer to the shared cache.

## Alternatives considered

- **Improve the error message only, keep rejecting.** Rejected: with no
  in-config workaround (above), a better message just documents a dead end for
  exactly the bundles Git includes exist to consume. Worth doing *alongside*
  the flag, not instead of it.
- **Allow `$HOME` as a third permitted root, unconditionally.** Rejected: this
  re-opens precisely the credential paths containment closes (`~/.ssh`,
  `~/.aws`), for every bundle, with the project owner never deciding.
- **Drop containment from `ratect-compat` entirely** (true Batect parity).
  Rejected: it removes the hardening from the binary most people actually run.
  The divergence is deliberate, documented, and worth keeping.
- **Recursive trust.** Rejected — see property 1.
- **Trust declarable by the bundle itself.** Rejected — see property 2;
  self-granted permission is not a control.
- **A repo-keyed grant instead of a per-include one** — a top-level block in
  the owner's config naming repositories and what each may do, rather than an
  annotation on the include entry. Deferred, and the fallback if the nested
  case ever bites (see Consequences), because it's the one form that can reach
  a bundle the owner didn't declare. Not chosen first for two reasons: every
  case observed so far is a *direct* include, which the per-include form covers
  without ambiguity; and keying on a repository URL introduces matching
  fragility the per-include form simply doesn't have — `https` versus `ssh`
  spellings, a trailing `.git`, case differences, and whether a grant covers
  every `ref` or just one. (Ratect's own include cache sidesteps that by keying
  on the URL *exactly as written*, so two spellings of one repository are
  already two cache entries.) Adding it later means two ways to express a grant
  for a direct include, which is a real but acceptable cost, and one worth
  paying only against a real case.
- **Design the allowlist first.** Deferred, not rejected. There is exactly one
  real data point so far (a tool cache), and the matching rules have genuine
  unresolved questions — glob versus prefix, whether allowlist entries
  themselves get `~`-expanded, and how matching interacts with the canonical
  (symlink-resolving) check containment already performs, since a permitted
  `~/.cache/x` that is a symlink to `~/.ssh` must not pass. Designing that on
  one example would over-fit; the boolean is forward-compatible, so nothing is
  lost by waiting for evidence.
- **A shared cache instead of this.** Deferred as complementary, not
  competing — see the three tracks above. It cannot help `ratect-compat`.
- **Revert the `~` expansion.** Rejected: it matches Batect and is correct on
  its own terms. The behaviour it replaced — silently mounting a literal `~`
  directory — is strictly worse than either working or failing loudly.

## Consequences

- A legitimate bundle using a home-directory cache works again, with the
  project owner making the trust decision explicitly, per bundle.
- The cost is one line per project that includes the bundle. That is the
  intended price of explicitness: the grant lives where the decision is.
- **A nested bundle can't be granted this** — the project owner can't annotate
  an include entry they didn't write. That's a real gap, not a safe
  impossibility: if bundle X (which you don't control) includes bundle Y (which
  you also don't control) and Y needs a host path, the per-include form has no
  answer and forking is the only way out.

  It is deliberately left open, on evidence: across the nine real bundles
  available to survey (`bundle-dev-bundle`, and `hadolint-bundle`/
  `shellcheck-bundle` at four refs each) **none uses a host path at all**, and
  the only one that nests includes pulls in two that don't need them. The
  pattern that wants host paths — a tool cache — shows up in bundles projects
  include *directly*.

  If a real case does appear, the shape to reach for is a **repo-keyed grant in
  the owner's own config** (see the alternatives above), *not* making
  `allow_host_paths` recursive. A repo-keyed grant still satisfies properties 1
  and 2: it lives in a file the owner controls, and it names the specific
  repository rather than blanket-trusting whatever X happens to pull in.
- `allow_host_paths` is a **security control**: any later change must preserve
  properties 1 and 2, and an allowlist form must apply the same canonical
  symlink check the containment already does.
- Needs documenting in [the config reference](../docs/config-reference.md#git-includes)
  and [Differences from Batect](../docs/differences-from-batect.md), since it
  modifies a divergence already described there.
