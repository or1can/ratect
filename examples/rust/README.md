# Rust worked example

A minimal `cargo` project — one `main.rs` with a `greet()` function, a unit
test for it, and a `ratect.toml` driving it. See
[Worked Examples](../../docs/worked-examples.md#rust)
for the full writeup.

## Tasks

```
$ ratect tasks list
```

- `build` — `cargo build`
- `test` — `cargo test` (checks `greet()` returns the right string)
- `run` — `cargo run`, prints "Hello, world!" for real
- `lint` — `cargo clippy`. The official `rust` image doesn't ship the
  `clippy` component by default, so this adds it first
  (`rustup --quiet component add clippy`)
- `shell` — an interactive shell in the build environment

## Running it

```
ratect run build
ratect run test
ratect run run
ratect run lint
```

Cargo's registry and `target/` are each their own `cache` volume mount, so
only the first run of any task is slow.

Note the `[workspace]` table at the top of `Cargo.toml` — without it, `cargo`
would try to treat this directory as part of the parent Ratect repository's
own workspace (it happens to sit inside it) and refuse to build standalone.
