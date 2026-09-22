# Getting Started tutorial project

The project [Getting Started](../../docs/getting-started.md) builds up step by
step — each step of that page includes one slice of this `ratect.toml`, so the
tutorial is always exactly this file. Clone this directory to follow along.

## Tasks

```
$ ratect tasks list
```

- `hello` — `ls /code`, listing this directory from inside the container (the
  tutorial's first run)
- `build` — `echo building...`, a stand-in for a real build step
- `test` — `echo testing...`, with `build` as a prerequisite
- `greet` — prints `hello-world in dev`, from one host environment variable
  expression and one config variable

## Running it

```
ratect run hello
ratect run test
ratect run greet
ratect run greet --config-var environment_name=staging
```

Unlike the [worked examples](../../docs/worked-examples.md) alongside, this
isn't a real project in any language — it's the smallest configuration that
shows a bind mount, a prerequisite, and an expression, which is what a first
tutorial needs.
