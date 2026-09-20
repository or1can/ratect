# Node.js worked example

A minimal TypeScript project — one `index.ts` with a `greet()` function, a
Vitest test for it, and a `ratect.toml` driving it. See
[Worked Examples](../../docs/worked-examples.md#nodejs)
for the full writeup.

## Tasks

```
$ ratect tasks list
```

- `build` — `tsc`, compiling to `dist/`
- `test` — `vitest run` (checks `greet()` returns the right string; runs
  directly against the TypeScript source, doesn't need `build` first)
- `run` — runs the compiled output, printing "Hello, world!" for real;
  depends on `build` having produced it first
- `lint` — `eslint .`, via `typescript-eslint`'s own config rather than
  `@eslint/js`'s `recommended` alone, which has no `.ts` file matcher at all
  (confirmed by testing: `eslint .` exited 0 while silently linting zero
  real files)
- `shell` — an interactive shell in the build environment

## Running it

```
ratect run build
ratect run test
ratect run run
ratect run lint
```

Every task runs `npm ci` itself before its real command — a task's own
container is recreated from scratch each run, so nothing installed by one
task's container is there for the next one's. The `~/.npm` cache mount is
what keeps a repeated `npm ci` fast.
