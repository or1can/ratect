# Introduction

Define your development tasks — build, test, lint, a database to develop
against, … — once in configuration, and run them identically on any machine
that has Docker. No "works on my machine", no per-developer setup drift, no
JVM to start: Ratect is a single native binary, so it runs instantly.

If that sounds like [Batect](https://github.com/batect/batect), that's
deliberate: Batect was archived in October 2023, and Ratect grew out of it —
same `batect.yml`, running on a native binary instead of a JVM one. But it's
not just a port: Ratect already fixes real bugs Batect never got to (leaked
containers and networks on anything but Ctrl+C, [an eight-year-old open proxy
issue](https://github.com/batect/batect/issues/10)) and adds tooling Batect
never had — `ratect doctor`, `ratect resources`, a native `ratect.toml`
format. See [Differences from Batect](differences-from-batect.md) for the
full list, drop-in replacement included. It's an independent project, not
affiliated with or endorsed by the original Batect project.

## See it in action

A real request against a real, composed stack — an app, a database, and a
cache, each waiting for the last to actually be ready (not just
"started") before it starts itself — no `wait-for-it.sh`, no manual
networking, no leftover containers once it's done. This is the dependency
graph from
[`examples/full-stack`](https://github.com/or1can/ratect/tree/main/examples/full-stack)
— that project also has `build`/`unit-test`/`lint`/`shell` tasks, omitted
here to keep this to the part that's actually running below:

```toml
{{#include ../examples/full-stack/ratect.toml:homepage-demo}}

{{#include ../examples/full-stack/ratect.toml:homepage-demo-task}}
```

<div id="demo-player"></div>
<script>
// mdBook injects `additional-js` near the end of <body>, after this inline
// script — AsciinemaPlayer isn't defined yet at this point in the page, only
// once the whole document (including that later script tag) has loaded.
window.addEventListener('DOMContentLoaded', function () {
  // Real speed, deliberately: the recording's own output prints how long
  // Ratect actually took, and slowing playback down would make that claim
  // and what's on screen disagree. `loop` has no built-in pause between
  // repeats, so that's driven manually via the `ended` event instead.
  var player = AsciinemaPlayer.create('demo.cast', document.getElementById('demo-player'), {
    autoPlay: true,
    cols: 80,
    rows: 24,
  });
  player.addEventListener('ended', function () {
    setTimeout(function () { player.play(); }, 2500);
  });
});
</script>

A real recording, not a mockup — `fancy` output shows all four containers'
status live, updating in place, each independently: `db` seeding a real
million-row table before it's ready (genuinely, not padded — that's why it
takes longer than `app`/`cache`), then running a real `setup_commands` step
(`ANALYZE visits`, giving the query planner fresh statistics after that
bulk insert) once it's healthy but before anything depends on it, then
`app` and `journey-test` both starting only once `cache` says it's ready —
`cache` itself, once, even though it's a dependency of both. `journey-test`
makes a real HTTP request to `app` (the JSON in the middle is its real
response), then checks Redis *directly* to confirm the value `app` read
got cached — see [Dependency Readiness](dependency-readiness.md)
for the full model behind all of it. Every container is removed afterwards
regardless of how the task ends.

## Two binaries, two formats

Ratect ships two binaries. **`ratect`** reads its own `ratect.toml` format and is
the one to install if you're starting fresh — it's the forward-looking CLI, free
to add things Batect never had. **`ratect-compat`** reads a `batect.yml`
unchanged, as a drop-in replacement for the (now unmaintained) `batect` binary,
and stays `batect.yml`-only permanently — pick it if you already have a Batect
project and want a maintained tool without editing any YAML. Both run the same
engine and exit with the same codes, are versioned and released independently,
and can be mixed: `ratect -f batect.yml` reads a Batect config too, a
`ratect.toml` project can `include` a `batect.yml` fragment as-is, and
[`ratect config convert`](ratect-cli.md#config) translates a whole `batect.yml`
when you're ready to move.

## Where to go next

- New here? Start with [Installation](installation.md) and
  [Getting Started](getting-started.md), or jump straight to a [worked
  example](worked-examples.md) for your language.
- Coming from Batect? Start with [Migrating a Batect Project to
  Ratect](migrating-batect-project.md), then [Differences from
  Batect](differences-from-batect.md) for what to expect.
- Weighing Ratect against Docker Compose, Make, Task, Earthly, Dagger or
  `just`? See [How Ratect Compares to Other Tools](comparison.md).
- Looking for a specific flag or config field? Jump to a reference:
  [`ratect` CLI](ratect-cli.md) · [`ratect.toml`](ratect-config-reference.md) ·
  [`ratect-compat` CLI](ratect-compat-cli.md) · [`batect.yml`](ratect-compat-config-reference.md).

Source and issue tracker: [or1can/ratect](https://github.com/or1can/ratect).
