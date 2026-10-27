# Interactive Mode

A task whose command drops you into a shell, or otherwise needs your input,
works with nothing to configure: there is no `interactive` field in either
format, and the same task is interactive under both binaries. This page is
the one description of what that means — which container is eligible, what
it gets, when a real terminal is allocated, and how the container learns
what kind of terminal it has. The examples are `ratect.toml`; the
`batect.yml` spelling is the same fields with the same meaning.

```toml
{{#include ../examples/full-stack/ratect.toml:shell}}
```

`ratect run shell` lands you in `bash` inside `dev`, with your keystrokes
reaching it and its output reaching you. That is the
[full-stack example](dependency-readiness.md)'s own `shell` task; each
project on [Worked Examples](worked-examples.md) carries one too.

## Which container is eligible

Each task's own container — the task named on the command line, and every
prerequisite it runs, however deeply nested. A dependency's or a sidecar's
container is never interactive. Prerequisites run one after another, and
each task container is attached to Ratect's stdin only while it runs.
Whatever input is waiting meanwhile is forwarded to it, whether or not it
reads it, so a prerequisite takes piped input ahead of the named task. If
the pipe ends while it runs, the named task's stdin is closed from the
start. What
you type at a terminal goes to the task container running at the time; what
you type between two of them waits for the next.

## What the eligible container gets

Always, independent of whether Ratect's own stdin and stdout are terminals,
**its stdin forwarded** from Ratect's own, so piped input reaches the
container — including its end: when Ratect's own stdin closes, so does the
container's, and a process that reads until end of input (`cat`, a stdio
server) exits. (The host's `TERM` isn't the eligible container's alone:
every container gets it — see [`TERM` propagation](#term-propagation)
below.)

Additionally, when *both* Ratect's own stdin *and* stdout are real
terminals, **a real Docker TTY**: the local terminal goes into raw mode for
the session, so a full-screen program (an editor, `top`, a REPL with line
editing) renders as it would in a shell. A raw terminal sends Ctrl+C to the
container as a keystroke, as `docker run -it` does, so it interrupts the
program in the container rather than the run — and a command running as the
container's main process with no handler of its own ignores it. Piped output,
a redirected non-terminal, or a CI job gets plain stdin forwarding and
streamed output instead — stdin still reaches the container either way.

The one exception is [`--output all`](output-styles.md#all), whose
line-prefixed output can't host an interactive session: under it no
container gets a TTY or stdin, and every container gets `TERM=dumb` instead,
matching Batect.

## Terminal resizing

When a TTY is allocated, the container's stays in sync with the local
terminal's size for the whole session, not just once at the start — a local
resize is forwarded live, via a `SIGWINCH` handler. That tracking is
Unix-only; on other platforms the size is synced once, at the start of the
session, and not tracked further. Interactive mode itself works on every
platform.

## `TERM` propagation

Ratect's own `TERM` environment variable is copied into the environment of
every container a run creates — the invoked task's own, each prerequisite's,
and every dependency and sidecar — whether or not that container is
eligible, matching Batect. Under [`--output all`](output-styles.md#all) every
container gets `TERM=dumb` instead. It isn't gated on a real TTY actually
being allocated either, so a full-screen program inside the container knows
the terminal type even when piped output lets it detect that it isn't
attached to a real TTY. Never applied to an image build.

Where it sits among the container's other environment variables — what
overrides it on a key collision — is [Environment
precedence](ratect-compat-config-reference.md#environment-precedence).

## Coming from Batect?

One known, deliberate divergence: Batect's real-TTY gate checks only whether
its output is a real terminal; Ratect's requires *both* stdin and stdout to
be. It is listed, with the Unix-only resize tracking, under [Differences from
Batect](differences-from-batect.md#runtime-behavior-gaps).
