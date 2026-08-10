# `ratect` CLI Reference

This documents the **`ratect`** binary — the forward-looking CLI, free to diverge
from Batect's interface. For the Batect-compatible binary, see the
[`ratect-compat` CLI reference](cli-reference.md) instead; the two are described
separately because they are deliberately different interfaces, not two spellings of
one.

> **Status.** From 0.3.0 `ratect` reads its own **native TOML configuration**
> (`ratect.toml` by default) rather than sharing `ratect-compat`'s `batect.yml` —
> see the [Roadmap](../ROADMAP.md#ratect) and
> [decisions/0003](../decisions/0003-ratect-native-config-format.md). Its full
> schema is the [`ratect.toml` reference](ratect-config-reference.md); it's the
> same schema [Configuration Reference](config-reference.md) documents for
> `batect.yml`, re-spelled in TOML, with `extends` in place of YAML anchors. A
> `batect.yml` is still readable by naming it with `-f`, so a project can migrate
> incrementally — [`ratect config convert`](#config) translates one automatically.
> The native format is `ratect`'s alone — `ratect-compat` stays `batect.yml`-only,
> permanently.

## The native config format

`ratect.toml` is [`batect.yml`](config-reference.md)'s schema in TOML: named
containers and tasks become tables, and list entries (`volumes`, `ports`,
`devices`) become inline tables or `[[...]]` blocks. A small example:

```toml
project_name = "my-app"

[containers.base]
image = "rust:1.90"
volumes = [{ local = ".", container = "/code" }]

[containers.build-env]
extends = "base"
working_directory = "/code"

[tasks.build]
run = { container = "build-env", command = "cargo build" }
```

The native additions over `batect.yml` are **`extends`** — a container inherits
one named parent's fields (shallow, per-field, single-parent), replacing YAML
anchors — and **mixed includes**: each `include` is parsed by its extension
(`.toml` native, `.yml`/`.yaml` as YAML), and a pathless `type: git` bundle
prefers `ratect-bundle.toml` over `batect-bundle.yml`. The full schema — the TOML
spelling of every field, the `extends` rules, object shapes, includes, and local
overrides — is the [`ratect.toml` reference](ratect-config-reference.md).

### Local overrides

A **`ratect.local.toml`** beside your config file is loaded automatically when
present — no `--config-vars-file` needed — supplying [config
variable](config-reference.md#configvariable) *values* (a flat `name = "value"`
map) for the current developer or machine. Gitignore it. See
[the reference](ratect-config-reference.md#local-overrides) for precedence and
the reasoning.

## Commands

Grouped by purpose. Within a command, the sub-verbs follow their natural
workflow order (`list` before `clean`/`refresh`) rather than alphabetically.
`ratect --help` lists the commands in this same order — `clap` can't render the
group headings there, so the order alone carries them.

### Running tasks

| Command | What it does |
| --- | --- |
| `ratect run <task> [-- ARGS...]` | Runs a task. Anything after `--` is appended to the task command's own arguments. |
| `ratect tasks list` | Lists the tasks this project defines. |

### Managing resources

| Command | What it does |
| --- | --- |
| `ratect caches list` | Lists the caches this project can see, project-scoped and shared, with the scope of each. |
| `ratect caches clean [NAME...]` | Removes this project's caches, or just the named ones — which may be shared. |
| `ratect includes list` | Lists the cached Git includes shared by every project on this machine. |
| `ratect includes clean [--all]` | Removes cached Git includes. |
| `ratect includes refresh` | Re-clones them, picking up a `ref` that has moved. |
| `ratect resources list` | Lists containers and networks left over from previous runs. |
| `ratect resources clean` | Removes them. |

### Configuration & diagnostics

| Command | What it does |
| --- | --- |
| `ratect config validate` | Checks the configuration loads and is problem-free, without a daemon — a CI-friendly gate. |
| `ratect config convert` | Converts a `batect.yml` (point `-f` at it) into a native `ratect.toml`. |
| `ratect doctor` | Checks this project and this machine for problems, without running anything. |

### Shell integration

| Command | What it does |
| --- | --- |
| `ratect completions <shell>` | Prints a shell completion script (`bash`, `zsh`, `fish`, `powershell`, `elvish`) to stdout — see [Shell completion](#shell-completion). |

There is deliberately **no `ratect <task>` shorthand**. `ratect-compat` takes a task
name as a bare positional argument, which works only because it has no subcommands;
as `ratect` grows verbs, "is `doctor` a task or a command?" becomes a question the
interface can't answer, so `run` is always explicit.

```bash
ratect run build
ratect run test -- --filter integration
ratect tasks list

ratect caches list
ratect caches clean gradle-cache
ratect includes list
ratect includes refresh
ratect resources list
ratect resources clean --older-than 1d

ratect config validate
ratect doctor

ratect completions zsh
```

## Global options

These work with every command, before or after it — `ratect -f custom.yml run build`
and `ratect run build -f custom.yml` are the same invocation.

| Option | Default | Description |
| --- | --- | --- |
| `-f`, `--config-file <PATH>` | `ratect.toml` | The configuration file. Parsed by extension — `.toml` as the native format, `.yml`/`.yaml` as Batect-format YAML — so `-f batect.yml` keeps reading a Batect config while migrating. `caches` uses it only to locate the project *directory* — it never reads the contents. |
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
| `--config-vars-file <PATH>` | `run`, `tasks list` | A file of config variable values (a flat `NAME = VALUE` map), parsed as TOML or YAML by extension. Defaults to an auto-discovered [`ratect.local.toml`](#local-overrides) beside the config file, when present. |

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
Docker volume it's stored in; that name is what `caches clean` takes back. Under
`-o quiet` it's one name per line and nothing else, for scripting — deduplicated,
since the same name can exist in both scopes. Pair it with `--scope` for a list
that is unambiguous, and so safe to pipe straight back into
`caches clean --scope`. Naming a cache that doesn't exist warns on stderr rather
than passing silently, since the likeliest cause is a typo.

`--scope <project|shared>` restricts both commands to one kind of cache. A
[shared cache](ratect-config-reference.md#shared-caches) is one every project on
the machine can use, so the two are worth telling apart:

```
$ ratect caches list
Caches for this project:
- build-output (project)
- cargo-registry (shared)
```

**`caches clean` with no names removes this project's caches only, never a shared
one.** Discarding storage other projects are still using should take naming it.
Naming one does remove it — and if a project cache and a shared cache happen to
share a name, the command refuses rather than guessing, since removing the wrong
one is silent either way:

```
$ ratect caches clean cargo-registry
Error: 'cargo-registry' names both a project cache and a shared one. Re-run with
'--scope project' or '--scope shared' to say which to remove.
```

Both scopes are read from *storage*, not from the configuration — a project cache
is found by its `batect-cache-<key>-` prefix, a shared one by
`ratect-shared-cache-`. That is what keeps these commands working on a project
whose configuration is broken.

## `includes` options

The Git include cache under `~/.ratect/incl` — where a `type: git`
[include](config-reference.md#git-includes) is cloned and kept.

```
$ ratect includes list
1 cached Git include(s), 16.4 MiB on disk:

  https://github.com/example/shared-tasks.git at v2.1.0
    16.4 MiB, last used 3 days ago
```

Unlike [`caches`](#caches-options) and [`resources`](#resources-options), this cache is
**global** — one directory shared by every project on this machine, keyed by
`(repo, ref)`. So there's no project scoping, and `clean` reaches other projects'
includes as well as your own. That matters less than it sounds: everything here is
re-cloneable, so the worst case is a fetch.

| Command | Description |
| --- | --- |
| `includes clean` | Removes includes nothing has used for 30 days — the same threshold the [automatic sweep](config-reference.md#git-includes) applies, done on demand. |
| `includes clean --older-than <AGE>` | A different threshold (`30m`, `2h`, `7d`). |
| `includes clean --all` | Everything, regardless of age. |
| `includes refresh` | Discards every cached clone and fetches it again. |

**`refresh` is how you pick up a moved `ref`.** A `(repo, ref)` pair is cloned once and
then never re-fetched, so if `ref` is a branch — or a tag someone re-pushed — your
project keeps using whatever it pointed at the first time, indefinitely. The automatic
sweep doesn't help, because it removes entries that go *unused*, and an include you're
actively using never becomes stale. Pinning `ref` to something immutable remains the
better answer; `refresh` is for when it isn't.

Under `-o quiet`, `list` prints `repo<TAB>ref` per line and nothing else.

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

Grouped by run, because that's the unit a leftover belongs to: a run that was killed
outright, or crashed, leaves a network and every container it started, and they only
make sense together. (An ordinary Ctrl+C isn't one of those cases any more — it cleans
up after itself; see [Differences from
Batect](differences-from-batect.md#runtime-behavior-gaps).)
A container is named as your configuration names it (`database`), not by the random
words Docker assigns.

| Option | Applies to | Description |
| --- | --- | --- |
| `--all-projects` | `list`, `clean` | Every *Ratect* project's leftovers, not just this one's — never anything Ratect didn't create. Also the way to use `resources` from outside a project directory, since the project scope is read from the configuration. |
| `--older-than <AGE>` | `list`, `clean` | Only leftovers older than `AGE` — `90s`, `30m`, `2h`, `7d`. |

**`resources list` is `clean`'s dry run.** Both take the same options and select
identically, so whatever `list` shows you is exactly what `clean` with those same
options will remove — there's no separate `--dry-run` because there's nothing for it
to do differently.

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

## `config`

`ratect config validate` is `doctor`'s configuration half on its own — it loads the
config, resolves it, and runs the same config-only checks (missing
`build_directory`/Dockerfile, floating image tags, dependencies with no
`health_check`), exiting non-zero on a problem. It never touches Docker, so it's the
gate to run in CI when all you want to know is "is the config valid?", without a
daemon. It takes the same `--config-var`/`--config-vars-file` options as `run`, since
resolving the config can need them.

`ratect config convert` migrates a Batect-format `batect.yml` to a native
`ratect.toml` — point `-f` at the `batect.yml`:

```bash
ratect -f batect.yml config convert          # writes ratect.toml beside it
ratect -f batect.yml config convert --stdout  # prints instead, to review or pipe
```

It's **one-directional** (`ratect-compat` stays YAML; the reverse would be lossy) and
writes `ratect.toml` only if one doesn't already exist — pass `--force` to overwrite,
or `--stdout` to print. The conversion **preserves behaviour, not formatting**: YAML
anchors/aliases/merge keys are expanded inline, `include`d files (Git bundles too) are
flattened into the one result, and comments are dropped — so the output carries a
header and is a *starting point to review*, not a blind drop-in. Before writing, the
conversion is checked to round-trip losslessly back to the same configuration, so
whatever it produces is guaranteed to behave identically to the original. (This first
version emits the compact `"8080:80"` / `.:/code` string forms for `ports`/`volumes`
rather than the object form; both are valid, and reformatting is a review step.)

## `doctor`

Answers "why did that fail?", or "will it?", without running a task:

```
$ ratect doctor
Checking ratect.toml...
  ok      Docker daemon reachable (29.4.0)
  ok      ratect.toml loads (3 container(s), 1 task(s))
  warning container 'database' uses a floating image tag — pin it, or the same configuration will run a different image later
  warning dependency 'cache' has no health_check — unless its image defines one, it counts as ready the moment it starts
  problem container 'app' has build_directory '/project/missing-dir', which doesn't exist
  warning 4 resource(s) left over from previous runs — see `ratect resources list`

6 check(s): 1 problem(s), 3 warning(s).
```

A **problem** will fail a run — an unreachable daemon, a configuration that doesn't
load, a missing `build_directory` or Dockerfile. A **warning** works but is likely to
bite: a floating image tag (`latest`, or no tag at all) means the same configuration
runs a different image next week, and a dependency with no `health_check` counts as
ready the moment it starts unless its image defines one, which is where "connection
refused" on the first run comes from.

If you're **migrating from Batect**, `doctor` also flags a leftover `batect`/`batect.cmd`
wrapper script. Those aren't harmless: `./batect` still downloads and runs the
unmaintained JVM binary, so you can think you've switched to Ratect while `./batect`
quietly runs the old tool.

**Delete the wrapper and run `ratect` (or `ratect-compat`, for strict Batect
compatibility) from your `PATH`.** Batect's committed wrapper *was* its installer — it
fetched the right JVM version on demand — whereas Ratect is an ordinary binary you
install once, so there's nothing for a committed wrapper to do. (Don't repoint the
wrapper at Ratect by symlinking it: a committed symlink is machine-specific and doesn't
work for `batect.cmd` on Windows, and it still needs the binary on the `PATH` anyway.)

The one exception is a codebase with `./batect` hardcoded across CI jobs, Makefiles and
docs that you can't change all at once: there, replacing the wrapper with a one-line
transitional shim — `exec ratect-compat "$@"` — keeps those call sites working while you
migrate them (it still needs `ratect-compat` on the `PATH`). A wrapper that no longer
runs Batect isn't flagged.

`doctor` **exits non-zero if it found any problem**, and zero for warnings alone, so
it works as a CI step. Under `-o quiet` it prints only warnings and problems.

The environment checks run even when the configuration itself won't load —
"your config is broken *and* your daemon isn't running" is more useful than fixing
one to discover the other. It also reports leftovers unprompted, since the whole
reason [`resources`](#resources-options) exists is that nobody thinks to look.

## Shell completion

`ratect completions <shell>` prints a completion registration script to stdout
for `bash`, `zsh`, `fish`, `powershell` or `elvish`. Source it from your shell's
startup file:

```bash
# bash — in ~/.bashrc
source <(ratect completions bash)

# zsh — in ~/.zshrc, *after* compinit has run (see the note below)
source <(ratect completions zsh)

# fish — in ~/.config/fish/config.fish
ratect completions fish | source
```

> **zsh needs its completion system initialized first.** The zsh script ends
> with a `compdef` call, and `compdef` only exists once `compinit` has run — so
> the `source` line must come *after* `autoload -U compinit && compinit` in your
> `~/.zshrc` (frameworks like oh-my-zsh already run `compinit` for you; just
> keep the `source` line after they load). Sourcing it in a bare shell that
> hasn't run `compinit` fails with `command not found: compdef`; run
> `autoload -U compinit && compinit` first to try it interactively. bash and
> fish need no such initialization.

It completes command and flag names, their fixed values (`-o fancy|simple|…`,
`--cache-type volume|directory`), file-path arguments, and — the part that earns
its keep on a task runner — **task names**: `ratect run <TAB>` lists the tasks
your config defines. That last one is *dynamic*: the installed script re-invokes
`ratect` at completion time to read the config, always without cloning, pulling,
or touching Docker, so a `<TAB>` is instant and safe.

Task completion honours an explicit `-f`, and follows the config's `include`s —
local files, and Git includes that are *already* cached. It never clones or
pulls, so tasks from a Git include that hasn't been fetched yet won't appear
until the first real run caches it.

> **Built on an unstable API.** Task-name completion uses clap's
> `unstable-dynamic` engine, so it may occasionally need a fix as that API
> settles. The static parts (commands, flags, values, paths) don't depend on it.

## Exit codes and diagnostics

Identical to `ratect-compat`: a task's own container exit code becomes `ratect`'s exit
code, an interrupted run (Ctrl+C) exits `130`, anything else that fails exits `1`, and
the reason always reaches stderr — in every output style, including `quiet`. Ctrl+C
abandons the run and then cleans up after it; a Ctrl+C *during* that cleanup stops the
cleanup too, and `ratect resources list` finds whatever that leaves. `RUST_LOG` controls Ratect's own internal
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
| Checking a project without running it | not available | `ratect doctor` |
| Managing the Git include cache | not available (only the automatic sweep) | `ratect includes list`/`clean`/`refresh` |
| Batect-inert flags (`--upgrade`, `--no-update-notification`, `--no-wrapper-cache-cleanup`) | accepted, no effect | not offered |
| `--log-file` | supported | not offered |
| Configuration | `batect.yml` | native `ratect.toml` (with `extends`); `batect.yml` still readable via `-f` |
