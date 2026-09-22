# Installation

## Prebuilt binaries

Every tagged release of `ratect-compat` or `ratect` publishes prebuilt
binaries as GitHub Release assets, for five platforms:

| Target triple | Platform |
| --- | --- |
| `x86_64-unknown-linux-gnu` | x64 Linux |
| `x86_64-unknown-linux-musl` | x64 MUSL Linux (Alpine, minimal containers) |
| `aarch64-unknown-linux-musl` | ARM64 MUSL Linux (ARM servers, Raspberry Pi) |
| `x86_64-apple-darwin` | Intel macOS |
| `aarch64-apple-darwin` | Apple Silicon macOS |

`ratect-compat` and `ratect` are tagged and released independently (see
[`ROADMAP.md`](../ROADMAP.md#versioning--releases)), so they're listed as
separate entries on the [Releases page](https://github.com/or1can/ratect/releases)
— tags look like `ratect-compat/vX.Y.Z` and `ratect/vX.Y.Z`. Find the most
recent tag for the binary you want, then download the archive matching
your platform from that release's assets.

Each archive extracts to a directory (named after the archive itself)
containing the binary alongside `LICENSE`/`NOTICE`/`README.md`/`RELEASES.md`
— the binary isn't at the archive's top level:

```bash
tar -xf ratect-compat-x86_64-unknown-linux-gnu.tar.xz
mv ratect-compat-x86_64-unknown-linux-gnu/ratect-compat ~/.local/bin/
```

(substitute the archive name for your platform and binary; `~/.local/bin`
assumes it's already on your `PATH` — use whatever directory you normally
install user binaries into.)

**macOS**: Ratect's binaries aren't code-signed or notarized. If macOS
refuses to run the extracted binary ("cannot be opened because the
developer cannot be verified" or similar — some download methods, like a
browser, mark a downloaded file quarantined; others, like `curl`, don't),
clear it:

```bash
xattr -d com.apple.quarantine ratect-compat-x86_64-apple-darwin/ratect-compat
```

### Homebrew

Each release also publishes a formula to a shared tap
([`or1can/homebrew-tap`](https://github.com/or1can/homebrew-tap)) for both
binaries:

```bash
brew install or1can/tap/ratect-compat
brew install or1can/tap/ratect
```

### Install script

Each release also publishes a shell installer that downloads, verifies, and
extracts the right archive for your platform in one step:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/or1can/ratect/releases/download/ratect-compat/vX.Y.Z/ratect-compat-installer.sh | sh
```

Substitute the tag (`ratect-compat/vX.Y.Z` or `ratect/vX.Y.Z`) for the
release you want, and `ratect-compat-installer.sh`/`ratect-installer.sh` for
the matching binary — see the [Releases page](https://github.com/or1can/ratect/releases)
for both. Installs to `$CARGO_HOME/bin` (or `$HOME/.cargo/bin`), adding
that directory to `PATH` via your shell profile if it isn't already there.

### Verifying a download

Each release also includes a `sha256.sum` covering every archive and the
source tarball, and every archive/SBOM/`sha256.sum` itself carries a
[GitHub Artifact Attestation](https://github.com/or1can/ratect/attestations)
confirming it was built by Ratect's own CI from the tagged source, not
tampered with in transit:

```bash
# Checksum (Linux: sha256sum -c sha256.sum). A trailing blank line in the
# file itself makes shasum warn "1 line is improperly formatted" — harmless,
# every real entry still verifies.
shasum -a 256 -c sha256.sum

# Provenance (requires the GitHub CLI, `gh`)
gh attestation verify ratect-compat-x86_64-unknown-linux-gnu.tar.xz --repo or1can/ratect
```

### Not yet available

**`cargo-binstall`** needs Ratect published to crates.io to discover a
release automatically, which is itself blocked — see
[decisions/0010](../decisions/0010-release-binary-distribution.md)'s
crates.io deferral.

## Building from source

The from-source path below is for contributors, or anyone on a platform
without a prebuilt binary.

### Prerequisites

- [Rust](https://www.rust-lang.org/) (stable toolchain)
- [Docker](https://www.docker.com/), running and reachable via the default local
  socket (Ratect connects the same way the `docker` CLI does — no extra
  configuration needed for a standard Docker install).

  **Docker 20.10 or newer.** Ratect negotiates the Docker Engine API version
  against your daemon at connection time, downgrading to whatever it offers, so
  any recent Docker install works with no extra configuration. 20.10 (December
  2020) is Ratect's own floor below that negotiation — the oldest release its
  *features* actually require, for the `host-gateway` sentinel behind [proxy
  support](ratect-compat-config-reference.md#proxy-environment-variables) — and a daemon
  older than that is refused with a clear error naming both its version and
  the one required, rather than some later request failing for an unexplained
  reason. Check yours with `docker version --format '{{.Server.APIVersion}}'`
  (API 1.41 corresponds to Docker 20.10).

### Build from source

Clone the repository, then build release binaries. The workspace has two binary
crates (see [Roadmap](../ROADMAP.md#two-binaries-ratect-and-ratect-compat)):
`ratect`, which reads its own `ratect.toml` format, and `ratect-compat`, the
drop-in replacement for Batect that reads a `batect.yml` unchanged. [Getting
Started](getting-started.md) says which to pick; building both costs little
extra, since they share almost all of their code:

```bash
git clone https://github.com/or1can/ratect.git
cd ratect
cargo build --release -p ratect -p ratect-compat
```

The compiled binaries will be at `target/release/ratect` and
`target/release/ratect-compat`.

### Install the binaries onto your `PATH`

To make `ratect` and `ratect-compat` available as regular commands (one line per
binary, so install only the one you want if that's all you need):

```bash
cargo install --path ratect
cargo install --path ratect-compat
```

This installs to `~/.cargo/bin` (assumed to already be on your `PATH`, which is the
default for a standard `rustup` install).

### Verify the install

```bash
ratect --version
ratect --help
ratect-compat --version
ratect-compat --help
```

### Development builds

If you're working on Ratect itself rather than just using it, a debug build is faster
to compile and sufficient for local testing:

```bash
cargo build --workspace
cargo run -p ratect -- tasks list
cargo run -p ratect-compat -- --list-tasks
```

See [`AGENTS.md`](../AGENTS.md) for the full contributor-facing tooling setup (formatting,
linting, tests, coverage, dependency auditing).
