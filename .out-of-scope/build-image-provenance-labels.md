# OCI Provenance Annotations/Labels on Built Images

Ratect does not offer a config field for setting build-time image labels
(`source`, `revision`, `created` — a built image's own provenance, distinct
from `Container.labels`, which applies to the *container*, not the image it
builds; also distinct from [decisions/0002](../decisions/0002-runtime-ownership-labels.md)'s
runtime-ownership labels, a different thing on a different object).

## Why this is out of scope

A Dockerfile `LABEL` instruction already does this natively, with zero
involvement from Ratect — there's nothing for a config field to add over just
writing `LABEL org.opencontainers.image.revision=...` in the Dockerfile
itself.

The one thing a config field could add is *ergonomics* — Ratect filling in
values like the current Git revision automatically, so the Dockerfile doesn't
need runtime-templated `LABEL` values. But Ratect shouldn't guess these:
shelling out to `git` against the build context would be wrong as often as
right (a build context vendored from elsewhere, a submodule, a CI checkout
with a detached/rewritten history, etc.), and any correct value already
requires the caller to compute it and either pass it as a build arg or
template the Dockerfile themselves — at which point a dedicated Ratect field
buys nothing a Dockerfile `LABEL` didn't already give them.

Only worth building if someone actually wants the ergonomics badly enough to
ask for it — no such request has surfaced yet.

## Prior requests

- #96 — "OCI annotations on built images"
