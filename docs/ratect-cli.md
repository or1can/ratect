# `ratect` CLI Reference

This documents the **`ratect`** binary — the forward-looking CLI, free to diverge
from Batect's interface. For the Batect-compatible binary, see the
[`ratect-compat` CLI reference](cli-reference.md) instead; the two are described
separately because they are deliberately different interfaces, not two spellings of
one.

> **Status.** `ratect` is at 0.2.0: the subcommand surface, running on the same
> engine and the same `batect.yml` configuration `ratect-compat` reads. Its own
> configuration format is planned next and will not be YAML — see the
> [Roadmap](../ROADMAP.md#ratect). Everything about the *configuration* in
> [Configuration Reference](config-reference.md) applies as written today; only the
> command line differs from `ratect-compat`.

## Commands

| Command | What it does |
| --- | --- |
| `ratect run <task> [-- ARGS...]` | Runs a task. Anything after `--` is appended to the task command's own arguments. |
| `ratect tasks list` | Lists the tasks this project defines. |

There is deliberately **no `ratect <task>` shorthand**. `ratect-compat` takes a task
name as a bare positional argument, which works only because it has no subcommands;
as `ratect` grows verbs, "is `doctor` a task or a command?" becomes a question the
interface can't answer, so `run` is always explicit.

```bash
ratect tasks list
ratect run build
ratect run test -- --filter integration
```

## Global options

These work with every command, before or after it — `ratect -f custom.yml run build`
and `ratect run build -f custom.yml` are the same invocation.

| Option | Default | Description |
| --- | --- | --- |
| `-f`, `--config-file <PATH>` | `batect.yml` | The configuration file to read. |
| `--config-var <NAME=VALUE>` | — | Sets a [config variable](config-reference.md#configvariable). Repeatable; wins over `--config-vars-file` and the variable's own default. |
| `--config-vars-file <PATH>` | — | A YAML file of config variable `NAME: VALUE` pairs. |
| `-o`, `--output <STYLE>` | auto | `fancy`, `simple`, `all` or `quiet` — see [output styles](cli-reference.md#output-styles), which behave identically here. |
| `--no-color` | — | No color in Ratect's own output (never affects a task's own output). |

## `run` options

Only `run` connects to a Docker daemon, so only `run` takes the options for reaching
one — an accepted-but-ignored flag is worse than one that isn't offered.

| Option | Default | Description |
| --- | --- | --- |
| `--docker-host <HOST>` | `DOCKER_HOST`, then Docker's default | The daemon to connect to. Mutually exclusive with `--docker-context`. |
| `--docker-context <NAME>` | `DOCKER_CONTEXT`, then the CLI's active context | The Docker CLI context to connect through. |
| `--docker-config <PATH>` | `DOCKER_CONFIG`, then `~/.docker` | Where the Docker CLI's own configuration lives. |
| `--docker-tls`, `--docker-tls-verify` | — | Connect over TLS, always verifying the daemon's certificate — see [TLS with a private CA](cli-reference.md#tls-with-a-private-certificate-authority). |
| `--docker-cert-path <PATH>` | `DOCKER_CERT_PATH`, then `~/.docker` | Directory holding `ca.pem`/`cert.pem`/`key.pem`. |
| `--docker-tls-ca-cert`, `--docker-tls-cert`, `--docker-tls-key` | from `--docker-cert-path` | Individual TLS file overrides. |
| `--enable-buildkit` | — | Force BuildKit for image builds, over the daemon's default and `DOCKER_BUILDKIT`. |
| `--use-network <NAME>` | — | Reuse an existing Docker network instead of creating one for the task. |
| `--disable-ports` | — | Never bind container ports on the host. |
| `--no-proxy-vars` | — | Don't propagate [proxy environment variables](config-reference.md#proxy-environment-variables). |
| `--skip-prerequisites` | — | Run the task alone, without its `prerequisites`. |
| `--override-image <CONTAINER=IMAGE>` | — | Replace a container's image. Repeatable. |
| `--tag-image <CONTAINER=TAG>` | — | Extra tag for an image a container builds. Repeatable. |
| `--no-cleanup`, `--no-cleanup-after-success`, `--no-cleanup-after-failure` | — | Leave containers running for investigation. |
| `--max-parallelism <N>` | unbounded | Cap concurrent image pulls/builds. |
| `--cache-type <TYPE>` | `volume` | `volume` or `directory` — see [cache volumes](config-reference.md#cache-volumes). |

## Exit codes and diagnostics

Identical to `ratect-compat`: a task's own container exit code becomes `ratect`'s exit
code, anything else that fails exits `1`, and the reason always reaches stderr — in
every output style, including `quiet`. `RUST_LOG` controls Ratect's own internal
logging (default `info`, on stderr). Unlike `ratect-compat` there's no `--log-file`;
redirect stderr if you want one.

## Differences from `ratect-compat` today

| | `ratect-compat` | `ratect` |
| --- | --- | --- |
| Run a task | `ratect-compat <task>` | `ratect run <task>` |
| List tasks | `ratect-compat --list-tasks` | `ratect tasks list` |
| Cache cleanup | `--clean`/`--clean-cache` | not yet — planned as its own verb |
| Batect-inert flags (`--upgrade`, `--no-update-notification`, `--no-wrapper-cache-cleanup`) | accepted, no effect | not offered |
| `--log-file` | supported | not offered |
| Configuration | `batect.yml` | `batect.yml` today; own format planned |
