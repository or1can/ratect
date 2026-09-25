# Migrating a Batect Project to Ratect

This page is specifically for a project that already has a `batect.yml` —
see [Getting Started](getting-started.md) instead if you're bringing Ratect
into a project that doesn't use Batect at all.

Most task runners assume a clean slate — convert everything, or don't bother.
Ratect doesn't: `ratect-compat` reads an existing `batect.yml` unchanged, and a
native `ratect.toml` project can `include` a `batect.yml` fragment as-is (see
[Two binaries, two formats](index.md#two-binaries-two-formats)). That
makes migration a series of independent, optional steps rather than one
all-or-nothing conversion — stop at whichever one matches how much of the
tool you actually want.

## Step 1: point `ratect-compat` at what you already have

The first step needs no config changes at all: run `ratect-compat` against the
`batect.yml` you already have, in place of the (now unmaintained) `batect`
binary itself. Same file, same flags, mostly the same behavior — see
[Differences from Batect](differences-from-batect.md) for the short list of
real exceptions. This alone gets a maintained tool without touching a single
line of YAML, and is a complete, permanent choice on its own if that's all you
want.

## From here, two paths

Once `ratect-compat` is in place, there are two ways to move onto the native
`ratect.toml` format. They aren't sequential steps — pick one:
[`ratect config convert`](ratect-cli.md#config) only ever reads a Batect-format
root file, so it has to run **before** the project's root becomes `ratect.toml`
(path A below), not after. Once the root is native, finishing off any
remaining `.yml` includes is a by-hand job (path B). What actually changes
between the two formats is tabulated in the `ratect.toml` reference — [at a
glance](ratect-config-reference.md#differences-from-batectyml-at-a-glance) and
[where the semantics differ](ratect-config-reference.md#where-the-semantics-differ)
— and between the two binaries in the [`ratect` CLI
reference](ratect-cli.md#differences-from-ratect-compat).

### Path A: convert everything in one pass

`ratect config convert` reads a project's actual root `batect.yml` — includes
and all — and flattens every included file (Git bundles too) into one
`ratect.toml`. `ratect config convert -f batect.yml`, captured in
[`examples/jvm`](https://github.com/or1can/ratect/tree/main/examples/jvm) —
the one example kept as a `batect.yml`:

```ansi
{{#include captures/config-convert.ansi}}
```

The output is a starting point, not a finished file: [`config
convert`](ratect-cli.md#config) doesn't carry comments over, though it does
check the result round-trips losslessly before writing it. Review it
field-by-field against [Configuration Reference](ratect-config-reference.md)
before deleting the original — this is the fastest route to fully native, but
still one diff worth reading closely, not a step to run and forget.

### Path B: migrate incrementally

#### Try the native binary without converting anything

The native format isn't all-or-nothing either. An `include` entry is parsed by
its file extension (see [Includes](ratect-config-reference.md#includes)), so a
`ratect.toml` project can pull in an entire existing `batect.yml` unconverted.
The one adjustment needed: an included file must not declare `project_name`,
since that's root-only (see [Includes](ratect-compat-config-reference.md#includes)) — move
that one line out into the new root file instead.

```yaml
# legacy.yml — the old batect.yml, minus its `project_name:` line
containers:
  build-env:
    image: alpine:3.18.2
    volumes:
      - .:/code
tasks:
  test:
    run:
      container: build-env
      command: echo "Hello from ratect!"
```

```toml
# ratect.toml
project_name = "my-project"
include = [
    { path = "legacy.yml" },
]
```

`ratect tasks list -f ratect.toml` and `ratect run test -f ratect.toml` now
work through the native binary and a native entry point, with zero lines of
the actual configuration translated — mostly the same behavior as before, with
one edge case worth knowing: a Git-included bundle that itself declares a
nested `type: git` include is refused under the native format unless
`allow_nested_git_includes` is set, where `ratect-compat` allows it
unconditionally (see [Where the semantics
differ](ratect-config-reference.md#where-the-semantics-differ)). It only bites
if `legacy.yml` itself has a nested Git include of that shape.

#### Migrate one file at a time

From here, migration is as granular as you want it to be. Split `legacy.yml`
into smaller files along whatever lines make sense (one per container group,
one per pipeline stage — Batect's own `include` already supports this), then
convert each piece in turn: since `config convert` only accepts a full root
project (a file with its own `project_name`), converting one piece means
pointing it at a standalone copy with a temporary `project_name` line, then
copying the relevant `[containers...]`/`[tasks...]` tables out of its output
into a new `.toml` file. Swap that one `include` entry from `.yml` to `.toml`,
and move on whenever you're ready — there's no deadline, and a project can
stay a mix of both formats indefinitely.

What decides *when* a piece has to move is the feature you want. A container
still declared in a `.yml` is a Batect container wherever it is included, so
it can use no `ratect`-native field — see [Which fields a file may
use](includes.md#which-fields-a-file-may-use). Reaching for `ulimits`,
`extends` or `stop_signal` on a container is therefore the signal to convert
*that container* to TOML; nothing else in the project has to move with it.

## Next steps

Wherever you stopped, [Differences from Batect](differences-from-batect.md)
is what behaves differently from `batect` itself — the exceptions Step 1
pointed at, in full.
