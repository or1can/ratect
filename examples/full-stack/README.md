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
  side-effect-free route used only by the readiness check; it deliberately
  isn't the same route `/hello` is, so a check never counts as a fake visit
  or pre-warms the cache before a real request does. That check is an
  [`external_health_check`](../../docs/ratect-config-reference.md#external_health_check-checking-a-container-from-outside-it):
  Ratect requests `/healthz` over the project's own network, so nothing runs
  inside `app` to establish that it is ready.
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
wrong. That is also why `dev` exists: a separate container on the same
image for `build`/`unit-test`/`lint`/`shell`, whose commands (`npm ci`, a
unit test, `eslint`) want none of `app`'s runtime wiring — running them in
`app` would start Postgres and Redis and take port 8080 for a task that
never serves anything.

It used to have a second reason that no longer applies, which is worth
knowing if you are reading older Ratect material: `app`'s check was once an
in-container `health_check`, and a task's own container waits on that, so
`npm ci` in `app` would have waited forever for a server it never starts.
An `external_health_check` is inert on a task's own container — nothing
waits on it, because running the container *is* the task.

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
