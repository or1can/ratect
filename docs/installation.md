# Installation

Ratect is currently **pre-release**. There are no published binaries or a `crates.io`
release yet, so the only way to install it today is to build it from source.

## Prerequisites

- [Rust](https://www.rust-lang.org/) (stable toolchain)
- [Docker](https://www.docker.com/), running and reachable via the default local
  socket (Ratect connects the same way the `docker` CLI does — no extra
  configuration needed for a standard Docker install).

  **Docker 20.10 or newer.** Ratect negotiates the Docker Engine API version
  against your daemon at connection time, downgrading to whatever it offers, so
  any recent Docker install works with no extra configuration. 20.10 (December
  2020) is Ratect's own floor below that negotiation — the oldest release its
  *features* actually require, for the `host-gateway` sentinel behind [proxy
  support](config-reference.md#proxy-environment-variables) — and a daemon
  older than that is refused with a clear error naming both its version and
  the one required, rather than some later request failing for an unexplained
  reason. Check yours with `docker version --format '{{.Server.APIVersion}}'`
  (API 1.41 corresponds to Docker 20.10).

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
