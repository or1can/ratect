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

Under twenty lines gets a real Postgres, waits for it to actually be ready (not
just "started"), seeds it, then queries what it just seeded — no
`wait-for-it.sh`, no manual networking, no leftover container or volume once
it's done:

```toml
project_name = "ratect-demo"

[containers.db]
image = "postgres:16"
environment = { POSTGRES_HOST_AUTH_METHOD = "trust" }
health_check = { command = "pg_isready -h 127.0.0.1 -U postgres", interval = "1s", retries = 10 }
setup_commands = [
    { command = "psql -U postgres -c \"CREATE TABLE greeting (message TEXT)\"" },
    { command = "psql -U postgres -c \"INSERT INTO greeting VALUES ('Hello from Ratect!')\"" },
]

[containers.client]
image = "postgres:16"

[tasks.demo]
run = { container = "client", command = "psql -h db -U postgres -c 'SELECT message FROM greeting'" }
dependencies = ["db"]
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

A real recording, not a mockup — `fancy` output shows both containers'
status live, updating in place. `health_check` is the readiness gate;
`setup_commands` runs *inside* `db` after it's healthy but before `client`
ever starts, so the row is already there by the time anything queries it —
see [Dependency readiness](config-reference.md#dependency-readiness) for the
full model. Both containers are removed afterwards regardless of how the
task ends, whether it's the first run (pulling the image first) or, as
recorded here, a later one.

- New here? Start with [Installation](installation.md) and
  [Getting Started](getting-started.md), or jump straight to a [worked
  example](worked-examples.md) for your language.
- Coming from Batect? See [Differences from Batect](differences-from-batect.md)
  for what to expect.
- Looking for a specific flag or config field? Jump to a reference:
  [`ratect-compat` CLI](cli-reference.md) · [`batect.yml`](config-reference.md) ·
  [`ratect` CLI](ratect-cli.md) · [`ratect.toml`](ratect-config-reference.md).

Source and issue tracker: [or1can/ratect](https://github.com/or1can/ratect).
