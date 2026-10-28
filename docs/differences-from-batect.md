# Differences from Batect

Ratect is a from-scratch Rust implementation inspired by
[Batect](https://github.com/batect/batect) (which is itself no longer maintained — the
upstream repository was archived in October 2023), not a wrapper or fork. It does not
read Batect's documentation or source at runtime. To move an existing project
over, follow [Migrating a Batect Project to Ratect](migrating-batect-project.md);
this page is what changes once you have.

**Every Batect configuration field and CLI flag is supported, field-for-field and
flag-for-flag, unless listed below** — see [config reference](ratect-compat-config-reference.md)/
[CLI reference](ratect-compat-cli.md) for the full accepted schema and flags. This page
lists the exceptions: a real behavioral divergence, an extension beyond what Batect
does, or a restriction narrower than it. It doesn't restate how a field or flag
works — [config reference](ratect-compat-config-reference.md)/[CLI reference](ratect-compat-cli.md)/
[task lifecycle](task-lifecycle.md) are the authoritative source for that; this page
only says what's *different* and points there for the rest.

> **Unrecognized fields fail closed**: Ratect's YAML parsing rejects unknown keys —
> a typo'd field name, or (unlikely, since Batect's own `include` `type`s beyond
> `file`/`git` are the only thing left genuinely unsupported here) a real gap —
> fails config loading with an error naming it, rather than silently ignoring it.
> There's no partial/best-effort mode.

## Configuration format

An unquoted number or boolean in a field that takes text is read as written,
as in Batect — `3.10` stays `3.10` — except in a file containing a YAML tag
(`!!int 7`, `!!str x`) anywhere in it. There, every such value in the file is
re-formatted: `3.10` becomes `3.1`, `1e3` becomes `1000.0`, `True` becomes
`true`. Batect keeps each as written. Quote a value whose spelling matters in
a file that uses tags. The same holds for a config variables file.

### Top-level fields

Every other top-level field is supported field-for-field — see [config
reference](ratect-compat-config-reference.md) for the full list. The exceptions:

| Field | Notes |
|---|---|
| `config_variables` | `description:` is recognized but inert — Ratect has no help/usage output to show one in. |
| `include` | Ratect enforces that a Git include's `path` (and anything it transitively includes) stays within that repository's own clone; Batect has no equivalent containment check. In `ratect.toml` specifically, a Git-included bundle also can't declare further Git includes of its own unless `allow_nested_git_includes` is set — `ratect-compat` stays unrestricted, matching Batect. See [What a bundle may do](includes.md#what-a-bundle-may-do). A Git include's clone is cached under `~/.ratect/incl`, keyed by a SHA-256 of the remote and ref with each length-prefixed, with a TOML sidecar — not Batect's `~/.batect/incl`, keyed by a hash of `git <remote> @<ref>`, which two different remote/ref pairs can share. Neither tool reuses or sweeps the other's clones. The sweep that removes a clone unused for 30 days counts them as 30 × 24 hours, where Batect counts 30 calendar days in your time zone, so across a daylight-saving change the two can disagree by up to an hour about whether a clone is due. It skips, with a warning, a sidecar it can't read or parse, and goes on to the rest; Batect's stops at the first such file, so while it's there no clone is ever removed. See [Git includes](ratect-compat-config-reference.md#git-includes). An include written as an object with no `type` loads as a `file` include; Batect requires `type`, and fails to load the file without it. |
| `forbid_telemetry` | Recognized, no effect — Ratect doesn't collect telemetry, so there's nothing to forbid. |

### Expressions

The syntax and which fields support it match Batect, field-for-field — see
[Expressions](ratect-compat-config-reference.md#expressions). The exceptions:

- **When a task's values are evaluated.** Both evaluate only what a task uses,
  once the run reaches it, after its prerequisites. Ratect evaluates every
  container in the task's graph before starting any of them, where Batect
  evaluates each as it creates or builds it — so here a value that can't be
  evaluated fails the task before its dependencies start, rather than after.
- `image` — see [Container fields](#container-fields) below.
- `batect.project_directory` has its `.` and `..` components resolved:
  `-f ../proj/batect.yml` gives `/parent/proj`, where Batect gives
  `/parent/cwd/../proj`. Both name the same directory, and a volume path built
  from either is normalised the same way, so the difference only shows where
  the value is used as a string — in `environment` or `build_args`.

### Container fields

Every other container field is supported field-for-field — see [config
reference](ratect-compat-config-reference.md#container) for the full list. The exceptions:

| Field | Notes |
|---|---|
| `image` | An [expression](#expressions)-looking value (`$VAR`) is rejected when the file loads rather than resolved or used as a literal — Batect resolves nothing here either, but silently treats it as a literal image name that then fails at pull time instead. `ratect.toml` does resolve them; see [config reference](ratect-compat-config-reference.md#container). |
| `volumes` | A `cache` mount's `name` must use Docker's own volume-name character set; Batect doesn't validate it at all, so an unvalidated name could bind-mount an arbitrary host directory under `--cache-type=directory`. Breaking change for `--cache-type=directory` only — `--cache-type=volume` already enforced this via Docker itself. See [Cache volumes](ratect-compat-config-reference.md#cache-volumes). A newly generated `.batect/caches/key` holds a UUID rather than Batect's six-character id, under a comment that doesn't name Batect; each tool reads the other's key file, so the volume names stay the same. A line of that file that starts with `#` after leading spaces is a comment, where Batect counts it as a key line: Batect refuses a file holding one beside the key, and takes one on its own as the key, where Ratect refuses the file as having no key line. A key path that is a symbolic link to nothing is refused, where Batect creates the key file wherever the link points. |
| `ports` | A port written with a leading `+` (`+80`, `'+80-+90'`, `'+80:+80'`), or unquoted as a YAML hexadecimal or octal integer (`0x50`, `0o120`), is accepted as the port it denotes; Batect rejects each as a malformed port range. The same holds for a task's `run.ports`. |
| `dependencies` | A name that isn't a declared container is rejected when the file loads — and so is one in a task's `dependencies` or `run.container`. Batect reports the same error in the same words, but only once a task that reaches the reference runs, so a stale reference in a container or task nothing runs never stops Batect and stops every `ratect-compat` command. A name given twice — here, in a task's `dependencies`, or in a task's `prerequisites` — is accepted, and that container starts, or that task runs, once; Batect rejects the duplicate when the file loads (`The dependency 'X' is given more than once`, and likewise for a prerequisite). `ratect doctor` and `ratect config validate` warn about it. See [config reference](ratect-compat-config-reference.md#container). |
| `capabilities_to_add` / `capabilities_to_drop` | Also accepts `BPF`/`CHECKPOINT_RESTORE`/`PERFMON` — Docker capabilities added after Batect's last release, so its own `Capability` enum predates them. A superset: every config Batect itself accepts here still parses identically. |
| `health_check` / `setup_commands` | A task's own container's readiness gate doesn't fail the task once its main command has exited 0, whatever the failure — typically the container stopped before reporting a health status, before a setup command could `exec` into it, or while one was running and was killed with it; Batect's does. A failure of that gate while the main command is still running fails the task as Batect's does, but cancels the main command up to two seconds later, and is forgiven if the main command exits 0 within that time. That, and the race it leaves open, are in [task lifecycle](task-lifecycle.md#known-limitations). Under `ratect` a task's own container has no readiness gate at all — see [Dependency Readiness](dependency-readiness.md#the-tasks-own-container). |
| `dockerfile` | A Dockerfile that is a symbolic link, or is reached through a linked directory, is refused before the build, since Ratect's build context doesn't carry symbolic links and the daemon would report the Dockerfile missing. Batect builds one whose link stays inside the build directory. |
| `log_driver` / `log_options` | An absent value leaves the daemon's own default alone; Batect's config model bakes in a literal `"json-file"` default explicitly. Immaterial in practice — that's Docker's own out-of-the-box default too. |
| `shm_size` | A size whose count times its unit doesn't fit in a 64-bit number of bytes is an `Invalid size` error when the file loads, like any other malformed size. Batect's multiplication overflows silently, and it sends Docker the meaningless size that results. Whitespace around a size string is ignored (`' 128m '`), as is whitespace that isn't ASCII, and an unquoted YAML integer is read as the number it denotes, whatever its spelling (`+5`, `0x10`, `0o17`); Batect refuses each. Both accept whitespace between the count and its unit (`'128 m'`) or after a bare count (`'2 '`). |
| `run_as_current_user` | Host-side uid/gid lookup only works on Unix — see [User mapping](ratect-compat-config-reference.md#user-mapping). A `home_directory` containing a `:` or a control character, or with no directory name to create (`/`, or a path ending in `..`), is rejected when the file loads. Batect checks neither: it writes the first into the generated `/etc/passwd` as it is, where a `:` shifts the entry's fields and a newline adds an entry of its own, and passes the second to Docker. A read-only `cache` mount (`options: ro`) isn't given to the mapped user, since nothing can write to it anyway; Batect tries to change its owner like any other cache's, which the read-only mount refuses, failing the run before the container starts. |

### Task fields

Every task field is supported field-for-field — see [config
reference](ratect-compat-config-reference.md#task) for the full list. The
exceptions are about when a mistake is caught: `dependencies` naming an
undeclared container, and a name given twice in `dependencies` or
`prerequisites`, are under `dependencies` in [Container
fields](#container-fields) above, and a `customise` naming a container the task
doesn't start is under [Load errors](#load-errors).

### `run` fields

Every `run` field is supported field-for-field — see
[TaskRun](ratect-compat-config-reference.md#taskrun) for the full list. The
exceptions are about when a mistake is caught: `container` naming an undeclared
container is under `dependencies` in [Container fields](#container-fields)
above, and a `container` also listed in the task's `dependencies` is under [Load
errors](#load-errors).

### Null values

A field given a YAML null (`volumes:` with nothing after it, `~` or `null`)
mostly loads as if it were left out — a container's `volumes`, `ports`,
`environment`, `health_check` or `run_as_current_user`, a task's
`prerequisites`, `dependencies` or `customise`, and the top-level
`forbid_telemetry` or `config_variables`, for example. Batect rejects a null on
each of those. Where Batect accepts one — `run`'s `command`, `entrypoint` and
`working_directory`, a config variable's `default` and `description`,
`health_check`'s `retries` — the two tools agree. Some nulls are rejected here
too, such as an `environment` variable's value and `shm_size`, in Ratect's own
words.

### Load errors

Ratect checks a config when the file loads, and the report is its own:

- **Some checks Batect makes only once a task reaches them are made when the
  file loads here**, for any command that loads it — `--list-tasks` included —
  and the first hard error stops it, so a config Batect lists, or runs a
  different task from, can fail to load here. Besides the undeclared names under
  `dependencies` in [Container fields](#container-fields), they include a
  `run_as_current_user` `home_directory` that isn't an absolute path, and a
  relative `cache` mount destination under `run_as_current_user` — Batect
  checks both as it configures the container, though its message for the cache
  path is never seen, since Docker has already refused the relative mount when
  the container was created; and a task's `customise` naming a container its
  task doesn't start, or its `run.container` also listed in its
  `dependencies`, checked for every task rather than only the ones run.
  `--tag-image` on a container that uses a pulled image isn't checked at load,
  but fails as soon as that container's image is resolved — after its
  dependencies have started and its prerequisite tasks have run, but before it
  runs — where Batect fails once the task using it has finished.
- **A negative `health_check` duration, such as `-2s`, is rejected when the
  file loads.** Batect loads one, and the run fails only once Docker refuses it.
  A negative zero, such as `-0s`, is zero, and loads in both.
- **The wording** is Ratect's, not Batect's — `Configuration file "batect.yml"
  not found.` rather than `The file '<absolute path>' does not exist.`, and
  likewise for malformed volumes, ports, devices and includes, expressions,
  durations, duplicate `build_ssh` ids, an undeclared config variable given a
  value — which names the variable but neither which of `--config-var` and
  `--config-vars-file` gave it nor, for the file, its line — a name defined
  in more than one file, a file that contains no configuration, and an
  included file that sets `forbid_telemetry` to `true`. A file inside a Git
  include is named by its path within the repository plus the remote and ref,
  `'1.yml' from the bundle '<repo>' at '<ref>'`, where Batect writes
  `<repo>@<ref>: 1.yml` or `1.yml from <repo>@<ref>`.
- **A check made once the file has parsed names what it's about** — the
  container or task — rather than giving Batect's YAML path, line and column.
- **An error raised while the file is parsed gives its line, column and YAML
  path in Ratect's own layout** — `deserialization error at line 8, column 23:
  containers.build-env.not_a_real_field: unknown field: not_a_real_field`,
  under the `Failed to parse config file "<path>"` line of the cause chain
  below — where Batect prints `<file> (at <path> on line L, column C):
  <message>`. The wording is Ratect's too: `unknown field: X` with no list of
  the known ones, `missing field: X`, and for a value of the wrong type the
  path and the mismatch, with no `Value for 'X' is invalid:` prefix — a
  refused null is `invalid type: null, expected <what the field takes>`, as
  in `containers.app.shm_size: invalid type: null, expected a size like
  '128', ...`, where Batect says `Unexpected null or empty value for
  non-null field.` A null where a mapping of fields is expected is read as
  an empty one, so it reports any field that mapping must have as missing. An
  unknown field's column is its value's, not its key's, and so is that of a
  [task](ratect-compat-config-reference.md#task-names), [config
  variable](ratect-compat-config-reference.md#config-variable-names) or
  [container](ratect-compat-config-reference.md#project-and-container-names)
  name Batect refuses — though the message for one is Batect's, verbatim. A
  key given twice in one mapping is `tasks.t: duplicate key "t" at line 9,
  column 3`, giving only the second occurrence's line, where Batect's
  `Duplicate key 't'. It was previously given at line L, column C.` also gives
  the first's.
- **The full cause chain is printed**: `Error: <message>`, then a `Caused by:`
  list when there is a cause, where Batect prints one message. A parse error's
  own reason is in that list, under `Failed to parse config file "<path>"`. The
  same holds for every fatal error, not only a config one.
- **With several problems, the one reported isn't necessarily Batect's first.**
  Of several containers, tasks or `customise` entries failing the same check,
  the first the files declare is reported, as in Batect, but each check runs
  over every container before the next check starts, and over containers
  before tasks, so of two failing different checks the one reported can
  differ. Every config check runs before an `--override-image` container name is
  looked up, a duplicate
  `build_ssh` id is checked after a container's image source, a volume's
  fields are checked in a different order, and of several undeclared config
  variables given values, the first by name is reported, where Batect reports
  the file's first, in file order, before any `--config-var`. A `command`,
  `entrypoint` or setup command that can't be tokenized is reported after
  every error raised while the files are parsed, after every container's
  image source, `image` expressions, `dns` entries and fields its file's
  format doesn't allow are checked, and after the same container's
  `volumes`, `build_directory` and `build_secrets`, where Batect reports
  whichever the file gives first.

## CLI flags

Every other flag from Batect's own [CLI
reference](https://github.com/batect/batect.dev/blob/main/docs/reference/cli.mdx)
is supported flag-for-flag — see [CLI reference](ratect-compat-cli.md) for the full
list. The exceptions:

| Flag | Notes |
|---|---|
| `--version` | Prints one line, `ratect-compat <version>`, rather than Batect's seven labelled lines (its version, build date and commit, and the JVM, OS, Docker and Git versions) and the help blurb after them. Also gets a `-V` short form Batect doesn't have (a `clap` default). Acts as soon as it's reached: whatever follows it on the command line is never parsed or checked, and no check across options is made, so `--version --bogus` and `--tag-image app=t --override-image app=i --version` exit `0`, where Batect checks every option first and rejects them. Given with `--help`, whichever of the two comes first wins; Batect always shows the help. |
| `--help` / `-h` | The layout and text are `clap`'s rather than Batect's, including `<CONFIG_FILE>`-style value placeholders and `[default: …]` for a fixed default. A description never shows an environment variable's current value or whether `batect.local.yml` exists — Batect's say, for example, whether `DOCKER_HOST` is currently set, and to what. Like `--version`, acts as soon as it's reached, without checking what follows it, so `--version --help` prints the version, where Batect shows the help. Exits `0`, as help conventionally does; Batect's `HelpCommand` returns `1`. See [Options](ratect-compat-cli.md#options) and [decisions/0016](../decisions/0016-exit-codes.md). |
| `--list-tasks` / `-T` | The listing opens with `Tasks in <project>:`, followed, when any task has a `group`, by a blank line before the first group heading. Batect's opens a flat list with `Available tasks:`, and a grouped one with its first group heading. The `-o quiet` listing, the one meant for scripts, has no header in either. |
| `--config-var` | `--config-var NAME=` sets the variable to an empty value, which Batect rejects as having no value. A value containing `=` is split at its first `=`, so `--config-var a=b=c` sets `a` to `b=c`; Batect splits at the last, setting `a=b` to `c`. `--override-image` and `--tag-image` also split at the first `=`. |
| `--tag-image` | A `TAG` Docker would refuse is rejected while the arguments are parsed, as a usage error, on every command, `--list-tasks` included. Batect rejects it only when that image's build step starts, after the task's prerequisites have run. The check is the one `docker tag` makes, with its reasons, so a digest (`app@sha256:…`) is refused too; Batect accepts one, and the daemon then refuses the build. Batect's parser is an older copy of the same grammar: it rejects a name whose first component has uppercase in it (`MyOrg/app`), which the current one takes as a registry host, and an IPv6 registry host, and it counts the 255-character limit over the whole name rather than the repository path; its uppercase message also doesn't name the repository. See [decisions/0018](../decisions/0018-built-images-named-for-people-found-by-labels.md). |
| `TASK_NAME` (positional) | A misspelled task name's `Did you mean` suggestions include every task tied on edit distance; Batect keeps only one name per distance. An unknown task's error reads `Task 'X' not found.`, where Batect's reads `The task 'X' does not exist.`, and its closing hint names `ratect-compat --list-tasks` and `ratect-compat --help` rather than `./batect`'s. No task name at all is a command-line error, as in Batect, reported like any other (see below): exit `125`, where Batect exits `255`. See [Exit codes and error reporting](ratect-compat-cli.md#exit-codes-and-error-reporting). |
| `-- ADDITIONAL_ARGS` | When neither the task's `run` nor its container sets a `command`, the arguments replace the image's default `CMD`, as `docker run <image> <args>` does; Batect refuses them with an error. Every invocation Batect accepts runs the same here. See [Using ADDITIONAL_ARGS in a task command](ratect-compat-cli.md#using-additional_args-in-a-task-command). |
| `--output` / `-o` | An explicit `-o fancy` fails up front with a clear error when stdout isn't a terminal, and so does one on a terminal with `TERM=dumb`; `TERM` unset is accepted, as in Batect. Batect accepts both refused cases: when stdout isn't a terminal it crashes with an unhandled exception on the first repaint, and on a terminal with `TERM=dumb` it writes cursor movements that terminal can't perform, garbling the display. `all`'s status lines also drop Batect's inner `Batect \| ` prefix — the outer prefix already says whose line it is. `fancy` clips a line to the terminal's width in display columns, so a wide character counts as two; Batect counts UTF-16 units, so a line holding wide characters is clipped earlier here. Choosing a style automatically doesn't special-case `TRAVIS=true`, as Batect does by picking `simple`; that only matters on Travis CI with a real terminal, since a CI runner without one gets `simple` anyway. See [Output Styles](output-styles.md). |
| `--no-color` | A superset, not a gap: Batect rejects `-o fancy --no-color` at parse time (its console couples color and cursor movement under one flag); Ratect's keeps them independent, so that combination renders colorless fancy instead. Ratect also honours two environment variables Batect ignores: `NO_COLOR` has `--no-color`'s full effect, including making `simple` the auto-selected style, and `CLICOLOR_FORCE` colours output to a non-terminal. See [Colour](output-styles.md#colour). |
| `--no-cleanup`, `--no-cleanup-after-failure`, `--no-cleanup-after-success` | Batect's own `DontCleanup` still stops a started container, just skips removing it; Ratect leaves it genuinely running (not just present-but-stopped) for investigation. With cleanup after success disabled, a task whose own container runs to completion exits with that container's exit code; Batect exits `255` whatever the code was, `0` included, so a script checking the exit status can't tell a pass from a fail. |
| `--docker-cert-path`, `--docker-tls`, `--docker-tls-verify`, `--docker-tls-ca-cert`, `--docker-tls-cert`, `--docker-tls-key` | Batect's bare `--docker-tls` (without `-verify`) disables *all* server certificate verification, not just hostname matching. Ratect doesn't support that mode at all — `--docker-tls` and `--docker-tls-verify` behave identically here, the daemon's certificate always fully verified. See [TLS with a private certificate authority](connecting-to-docker.md#tls-with-a-private-certificate-authority) for the supported alternative (your own CA). A superset in one respect: Batect requires all of `ca.pem`, `cert.pem` and `key.pem`, and Ratect none of them — without a CA the system trust store is used, and without a `cert.pem`/`key.pem` pair no client certificate is presented. A CA that is given is the only one trusted. See [TLS through the options](connecting-to-docker.md#tls-through-the-options). |
| `--docker-tls-ca-cert`, `--docker-tls-cert`, `--docker-tls-key` | A file that exists but isn't a regular file is accepted — a pipe, such as `<(…)`, to keep a key off disk, is read like a file, once. Batect refuses one as `refers to something other than a file`. A missing file or a directory is refused before any command runs, as in Batect. See [TLS through the options](connecting-to-docker.md#tls-through-the-options). |
| `--docker-context` | A context's `SkipTLSVerify` is not honoured: the daemon's certificate is always fully verified, as with `--docker-tls`. Batect honours it, skipping verification. See [Which daemon is used](connecting-to-docker.md#which-daemon-is-used). |
| `--docker-host` | An `https://` host connects over TLS, as if `--docker-tls-verify` were given — from the flag, `DOCKER_HOST`, or a Docker context. Batect (and the Docker CLI) connect to such a host without TLS unless the TLS options or the context's stored files ask for it. See [Options](connecting-to-docker.md#options). |
| `--docker-host` | A host whose scheme isn't `unix://`, `tcp://`, `http://` or `https://` — from the flag, `DOCKER_HOST`, or a Docker context — is refused on connecting, in Ratect's own words: from the flag or `DOCKER_HOST`, `Failed to connect to Docker host '<host>'`, caused by `URI scheme is not supported: <host>`. That includes `ssh://`, which Batect's Docker client accepts. Where Batect refuses a host, as it does `npipe://` off Windows, its message is `protocol not available`. |
| `--docker-context`, `--docker-config` | A Docker CLI `config.json` that can't be read or isn't valid JSON, when the active context would be read from it, is a warning naming the file, and the default context is used, as the current Docker CLI does. Batect fails, as the Docker CLI it was built against did. See [Which daemon is used](connecting-to-docker.md#which-daemon-is-used). |
| `--docker-host`, `--docker-context` | An empty `DOCKER_HOST` or `DOCKER_CONTEXT` counts as unset, as in the Docker CLI. Batect takes an empty value as set: an empty `DOCKER_HOST` still rules out every context and becomes the host, and an empty `DOCKER_CONTEXT` names a context called `""`. See [Which daemon is used](connecting-to-docker.md#which-daemon-is-used). |
| `--docker-tls-verify`, `--enable-buildkit` | An empty `DOCKER_TLS_VERIFY` or `DOCKER_BUILDKIT` counts as unset, as in the Docker CLI: no TLS, and the daemon's own builder. Batect rejects an empty value as not a boolean. Both are also read in any case, so `True` and `Yes` are accepted; Batect takes only the all-lower and all-upper spellings. An invalid `DOCKER_TLS_VERIFY` is rejected only when connecting to a host without `--docker-tls`/`--docker-tls-verify`, so `--list-tasks` and a connection through a context never read it; Batect rejects it on every command. See [Environment variables](connecting-to-docker.md#environment-variables). |
| `--docker-config`, `--docker-cert-path` | A directory given by the flag, `DOCKER_CONFIG` or `DOCKER_CERT_PATH` that doesn't exist or isn't a directory is refused before any command runs, as in Batect, but in Ratect's own words, naming the flag or variable, the path and what is wrong with it. `ratect doctor` reports one as a problem and carries on with its other checks. See [Options](connecting-to-docker.md#options). |
| `--enable-buildkit` | An invalid `DOCKER_BUILDKIT` (the flag's default) is rejected the first time a run builds an image — every run of a task with a `build_directory` container, since Ratect always builds and leaves Docker's layer cache to skip unchanged work — rather than on every command. A task that only pulls images, `--list-tasks` and `--clean` never read it. Batect, like the Docker CLI, rejects an invalid value on every command. A daemon whose `Builder-Version` ping header is none of empty, `1` and `2` is reported at the same point, and not at all when `DOCKER_BUILDKIT` or the flag chooses the builder; Batect fails every run that connects to it, with an unhandled exception. |
| `--cache-type` | Unlike Batect, not forced to `directory` for Windows containers — Ratect has no Windows support to special-case. A `BATECT_CACHE_TYPE` that is no cache type is refused, as in Batect, in Ratect's own words, naming the variable, its value and the two cache types. |
| `--clean`, `--clean-cache` | Under `--cache-type=directory`, the Docker daemon is never contacted, so cleaning works while it's unreachable; Batect checks it can reach the daemon first, under either cache type, and fails without it. A project with no `.batect/caches` directory has nothing to remove, and Ratect warns that the directory isn't there, where Batect fails with exit code `255`. An entry there that is a symbolic link is skipped rather than followed, with a warning naming it: Batect follows it and deletes the contents of whatever it points to, even outside the project. |
| `--max-parallelism` | Batect's flag caps every setup/cleanup step, via a step-scheduling model Ratect doesn't have, but one: a container's run — its start, then the wait for it to exit — never counts, for any container, dependencies included, since it lasts the container's whole lifetime; every container's create counts. Ratect's caps image pulls/builds, every container's create (the task's own included), a dependency's start, and setup commands. So a dependency's start is capped where Batect's isn't: under a low cap, dependencies start one at a time where Batect starts them together — timing only, the run's outcome is the same. The task's own container's start and run are never gated, as in Batect. Health-check waits and cleanup teardown, which Batect's cap covers, are deliberately excluded (teardown is concurrent, gated only by dependency order). |
| `--log-file` | Batect's own default (no `--log-file`) is a silent `NullLogSink`, nothing anywhere; Ratect always logs to stderr regardless, so `--log-file` here tees into a file *in addition to* stderr, not instead of it. The file is plain text, one record per line, where Batect writes one JSON object per message, and logs Ratect's own records at `debug` and other crates' at `info` unless `RUST_LOG` says otherwise, where Batect logs every message. The records are Ratect's own rather than Batect's, ending, at that default, with why the run failed when it did: the Git include cache sweep, for example, logs only what goes wrong, in its own words, and none of Batect's `No repositories ready for clean up.`, `Deleting repository as it has not been used in over 30 days.` or `Repository deletion completed.` records. |
| `--generate-completion-script`, `--generate-completion-task-info` | Not accepted — an unrecognized-argument error. Batect's are hidden flags for its own shell completion, which reaches them only through Batect's wrapper script. `ratect-compat` has no shell completion; the native binary prints its own script with [`ratect completions <shell>`](ratect-cli.md#shell-completion). |
| `--no-update-notification`, `--upgrade`, `--no-wrapper-cache-cleanup` | Recognized, no effect — permanently inapplicable, since Ratect is a single native binary with no self-updating wrapper script to disable notifications for, clean caches for, or upgrade. Recognized rather than rejected so an existing Batect invocation carrying one doesn't hard-fail outright. Hidden from `--help`, where Batect lists all three — `--upgrade` under its help options, the other two under its execution options. See [CLI reference](ratect-compat-cli.md#recognized-no-effect). |

Beyond any one flag:

- **A mistake on the command line is reported in `clap`'s format**: an
  `error: …` line in its own words, sometimes followed by a tip or a usage
  line, then `For more information, try '--help'.` Batect prints a single message of its
  own instead, such as `Invalid option '--bogus'. Run './batect --help' for a
  list of valid options.` The exit code is `125` on a line meant to run a task,
  `docker run`'s code for its own failure, and `2` on one that replaces the task
  with `--list-tasks`, `--clean`, `--clean-cache` or `--upgrade`, the usage-error
  convention `clap` and the POSIX utilities share. So is an option value that
  fails its own check, such as a `-f` or `--config-vars-file` naming no file, a
  `batect.local.yml` that isn't a file, or a `BATECT_CACHE_TYPE` that is no
  cache type.
  Batect's is `-1` (255) for both, the same as for any other failure
  ([decisions/0016](../decisions/0016-exit-codes.md)).
- **`--help` and `--version` exit `0` even when an option's value or
  default is invalid** — a `--docker-config` naming no directory, a
  `batect.local.yml` that isn't a file, or a bad `BATECT_CACHE_TYPE`, say: they
  act before any value they don't need is checked. Batect checks its
  options and their defaults first, so an invalid one makes `--help` exit
  `-1` (255) instead ([decisions/0016](../decisions/0016-exit-codes.md)).
- **Short options combine, and take an attached value**: `-Tf batect.yml`,
  `-fbatect.yml` and `-oquiet` are accepted. Batect looks an option up by its
  exact text up to any `=`, and rejects each of those as an invalid option.
- **An option's value can't start with `-` when it's given as the next
  argument**: `--use-network -x` and `--clean-cache --list-tasks` are usage
  errors, whose tip is the `=` form (`--use-network=-x`), which is accepted.
  Batect takes the next argument as the value whatever it starts with, so a
  forgotten value silently swallows the flag after it, while no Docker
  network, volume or context name and no Batect task name can start with `-`,
  and a path that does can be written `./-x`.

## Runtime behavior gaps

Batect behavior not implemented in task execution, beyond what's covered by the field
tables above:

- **Cleanup on a termination signal (Ctrl+C, `SIGTERM`, `SIGHUP`)**: four deliberate
  differences from Batect, which traps `SIGINT` only:

  - **Ratect also traps `SIGTERM` and `SIGHUP`**, down the same cleanup path — a task
    runner is stopped by more than a keystroke (an editor closing its subprocess,
    `docker stop`, `systemd`, most CI cancel buttons), and under Batect each
    leaks a container and network.
  - **The exit code names the signal**: 128 + the signal's own number (`130`/`143`/`129`
    for Ctrl+C/`SIGTERM`/`SIGHUP`). Batect returns `-1`/255 for an interrupted run, as
    for any failure other than the task's own command exiting non-zero.
  - **A further signal during cleanup hurries it, then Ctrl+C stops it**: a second
    signal of any kind switches whatever is left to forced removal (kill and remove, no
    grace period), and a `SIGINT` after that abandons cleanup. Batect instead lets
    cleanup finish and then prints manual cleanup commands. Only `SIGINT` abandons
    because a keyboard is the one thing that sends it twice — a supervisor's timed
    follow-up is a `SIGTERM`, which only ever means "exit" — so a supervisor that
    escalates `SIGINT` → `SIGTERM` → `SIGKILL` on a timer usually gets a finished
    cleanup before the `SIGKILL`, though a slow daemon can still lose that race. A run
    that had already finished when the signal arrived keeps its own exit code, since the
    signal ended nothing — unless the cleanup is then abandoned with something still to
    remove, which turns a `0` into `125` ([decisions/0016](../decisions/0016-exit-codes.md)).
    Batect fails such a run with `-1` (255) whatever its code was. Whatever an abandoned
    cleanup leaves still carries Ratect's ownership labels, so [`ratect resources
    list`/`clean`](ratect-cli.md#managing-resources) finds it (`ratect`-only;
    `ratect-compat` has no equivalent verb, so from it the sweep is `docker` itself,
    filtering the same labels).
  - **The notice names the signal and the next step**: a line on stderr such as
    `Interrupted; cleaning up. Press Ctrl+C again to force-remove what's left,
    and Ctrl+C after that to stop cleaning up.`, printed in every style whatever
    `RUST_LOG` says. Once cleanup ends, `Cleanup complete.` follows, and no
    `Error: Interrupted` — the exit code says the run was signalled, and a
    cleanup that left something behind is reported, in place of `Cleanup
    complete.`, as the error it is, naming what was left and the `docker`
    command that removes it. Batect prints `Task cancelled: Interrupt received
    during execution.` and that it is waiting for outstanding operations, then
    its manual-cleanup commands.

  `SIGKILL` remains untrappable by either tool, same underlying OS limitation — the
  same labels are what a post-hoc `ratect resources clean` needs to find what it left.
- **A closed stdout cleans up**: a run whose stdout reader goes away (`| head -n 1`)
  stops its containers, removes them and the task network, and exits `141`, with a
  notice on stderr saying why. Batect's container output is written
  by its Go wrapper straight to its stdout, and Go's runtime ends a process whose write
  to stdout meets a closed pipe by `SIGPIPE` — from reading its source rather than
  running it, the same `141`, with nothing cleaned up. Ratect matches the code and
  declines the leak
  ([decisions/0013](../decisions/0013-when-ratect-compat-copies-batect.md) rule 3).
- **A run that fails without its task's command exiting exits `125`**: a config
  that doesn't load, an unknown task, an unreachable daemon, a failed image pull or
  build, a dependency that never becomes ready, a setup command that fails. `125` is
  `docker run`'s own code for its own failure. Batect returns `-1` (255) for all of
  these, as for every failure other than the task's own command exiting non-zero.
  Every other command — `--list-tasks`, `--clean`, `--clean-cache` — exits `1` on
  failure, or `2` for a usage error or an invalid option value; Batect's is `-1`
  there too. See
  [Exit codes and error reporting](ratect-compat-cli.md#exit-codes-and-error-reporting)
  and [decisions/0016](../decisions/0016-exit-codes.md).
- **A cleanup that fails after a passing run exits `125`; after a failing one, the
  task's own code stands**: the daemon refusing to remove a container or the task
  network fails a run whose containers all exited `0`, and leaves a run that already
  failed with its own code. Batect fails both with `-1`
  (255), so the task's own code is lost whenever its cleanup fails. See
  [decisions/0016](../decisions/0016-exit-codes.md).
- **Ownership labels**: every container and network Ratect creates carries
  `eu.orican.ratect.*` labels; Batect labels nothing of its own. An additive
  divergence — changes no behavior. See
  [decisions/0002](../decisions/0002-runtime-ownership-labels.md).
- **Interactive mode**: two known divergences. Batect's real-TTY gate checks only
  whether its output is a real terminal; Ratect's requires *both* stdin and stdout
  to be real terminals. Live terminal-resize tracking is also Unix-only — synced
  once at session start elsewhere, not tracked further. See [Interactive
  Mode](interactive-mode.md).
- **Proxy support**: three deliberate differences — see [Proxies](proxies.md)
  for the full mechanics.

  - **A `localhost` proxy is rewritten on Linux too.** Batect rewrites on
    macOS/Windows only; on Linux it propagates the URL verbatim, where `localhost`
    means the container itself. This is [Batect's oldest open
    issue](https://github.com/batect/batect/issues/10), eight years old.
  - **A proxy bound to loopback only is diagnosed, not left to fail** — Batect's own
    roadmap notes the same warning is needed and never shipped one.
  - **`--no-proxy-vars` keeps the proxies in Docker's `config.json` out of a
    build too.** Batect's flag only stops its own proxy build arguments; its
    Docker client adds the `config.json` ones to every build regardless.

  Batect's Docker-version-gated hostname fallback chain, which reaches back
  to Docker 17.06, isn't ported: its first choice, `host.docker.internal`,
  is the one for Docker 18.03 and later, so every Docker release Ratect
  connects to would get it (see the Docker 20.10 entry under [Runtime behavior
  gaps](#runtime-behavior-gaps)).
- **Private registry credentials**: Batect's own Go client swallows every
  credential-helper error unconditionally; Ratect warns instead, naming the
  registry that failed to resolve, or, for a build, the `credsStore` whose
  helper couldn't list what it holds. See [Private registry
  credentials](ratect-compat-config-reference.md#private-registry-credentials).
- **A dependency exiting unexpectedly is reported, not silent.** Batect has no
  equivalent: a dependency that dies after becoming ready — while the task's own
  command, or a later dependency's own health/setup wait, is still going — is
  otherwise invisible until whatever depended on it fails for a confusing,
  unrelated-looking reason (a connection refused, a timeout). Ratect prints a
  warning naming the container and its exit code instead, in every output mode.
  See [Dependency Readiness](dependency-readiness.md#once-ready).
- **`all` mode splits on a lone carriage return too, not just `\n`.** Batect's
  `InterleavedContainerOutputSink` splits on `\n` only, so a container using
  `\r` to redraw progress in place (pip/curl/apt-style) produces no output at
  all until its next `\n`, then dumps everything since as one giant
  concatenated line — or produces none at all, if the stream ends first. A
  deliberate divergence: Ratect flushes on a lone `\r` (one not
  immediately followed by `\n` — a CRLF pair still folds to a single line
  break) the same way it already does on `\n`, so a real progress bar
  prints one interleaved line per redraw tick instead of staying silent —
  spammier, but never silent-then-dumped.
- **`all` mode prints a container's last line even without a trailing
  newline.** Batect prints a line only once its `\n` arrives, so a container
  whose output ends without one loses that last line.
- **Failure messages are Ratect's own.** A task fails at the same step as under
  Batect — an image that won't pull or build, a task network that can't be
  created, a container that can't be created, set up for
  `run_as_current_user`, or started, a dependency that doesn't become healthy,
  a failing setup command, a dependency cycle — but Batect's headline and
  wording aren't reproduced: `Failed to pull image X` and its `Caused by:`
  list, for example, rather than `Could not pull image X.` and the daemon's
  message — which, where the daemon refused a request, follows `Docker
  responded with status code N:` rather than `Error response from daemon:` —
  a failed build's output under `Build output:` rather than `Output
  from Docker was:`, and `Failed to start container 'X'` rather than `Could
  not run container X.`. A `run_as_current_user` or network failure is
  reported as itself, not, as Batect frames it, as a failure to create the
  container; either way the container is named as configured. See
  [Load errors](#load-errors) for the cause chain. The message is an
  unprefixed `Error: …` line on stderr in every output style, `all` included,
  where Batect's `all` prints `<task> ! ` or `<container> ! ` lines on stdout;
  and `all` reports a failed setup command or network check, two failures
  Batect's `all` doesn't print at all. A setup command's output is trimmed, so
  output that is only whitespace counts as none.
- **No white text.** Batect paints its `simple` status lines, and `all`'s
  task-level prefix, in the terminal palette's white (SGR 37) — light grey on a
  dark theme, near-invisible on a light one. Ratect's own lines are the
  terminal's default foreground, with bold for emphasis, and it never sets the
  palette's white or black
  ([decisions/0017](../decisions/0017-output-streams-and-styles.md)). The
  interrupt notice, the `--no-cleanup*` reminder and a cleanup that left
  something behind are printed on stderr in every style rather than logged;
  Batect prints its equivalents on stdout, as task output.
- **An unhealthy dependency's error can lack the last health check's
  details.** If the container, or the daemon, goes away between Docker's
  unhealthy verdict and Ratect looking up that check's exit code and output,
  the error gives the verdict alone. Batect reports the failed lookup instead,
  or crashes.
- **A startup failure drops the rest of startup at once.** When one of a
  task's containers can't be started — its image won't pull or build, or it
  doesn't become ready — whatever else was starting alongside it is dropped
  there and then, and cleanup begins. Batect cancels that work and waits for it
  to wind down first. A container whose create the daemon already had is still
  removed: cleanup waits for that create's answer before it removes anything,
  as Batect waits for its creates to finish. A further signal ends that wait
  as it would a graceful stop — everything else is force-removed — and a
  warning names the container and the run label that finds it.
- **Docker 20.10 or newer is required.** Ratect refuses a daemon whose API
  is older than 1.41 (Docker 20.10) when it connects — before any task runs,
  whether or not BuildKit is asked for — because its proxy support needs
  20.10's `host-gateway` address (see
  [Installation](installation.md#prerequisites)). Batect's floor is API 1.37
  (Docker 18.03), so Docker 18.03 to 19.03 runs under Batect and is refused by
  Ratect. Below API 1.37 both refuse, in different words: Ratect's error names
  the API version the daemon supports and the 1.41 it requires; Batect's, in
  red, names the daemon's Docker version. The exit code is any failure's (see
  [Exit codes and error
  reporting](ratect-compat-cli.md#exit-codes-and-error-reporting)) — `125` on
  a run — where Batect's is `-1` (255). Batect's refusals of BuildKit on a
  daemon too old for it, or without experimental features turned on, apply
  only below Docker 18.09, so none could fire on a daemon Ratect accepts, and
  they aren't ported.
- **The daemon is first asked for its version, not pinged.** Ratect's first
  request is `/version`; Batect sends `/_ping` and then asks for the version,
  before running anything. The image builder — BuildKit or the classic one — is
  chosen only once a run first builds an image, from a `/_ping` made then
  (see `--enable-buildkit` under [CLI flags](#cli-flags)). And a daemon that
  can't be reached, or is too old, is reported in Ratect's own words, not
  Batect's `Docker is not installed, not running or not compatible with
  Batect: …` — for an unreachable one, naming the host tried (see [What is
  checked on connecting](connecting-to-docker.md#what-is-checked-on-connecting)).
- **A daemon running Windows containers is refused.** Ratect checks the
  operating system the daemon reports and refuses `windows` before running
  anything, since it only supports Linux containers. Batect accepts a Windows
  daemon, and refuses only one reporting neither Linux nor Windows, which
  Ratect doesn't check.
- **The task network is ready before any image work starts.** Ratect creates
  it — or, under `--use-network`, checks that it exists — and only then pulls
  or builds an image, where Batect does both at once. So a network problem,
  such as a missing `--use-network` network or an exhausted address pool,
  fails the task straight away, rather than once the pulls and builds already
  in flight have finished.
- **Cleanup carries on past a container it fails to stop or remove.** A
  container whose stop fails isn't removed, as under Batect, but cleanup still
  stops and removes the task's other containers, including the ones it depends
  on; Batect's cleanup stalls there and leaves them running. Ratect also still
  goes on to remove the task network, where Batect keeps it and lists it among
  the manual cleanup commands it prints. While that container is still attached
  to the network, Docker refuses, and that refusal is reported in the same
  `Cleanup failed:` error as the container's, which lists both as left behind.
- **A task with only `prerequisites` says so in every style but `quiet`.**
  Once its prerequisites have run, `The task <name> only defines prerequisite
  tasks, nothing more to do.` is printed as under Batect, but not under
  `-o quiet`, which prints only errors, warnings and notices; Batect prints it
  in every style.
- **What cleanup leaves is reported in one line, in every style.** A
  `--no-cleanup*` run ends with `Left in place (--no-cleanup): …`, and a cleanup
  that failed with `Error: Cleanup failed: …; left behind: …`, each naming the
  containers by config name and short id and ending `Remove with:` one `docker
  rm -f -v` for every container, then `docker network rm` for the network. After
  a `--no-cleanup*` run, a line per kept container gives Batect's `docker logs`
  and `docker exec -it` hints, with `docker start` first for one that isn't
  running. Where they differ from Batect's:
  - They are printed under `-o quiet` too; Batect's `quiet` prints none of them.
  - After a failed cleanup only what was actually left behind is listed;
    Batect lists a command for every container and network the run created,
    removed or not, and says "some or all" of them may be needed.
  - Containers are named by the twelve-character id `docker ps` shows, and the
    network by its name; Batect gives each container's full id and the
    network's id, one `docker rm --force --volumes` per container.
  - Under `-o all` a failed cleanup is that one `Error:` line, with no
    container prefix; Batect prints `Could not stop container <name>.` or
    `Could not remove container <name>.` on each container's own prefix, and
    `Could not delete the task network.` on the task's.
- **A crash skips cleanup.** If Ratect itself crashes mid-run — a bug, which
  exits `101` and asks for a report — the run's containers and network are left
  behind. Batect catches an internal error in a step and still cleans up. What
  a crash leaves carries Ratect's ownership labels: `ratect resources clean`
  removes it (`ratect`-only), and from `ratect-compat`, which has no such
  command, `docker` does, filtering on the project's label — `docker rm -f -v
  $(docker ps -aq --filter label=eu.orican.ratect.project=<project>)`, then the
  same filter on `docker network ls -q` for `docker network rm`.
- **No pull lines for an image that's already local.** Under
  `image_pull_policy: IfNotPresent`, an image that's already present prints no
  `Pulling X...`/`Pulled X.` lines; Batect prints both even though nothing is
  pulled. See [Task Lifecycle](task-lifecycle.md).
- **Under `image_pull_policy: Always`, an image is pulled once per run**, not
  again for each task in a prerequisite chain that uses it, as Batect does. A
  `build_directory` image is likewise built once per run, where Batect builds
  it again for each task — and, under `Always`, pulls its base image again each
  time. See [Task Lifecycle](task-lifecycle.md).
- **`fancy` mode draws a fresh block after the terminal's width changes.**
  The terminal has re-wrapped the old lines by then, so moving the cursor back
  up over as many lines as were drawn, as Batect does, lands it in the wrong
  place on common terminals and corrupts the display. Ratect leaves the old
  block where it is and draws the new one below it, at the cost of that
  leftover block.
- **`fancy` mode treats a reported terminal width of 0 as unknown.** Some
  pseudo-terminals report it (`script`'s, some CI runners'), and lines are
  then printed unclipped; Batect clips every line to nothing.
- **Under `run_as_current_user`, a mount point nested inside a `local` mount is
  created with the type its source needs.** For a file or socket mounted inside a
  `local` mount — `./config.json`<!-- example --> at `/code/config.json` under `.` at `/code` —
  Ratect creates an empty file at the mount point, and the mount works. Batect
  always creates a directory there, which fails the run: a file or socket can't be
  mounted over a directory, and an existing file at the mount point fails the
  creation itself. Ratect also follows a symlink on the way to a mount point only
  while it stays inside the enclosing mount's host directory, leaving the mount point
  to the daemon otherwise; Batect follows it even out of that directory, creating
  the mount point outside the project. See [User mapping](ratect-compat-config-reference.md#user-mapping).

## Next steps

The behaviour both binaries share is documented in its own right rather
than against Batect: [Task Lifecycle](task-lifecycle.md)
for what a run does step by step, [Dependency
Readiness](dependency-readiness.md) for when a dependency counts as ready,
[Includes](includes.md) for how included files combine and what a Git bundle
may do, and the [FAQ](faq.md).
