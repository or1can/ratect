# Connecting to Docker

Every command that reaches a Docker daemon takes the same set of connection
options, on both binaries: `ratect run`, `ratect caches`, `ratect resources`
and `ratect doctor` each accept them (`ratect tasks list`, `ratect includes`
and `ratect config` never connect, so they don't), and `ratect-compat` accepts
them on every invocation and uses them whenever the invocation actually
connects (a `--list-tasks` run never does). This page is the one description
of those options; the [`ratect`](ratect-cli.md#docker-connection-options) and
[`ratect-compat`](ratect-compat-cli.md#docker-connection) CLI references only
link here.

With none of them given, Ratect connects through a Docker context if one
resolves (`DOCKER_CONTEXT`, then the active context), otherwise to
`DOCKER_HOST` if it is set, otherwise to Docker's own platform default (a Unix
socket or Windows named pipe) — see [Which daemon is
used](#which-daemon-is-used).

## Options

| Flag | Default | Description |
|---|---|---|
| `--docker-host <HOST>` | `DOCKER_HOST` | Docker host to connect to, e.g. `unix:///var/run/docker.sock` or `tcp://1.2.3.4:5678`. Cannot be combined with `--docker-context`. |
| `--docker-context <NAME>` | `DOCKER_CONTEXT`, then the active context | Docker CLI context to connect through — read from the Docker CLI's own context store (`~/.docker/contexts/`, or `--docker-config`'s directory). The active context is `~/.docker/config.json`'s `currentContext`. Cannot be combined with `--docker-host` or any of the TLS options below. Errors clearly if the named context doesn't exist in the store. |
| `--docker-config <PATH>` | `DOCKER_CONFIG`, then `~/.docker` | Directory containing the Docker CLI's own configuration files (context store, `config.json`). |
| `--docker-tls` | — | Use TLS when connecting to the Docker host. Behaves identically to `--docker-tls-verify` — the daemon's certificate is always fully verified; there is no way to skip verification. |
| `--docker-tls-verify` | `DOCKER_TLS_VERIFY` | Use TLS when connecting to the Docker host, verifying its certificate. Needs a host: `--docker-host` or `DOCKER_HOST`. |
| `--docker-cert-path <PATH>` | `DOCKER_CERT_PATH`, then `~/.docker` | Directory containing `ca.pem`/`cert.pem`/`key.pem` to authenticate to the Docker host and verify it, unless overridden individually by the three options below. |
| `--docker-tls-ca-cert <PATH>` | `ca.pem` in `--docker-cert-path` | The TLS CA certificate used to verify the Docker host's own certificate. |
| `--docker-tls-cert <PATH>` | `cert.pem` in `--docker-cert-path` | The TLS certificate used to authenticate to the Docker host. |
| `--docker-tls-key <PATH>` | `key.pem` in `--docker-cert-path` | The TLS key used to authenticate to the Docker host. |

## Which daemon is used

A flag always wins over its environment variable. Between the two ways of
naming a daemon, the order is:

1. `--docker-context <NAME>` on the command line, if given.
2. Otherwise, if `--docker-host` is given on the command line, that host — no
   context is consulted at all.
3. Otherwise a context, if one resolves: `DOCKER_CONTEXT`, then the active
   context.
4. Otherwise `DOCKER_HOST`, then Docker's own platform default.

A context named `default` — from any of the three sources — means "no
context", as in the Docker CLI, so step 4 applies.

A connection through a context uses the endpoint that context stores; the TLS
options only apply to a host connection, and naming one alongside
`--docker-context` is an error.

## Environment variables

| Variable | Effect |
|---|---|
| `DOCKER_HOST` | Docker host to connect to — the default for `--docker-host`. |
| `DOCKER_CONTEXT` | Docker CLI context to connect through — the default for `--docker-context`. |
| `DOCKER_CONFIG` | Directory containing the Docker CLI's own configuration files — the default for `--docker-config`. |
| `DOCKER_CERT_PATH` | Directory containing `ca.pem`/`cert.pem`/`key.pem` for TLS — the default for `--docker-cert-path`. |
| `DOCKER_TLS_VERIFY` | `1` or `true` (any case) enables TLS, fully verified — the default for `--docker-tls-verify`. Any other value leaves it off. |

## TLS with a private certificate authority

`--docker-tls`/`--docker-tls-verify` always fully verify the Docker daemon's
certificate — there is no flag or environment variable that skips verification, unlike
Batect's own bare `--docker-tls` (which sets Go's `tls.Config.InsecureSkipVerify`,
disabling chain-of-trust, expiry, *and* hostname checks all at once, not just the
hostname check). This isn't just inherited from a missing feature: `rustls`, the
library Ratect's TLS support is built on, takes the same position deliberately —
there's no boolean toggle for skipping verification in `rustls` either, only a
`dangerous()` accessor that requires implementing the `ServerCertVerifier` trait from
scratch to bypass it. Ratect doesn't reach for that. If you reach for `--docker-tls`
(skip-verify) because your daemon's certificate is self-signed —
including for local development or CI — the fix isn't to skip verification, it's to
make the certificate verifiable: run your own certificate authority, and trust *that*,
rather than trusting nothing.

The daemon side of this (configuring `dockerd` to require TLS, generating its
server certificate) is standard Docker documentation, not Ratect-specific — see
[Protect the Docker daemon socket](https://docs.docker.com/engine/security/protect-access/).
What follows is the client side: a self-contained, worked example of generating a
private root CA, signing a server certificate for the daemon with it, and pointing
Ratect at the result.

1. **Create a root CA.** This is the one certificate you'll trust from now on — keep
   `ca-key.pem` private; it's the only thing standing between "verified" and "not".

   ```bash
   openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 -nodes \
     -keyout ca-key.pem -out ca.pem -subj "/CN=my-docker-ca"
   ```

2. **Generate and sign the daemon's own certificate**, naming every hostname/IP
   clients will actually connect through as a Subject Alternative Name (SAN) —
   verification checks this, not the certificate's `CN`:

   ```bash
   openssl req -newkey rsa:4096 -sha256 -nodes \
     -keyout server-key.pem -out server-req.pem -subj "/CN=docker-daemon"
   openssl x509 -req -in server-req.pem -CA ca.pem -CAkey ca-key.pem -CAcreateserial \
     -out server-cert.pem -days 3650 -sha256 \
     -extfile <(printf "subjectAltName=DNS:docker-daemon.example.com,IP:203.0.113.10")
   ```

3. **Configure `dockerd`** to require TLS with this certificate (`/etc/docker/daemon.json`
   or the equivalent `dockerd` flags — see the Docker documentation linked above),
   using `ca.pem`/`server-cert.pem`/`server-key.pem` from steps 1–2.

4. **Point Ratect at the CA** (client certificate/key are only needed if the daemon
   itself also requires client auth — generate a second cert signed by the same CA
   for that, following step 2's pattern):

   ```bash
   ratect run test \
     --docker-host tcp://docker-daemon.example.com:2376 \
     --docker-tls-verify \
     --docker-tls-ca-cert ./ca.pem
   ```

   `ratect-compat` takes the same three options, before the task name. Or set
   `--docker-cert-path` to a directory containing `ca.pem` (and
   `cert.pem`/`key.pem`, if the daemon requires client auth) instead of naming each
   file individually — see [Options](#options).

If verification fails, the error names the problem (expired, wrong host, untrusted
issuer) rather than silently connecting anyway — that's the entire point of not
supporting skip-verify. Regenerate whichever certificate is actually at fault, rather
than reaching for a flag Ratect doesn't have.
