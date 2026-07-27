# 0003 — `ratect`-native config format (TOML)

**Status:** Accepted — planned (ratect 0.3.0). Design settled; not yet
implemented. Supersedes the exploratory "Alternative Configuration Format (TOML)"
[Future Vision](../ROADMAP.md#future-vision) item.

## Context

`ratect` (the forward-looking binary — [ADR-0001](0001-two-binaries.md)) still
reads today's `batect.yml` unchanged. Its 0.3.0 scope is a **native config format
for this binary only** (`ratect-compat` stays YAML-forever, for Batect
compatibility). The point isn't the syntax — it's shedding the parts of the
`batect.yml` schema that only exist because YAML happened to be the format:

- YAML **anchors/aliases/merge keys** (`&`/`*`/`<<:`) are the only reuse
  mechanism, and they're document-scoped — they can't reach across `include`
  boundaries.
- Several fields accept a **compact string shorthand** (`"8080:80"`, `.:/code`, a
  bare include path) *and* an object form, which a native schema needn't carry.
- There's no first-class, native-named **local overrides file** — a gitignored,
  auto-loaded companion has no coherent name while the primary file is
  `batect.yml`.

## Decision

### Format: TOML, native default `ratect.toml`

**TOML** — idiomatic for a Rust tool, already a `ratect-core` dependency
(`cache.rs`'s sidecar, so no new crate), and a clean map for most of the schema
(named containers/tasks → dotted table headers; scalars/string-maps/scalar-lists
1:1). **Inline tables** keep the terse cases terse
(`volumes = [{ local = ".", container = "/code" }]`) alongside `[[...]]` blocks
for elaborate ones. `ratect` **defaults `-f` to `ratect.toml`** — a clean break,
with `batect.yml` reachable only by naming it explicitly; a native format that
stayed second-class in its own binary would be backwards.

### `extends` — inheritance replacing anchors

An explicit **`extends = "<name>"`** field on `Container` (not `inherits` — the
functional lineage is Docker Compose, though the *semantics* are Cargo's profile
model):

- **Resolved as a final pass, after expression/path resolution** — not merely
  after `include` merge. An inherited relative path is already absolute by the
  time a child inherits it, so it stays anchored to the *parent's* own file; this
  is the only ordering under which inheritance across `include` boundaries doesn't
  silently re-anchor paths to the child's directory.
- **Shallow, per field** — mechanically `child.or(parent)` over the (already
  `Option`) fields, exactly Cargo's `inherits`: a set field replaces, an unset one
  inherits, no recursion into nested maps.
- **Single-parent**, **transitive** (`A extends B extends C`), **cycle-checked**
  with the same ancestor-path walk `engine.rs` already runs for
  `dependencies`/`prerequisites`.
- **Base-only containers need no `abstract` marker**: the
  `image`-xor-`build_directory` requirement is enforced lazily in
  `engine.rs::resolve_image`, only for containers actually instantiated, so a
  `base` nothing runs never needs an image.
- **Container → container only**; tasks don't extend in 0.3.0.

### Object-shape schema, lenient parser

`volumes`/`ports`/`devices`/`include` standardize on **one object shape per
entry**, dropping the string shorthand *from the documented schema* — enforced by
the committed JSON schema (`schema.rs`), the docs, and `config validate`, **not by
the parser**. The string-or-object leniency lives in the field types' hand-written
`Deserialize` impls (`PortMapping`/`VolumeMount`/`IncludeEntry`), generic over any
serde `Deserializer`, so **one set of impls deserializes both a native TOML file
and a legacy YAML include** — which is what lets the two formats share a single
`ConfigFile` type.

### `ratect.local.toml` — local overrides

A **sibling** file (a `[local]`-style *section* can't be gitignored independently
of the tracked config it'd sit in), holding **config-variable values only**, not a
general field overlay. Config variables are already the sanctioned "this varies by
environment" surface, so anything locally overridable should be a declared
variable interpolated into the field (`image = "app:<{tag}>"`) — keeping
variability opt-in and visible in the tracked file rather than lurking in an
untracked one (the same stance `doctor`'s floating-tag warning takes). The escape
valve for "override a non-variable field" is "promote it to a variable," a
one-line change.

Mechanically it's just the **native default for `--config-vars-file`**
(TOML-parsed), as `ratect-compat` defaults that flag to `batect.local.yml`
(0.23.0) but native-named — a three-level precedence: `config_variables` defaults
< the config-vars file (auto `ratect.local.toml`, or whatever `--config-vars-file`
names instead) < `--config-var`.

### Mixed TOML/YAML includes

An `include` entry's format is chosen by the resolved file's **extension**
(`.toml` native, `.yml`/`.yaml` legacy; an unrecognized extension errors rather
than being content-sniffed). A git bundle with no explicit `path` is discovered in
order — **`ratect-bundle.toml` before `batect-bundle.yml`** (today's lone
`DEFAULT_GIT_INCLUDE_PATH` constant becomes a two-candidate probe). This keeps
unmigrated Batect bundles usable from a native project and lets a bundle author
support both tools at once (ship both; `ratect` prefers the TOML, Batect/
`ratect-compat` take the YAML). `extends` flows one way — a native container can
`extends` one from a YAML bundle (flat post-merge namespace), never the reverse.

Both new behaviours (per-extension selection and the `ratect-bundle.toml`-first
order) are **native-only** — a caller-supplied format/bundle policy — so
`ratect-compat` stays byte-compatible with Batect (YAML-only, `batect-bundle.yml`).

### Migration: `ratect config convert` / `validate`

A **`ratect config` verb**, landing with or just after 0.3.0, alongside
`run`/`tasks`/`caches`/`resources`/`doctor`/`includes`. **One-directional**
(`ratect-compat` stays YAML forever; the reverse is pointless and lossy):

- `convert`'s model is **correctness by inlining, ergonomics by best-effort
  lift**: an alias is a node copy, so expanding every `*ref` in place is always
  behaviour-preserving; the converter then *recognises* the idiomatic
  whole-container-`<<:`-merge case and lifts it to `extends`, warning where reuse
  structure (a non-container alias, a multi-merge `<<: [*a, *b]`) had to be
  flattened.
- It preserves **behaviour, not formatting** — comments are lost (noyalib
  discards them), ordering may change — so the output carries a provenance header
  and is a reviewable starting point. But it **self-verifies**: both formats
  resolve to the same `Config`, so it loads each and asserts the resolved models
  are equal (everything not surviving to the resolved model is exactly what's
  conceded).
- Writes `ratect.toml` **no-clobber** (`--force`/`--stdout` to override) and is
  **advisory-only** about the rest (suggests removing `batect.yml`, gitignoring
  `ratect.local.toml`, deleting the Batect wrapper `doctor` already flags — never
  does them).
- `config validate` is `doctor`'s config-only half as a CI-friendly, Docker-free
  command.

Designing `convert` now doubles as a **completeness test for the format** —
anything a `batect.yml` can express that `convert` can't represent is a schema gap.

## Alternatives considered

- **Format: KDL.** Genuinely more readable for this nested, repeated-node shape
  (no `[[containers.app.volumes]]` header repetition). Rejected: no mature
  serde deserializer — `knus` uses its own derive — so reusing
  `Config`/`Container`/`Task` would mean annotating every type in two derive
  systems forever, breaking the "only the front-end is new" property this whole
  plan rests on. Inline tables recover most of TOML's terseness anyway.
- **Format: a strict YAML dialect** (forbid anchors, require object shapes).
  Rejected: "it's YAML, except these features error" is more confusing than
  either a real new format or plain YAML, and gives no native file name that
  means anything.
- **Format: a config *language* (CUE/Dhall/Nickel/Jsonnet).** Would subsume
  `extends` and `config_variables` natively. Rejected: wildly overkill and a huge
  dependency for a devtask file.
- **`extends`: deep (recursive) merge.** Rejected: TOML has no `null`, so deep
  merge's unavoidable companion — an explicit "unset this inherited key" — has no
  natural expression (the RFC-7386 null-deletes trick, Compose's `!reset`, Nix's
  `mkForce` all need a null/tag/expression TOML lacks). Shallow also matches
  `<<:`'s existing model, so migrated configs behave identically. Ansible
  (`hash_behaviour` defaults to replace) and Cargo profiles (shallow-per-field)
  are the closest precedents and both chose shallow.
- **`extends`: keyword `inherits`** (matching Cargo exactly). Rejected: the
  *semantics* being Cargo-like is the valuable part; the domain term users will
  search for is Compose's `extends`.
- **Local file: a general field overlay** (override any field, not just config
  vars). Rejected: makes every field implicitly locally-overridable via an
  untracked file — the floating-tag reproducibility hazard generalized — and
  reopens all the merge-semantics questions `extends` just closed.
- **Local file: a `[local]` section in `ratect.toml`.** Rejected: a section can't
  be gitignored independently of the tracked file it lives in, which is the whole
  contract.
- **`extends` resolved before expression/path resolution.** Rejected: a child
  inheriting a relative `build_directory` from a base in another directory would
  re-anchor it to the child's directory — silently wrong. See the resolve-then-
  extend rationale above.
- **Includes: native-only (no YAML includes).** Rejected: it would make git
  bundles a hard break until every upstream bundle repo migrated, and would block
  incremental migration. Accepting YAML includes dissolves the migration corner
  entirely.

## Consequences

- **`engine.rs`/`docker.rs`/`ui/` need no changes at all** — only the TOML
  front-end and the `extends` pass are new, the same way `include` resolution is
  already invisible past `Config::load_from_file`. The format reuses
  `ratect-core`'s resolved `Config`/`Container`/`Task` types unchanged past
  parsing.
- **Incremental migration works at every step**: a `ratect.toml` root can include
  a not-yet-converted local `.yml` fragment, and git bundles keep working
  untouched, so a project is valid throughout the move.
- `ratect-compat` is entirely unaffected — every new behaviour is gated behind a
  native-only policy, preserving its Batect byte-compatibility.
- A new config field now carries three obligations in lockstep: its value type,
  the (lenient) `Deserialize`, and — for a string-or-object type — a
  hand-written `JsonSchema` (already true; see `schema.rs`). `extends` adds a
  fourth consideration only for whether a field should be `Option` so "unset vs.
  inherit" is representable.
