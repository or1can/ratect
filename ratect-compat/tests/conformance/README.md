# Batect conformance corpus

This directory vendors test projects from **Batect's own journey-test suite**
(`app/src/journeyTest/resources/` in the upstream repository) and runs
`ratect-compat` against them, asserting the same observable behaviour Batect's
own acceptance tests assert. It is the strongest evidence available that
`ratect-compat` is a drop-in replacement: the scenarios are Batect's, not ours,
so they cover cases we wouldn't have thought to test — the whole point (see
`ROADMAP.md`'s conformance section, the run-up to `ratect-compat` 1.0.0).

## Provenance and licence

The projects under `batect-journey/<name>/` are **copied verbatim** from Batect
(<https://github.com/batect/batect>), which is licensed under the Apache License,
Version 2.0 — the same licence as Ratect. Vendoring the frozen, archived fixtures
(rather than depending on the live repository at test time) is deliberate: they
never change, which is exactly what a conformance corpus wants, and there's no
network or submodule dependency. Attribution is recorded in the repository's
[`NOTICE`](../../../NOTICE) file, alongside the `dockerignore` port.

## Deviations from the vendored originals

Two projects are **not** verbatim, and each says so in the file itself:
`run-as-current-user` and `run-as-current-user-with-mount` bind `/output` to
`../../../../build/test-results/journey-tests/<name>` upstream — a path inside
Batect's own Gradle build tree, which does not exist here. Both now use
`<{batect.project_directory}/output`, which the harness resets before each run
and `.gitignore` covers. Nothing else in either project changes, and the
assertion that matters — that the file the container creates is owned by the
invoking user rather than root — is unaffected by where it is written.

Recorded here rather than only in a commit message because a conformance corpus
is only as trustworthy as its provenance: an unremarked edit to a vendored
fixture turns "Batect's own scenario passes" into "our version of it passes".

`git-include` is verbatim but **needs network access on its first run** — it
clones a bundle from GitHub, then reuses `~/.ratect/incl`. It is the only case
here that reaches outside Docker.

`container-with-dependency`'s and `container-with-multiple-dependencies`'s
Dockerfiles (`http-server`, `server-1`, `server-2`) bump their base image from
`nginx:1.25.0` to `nginx:1.30.4` — nginx's stable line uses even minor
versions (`1.30.x`), odd ones (`1.25.x`, `1.27.x`) are mainline/development.
The original pin's Debian bullseye is now
past its own upstream security-support window, so its `apt update` step
started failing — first on an expired Release file, then (once that's
bypassed) on packages the live mirror has already rotated out from under a
freshly-fetched index. Neither the health check nor the test's assertions
depend on the nginx version at all — it's a static HTML page behind a plain
`curl http://localhost` — so this is a maintenance bump, not a behavioral
change; a workaround that kept the EOL image alive (pointing `apt` at a frozen
`snapshot.debian.org` mirror) was considered and rejected as solving the wrong
problem. `task-with-unhealthy-dependency`'s Dockerfile and
`dependency-container-with-setup-command`'s config also pin `nginx:1.25.0` but
run no `apt` step, so neither is actually broken — left untouched rather than
bumped pre-emptively.

## What is (and isn't) asserted

Batect's own assertions are Kotlin and often check Batect's *exact* output
wording. `ratect-compat` deliberately diverges from some of that wording and UI
framing (see [`docs/differences-from-batect.md`](../../../docs/differences-from-batect.md)),
so the harness asserts on **observable behaviour** — exit codes, and the task
command's own output — not on Batect's exact transcript. That is the primary way
divergence is handled: assert what the container did, not how Batect framed it.

Where `ratect-compat` diverges *behaviourally* on purpose (a documented
simplification, not a bug), the harness records it as an explicit expectation
with a `divergence` note, so a difference is an asserted, documented fact rather
than a red test. Over time this turns `differences-from-batect.md` from prose
into an executable report.

The tests need a real Docker daemon and are `#[ignore]`d by default, like the
rest of the end-to-end suite. Run them with:

```
cargo test -p ratect-compat --test conformance -- --ignored
```
