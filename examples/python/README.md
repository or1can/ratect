# Python worked example

A minimal script — one `hello.py` with a `greet()` function, a pytest test
for it, and a `ratect.toml` driving it. See
[Worked Examples](../../docs/worked-examples.md#python)
for the full writeup.

## Tasks

```
$ ratect tasks list
```

- `build` — `python -m compileall .`, a minimal stand-in for a real build
  step (there isn't a natural one for a script with no compiled artifact)
- `test` — `pytest` (checks `greet()` returns the right string)
- `run` — `python hello.py`, prints "Hello, world!" for real
- `lint` — `ruff check .`
- `shell` — an interactive shell in the build environment

## Running it

```
ratect run build
ratect run test
ratect run run
ratect run lint
```

`test`/`lint` each reinstall from `requirements-dev.txt` themselves rather
than relying on a separate install step, for the same reason as the Node
example: a task's own container is recreated from scratch each run. The
`pip` cache mount is what keeps that cheap.
