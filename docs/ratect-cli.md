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
| `ratect caches list` | Lists this project's existing caches. |
| `ratect caches clean [NAME...]` | Removes this project's caches, or just the named ones. |
| `ratect resources list` | Lists containers and networks left over from previous runs. |
| `ratect resources clean` | Removes them. |

There is deliberately **no `ratect <task>` shorthand**. `ratect-compat` takes a task
name as a bare positional argument, which works only because it has no subcommands;
as `ratect` grows verbs, "is `doctor` a task or a command?" becomes a question the
interface can't answer, so `run` is always explicit.

```bash
ratect tasks list
ratect run build
ratect run test -- --filter integration
ratect caches list
ratect caches clean gradle-cache
ratect resources list
ratect resources clean --older-than 1d
```

## Global options

These work with every command, before or after it — `ratect -f custom.yml run build`
and `ratect run build -f custom.yml` are the same invocation.

| Option | Default | Description |
| --- | --- | --- |
| `-f`, `--config-file <PATH>` | `batect.yml` | The configuration file. `caches` uses it only to locate the project *directory* — it never reads the contents. |
| `-o`, `--output <STYLE>` | auto | `fancy`, `simple`, `all` or `quiet` — see [output styles](cli-reference.md#output-styles), which behave identically here. |
| `--no-color` | — | No color in Ratect's own output (never affects a task's own output). |

Narrower options attach to the commands that actually use them, rather than being
global: a flag that's accepted and then ignored reads as a promise. So the
config-variable options below belong to `run` and `tasks list` (the commands that read
configuration), and the Docker connection options to `run` and `caches` (the ones that
reach a daemon).

| Option | Applies to | Description |
| --- | --- | --- |
| `--config-var <NAME=VALUE>` | `run`, `tasks list` | Sets a [config variable](config-reference.md#configvariable). Repeatable; wins over `--config-vars-file` and the variable's own default. |
| `--config-vars-file <PATH>` | `run`, `tasks list` | A YAML file of config variable `NAME: VALUE` pairs. |

## Docker connection options

Taken by `run` and by `caches` (whose default storage is Docker volumes); never by
`tasks list`, which reaches no daemon at all.

| Option | Default | Description |
| --- | --- | --- |
| `--docker-host <HOST>` | `DOCKER_HOST`, then Docker's default | The daemon to connect to. Mutually exclusive with `--docker-context`. |
| `--docker-context <NAME>` | `DOCKER_CONTEXT`, then the CLI's active context | The Docker CLI context to connect through. |
| `--docker-config <PATH>` | `DOCKER_CONFIG`, then `~/.docker` | Where the Docker CLI's own configuration lives. |
| `--docker-tls`, `--docker-tls-verify` | — | Connect over TLS, always verifying the daemon's certificate — see [TLS with a private CA](cli-reference.md#tls-with-a-private-certificate-authority). |
| `--docker-cert-path <PATH>` | `DOCKER_CERT_PATH`, then `~/.docker` | Directory holding `ca.pem`/`cert.pem`/`key.pem`. |
| `--docker-tls-ca-cert`, `--docker-tls-cert`, `--docker-tls-key` | from `--docker-cert-path` | Individual TLS file overrides. |

## `run` options

| Option | Default | Description |
| --- | --- | --- |
| `--enable-buildkit` | — | Force BuildKit for image builds, over the daemon's default and `DOCKER_BUILDKIT`. Only `run` builds images, so only `run` takes it. |
| `--use-network <NAME>` | — | Reuse an existing Docker network instead of creating one for the task. |
| `--disable-ports` | — | Never bind container ports on the host. |
| `--no-proxy-vars` | — | Don't propagate [proxy environment variables](config-reference.md#proxy-environment-variables). |
| `--skip-prerequisites` | — | Run the task alone, without its `prerequisites`. |
| `--override-image <CONTAINER=IMAGE>` | — | Replace a container's image. Repeatable. |
| `--tag-image <CONTAINER=TAG>` | — | Extra tag for an image a container builds. Repeatable. |
| `--no-cleanup`, `--no-cleanup-after-success`, `--no-cleanup-after-failure` | — | Leave containers running for investigation. |
| `--max-parallelism <N>` | unbounded | Cap concurrent image pulls/builds. |
| `--cache-type <TYPE>` | `volume` | `volume` or `directory` — see [cache volumes](config-reference.md#cache-volumes). |

## `caches` options

`--cache-type <volume|directory>` (default `volume`) selects which storage to act on,
for both `list` and `clean` — a cache in one is invisible to the other, so this has to
match how the project runs its tasks.

`caches` never reads the configuration file. A cache belongs to the project
*directory*, so both commands work on a project whose configuration is broken or
missing entirely — which is exactly when clearing a cache tends to be what's needed.

`caches list` prints each cache under the name a `volumes` entry gives it, not the
`batect-cache-<key>-<name>` Docker volume it's stored in; that name is what
`caches clean` takes back. Under `-o quiet` it's one name per line and nothing else,
for scripting. Naming a cache that doesn't exist warns on stderr rather than passing
silently, since the likeliest cause is a typo.

## `resources` options

Containers and networks outlive a run when something goes wrong — a crash, a
`docker kill`, a `--no-cleanup` run, or a cleanup that failed. `resources` finds
them by the labels Ratect stamps on everything it creates, so they're identifiable
however long ago they were made:

```
$ ratect resources list
2 left over from 1 previous run:

  integration-test (3 days ago, run a01df375-8365-4689-85e4-11b33dee70b8):
    - container database (running)
    - network ratect-a01df375-8365-4689-85e4-11b33dee70b8

Remove them with: ratect resources clean
```

Grouped by run, because that's the unit a leftover belongs to: one interrupted task
leaves a network and every container it started, and they only make sense together.
A container is named as your configuration names it (`database`), not by the random
words Docker assigns.

| Option | Applies to | Description |
| --- | --- | --- |
| `--all-projects` | `list`, `clean` | Every *Ratect* project's leftovers, not just this one's — never anything Ratect didn't create. Also the way to use `resources` from outside a project directory, since the project scope is read from the configuration. |
| `--older-than <AGE>` | `list`, `clean` | Only leftovers older than `AGE` — `90s`, `30m`, `2h`, `7d`. |

**`--older-than` matters for `clean`.** A task running *right now* carries exactly the
same labels as a leftover, because until it finishes it is one. Ratect can't tell the
difference — the daemon can't say whether some other `ratect` process still cares
about a container — so a bare `resources clean` on a shared machine can tear down an
in-flight run. `--older-than 1h` is the safe form when anything else might be running.

Under `-o quiet`, `list` prints resource ids one per line and nothing else, ready to
pipe into `docker rm`. Removal takes containers before networks, since a network
still holding an endpoint can't be removed; a resource that fails to remove is
reported and the rest still go.

Like `caches`, `resources` reads the configuration only for the project's name —
never for what to remove, which comes from the labels alone.

Nothing without Ratect's own labels is ever listed or removed, `--all-projects`
included: containers started by other tools, and Docker's built-in `bridge`/`host`/
`none` networks, are invisible to both commands.

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
| Cache cleanup | `--clean`/`--clean-cache` | `ratect caches clean [NAME...]` |
| Listing caches | not available | `ratect caches list` |
| Finding leftovers from a previous run | not available | `ratect resources list`/`clean` |
| Batect-inert flags (`--upgrade`, `--no-update-notification`, `--no-wrapper-cache-cleanup`) | accepted, no effect | not offered |
| `--log-file` | supported | not offered |
| Configuration | `batect.yml` | `batect.yml` today; own format planned |
