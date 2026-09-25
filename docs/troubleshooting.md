# Troubleshooting

Where to look when a run fails, or hangs. Each of these has a home
elsewhere in the documentation already; this page is the entry point — one
line per symptom, linking where it lives — plus the one thing every
diagnosis ends up needing, [reading Ratect's own logs](#filtering-rust_log),
which is written here.

| Symptom | Where it lives |
| --- | --- |
| Will this run fail? Why did it? | [`ratect doctor`](ratect-cli.md#doctor) (`ratect` only) answers both without running a task: an unreachable daemon, a configuration that doesn't load, a missing `build_directory` or Dockerfile, and the warnings that bite later (a floating image tag, a dependency with no health check). |
| A dependency never becomes ready | [How Docker reaches its verdict](dependency-readiness.md#how-docker-reaches-its-verdict): `docker ps` and `docker inspect` show the health state Ratect is waiting on, and Ratect imposes no timeout of its own, so a check configured to retry forever waits forever. |
| The build failed | [Image building](ratect-compat-config-reference.md#image-building): a failing build's whole transcript is in the error Ratect reports, under every output style; for a *live* transcript, see [filtering `RUST_LOG`](#filtering-rust_log) below. |
| I need to see what Ratect is doing | [Filtering `RUST_LOG`](#filtering-rust_log), below. |
| A container was left behind, or I want to keep one | [`ratect resources`](ratect-cli.md#resources-options) lists and removes what a crash, a `docker kill` or a failed cleanup left; `--no-cleanup`, `--no-cleanup-after-success` and `--no-cleanup-after-failure` keep a run's containers on purpose, running, so you can look inside — on [`ratect run`](ratect-cli.md#run-options) and [`ratect-compat`](ratect-compat-cli.md#cleanup-after-a-run). |
| What does this exit code mean | [`ratect`](ratect-cli.md#exit-codes-and-diagnostics) and [`ratect-compat`](ratect-compat-cli.md#exit-codes-and-error-reporting) exit with the same codes: the task's own container's, 128 + the signal's number for a run ended by one, `1` for anything else that fails, `101` for a crash. |
| A JVM or Node build hangs or dies silently | [How do I raise Docker Desktop's resource limits?](faq.md#how-do-i-raise-docker-desktops-resource-limits) — every container runs under Docker's own CPU/memory ceiling, and a heavy build under a tight one looks like a hang or an OOM kill. |
| `&&` or `$VAR` in a `command` did nothing | [Why doesn't `&&` or `$VAR` work in my `command`?](faq.md#why-doesnt--or-var-work-in-my-command) — `command` is literal argv, with no shell and no expression support. |

## Filtering `RUST_LOG`

Ratect's own diagnostics go to stderr, filtered by the `RUST_LOG`
environment variable (default `info`) — [Logging vs.
output](how-it-works.md#5-logging-vs-output) is the split between them and
the task's own output. `RUST_LOG=debug` adds every container creation,
start and removal, each `setup_commands` exec's raw output, and every image
build's log lines, which is usually what "what is Ratect doing?" is asking
for.

`RUST_LOG` isn't just an on/off level switch — `tracing-subscriber`'s
[`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
syntax lets you scope it to specific modules (`target=level` directives, comma-separated).
This matters in practice once you turn on `debug` for anything build-related (e.g. to see
a live [image build](ratect-compat-config-reference.md#image-building) transcript): `bollard` (the Docker
API client Ratect is built on) also logs at `debug`, and a bare `RUST_LOG=debug` includes
*all* of its raw API traffic — usually far more noise than signal.

A directive with no target (e.g. `RUST_LOG=debug`) applies everywhere, including
dependencies like `bollard`. Scoping to a specific target instead — `ratect_core` covers
everything Ratect itself logs — excludes anything not matched, including `bollard`,
without needing to name it:

```sh
# Only ratect_core's own logs, at debug — no bollard noise at all.
RUST_LOG=ratect_core=debug ratect run build

# Keep the normal `info` default everywhere else, but add ratect_core's debug-level
# output on top (e.g. build transcripts) — usually the more useful combination.
RUST_LOG=info,ratect_core=debug ratect run build

# Narrower still: just the Docker/build/container-runtime module, not task
# orchestration (`ratect_core::engine`) as well.
RUST_LOG=ratect_core::docker=debug ratect run build
```

If you do want a blanket `debug` sweep across everything (including `bollard`) but need to
silence one specific dependency, add it as its own `=off` directive instead:
`RUST_LOG=debug,bollard=off`.

The same variable and syntax apply to `ratect-compat`, which can also tee
the same output into a file with
[`--log-file`](ratect-compat-cli.md#output).

**Before pasting a `debug` log anywhere**, remember that a `setup_commands`
exec's output is whatever the command itself printed — so if a setup
command's own output could include something sensitive (a failed connection
string, a verbose HTTP client dumping request headers), that ends up in the
debug log too. Treat `RUST_LOG=debug` (or narrower `ratect_core=debug`)
output with the same care you'd give the command's own output before pasting
it into a support ticket, chat message, or CI log.

`RUST_LOG=off` never hides why a run failed: a fatal error bypasses the filter —
see [Exit codes and error
reporting](ratect-compat-cli.md#exit-codes-and-error-reporting).

## Coming from Batect?

One deliberate difference in where the logs go, `--log-file` or not — listed
under [Differences from Batect](differences-from-batect.md#cli-flags).
