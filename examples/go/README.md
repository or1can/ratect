# Go worked example

A minimal `go.mod` project — one `main.go` with a `Greet()` function, a unit
test for it, and a `ratect.toml` driving it. See
[Worked Examples](../../docs/worked-examples.md#go)
for the full writeup.

## Tasks

```
$ ratect tasks list
```

- `build` — `go build ./...`
- `test` — `go test ./...` (checks `Greet()` returns the right string)
- `run` — `go run .`, prints "Hello, world!" for real
- `lint` — `go vet ./...`, part of the standard toolchain already in the
  build image — no second tool or container needed for a project this small
- `shell` — an interactive shell in the build environment

## Running it

```
ratect run build
ratect run test
ratect run run
ratect run lint
```

Go's module cache and build cache are each their own `cache` volume mount,
so only the first run of any task is slow.
