# Installation

Ratect is currently **pre-release**. There are no published binaries or a `crates.io`
release yet, so the only way to install it today is to build it from source.

## Prerequisites

- [Rust](https://www.rust-lang.org/) (stable toolchain)
- [Docker](https://www.docker.com/), running and reachable via the default local
  socket (Ratect connects the same way the `docker` CLI does — no extra
  configuration needed for a standard Docker install).

  **A recent one.** Ratect speaks the Docker Engine API at version **1.53** and
  does not negotiate down to what your daemon offers, so a daemon older than that
  rejects every request with a "client version is too new" error — not just the
  newer features. For calibration, Docker Engine 29.4 reports API 1.54; check
  yours with `docker version --format '{{.Server.APIVersion}}'`.

  That floor is higher than Ratect actually needs, and is a consequence of not
  negotiating rather than a deliberate requirement — see
  [ROADMAP.md](../ROADMAP.md#batect-parity). The oldest release Ratect's *features*
  require is 20.10 (December 2020), for the `host-gateway` sentinel behind
  [proxy support](config-reference.md#proxy-environment-variables).

## Build from source

Clone the repository, then build a release binary. The workspace has two binary
crates (see [Roadmap](../ROADMAP.md#two-binaries-ratect-and-ratect-compat)) —
`ratect-compat` is the one that implements Batect-compatible behavior today:

```bash
git clone <repository-url>
cd ratect
cargo build --release -p ratect-compat
```

The compiled binary will be at `target/release/ratect-compat`.

## Install the binary onto your `PATH`

To make `ratect-compat` available as a regular command:

```bash
cargo install --path ratect-compat
```

This installs to `~/.cargo/bin` (assumed to already be on your `PATH`, which is the
default for a standard `rustup` install).

## Verify the install

```bash
ratect-compat --version
ratect-compat --help
```

## Development builds

If you're working on Ratect itself rather than just using it, a debug build is faster
to compile and sufficient for local testing:

```bash
cargo build --workspace
cargo run -p ratect-compat -- --list-tasks
```

See [`AGENTS.md`](../AGENTS.md) for the full contributor-facing tooling setup (formatting,
linting, tests, coverage, dependency auditing).
