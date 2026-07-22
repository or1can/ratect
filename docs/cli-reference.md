# CLI Reference

```
ratect-compat [OPTIONS] [TASK_NAME] [-- ADDITIONAL_ARGS...]
```

This reflects the flags Ratect actually implements today (`ratect-compat/src/main.rs`),
not the full Batect CLI — see [differences from Batect](differences-from-batect.md) for
what's missing.

## Options

| Flag | Short | Default | Description |
|---|---|---|---|
| `--config-file <PATH>` | `-f` | `batect.yml` | Path to the configuration file to load. |
| `--list-tasks` | `-T` | — | List all tasks defined in the config file, then exit. Doesn't run anything. |
| `--config-var <NAME=VALUE>` | — | — | Sets a [config variable](config-reference.md#configvariable)'s value; repeatable. Takes precedence over `--config-vars-file` and the variable's `default`. |
| `--config-vars-file <PATH>` | — | — | A flat YAML file of config variable `name: value` pairs, in the same format as `batect.yml` itself. Lower precedence than `--config-var`. |
| `--use-network <NAME>` | — | — | Reuses an existing Docker network for every task in this invocation instead of creating (and removing) a fresh one per task. Errors clearly if the named network doesn't exist. See [task lifecycle](task-lifecycle.md). |
| `--disable-ports` | — | — | Disables publishing of any container's `ports` to the host, regardless of what's configured. |
| `--no-proxy-vars` | — | — | Don't propagate proxy-related environment variables (`http_proxy`, `https_proxy`, `ftp_proxy`, `no_proxy`) to image builds or containers. See [Proxy environment variables](config-reference.md#proxy-environment-variables). |
| `--skip-prerequisites` | — | — | Don't run the named task's own `prerequisites`. Only ever affects the task actually named on the command line — if that task is itself reached as someone else's prerequisite in a later invocation, this flag has no bearing on that. |
| `--override-image <CONTAINER=IMAGE>` | — | — | Overrides the image used by `CONTAINER`; repeatable. Replaces the container's `image`/`build_directory` and `image_pull_policy` entirely — the override is always pulled under the default `IfNotPresent` policy, regardless of what the container itself configures. Errors immediately if `CONTAINER` isn't defined in the config. |
| `--tag-image <CONTAINER=TAG>` | — | — | Tags the image built by `CONTAINER` with `TAG`, in addition to the default `<project_name>-<container_name>` tag; repeatable, and `CONTAINER` may be given more than once to apply multiple tags. Only valid for a container that actually builds an image — errors immediately if `CONTAINER` ends up using a pulled image (whether configured that way or via `--override-image`), and errors once the whole task (and its prerequisites) finishes if `CONTAINER` never actually ran. |
| `--no-cleanup-after-failure` | — | — | If an infrastructure error occurs (a build/pull/health-check/setup-command failure, or anything else before the task's own container gets to run), leave every container and network created for that task in place instead of removing them, so the issue can be investigated. A task's own container exiting non-zero is *not* "failure" for this purpose — see `--no-cleanup-after-success`. One divergence from Batect: containers are left genuinely *running*, not just present-but-stopped — see [Differences from Batect](differences-from-batect.md#cli-flags). |
| `--no-cleanup-after-success` | — | — | If the task's own container runs to completion — regardless of its exit code — leave every container and network created for that task in place instead of removing them. Same divergence as `--no-cleanup-after-failure`: left genuinely running, not stopped-but-present. |
| `--no-cleanup` | — | — | Equivalent to providing both `--no-cleanup-after-failure` and `--no-cleanup-after-success`. |
| `--enable-buildkit` | — | — | Use BuildKit for image builds, taking precedence over the `DOCKER_BUILDKIT` environment variable — see [config reference](config-reference.md#image-building). No `--disable-buildkit` counterpart; force the classic builder via `DOCKER_BUILDKIT=0`/`false` instead. |
| `--docker-host <HOST>` | — | — | Docker host to connect to, e.g. `unix:///var/run/docker.sock` or `tcp://1.2.3.4:5678`. Defaults to the `DOCKER_HOST` environment variable, then Docker's own platform default (a Unix socket or Windows named pipe). Cannot be combined with `--docker-context`. |
| `--docker-context <NAME>` | — | — | Docker CLI context to connect through — read from the Docker CLI's own context store (`~/.docker/contexts/`, or `--docker-config`'s directory). Defaults to the `DOCKER_CONTEXT` environment variable, then the Docker CLI's own active context (`~/.docker/config.json`'s `currentContext`). Cannot be combined with `--docker-host`. Errors clearly if the named context doesn't exist in the store. |
| `--docker-config <PATH>` | — | — | Directory containing the Docker CLI's own configuration files (context store, `config.json`). Defaults to the `DOCKER_CONFIG` environment variable, then `~/.docker`. |
| `--docker-tls` | — | — | Use TLS when connecting to the Docker host. Behaves identically to `--docker-tls-verify` — the daemon's certificate is always fully verified; there is no way to skip verification. Cannot be combined with `--docker-context`. |
| `--docker-tls-verify` | — | — | Use TLS when connecting to the Docker host, verifying its certificate. Defaults to the `DOCKER_TLS_VERIFY` environment variable. Cannot be combined with `--docker-context`. |
| `--docker-cert-path <PATH>` | — | — | Directory containing `ca.pem`/`cert.pem`/`key.pem` to authenticate to the Docker host and verify it, unless overridden individually by `--docker-tls-ca-cert`/`-cert`/`-key`. Defaults to the `DOCKER_CERT_PATH` environment variable, then `~/.docker`. Cannot be combined with `--docker-context`. |
| `--docker-tls-ca-cert <PATH>` | — | — | Path to the TLS CA certificate file used to verify the Docker host's own certificate. Defaults to `ca.pem` in `--docker-cert-path`'s directory. Cannot be combined with `--docker-context`. |
| `--docker-tls-cert <PATH>` | — | — | Path to the TLS certificate file used to authenticate to the Docker host. Defaults to `cert.pem` in `--docker-cert-path`'s directory. Cannot be combined with `--docker-context`. |
| `--docker-tls-key <PATH>` | — | — | Path to the TLS key file used to authenticate to the Docker host. Defaults to `key.pem` in `--docker-cert-path`'s directory. Cannot be combined with `--docker-context`. |
| `--max-parallelism <N>` | — | unbounded | Caps how many image pulls/builds, dependency container starts, and setup-command executions run concurrently across the whole invocation. Health-check waits and container stop/removal are never gated — see [Differences from Batect](differences-from-batect.md#cli-flags). |
| `--cache-type <volume\|directory>` | — | `volume` | Storage mechanism for a `cache` volume mount (see [Cache volumes](config-reference.md#cache-volumes)): `volume` resolves it to a Docker named volume, `directory` to a host directory under `<project_directory>/.batect/caches/<name>/`. Has no effect on a config with no `cache` mounts — but does still select which storage `--clean`/`--clean-cache` act on. |
| `--clean` | — | — | Removes every one of this project's own cache volumes/directories (per `--cache-type`) and exits — doesn't run anything, and doesn't need `--config-file` to actually exist. See [Cache volumes](config-reference.md#cache-volumes). |
| `--clean-cache <NAME>` | — | — | Removes just the named cache (repeatable) and exits, instead of every one of them. Given together with `--clean`, the explicit name(s) win — `--clean`'s own "everything" behavior only applies when `--clean-cache` is never given at all. |
| `--log-file <PATH>` | — | — | Writes Ratect's own internal logs to this file, in addition to stderr (both still governed by `RUST_LOG` — see [Environment variables](#environment-variables)). Plain text, no ANSI color codes, regardless of stderr's own coloring. |
| `--output <STYLE>` | `-o` | auto | Forces a particular output style for Ratect's own progress reporting: `fancy` (a live-updating status block, one line per container), `simple` (plain, append-only milestone lines), `quiet` (error messages only, and a machine-readable `--list-tasks` format), or `all` (line-by-line output from *every* container, prefixed with its name — the only style that changes what the task command's own output looks like; the others never touch it) — see [Output styles](#output-styles). Unset means auto-select: `fancy` on an interactive console, `simple` otherwise. |
| `--no-color` | — | — | Disables colored output from Ratect itself (task command output is never affected). Colors are already skipped automatically when stdout isn't a terminal, so this only matters on an interactive console. Also makes `simple` the auto-selected output style. |
| `--help` | `-h` | — | Print help (auto-generated by `clap`). |
| `--version` | `-V` | — | Print the Ratect version. |

### Recognized for Batect compatibility, no effect

`--upgrade`, `--no-update-notification`, and `--no-wrapper-cache-cleanup` are accepted
but do nothing — hidden from `--help`, since they're not real Ratect features, just
recognized so an existing Batect invocation carrying one doesn't hard-fail outright
(before these were recognized, any of them caused a `clap` parse error that killed the
*entire* invocation before anything ran at all, including `--list-tasks`). All three
only make sense for Batect's own self-updating wrapper script, which Ratect — a single
native binary — doesn't have and isn't planning to grow. `--upgrade` specifically
prints a one-line notice to stderr and exits `0` rather than running silently, since a
user invoking it is likely expecting *some* visible response; reinstall or rebuild
Ratect to get a newer version instead. The other two have no wrapper-cache/update
notification to disable in the first place, so they're silently accepted with no
message at all.

## Output styles

`--output`/`-o` controls how Ratect reports its own progress on stdout — never what
the task's command itself prints, which always streams through unmodified. The
styles are Batect's own four, all implemented:

- **`fancy`** — a live status block, one line per container in the task's
  dependency graph (`<name>: <what it's doing right now>` — pulling/building with
  live progress detail, waiting for dependencies, starting, waiting to become
  healthy, running setup commands, ready), repainted in place as events arrive.
  There is no spinner — the animation is purely rewriting changed lines, exactly
  like Batect. The moment the task's own container starts, the block freezes
  behind a blank line and the container's raw output streams below it untouched;
  after it exits, a single live `Cleaning up: ...` countdown line tracks
  teardown, then makes way for the final summary line. Lines are clipped to the
  terminal's current width. Requires an interactive console — an explicit
  `-o fancy` without one fails up front with a clear error (Batect instead
  accepts it and crashes on the first repaint). Works with
  [`--no-color`](#options) (the repaint stays; bold/color go — a combination
  Batect rejects).
- **`simple`** — plain, append-only milestone lines: `Running <task>...`,
  `Pulling <image>...`/`Pulled <image>.`, `Building <container>...`/`Built
  <container>.`, dependency start/health/setup-command milestones, a blank line +
  `Cleaning up...`, and a final `<task> finished with exit code <n> in
  <duration>.` summary (the exit code green/red on a color-capable console). No
  live-updating progress detail at all — safe for CI logs and redirected output.
  The health/setup-command milestones are shown for *dependency* containers
  only: the task's own container's readiness runs concurrently with its command
  (see [task lifecycle](task-lifecycle.md#known-simplifications-relative-to-batect)),
  so printing them would drop a line into the middle of that command's own
  output — use `all` (below) to see them. A readiness *failure* is still
  reported, on stderr, in every style.
- **`quiet`** — no milestone lines at all: stdout is exactly the containers' own
  output, so it's safe to pipe (error reporting stays on stderr, unchanged). Also
  switches `--list-tasks` to a machine-readable format: one task per line, sorted
  by name, as `name` alone or `name<TAB>description` — no header, no
  [grouping](config-reference.md#list-tasks-output).
- **`all`** — every line of output prefixed with the container it belongs to
  (`name    | `, padded to a common column, each container's prefix in its own
  color), interleaved as it happens. The only style that shows *dependency*
  containers' stdout/stderr, setup-command output (`Setup command N | ...`), and
  full image-build output (`Image build | ...`) — everything the other styles
  discard. In exchange, no container is interactive in this mode: the task
  container gets no TTY and no stdin, and every container gets `TERM=dumb`
  (matching Batect — a full-screen program can't render into line-prefixed
  output). Task-level lines (the `Running <task>...` preamble, `Cleaning up...`,
  the summary) carry the task's own name as their prefix.

When `--output` isn't given, Ratect auto-selects: `fancy` on an interactive
console (stdout a real terminal, `TERM` set and not `dumb`, terminal size
queryable, no `--no-color`); `simple` otherwise. `quiet` and `all` are never
auto-selected.

## TLS with a private certificate authority

`--docker-tls`/`--docker-tls-verify` always fully verify the Docker daemon's
certificate — there is no flag or environment variable that skips verification, unlike
Batect's own bare `--docker-tls` (which sets Go's `tls.Config.InsecureSkipVerify`,
disabling chain-of-trust, expiry, *and* hostname checks all at once, not just the
hostname check). This isn't just inherited from a missing feature: `rustls`, the
library Ratect's TLS support is built on, takes the same position deliberately —
there's no boolean toggle for skipping verification in `rustls` either, only a
`dangerous()` accessor that requires implementing the `ServerCertVerifier` trait from
scratch to bypass it. Ratect doesn't reach for that. If you've historically reached
for `--docker-tls` (skip-verify) because your daemon's certificate is self-signed —
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
   ratect-compat --docker-host tcp://docker-daemon.example.com:2376 \
     --docker-tls-verify \
     --docker-tls-ca-cert ./ca.pem \
     test
   ```

   Or set `--docker-cert-path` to a directory containing `ca.pem` (and
   `cert.pem`/`key.pem`, if the daemon requires client auth) instead of naming each
   file individually — see [Options](#options).

If verification fails, the error names the problem (expired, wrong host, untrusted
issuer) rather than silently connecting anyway — that's the entire point of not
supporting skip-verify. Regenerate whichever certificate is actually at fault, rather
than reaching for a flag Ratect doesn't have.

## Positional arguments

| Argument | Description |
|---|---|
| `TASK_NAME` | The name of the task to run, as defined under `tasks:` in the config file. Optional — if omitted (and `--list-tasks` isn't given), Ratect logs a warning and exits without doing anything. |
| `-- ADDITIONAL_ARGS...` | Anything after a literal `--` is appended as literal argv entries after the task's own tokenized `command` — see below. Only applies to the task named on the command line, never to its prerequisites. |

## Examples

```bash
# List tasks defined in ./batect.yml
ratect-compat --list-tasks

# Run a task from ./batect.yml
ratect-compat test

# Use a config file in a different location
ratect-compat -f ./ci/batect.yml build

# Pass extra arguments through to the task's command
ratect-compat test -- --verbose some/specific/file.rs

# Set a config variable referenced via `<name`/`<{name}` in `environment`
ratect-compat --config-var environment_name=staging test

# Load config variable values from a file instead
ratect-compat --config-vars-file ./ci/config-vars.yml test
```

### Using ADDITIONAL_ARGS in a task command

`run.command` is tokenized into literal argv (quote/backslash-aware whitespace
splitting, no shell involved — matching Batect's own tokenizer exactly), and anything
after `--` is appended as further literal argv entries — no special syntax needed in
`command` itself to receive them:

```yaml
tasks:
  test:
    run:
      container: build-env
      command: cargo test
```

Running `ratect-compat test -- --nocapture` here runs `cargo test --nocapture` inside the
container. Args are appended as literal argv entries (never concatenated into the
command string and re-parsed), so they're safe even if they contain characters that
would be shell metacharacters elsewhere, like `;`, `&&`, or backticks — Ratect never
passes `command`/`ADDITIONAL_ARGS` through a shell at all.

If the task's container has no `command` at all, `ADDITIONAL_ARGS` (when given) are
passed directly as the container's entrypoint arguments instead, matching plain
`docker run <image> <args>`.

## Exit codes and error reporting

Ratect uses a plain `0` (success) / non-zero (failure) convention, but note the current
actual behavior — it doesn't yet distinguish "nothing to do" from "success":

- Running with no task name at all (and not `--list-tasks`) currently **exits `0`** —
  Ratect logs a warning but doesn't fail the process. This is a rough edge, not
  intentional design; don't rely on it in scripts.
- A missing or malformed config file (fails to parse), a task/container referenced by
  name that doesn't exist, or a dependency cycle all cause a non-zero (`1`) exit. The
  error is printed to stderr as `Error: <message>` — deliberately *not* through
  `tracing::error!`/`RUST_LOG` (which every other diagnostic goes through — see
  [how it works](how-it-works.md#5-logging-vs-output)): a fatal error is the reason the
  process is about to exit non-zero, not an optional diagnostic, so it stays visible
  even under `RUST_LOG=off` or a filter that excludes Ratect's own target — including
  under [`-o quiet`](#output-styles), whose whole contract is "only error messages".
- A misspelled task name (whether given directly on the command line, or as a
  [`prerequisites`](config-reference.md#task) entry) gets a `Did you mean 'x'?`
  suggestion appended to the error, for every existing task name within a Levenshtein
  edit distance of 3 — ported from Batect's own `TaskSuggester`/`EditDistanceCalculator`
  (confirmed by reading Batect's source). Multiple equally-close matches are all
  suggested, e.g. `Did you mean 'build' or 'bulid'?` — Batect's own implementation
  can silently drop one of two equally-close suggestions (its sorting comparator
  doubles as its de-duplication key), which Ratect's deliberately doesn't replicate.
- **A failing command *inside* the container fails the `ratect` process too, with the
  same exit code.** Ratect waits for the container to exit and inspects its status —
  a task whose command is `exit 42` makes `ratect` itself exit `42`, matching
  `docker run`'s convention rather than collapsing every failure to a generic `1`. A
  task that runs as a [prerequisite](config-reference.md#task) and fails this way
  stops the rest of the chain immediately — no other prerequisites, and not the task
  that depended on it, will run — matching
  [Batect's documented behavior](https://github.com/batect/batect.dev/blob/main/docs/reference/config/tasks.md#prerequisites).

## Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | Controls log verbosity on stderr (`error`, `warn`, `info` [default], `debug`, `trace`) — and, if `--log-file` is given, the same file too. See [how it works](how-it-works.md#5-logging-vs-output). Unlike Batect, Ratect always logs to stderr regardless of `--log-file`; Batect's own default with no `--log-file` is silent. See [Differences from Batect](differences-from-batect.md#cli-flags). |
| `DOCKER_HOST` | Docker host to connect to — see `--docker-host`. |
| `DOCKER_CONTEXT` | Docker CLI context to connect through — see `--docker-context`. |
| `DOCKER_CONFIG` | Directory containing the Docker CLI's own configuration files — see `--docker-config`. |
| `DOCKER_CERT_PATH` | Directory containing `ca.pem`/`cert.pem`/`key.pem` for TLS — see `--docker-cert-path`. |
| `DOCKER_TLS_VERIFY` | Enables TLS (fully verified — see [TLS with a private certificate authority](#tls-with-a-private-certificate-authority)) — see `--docker-tls-verify`. |
| `DOCKER_BUILDKIT` | Forces the image builder on (`1`/`true`) or off (`0`/`false`) — see `--enable-buildkit` and [config reference](config-reference.md#image-building). |

Ratect supports interpolating host environment variables and config variables into
`environment` values, volume host paths, `build_directory`, and `build_args` in
`batect.yml` (`$VAR`, `${VAR:-default}`, `<name` — see
[config reference](config-reference.md#expressions)), but not yet within fields that
don't exist yet (`build_secrets.path`, `build_ssh.paths`) — see
[differences from Batect](differences-from-batect.md).
