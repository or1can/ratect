# Wildcard (Glob) Includes

Ratect does not support glob patterns in `include` entries (e.g.
`containers/*.yaml`). Every include is a single, explicit, literal path.

## Why this is out of scope

Batect wanted this too and never built it — there's no real forcing use case
behind the request, just the general convenience of not listing files one by
one. Explicit, literal includes already work today and are simple to reason
about: what's included is exactly what's named, fixed at the commit that
named it.

A glob's main cost isn't a security hole — containment/trust checks
(`include_trust.rs`) apply per resolved path regardless of how a candidate
was discovered, so a glob-matched file is checked exactly like a literal one.
The cost is *reviewability*: a glob resolves lazily against directory
contents at load time, so its effective file set can change in a later,
unrelated commit that never touches the file declaring the glob. That's a
real, if minor, design smell — worth accepting once there's a concrete need
pulling the feature in, not worth taking on speculatively.

If this is revisited, the design work is already done (see #99's own
discussion) and doesn't need re-deriving:

- Local-file includes only — `type: git` includes stay out, since they
  already carry their own trust/grant complexity that shouldn't be tangled
  with glob-matching semantics in the same change
- Single directory level only (`*`/`?`/`[...]`), no recursive `**`
- A glob matching zero files is a config-load error, not a silent no-op
- Matches included in a deterministic (sorted) order
- `ratect`-only (`ratect.toml`) — no `batect.yml` equivalent
- The lazy-resolution tradeoff gets documented, not solved with new tooling
  (`config validate` already resolves everything, on demand)

## Prior requests

- #99 — "Wildcard includes"
