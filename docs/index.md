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

- New here? Start with [Installation](installation.md) and
  [Getting Started](getting-started.md).
- Coming from Batect? See [Differences from Batect](differences-from-batect.md)
  for what to expect.
- Looking for a specific flag or config field? Jump to a reference:
  [`ratect-compat` CLI](cli-reference.md) · [`batect.yml`](config-reference.md) ·
  [`ratect` CLI](ratect-cli.md) · [`ratect.toml`](ratect-config-reference.md).

Source and issue tracker: [or1can/ratect](https://github.com/or1can/ratect).
