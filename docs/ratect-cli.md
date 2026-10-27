# `ratect` CLI Reference

This documents the **`ratect`** binary — the forward-looking CLI, with its own
subcommand-based interface. For the compatibility binary, see the
[`ratect-compat` CLI reference](ratect-compat-cli.md) instead; the two are described
separately because they are deliberately different interfaces, not two spellings of
one ([Two binaries, two formats](index.md#two-binaries-two-formats) says which to
pick). If you haven't run a task yet, [Getting Started](getting-started.md) runs a
first one end to end, and the [worked examples](worked-examples.md) show a real
project per language.

> **Configuration.** `ratect` reads its own **native TOML configuration**
> (`ratect.toml` by default) — see
> [decisions/0003](../decisions/0003-ratect-native-config-format.md) for why. Its
> full schema is the [`ratect.toml` reference](ratect-config-reference.md); it's the
> same schema [Configuration Reference](ratect-compat-config-reference.md) documents for
> `batect.yml`, re-spelled in TOML, with `extends` in place of YAML anchors. A
> `batect.yml` is still readable by naming it with `-f`, so a project can migrate
> incrementally — [`ratect config convert`](#config) translates one automatically.

## The native config format

The format itself — the TOML spelling of every field, `extends`, includes, and
the auto-loaded `ratect.local.toml` — is the [`ratect.toml`
reference](ratect-config-reference.md) ([the
file](ratect-config-reference.md#the-file), [local
overrides](ratect-config-reference.md#local-overrides)).

## Commands

Grouped by purpose. Within a command, the sub-verbs follow their natural
workflow order (`list` before `clean`/`refresh`) rather than alphabetically.
`ratect --help` lists the commands in this same order — `clap` can't render the
group headings there, so the order alone carries them.

### Running tasks

| Command | What it does |
| --- | --- |
| `ratect run <task> [-- ARGS...]` | Runs a task. Anything after `--` is appended to the task command's own arguments, or replaces the image's default `CMD` when the task has no command. |
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
| `ratect config convert` | Converts a `batect.yml` project (point `-f` at it) into native `.toml` files, one per file. |
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
| `-f`, `--config-file <PATH>` | `ratect.toml` | The configuration file. Parsed by extension — `.toml` as the native format, `.yml`/`.yaml` as Batect-format YAML — so `-f batect.yml` keeps reading a Batect config while migrating — until it includes a `.toml` file, which needs a [`ratect_version`](ratect-config-reference.md#ratect_version-the-version-a-project-was-written-for) only a `ratect.toml` root can declare. `caches` uses it only to locate the project *directory* — it never reads the contents. |
| `-o`, `--output <STYLE>` | auto | `fancy`, `simple`, `quiet` or `all`; unset auto-selects `fancy` when stderr is an interactive console, `simple` otherwise. `ratect`'s own progress is on stderr in every style; stdout is the task's own output — see [Output Styles](output-styles.md#streams). |
| `--no-color` | — | No color in Ratect's own output (never affects a task's own output), and `simple` becomes the auto-selected style — see [Colour](output-styles.md#colour) for how it combines with `NO_COLOR`/`CLICOLOR_FORCE`. |

Narrower options attach to the commands that actually use them, rather than being
global: a flag that's accepted and then ignored reads as a promise. So the
config-variable options below belong to `run`, `config validate` and `doctor` (the
commands that evaluate configuration), and the Docker connection options to `run`, `caches`, `resources` and `doctor` (the
ones that reach a daemon).

| Option | Applies to | Description |
| --- | --- | --- |
| `--config-var <NAME=VALUE>` | `run`, `config validate`, `doctor` | Sets a [config variable](ratect-compat-config-reference.md#configvariable). Repeatable; wins over `--config-vars-file` and the variable's own default. |
| `--config-vars-file <PATH>` | `run`, `config validate`, `doctor` | A file of config variable values (a flat `NAME = VALUE` map), parsed as TOML or YAML by extension. In a YAML file an unquoted number or boolean is read as written, as in `ratect-compat`. Defaults to an auto-discovered [`ratect.local.toml`](ratect-config-reference.md#local-overrides) beside the config file, when present. |

A path given to `-f` or `--config-vars-file` has a leading `~` expanded to your home
directory, and `.` and `..` removed before the path is used, so `link/../ratect.toml` <!-- example -->
is `./ratect.toml` whether or not `link` is a symlink. A leading `..` stays. A
`--config-vars-file` you give must name a file: `run` refuses one that doesn't before
reading the config, and `config validate` and `doctor` report it as a problem.

`tasks list` still accepts both, deprecated: they have no effect there, since listing
tasks evaluates no value, and passing either prints a warning saying so. They'll be
removed in a later release.

## Docker connection options

Taken by `run`, `caches`, `resources` and `doctor` — every command that reaches a
daemon — and never by `tasks list`, `includes` or `config`, which don't. Which
daemon they reach, their `DOCKER_*` environment-variable defaults, and TLS with a
private certificate authority are on [Connecting to
Docker](connecting-to-docker.md), shared with `ratect-compat`.

| Option | Default | Description |
| --- | --- | --- |
| `--docker-host <HOST>` | `DOCKER_HOST` | Docker host to connect to — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-context <NAME>` | `DOCKER_CONTEXT`, then the active context, unless a host is set | Docker CLI context to connect through — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-config <PATH>` | `DOCKER_CONFIG`, then `~/.docker` | Directory containing the Docker CLI's own configuration — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-tls` | — | Use TLS, identically to `--docker-tls-verify` — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-tls-verify` | `DOCKER_TLS_VERIFY` | Use TLS, verifying the daemon's certificate — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-cert-path <PATH>` | `DOCKER_CERT_PATH`, then `--docker-config`/`DOCKER_CONFIG`, then `~/.docker` | Directory containing any of `ca.pem`/`cert.pem`/`key.pem` — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-tls-ca-cert <PATH>` | `ca.pem` in the certificate directory | The TLS CA certificate — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-tls-cert <PATH>` | `cert.pem` in the certificate directory | The TLS client certificate — see [Connecting to Docker](connecting-to-docker.md#options). |
| `--docker-tls-key <PATH>` | `key.pem` in the certificate directory | The TLS client key — see [Connecting to Docker](connecting-to-docker.md#options). |

## `run` options

| Option | Default | Description |
| --- | --- | --- |
| `--enable-buildkit` | — | Force BuildKit for image builds, over the daemon's default and `DOCKER_BUILDKIT`. Only `run` builds images, so only `run` takes it. |
| `--use-network <NAME>` | — | Reuse an existing Docker network for every task in this invocation instead of creating one per task — see [`ratect-compat`'s description](ratect-compat-cli.md#task-execution). |
| `--disable-ports` | — | Never bind container ports on the host — see [`ratect-compat`'s description](ratect-compat-cli.md#task-execution). |
| `--no-proxy-vars` | — | Don't propagate proxy environment variables to image builds or containers — see [Proxies](proxies.md). |
| `--skip-prerequisites` | — | Run the task alone, without its `prerequisites` — see [`ratect-compat`'s description](ratect-compat-cli.md#task-execution). |
| `--override-image <CONTAINER=IMAGE>` | — | Replace a container's image. Repeatable — see [`ratect-compat`'s description](ratect-compat-cli.md#task-execution) for what it replaces and when it errors, except that here an empty `CONTAINER` or `IMAGE`, or a `CONTAINER` given twice (the last one wins), isn't rejected. |
| `--tag-image <CONTAINER=TAG>` | — | Extra tag for an image a container builds. Repeatable — see [`ratect-compat`'s description](ratect-compat-cli.md#task-execution) for when it errors, except that here an empty `CONTAINER` isn't rejected. |
| `--no-cleanup`, `--no-cleanup-after-success`, `--no-cleanup-after-failure` | — | Leave the task's containers and network in place, running, after success (the task's own container ran, whatever its exit code), after failure (something failed before it could), or both. After success that is the named task's alone — its prerequisites are still cleaned up; after failure it is whichever task failed — see [`ratect-compat`'s description](ratect-compat-cli.md#cleanup-after-a-run). |
| `--max-parallelism <N>` | unbounded | Cap concurrent image pulls/builds, container creates, dependency container starts and setup commands — see [`ratect-compat`'s description](ratect-compat-cli.md#task-execution). |
| `--cache-type <TYPE>` | `volume` | `volume` or `directory` — see [`ratect-compat`'s description](ratect-compat-cli.md#task-execution) and [cache volumes](ratect-compat-config-reference.md#cache-volumes). |

## `caches` options

`--cache-type <volume|directory>` (default `volume`) selects which storage to act on,
for both `list` and `clean` — a cache in one is invisible to the other, so this has to
match how the project runs its tasks. Under `directory`, this project's caches are
host directories under `<project>/.batect/caches/<name>` and shared ones live at
`~/.ratect/caches/<name>`.

`caches` never reads the configuration file. A cache belongs to the project
*directory*, so both commands work on a project whose configuration is broken or
missing entirely — which is exactly when clearing a cache tends to be what's needed.

`caches list` prints each cache under the name a `volumes` entry gives it, not the
Docker volume it's stored in; that name is what `caches clean` takes back. Under
`-o quiet` it's one name per line and nothing else, for scripting — and it prints
**this project's caches only**, unless `--scope shared` asks otherwise. What it
emits is exactly what a `caches clean` carrying the same flags would act on, so
the output can be piped straight back. Naming a cache that doesn't
exist warns on stderr rather than passing silently, since the likeliest cause is
a typo. Under `--cache-type directory`, `caches clean` also warns when the project
has no `.batect/caches` directory, and names each entry there that it skips
because it is a symbolic link, which is never followed.

`--scope <project|shared>` restricts both commands to one kind of cache. A
[shared cache](ratect-config-reference.md#shared-caches) is one every project on
the machine can use, so the listing keeps them apart — a shared cache is
**not** this project's, and most of the ones shown will belong to other
projects. `ratect caches list`, captured on a terminal in `examples/full-stack`
after a run, while
[`ratect/tests/fixtures/shared-cache.toml`](https://github.com/or1can/ratect/blob/main/ratect/tests/fixtures/shared-cache.toml)'s
`warm-cache` task had created its shared cache:

```ansi
{{#include captures/caches-list.ansi}}
```

**Removing a shared cache always takes `--scope shared`** — whether it is named
or not, and whether or not this project has a cache of the same name. A shared
cache holds storage every other project on the machine is using, so it is never
reached by an unqualified `clean`. `ratect caches clean shared-tools`, in the
same state:

```ansi
{{#include captures/caches-clean-shared.ansi}}
```

`caches clean` with no names therefore sweeps this project's caches only.

Both scopes are read from *storage*, not from the configuration — a project cache
is found by its `batect-cache-<key>-` prefix, a shared one by
`ratect-shared-cache-`. That is what keeps these commands working on a project
whose configuration is broken.

## `includes` options

The Git include cache under `~/.ratect/incl` — where a `type: git`
[include](ratect-compat-config-reference.md#git-includes) is cloned and kept.
`ratect includes list`, captured after
[the one checked-in project with a Git include](https://github.com/or1can/ratect/blob/main/ratect-compat/tests/conformance/batect-journey/git-include/batect.yml)
had been loaded once into an otherwise empty cache:

```ansi
{{#include captures/includes-list.ansi}}
```

Unlike [`caches`](#caches-options) and [`resources`](#resources-options), this cache is
**global** — one directory shared by every project on this machine, keyed by
`(repo, ref)`. So there's no project scoping, and `clean` reaches other projects'
includes as well as your own. That matters less than it sounds: everything here is
re-cloneable, so the worst case is a fetch.

| Command | Description |
| --- | --- |
| `includes clean` | Removes includes nothing has used for 30 days — the same threshold the [automatic sweep](ratect-compat-config-reference.md#git-includes) applies, done on demand. |
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
however long ago they were made. `ratect resources list`, captured in
`examples/full-stack` after `ratect run journey-test --no-cleanup-after-success`:

```ansi
{{#include captures/resources-list.ansi}}
```

Grouped by run, because that's the unit a leftover belongs to: a run that was killed
outright, or crashed, leaves a network and every container it started, and they only
make sense together. (Ctrl+C, `SIGTERM` and `SIGHUP` aren't those cases any more — each
cleans up after itself. `SIGKILL` can't be trapped by anything, so it still is; see
[Differences from Batect](differences-from-batect.md#runtime-behavior-gaps).)
A container is named as your configuration names it, not by the random words
Docker assigns.

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
still holding an endpoint can't be removed; a resource that fails to remove doesn't
stop the rest, but `clean` then exits `1` with one `Error:` naming each refusal.

`resources` reads one thing from the configuration — the root file's
`project_name`, to scope itself to this project — and nothing else in it, so neither
the [`ratect_version`](ratect-config-reference.md#ratect_version-the-version-a-project-was-written-for)
check nor a value this machine can't evaluate gets in the way. What to remove comes
from the labels alone.

Nothing without Ratect's own labels is ever listed or removed, `--all-projects`
included: containers started by other tools, and Docker's built-in `bridge`/`host`/
`none` networks, are invisible to both commands.

**What this doesn't cover**: cache volumes/directories are [`caches`](#caches-options)'
territory, not `resources`' — they're a deliberate cache, not a leftover. Likewise the
Git include cache under `~/.ratect/incl` is [`includes`](#includes-options)' own
command. Built images are tagged `<project>-<container>` and reused/overwritten on
every run rather than tracked as a resource — also a deliberate cache. Tmpfs mounts
and exec instances die with their container, so there's nothing left to find.

## `config`

`ratect config validate` is `doctor`'s configuration half on its own — it loads the
config, evaluates every container and task against this machine, and runs the same
config-only checks (values this machine can't evaluate, missing
`build_directory`/Dockerfile, floating image tags, dependencies with no
`health_check`, an entry given more than once in one `dependencies` or
`prerequisites` list), exiting non-zero on a problem. It never touches Docker, so it's the
gate to run in CI when all you want to know is "is the config valid?", without a
daemon. It takes the same `--config-var`/`--config-vars-file` options as `run`, and
reads `ratect.local.toml` the same way, since evaluating the config can need them.
A config that doesn't load stops at its first error.

`ratect config convert` migrates a Batect-format `batect.yml` project to the native
format — point `-f` at the `batect.yml`:

```bash
ratect -f batect.yml config convert          # writes ratect.toml beside it, and a .toml beside each include
ratect -f batect.yml config convert --stdout  # prints instead, to review or pipe
```

It's **one-directional** (`ratect-compat` stays YAML; the reverse would be lossy) and
converts **each file the project owns beside itself**: the root becomes `ratect.toml`,
and every local `include`d file, at any depth, becomes a `.toml` with the same name in
its own directory (`more/c.yml` <!-- example --> → `more/c.toml` <!-- example -->), with each `include` entry rewritten to
point at the converted file. Nothing is re-based — every value is copied exactly as
written — so a relative path in `more/c.toml` <!-- example --> resolves against `more/`, exactly as it
did in `more/c.yml` <!-- example --> (see [Where relative paths resolve](includes.md#where-relative-paths-resolve)).
A Git include stays a Git include, its `repo`, `ref`, `path` and `allow_host_paths`
carried across: nothing from a bundle is copied out, and no file in one is converted.
A root `batect.yml` with no `project_name` converts to a `ratect.toml` naming the
project as `ratect-compat` does — after the root file's directory — since `ratect.toml`
requires one.
Nothing is written if any converted file already exists — the error lists them all;
pass `--force` to overwrite them, or `--stdout` to print instead. With more than one
file, `--stdout` prints each document in order, the root first, each under a
`# === … ===` banner naming its file (`more/c.toml` <!-- example -->).

The conversion **preserves each file's configuration, not formatting**: YAML
anchors/aliases/merge keys are expanded inline and comments are dropped — so each
output carries a header and is a *starting point to review*, not a blind drop-in.
Before anything is written, every conversion is checked to round-trip losslessly back
to the same configuration, so each file encodes the configuration you started with —
and every way `ratect` *runs* that configuration differently is listed, as below.

A task's own container has no readiness gate, so its health check — from
`health_check` or from its image's own `HEALTHCHECK` — isn't waited on, and its
`setup_commands` don't run (see [Dependency
Readiness](dependency-readiness.md#the-tasks-own-container)). An image's `HEALTHCHECK`
can't be seen without the image, so check those yourself. `run_as_current_user.home_directory`
is used as written in a `batect.yml` but
[evaluated](ratect-config-reference.md#where-the-semantics-differ) in a `ratect.toml`,
so one containing a `$` or `<` expression changes value. Likewise, a string-form
`volumes` entry's host path is used as written in a `batect.yml` but evaluated in a
`ratect.toml`. A `\` in either stays a backslash (`foo\bar` becomes
`foo\\bar`), since a `batect.yml` never reads one as an escape, so a `$` or `<`
after one is still converted as an expression, and listed. One that would then
be no valid expression — `img$`, a `$` with no name after it — can't have meant
one, so convert [escapes](ratect-compat-config-reference.md#escaping-with-) it
whole (`img\$`) and it keeps its value, unlisted. An `image` is always
escaped whole: a `batect.yml` refuses an expression there, so nothing in one
was meant as one. Two more concern a Git include. A pathless entry whose bundle also offers
a `ratect-bundle.toml` would have `ratect` load that file instead of the
`batect-bundle.yml` the `batect.yml` loaded, so convert sets its `path` to
`batect-bundle.yml` and says so; remove `path` to switch to the native file. And a
bundle declaring Git includes of its own is refused by `ratect` unless
`allow_nested_git_includes` is set on that entry (see [Nested Git
includes](includes.md#nested-git-includes)) — a grant convert never writes for you.

`config convert` lists each place in the converted project where one of these
applies — per converted file, its `include` entries' items first, then its
containers' in order of container name — after converting, and again in that
file's own header, so the list travels with the file. `ratect -f
batect.yml config convert --stdout`, captured in
[`ratect/tests/fixtures/convert`](https://github.com/or1can/ratect/tree/main/ratect/tests/fixtures/convert)
— a project with something to check:

```ansi
{{#include captures/config-convert-checks.ansi}}
```

With `--stdout`, the documents are all that reaches stdout; each heading and its list
after it are on stderr. Without it, they follow the success message on stdout,
naming the file written. With nothing to check, neither the list nor the header
block appears. Either way the conversion succeeds and exits 0, and the list prints
whatever `RUST_LOG` is set to, under `-o quiet` too.

The result declares the converting binary's own version as its
[`ratect_version`](ratect-config-reference.md#ratect_version-the-version-a-project-was-written-for). (It emits
the compact `"8080:80"` string form for `ports` rather than the object form, and
each `volumes` entry in the form it was written in; both forms are valid, and
reformatting is a review step.)

## `doctor`

Answers "why did that fail?", or "will it?", without running a task.
`ratect doctor`, captured in
[`ratect/tests/fixtures/doctor`](https://github.com/or1can/ratect/tree/main/ratect/tests/fixtures/doctor)
— a project written to trip each warning and a problem — after
`ratect run build --no-cleanup-after-success` there:

```ansi
{{#include captures/doctor.ansi}}
```

A **problem** will fail a run — an unreachable daemon, a configuration that doesn't
load, a missing `build_directory` or Dockerfile. A **warning** works but is likely to
bite: a floating image tag (`latest`, or no tag at all) means the same configuration
runs a different image next week, and a dependency with no `health_check` counts as
ready the moment it starts unless its image defines one, which is where "connection
refused" on the first run comes from.

So is each value this machine can't evaluate — an unset `$VAR`, a `<var` with no
value — naming its container or task, its field and the expression, as `deploy`'s
`$DEPLOY_TOKEN` above. Every one is reported, not only the first. It's a warning
rather than a problem because a variable only CI sets is expected to be missing on
a laptop, and it fails only a run that [reaches it](ratect-config-reference.md#config-variables-and-expressions):
`ratect run build` there never evaluates `deploy`. The same goes for a value that
evaluates but can't be used — an expression-valued
`run_as_current_user.home_directory` that isn't an absolute path, or a
Git-included container's host path that evaluates to somewhere outside its
boundary. A container with such a value is left out of the checks that read its
values, so its image tag and `build_directory` aren't judged until it evaluates.
When everything evaluates, `doctor` says so on its own `ok` line.

A `dependencies` or `prerequisites` list that gives the same entry twice is a warning,
as `app`'s `cache` above. Batect refuses such a file; Ratect loads it and starts
or runs the entry once, so the repeat is reported rather than refused. Entries are
compared as written: a wildcard prerequisite given twice is a repeat, but a wildcard
beside a task it matches is not.

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

Identical to `ratect-compat`'s: both binaries share one mapping, whose design is
[decisions/0016](../decisions/0016-exit-codes.md). An invocation is on one of two paths,
and its exit code depends on which. `ratect run` is a **run**, even when its arguments
don't parse; every other verb isn't. For a line that doesn't parse, the subcommand
counts where it would be read as one — not after `--`, and not as another option's
value — so `ratect --bogus run test` is a run. `--help` and `--version` exit `0` on
either path, once the arguments reach them.

A run exits with one of three things, answering one question: did the task's own
container run to an exit code of its own?

1. **It did: that exit code, `0` included.** A task's own `125`, `126` or `127` passes
   through the same way, and a prerequisite that fails this way stops the chain. One
   exception: if every container exited `0` but Ratect then **fails to clean up** — a
   removal the daemon refused, or a cleanup stopped with Ctrl+C with something still to
   remove — the run exits `125` instead, and a prerequisite whose cleanup fails stops
   the chain there. A task's own non-zero code is never overridden.
2. **A termination signal ended it: 128 + that signal's own number** — `130` for Ctrl+C
   (`SIGINT`), `143` for `SIGTERM`, `129` for `SIGHUP`. Any of the three abandons the run
   and then cleans up after it; a second one *during* that cleanup force-removes
   whatever is left, a Ctrl+C after that stops the cleanup, and `ratect resources list`
   finds whatever that leaves. A signal that arrives only during the cleanup of a run
   that had already finished doesn't change its exit code; one during a prerequisite's
   cleanup ends the run before the next task starts. A run whose **stdout** reader goes
   away — `ratect run test | head -n 1`, once `head` has its line — ends the same way,
   as though `SIGPIPE` had ended it: `141`, after the same cleanup. A closed stderr never
   ends a run.
3. **It never ran, or never finished, for any other reason: `125`** — an unknown or
   misused flag (`ratect run --bogus`, `ratect -o bogus run test`), a config file that
   isn't there, a config that doesn't load, an unknown task, an unreachable daemon, a
   failed pull or build, a dependency that never became ready.

Every other verb runs nothing, so it exits `0` when it succeeds; `1` when it fails
(`config validate` on a file that doesn't parse, `caches list` with the daemon down,
`doctor` finding a problem); and `2` when it was invoked wrong — a flag that's rejected
(`ratect --bogus`, `ratect tasks list --bogus`, a missing subcommand), or an option value
that fails its own check, such as a `-f` naming no file (`ratect tasks list` where there
is no `ratect.toml`) or a `--docker-config` naming no directory. `doctor` and `config
validate` report a `-f` naming no file as a problem they found instead, and exit `1`, as
`doctor` does a Docker directory that isn't one; a flag rejected in their own arguments is still
`2`.

Every failure of Ratect's own reaches stderr as `Error: <message>`, in every output style
including `quiet`, whatever `RUST_LOG` says. Two kinds of failed run print no `Error:`
line, because something else already reported them: a task's own non-zero exit (rule 1 —
the code itself and, outside `quiet`, the `finished with exit code N` summary line), and
a run ended by a signal or a closed stdout (rule 2 — the notice saying so).
[`ratect-compat`'s
section](ratect-compat-cli.md#exit-codes-and-error-reporting) has each rule in full.

- `RUST_LOG` controls Ratect's own internal logging (default `info`, on stderr — see
  [Filtering `RUST_LOG`](troubleshooting.md#filtering-rust_log)). Unlike `ratect-compat`
  there's no `--log-file`; redirect stderr if you want one.
- **A crash (a genuine bug) exits `101`** and prints where to report it, `ratect`'s
  version and platform, and a reminder to re-run with `RUST_BACKTRACE=1` if it isn't
  already set — see
  [`ratect-compat`'s own note on this](ratect-compat-cli.md#exit-codes-and-error-reporting),
  which applies identically here.

## Differences from `ratect-compat`

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
