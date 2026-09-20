# Full-stack worked example

Unlike the other five examples (one hello-world per ecosystem), this one is
a small but real multi-service app — deliberately more complex, to exercise
the parts of Ratect a single-container hello world can't reach: dependency
readiness, a dependency shared between two paths in the same task's graph,
and a genuine (not padded) wait for a slow-starting dependency.

## What's here

- **`app`** — a Node HTTP service (`GET /hello`): inserts a row into
  Postgres, then returns the total row count — read from Redis if a recent
  count is cached, from Postgres if not. `GET /healthz` is a separate,
  side-effect-free route used only by the health check; it deliberately
  isn't the same route `/hello` is, so a health-check poll never counts as
  a fake visit or pre-warms the cache before a real request does.
- **`db`** — Postgres, seeded via a real
  [`docker-entrypoint-initdb.d`](db/init/01-schema.sql) script with a
  million rows of sample history — real "sample data to develop against"
  (the project's own README opens with exactly that phrase), and, as a
  side effect, genuinely a few real seconds of work for `db` to do before
  it's healthy — not a `sleep` added for pacing.
- **`cache`** — Redis, backing the read-through cache above.
- **`journey-test`** — a black-box test: makes a real HTTP request to
  `app`, then checks Redis *directly* to confirm the value it just read got
  cached. Its task depends on both `app` (which itself depends on `db` and
  `cache`) and `cache` directly — two paths to the same dependency, so it's
  a real demonstration that Ratect starts `cache` once, not twice.

`app`'s own `dependencies = ["db", "cache"]` lives on the *container*, not
on the `run`/`journey-test` tasks that use it — it genuinely cannot start
without both (it connects to `cache` at module load, `db` on its first
request), so repeating it per task would just be two chances to get it
wrong. Compare `dev`, a separate container on the same image for
`build`/`unit-test`/`lint`/`shell`: those tasks' commands (`npm ci`, a unit
test, `eslint`) never start a server, so if they ran in `app` instead, its
health check would wait forever for a server that command was never going
to start — this is why `dev` exists as its own container rather than reusing
`app` for everything.

## Tasks

```
$ ratect tasks list
```

- `build` — `npm ci`
- `unit-test` — tests a pure helper function, no live dependencies —
  contrast with `journey-test` below
- `lint` — `eslint .`
- `run` — starts `db`, `cache`, then `app`, listening on `:8080`. Long-lived;
  stop it with Ctrl+C
- `journey-test` — starts the whole stack and exercises it for real
- `shell` — an interactive shell in the `dev` environment

## Running it

```
ratect run build
ratect run unit-test
ratect run lint
ratect run journey-test
```

`ratect run run` starts everything and keeps running in the foreground —
from another terminal:

```
curl localhost:8080/hello
```

The first call reports `"count_source": "database"`; call it again within
five seconds and it reports `"count_source": "cache"`.
