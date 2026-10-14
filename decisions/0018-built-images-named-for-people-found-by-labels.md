# 0018 — Built images are named for people and found by labels

**Status:** Accepted — the naming half is planned for ratect-compat 0.33.0 ·
ratect 0.12.0 ([#290](https://github.com/or1can/ratect/issues/290),
[#302](https://github.com/or1can/ratect/issues/302),
[#436](https://github.com/or1can/ratect/issues/436),
[#453](https://github.com/or1can/ratect/issues/453)); the lifecycle half is
unscheduled ([#599](https://github.com/or1can/ratect/issues/599),
[#600](https://github.com/or1can/ratect/issues/600)). Designed in
[#547](https://github.com/or1can/ratect/issues/547).

## Context

A container with a `build_directory` is built and tagged
`<project_name>-<container name>` — the *automatic tag*, in
[`CONTEXT.md`](../CONTEXT.md)'s terms — in both binaries, as Batect does
(`BuildImageStepRunner.imageTagFor`). The run itself doesn't use that tag:
`resolve_image` runs and caches the image ID the build reports, so two
overlapping runs re-tagging the same name can't make each other run the wrong
image. The tag exists for people — `docker images`, a script, a Dockerfile's
`FROM` of a sibling's previous build.

Nothing checks that the two names make a valid tag. Batect rejects a
`project_name` or container name that isn't a valid image-name component when
it loads the file (`ImageNameValidator`, the grammar Docker's own reference
parser uses for a name component); Ratect accepts anything, so an uppercase
name fails at build time with the daemon's `invalid reference format`, and a
`:` in `project_name` silently moves the container name into the tag position
(`s4-i290:x-c` is repository `s4-i290`, tag `x-c` —
[#290](https://github.com/or1can/ratect/issues/290)'s reproduction). The
`--tag-image` values are checked nowhere before the build either
([#302](https://github.com/or1can/ratect/issues/302)), and Batect only checks
them inside the build request, so a typo there costs a prerequisite run.

The parity sweep filed these one at a time, alongside two questions about the
flags that reach the same tag — `--tag-image` and `--override-image` naming the
same container ([#436](https://github.com/or1can/ratect/issues/436)), an
override for a container the run never uses
([#453](https://github.com/or1can/ratect/issues/453)) — and the container
and network naming moving to Batect's suffixed scheme
([#299](https://github.com/or1can/ratect/issues/299)). Deciding them
separately risked settling the name before the thing the name has to serve
next: the roadmap's Image Lifecycle item (ownership labels on built images,
cleaning up dangling rebuilds, building and pushing independently of a task),
none of which Batect has — it never removes an image, and labels nothing on
one.

A survey of comparable tools (Docker Compose v2, Tilt, Skaffold, Earthly,
Dagger, the devcontainers CLI; primary sources, October 2026) found one close
precedent and a spread of others. Compose names a built image
`<project>-<service>` with no tag, labels it at build time with
`com.docker.compose.project`, `.service` and `.version`, and `down --rmi
local` removes "only images that don't have a custom tag", found by those
labels — with a fallback by name for images "produced by an older version of
Compose" that "did not always label built images", and orphan services'
images spared unless asked. A plain `compose build` never removes the previous
build. Its project name is lowercased and stripped to `[a-z0-9_-]` when
*inferred*, but an explicit `name:` that differs from its normalised form is
an error. Tilt tags by content digest (`tilt-<hex>`), labels
`dev.tilt.gc=true`, and prunes on a timer keeping the two most recent per
reference, skipping anything multi-tagged. Skaffold and Earthly require a
user-supplied name (Earthly labels `dev.earthly.*`); Dagger has no default
name and touches no daemon-side image; devcontainers derives
`vsc-<folder>-<hash>` and sanitises rather than rejects. The pattern that
recurs wherever a tool both names and cleans up: the name is a convenience,
a label is the ownership record, and a rebuild leaves the old image for a
separate command.

## Decision

### The name is for people; the label is for Ratect

- **Lifecycle identifies a built image by label, never by tag.** Ownership
  labels, baked into the image at build time in the namespace
  [0002](0002-runtime-ownership-labels.md) established, are what any `images`
  or `resources` verb will match on ([#599](https://github.com/or1can/ratect/issues/599)).
  A tag can't see an untagged rebuild — the very thing cleanup is for — and
  can't tell project `p`'s container `a-b` from project `p-a`'s container `b`.
- **So the automatic tag stays `<project_name>-<container name>`**, with no
  suffix and no uniqueness device, in both binaries. For `ratect-compat` it's
  parity; for `ratect` it follows by [0013](0013-when-ratect-compat-copies-batect.md)'s
  default, and there's no reason to depart: a suffix would destroy the one
  property the tag has that matters, a stable name for "this container's
  latest build". Collisions across projects on one daemon, and the `p`/`p-a`
  ambiguity, are accepted as the user's to avoid, as Batect and Compose
  accept them: a collision re-tags, and never runs the wrong image.
- Nothing native-only is added now. A user-chosen image name is `--tag-image`'s
  job, and can become a config field later without the default changing.
  Referring to the *current* run's images or containers from configuration —
  a `FROM` of a sibling's build within one run, ordered after it — is a
  separate design ([#601](https://github.com/or1can/ratect/issues/601)); the
  only thing this record promises it is that the automatic tag stays what it
  is and stays stable.

### Names are checked at load, as image-name components

- **`project_name` and every container name must be an image-name
  component** — `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*$`, Batect's rule and
  Docker's — checked when the file loads
  ([0014](0014-config-checked-at-load-evaluated-per-task.md)), in both
  dialects. Every container, not only one with a `build_directory`: the name
  also becomes a Docker container name under
  [#299](https://github.com/or1can/ratect/issues/299) and a label value, and
  a container gains a `build_directory` later without anyone revisiting its
  name. If both halves satisfy the rule, so does `<project>-<container>`, so
  each is checked on its own and the error names the one field that's wrong.
- **Rejected, not normalised.** Lowercasing or stripping a `:` would silently
  change what the tag means, the failure [0013](0013-when-ratect-compat-copies-batect.md)
  rates worst; the fix is a one-word edit.
- **The one exception is an inferred project name.** Once `ratect-compat`
  infers `project_name` from the directory
  ([#395](https://github.com/or1can/ratect/issues/395)) it does what Batect
  does: lowercase the directory name, then apply the rule, then tell the user
  to set `project_name` if it still fails. The directory name was never the
  user's choice of image name, so lowercasing it changes nothing they meant —
  the same line Compose draws between an inferred name and an explicit
  `name:`. The native format keeps requiring `project_name`.
- **`--tag-image` values are full image references** —
  `registry.example.com/team/app:1.2` is legitimate — so they're checked
  against Docker's reference grammar, not the component rule, and **at
  argument parse**, before anything runs, in both binaries. The value is known
  before a single container starts, and a typo shouldn't cost a prerequisite
  run. The outcome matches Batect (rejected); the point of failure is earlier,
  a divergence recorded on the differences page. `--override-image` values
  stay unchecked, as in Batect: a bad one fails at pull with the value in the
  message, and checking a reference the daemon is about to parse gains nothing.
- **Tags are still applied after the build**, not in the build request as
  Batect does. Once the values are checked up front a post-build tag can fail
  only on a daemon error, and either way the run fails; what changes is that
  every failed tag is reported, not the first.

### What lifecycle may do, decided now so the name doesn't preclude it

- **A run never removes an image.** Runs use image IDs precisely so one can't
  pull an image from under another; removal is only ever a verb, as it is for
  Batect and Compose ([#600](https://github.com/or1can/ratect/issues/600)).
- **The verb's default removes only an image that is labelled Ratect's *and*
  has no tag** — a dangling rebuild. The current build, carrying the
  automatic tag, is never a default candidate; removing a tagged image needs
  an explicit ask. The rule is this narrow because an image a user builds
  `FROM <project>-<container>` *inherits* Ratect's labels: "labelled ours"
  alone doesn't mean "built by Ratect". Under the narrow rule such an image
  is safe unless the user built it untagged, which `docker image prune` would
  remove too.
- **Images built before labels existed** can be matched, for a release or
  two — as Compose still matches images an older Compose left unlabelled —
  by the one thing Ratect alone would have written — an image whose
  only tag is the automatic tag of a container the project declares — then
  the fallback is dropped, in [0002](0002-runtime-ownership-labels.md)'s
  "sticky, not irreversible" spirit. A dangling pre-label image is
  unmatchable; that's the honest limit.
- These are *ownership* labels on the object Ratect made, not provenance
  annotations: the rejection of a provenance field
  ([`.out-of-scope/build-image-provenance-labels.md`](../.out-of-scope/build-image-provenance-labels.md))
  stands.

## Alternatives considered

- **A unique automatic tag** — a per-run suffix like
  [#299](https://github.com/or1can/ratect/issues/299)'s container names, or
  a hash of the project path. Rejected: it leaves no stable name to refer to
  the latest build by, which is the tag's only job once labels carry
  identity, and a hash makes the name useless to the person reading `docker
  images`.
- **`<project>/<container>`**, a repository path with separable halves.
  Rejected: it's not what Batect writes, so every `ratect-compat` user's
  `FROM` and script would change for a benefit labels already give.
- **Validating only containers that build.** Rejected: the same name is a
  container name and a label value, and a rule that depends on which fields a
  container has today is the per-call-site guard
  [`AGENTS.md`](../AGENTS.md) warns about — the next use of the name is
  unguarded by default.
- **Normalising invalid names** (lowercasing an explicit `project_name`).
  Rejected: see above; only an inferred name, which the user never typed as
  an image name, is normalised.
- **Checking `--tag-image` where Batect does**, inside the build. Rejected:
  it reports a known-bad command line only after prerequisites have run, and
  as a build failure rather than a usage error.
- **Removing the previous build during a run**, when a rebuild leaves it
  dangling. Rejected: another run may be using it, and the daemon can't say.
  No surveyed tool does it on build either; Tilt, the one that prunes
  unasked, does so on a timer with an age floor and keeps the two newest.
- **Removing the current, automatically tagged build by default**, as
  Compose's `down --rmi local` does. Rejected for the default: `down` is a
  teardown, where a leftover sweep's default has to be safe to run between
  two builds on two terminals. The explicit ask covers it.
- **Finding images by the automatic tag instead of labels.** Rejected: it
  can't see a dangling rebuild, and it's ambiguous across projects.

## Consequences

- `ratect-compat` and `ratect` reject a `project_name` or container name that
  Batect rejects, at load, for every command that loads the file. For
  `ratect.toml` that's a breaking change — a pull-only container named `Web`
  loads today — so the entry is marked `**ratect.toml:**` and
  `written_for::BREAKING_CHANGES` gets a row
  ([0015](0015-native-project-records-its-written-for-version.md)).
- A bad `--tag-image` value is a usage error on every command that accepts
  the flag, `--list-tasks` included — consistent with the parse-time
  `--tag-image`/`--override-image` cross-check
  ([#436](https://github.com/or1can/ratect/issues/436)). Recorded as a
  divergence.
- The first build under a version that labels images produces a new image
  ID, leaving the previous build dangling; so does every Ratect upgrade if
  `version` is among the labels. Layer caching is unaffected. Whether
  `version` is worth that is [#599](https://github.com/or1can/ratect/issues/599)'s
  to decide.
- Image labels are a parity divergence of the additive kind
  [0002](0002-runtime-ownership-labels.md) already records; the differences
  page gains a line when they land.
- The `p`/`p-a` ambiguity and cross-project collisions remain. Labels resolve
  them for Ratect; nothing resolves them for a person reading `docker
  images`, and this record doesn't pretend to.
