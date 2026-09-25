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

Only the invoked task's own container — the task actually named on the
command line. A prerequisite's, a dependency's, or a sidecar's container is
never interactive, however it is reached: a task that is interactive when you
name it is not when it runs as another task's prerequisite, because stdin can
only usefully attach to one container.

## What the eligible container gets

Always, independent of whether Ratect's own stdin and stdout are terminals:

- **Its stdin forwarded** from Ratect's own, so piped input reaches the
  container — including its end: when Ratect's own stdin closes, so does the
  container's, and a process that reads until end of input (`cat`, a stdio
  server) exits.
- **The host's `TERM`** propagated into its environment — see [`TERM`
  propagation](#term-propagation) below.

Additionally, when *both* Ratect's own stdin *and* stdout are real
terminals, **a real Docker TTY**: the local terminal goes into raw mode for
the session, so a full-screen program (an editor, `top`, a REPL with line
editing) renders as it would in a shell. Piped output, a redirected
non-terminal, or a CI job gets plain stdin forwarding and streamed output
instead — stdin still reaches the container either way.

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

Ratect's own `TERM` environment variable is copied into the eligible
container's environment whenever that container is eligible — not gated on a
real TTY actually being allocated, matching Batect. So a full-screen program
inside the container knows the terminal type even when piped output lets it
detect that it isn't attached to a real TTY. Never applied to a
prerequisite's, a dependency's, or a sidecar's container, and never to an
image build.

Where it sits among the container's other environment variables — what
overrides it on a key collision — is [Environment
precedence](ratect-compat-config-reference.md#environment-precedence).

## Coming from Batect?

One known, deliberate divergence: Batect's real-TTY gate checks only whether
its output is a real terminal; Ratect's requires *both* stdin and stdout to
be. It is listed, with the Unix-only resize tracking, under [Differences from
Batect](differences-from-batect.md#runtime-behavior-gaps).
