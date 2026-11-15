# Concurrent clients on one container group: how Compose and the daemon behave

Research for [ratect#103](https://github.com/or1can/ratect/issues/103)
(service groups, `services up`/`down`) and
[decisions/0026](../decisions/0026-service-instances.md). It answers one
question: when two clients act on the same group of containers at once, what
serialises them, and what does each see? The clients are two `up`s from
nothing, an `up` that starts while another is creating or tearing down, a
foreground `up` whose containers someone else stops or removes, and the
daemon calls such a client depends on. It records facts and precedents only.
It does not choose a design for ratect.
[leftover-selection-and-instance-identity.md](leftover-selection-and-instance-identity.md)
covers identity across invocations and leftover selection, and this file
does not repeat it.

Every claim cites the source that owns it, pinned to a tag: Docker Compose
`v5.5.0`, moby `docker-v29.8.2`, and moby's Go client module `client/v0.5.1`
(the version Compose `v5.5.0`'s
[go.mod](https://github.com/docker/compose/blob/v5.5.0/go.mod#L33-L34)
requires). compose-go and docker/cli were not needed for any claim here.
Where this document reasons from a source without quoting it, it says so
("inference"). Claims that rest on a local probe are marked "observation",
and [The probe](#the-probe) says what was run and what it did not vary.
Anything that could not be checked is in [Not verified](#not-verified).

## Summary

1. **Nothing serialises two Compose `up`s.** Compose takes no lock for `up`.
   Its only lock is `watch`'s PID file. Each `up` snapshots the project's
   containers, networks and volumes, plans from that snapshot, and executes
   the plan. The daemon's name uniqueness is the only arbiter.
2. **Compose absorbs a network-name conflict and not a container-name
   conflict.** A 409 from network create is treated as success, citing "a
   concurrent `docker compose up|run`". A 409 from container create fails
   that `up`. Observation: in five runs of two simultaneous `up -d` from
   nothing, one exited 1 with the name conflict every time and the other
   brought the project up. The loser left nothing behind.
3. **An `up` that starts during another's work adopts whatever it observes.**
   A container that is `created`, `exited` or `running` and whose config hash
   matches is kept and started if not running. Observation: a second `up`
   started while the first was waiting on a health check started the
   `created` container the first had made, and both exited 0. A second `up -d`
   started during `compose down`'s stop grace period printed `Running`,
   exited 0, and then `down` removed the container and the network.
4. **A foreground `up` does not stop or remove anything when someone else
   stops the project. It notices and ends.** Its monitor ends when every
   container it watches has died. Observation: after another `up`'s Ctrl+C,
   `compose stop`, `compose down` or `docker rm -f`, the first `up` printed
   `<name> exited with code N` per container and exited 0, even for code 137.
   With `--abort-on-container-exit` it exits with the first exited
   container's code. With `--exit-code-from=<svc>` it exits with that
   service's code. If only some containers stop, a plain `up` keeps running.
5. **An attach stream ends at the container's next exit and survives removal
   of a stopped container.** Attach checks only for `paused` and
   `restarting`. On a stopped container it waits, delivers the next run's
   output, and ends at that run's exit. Observation: an attach to an exited
   container stayed open after the container was removed. Both attach and
   `logs --follow` end at every exit, a restart-policy restart included.
6. **`wait` and removal.** `not-running` on a stopped container returns at
   once. `next-exit` and `removed` block. Removal wakes every waiter,
   including a failed removal. A removed container gives 404.
7. **A stopped container holds no network endpoint.** Its sandbox is deleted
   when it stops. Observation: `docker network rm` succeeded with a `created`
   and an `exited` container attached, and the `created` one then failed to
   start. A running container's endpoint makes the removal fail with HTTP 403
   ("has active endpoints"). Observation: run together with `docker rm -f`, a
   network removal was refused at a 0 to 20 ms delay and succeeded from
   50 ms on.
8. **Duplicate network names have been refused since API 1.44 (Docker 25).**
   The name check runs under a per-name lock in libnetwork.

## 1. Two concurrent `compose up`s on one project

**No lock.** `up` is `create` followed by `start`. `create` checks the model,
pulls images, collects the observed state, reconciles it into a plan, and
executes the plan
([create.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/create.go#L68-L131),
[up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L44-L54)).
The observed state is one listing of the project's containers (one-offs
included, stopped included), networks and volumes, filtered on the project
label
([observed_state.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/observed_state.go#L141-L228)).
Every `sync.Mutex` in `pkg/compose` is in-process. One of them, `startMx`,
serialises `ContainerStart` "to prevent race condition in engine assigning
ports from ranges", within one process
([convergence.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/convergence.go#L326-L327),
[executor_ops.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/executor_ops.go#L130-L135)).
The only file lock in Compose is `watch`'s PID file, and `watch.go` is its
only caller
([watch.go](https://github.com/docker/compose/blob/v5.5.0/cmd/compose/watch.go#L96-L103)).
Inference: between the snapshot and each create, nothing prevents another
client from changing the project.

**Network create: a conflict counts as success.** `createNetwork` calls
`NetworkCreate`. On an error for which `errdefs.IsConflict` holds, it emits
`Created` and returns nil: "A concurrent `docker compose up|run` may have
created the same network between the observed-state snapshot and now. Treat
the resulting conflict as success rather than failing hard, mirroring the
retry the previous ensureNetwork performed"
([create.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/create.go#L1432-L1446)).
It doesn't inspect the network that won, so inference: a same-named network
with a different config hash, or one that isn't Compose's, is accepted the
same way. Volume create has no such branch
([create.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/create.go#L1499-L1519)).

**Container create: a conflict fails `up`.** `createMobyContainer` returns any
`ContainerCreate` error as is, and `createContainer` reports it as an error
event
([convergence.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/convergence.go#L306-L360)).
In the non-test source of `pkg/compose`, `errdefs.IsConflict` appears only at
network create, best-effort network removal, `down`'s removals and stops,
and `up`'s second-signal kill
([create.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/create.go#L1437),
[executor_ops.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/executor_ops.go#L39-L49),
[down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L266-L282),
[down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L332-L353),
[up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L171-L174)).
The plan executor runs nodes under `errgroup.WithContext`, so the first
failing node cancels nodes that are still waiting on their dependencies
([executor.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/executor.go#L90-L135)).
Nothing is retried or re-observed.

The daemon reserves the name in `reserveName`. A taken name returns
`nameConflictError` ("The container name %q is already in use by container
%q"), whose `Conflict()` makes it a 409
([names.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/names.go#L63-L83),
[errors.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/errors.go#L57-L66)).
A removed container's names are released only after it has been deleted
from the daemon's store, near the end of `cleanupContainer`
([delete.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/delete.go#L170-L186)).
Inference: a create under that name fails with 409 for as long as a removal
is in progress.

Observation: two `docker compose up -d` started together on a two-service
project, five times. Each time one exited 0 and one exited 1 with `Error
response from daemon: Conflict. The container name "/research-cc1-a-1" is
already in use by container "…"`. Both printed `Network … Created`, and the
loser created no other container. Each run ended with one network and two
running containers.

**An `up` that starts while another is mid-create adopts what it sees.** In
reconciliation, a container whose config hash, image digest and so on match
(`mustRecreate` false) is left alone if it is `running`, `created`,
`restarting` or `exited`. Any other state gets a start operation: "Any other
state (paused, dead, ...): attempt to (re)start"
([reconcile.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/reconcile.go#L710-L729)).
New containers are numbered after the highest observed number
([reconcile.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/reconcile.go#L731-L745)).
`start` then lists the project's containers again, stopped included, and
starts every container of each service whose state is not `running`, in
dependency order, after waiting for that service's dependencies
([start.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/start.go#L37-L71),
[convergence.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/convergence.go#L519-L563),
[containers.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/containers.go#L165-L167)).
Starting a container that is already running is not an error. The daemon
answers 304 ("container is already running")
([start.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/start.go#L25-L46)),
and the client treats any status below 400 as success
([request.go](https://github.com/moby/moby/blob/client/v0.5.1/client/request.go#L221-L227)).
Inference: two `up`s that both reach `start` for one container both succeed.

Observation: service `b` depended on `a` being healthy, and `a` turned healthy
after 4 seconds. A second `up -d` was started 1.5 seconds after the first, by
which time `a` was `running` and `b` was `created`. The second printed `a-1
Running`, `Waiting`, `Healthy`, then `b-1 Starting`, `Started`. Both exited 0
and the container ids were unchanged. Three runs gave the same result.

## 2. A foreground `up` whose project someone else stops

**`up` attaches before it starts, then waits on a monitor.** In attached mode,
`up` installs its `SIGINT`/`SIGTERM` handler, attaches to every container of
the project, starts the monitor, and only then calls `start`
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L67-L73),
[up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L239-L291)).
The attach uses `ContainerAttach` with `Stream: true` and `Logs: false`, and
falls back to `ContainerLogs` with `Follow: true` if the attach fails
([attach.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/attach.go#L35-L64),
[attach.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/attach.go#L133-L156)).
Compose's signal handling (graceful stop on the first signal, kill on the
second) is in
[the earlier file](leftover-selection-and-instance-identity.md#4-whether-a-supervising-process-is-alive).

**The monitor returns once every container it tracks has died.** It seeds its
set with the project's non-one-off containers, stopped ones included, and
then reads the daemon's event stream for the project. A `create` or `start`
event adds a container to the set. A `die` event inspects the container. If
the inspect says it is restarting or running, the container stays in the
set. Otherwise, a not-found inspect included ("Source is already removed"),
the monitor emits an exited event and drops the container. The loop returns
nil when the set is empty
([monitor.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/monitor.go#L57-L165)).
The struct's comment calls the services filter the way to ignore containers
"maybe ran by a concurrent compose command"
([monitor.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/monitor.go#L32-L38)).
When the monitor returns, `up` cancels its global context, which ends the
signal goroutine without stopping anything
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L148-L155),
[up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L277-L283)).
No branch of `up` removes containers.

**The exit code.** `up` returns the joined errors, or a `StatusError` carrying
`exitCode` if that is non-zero
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L293-L302)).
`exitCode` is set only by two listeners. With `--abort-on-container-exit` (or
`--abort-on-container-failure`, which skips a code of 0), the first exited
event sets it, prints "Aborting on container exit...", and stops the project.
With `--exit-code-from`, the first exited event of that service sets it
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L204-L237)).
`--exit-code-from` implies `--abort-on-container-exit` unless
`--abort-on-container-failure` is given
([up.go](https://github.com/docker/compose/blob/v5.5.0/cmd/compose/up.go#L194-L196)).
Inference: a plain `up` whose containers are stopped by another client exits
0, whatever their exit codes were.

Observation, with two services trapping `SIGTERM` to exit 0:

| Running `up` flags | External action | `up` printed | `up` exit | Left behind |
| --- | --- | --- | --- | --- |
| none | Ctrl+C (`SIGINT`) of a second foreground `up` | `b-1 exited with code 0`, `a-1 exited with code 0` | 0 (second `up`: 130) | both `exited` |
| none | `compose down` | the two exit lines | 0 | nothing |
| none | `compose stop` | the two exit lines | 0 | both `exited` |
| none | `docker kill` of both | `exited with code 137` twice | 0 | both `exited` |
| none | `docker rm -f` of both | `exited with code 137` twice | 0 | nothing |
| none | `docker stop` of one | one exit line; still running 10 s later | 130 after `SIGINT` | both `exited` |
| `--abort-on-container-exit` | `compose stop`, `compose down` or `docker stop` of one | one exit line, "Aborting on container exit...", its own stop of both, the second exit line | 0 | as the action left it |
| `--abort-on-container-exit` | `docker kill` of `b` | `b-1 exited with code 137`, then aborts | 137 | both `exited` |
| `--exit-code-from=a` | `docker kill` of `a` | `a-1 exited with code 137`, then aborts | 137 | both `exited` |
| `--exit-code-from=a` | `docker kill` of `b` | aborts, `a-1 exited with code 0` | 0 | both `exited` |

Under `compose down`, the aborting `up` ran its own stop alongside `down`.
`down` still removed both containers and the network, and both exited 0.

## 3. `compose up` on a partly running or stopped project

**Stopped containers are started in place, not recreated.** A container whose
hash matches and that is `exited` or `created` gets no plan node, and `start`
then starts it ([section 1](#1-two-concurrent-compose-ups-on-one-project)).
Recreation follows `mustRecreate`'s rules, which are in
[the earlier file](leftover-selection-and-instance-identity.md#2-a-second-invocation-against-a-running-group).
Network mismatch is checked only for a running container
([reconcile.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/reconcile.go#L776)).
Observation: after `compose stop`, `up -d` printed `Starting`/`Started` for
both containers, and the ids were unchanged. With one of two containers
stopped by `docker stop`, it printed `b-1 Running` and started `a-1`, again
with the same id. After `compose create`, `up -d` started both `created`
containers with the same ids.

**The other states, as the daemon reports them.** The state string is
`running`, `paused` or `restarting` while the `Running` flag is set. Otherwise
it is `removing` while a removal is in progress, `dead` once marked dead,
`created` if never started, and `exited` after that
([state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L121-L147)).
`SetRestarting` sets `Running`, "because of all the checks in docker around
rm/stop/etc"
([state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L294-L311)).
A force removal marks a container dead after stopping it, "We don't want
anybody to be restarting it"
([delete.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/delete.go#L92-L140)).
What Compose meets in each:

| State observed | Compose's plan (hash matches) | What the daemon does with it |
| --- | --- | --- |
| `created`, `exited` | nothing in `create`; started by `start` | started |
| `running` | nothing | start answers 304 |
| `restarting` | nothing in `create`; `start` calls start, as it is not `running` | 304, as `Running` is set (inference) |
| `paused` | start node | 409 "cannot start a paused container, try unpause instead" |
| `dead`, `removing` | start node | 409 "container is marked for removal and cannot be started" |
| gone by the time of the call | as observed | 404 |

The daemon side of the table is `validateState`
([start.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/start.go#L25-L46)).
Inference from the table: a `compose up` that observes a container mid-removal
by another client fails its start with 409. One that observes the container
after its removal creates a new one under the same name.

**A container that is `running` while being removed reads as running.**
`containerRm` sets `RemovalInProgress` first, but `State()` reports `running`
until the kill takes effect
([delete.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/delete.go#L38-L46),
[state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L121-L136)).
A second concurrent removal returns 409, "removal of container %s is already
in progress". The daemon's own TODO says this "makes it indistinguishable from
actual state conflicts"
([delete.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/delete.go#L38-L45)).
Compose's `down` treats a not-found or conflict from container remove as
removed
([down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L343-L351)).
Observation: two simultaneous `compose down`s both exited 0, and both printed
`Removed` for every container. In one of three runs the second printed
`Network … No resource found to remove`.

Observation, `up` during `down`: a service ignoring `SIGTERM` had a
6-second `stop_grace_period`. While `compose down` was waiting it out, a
second `compose up -d` printed `Container research-cc3-a-1 Running` and
exited 0. `down` then removed the container and the network, and nothing of
the project was left. The same happened for start delays of 5.9 to
6.1 seconds. From 6.12 seconds the second `up` created a fresh container,
and it was left running. No run hit the window in which `up` would observe
the network that `down` was about to remove, or the container in the
`removing` state.

Observation, a stopped container whose network is gone: after `compose stop`
and `docker network rm <project>_default`, `compose up -d` recreated the
network and then failed to start both containers with `failed to set up
container networking: network <old id> not found`. It exited 1 and left both
`exited`. A container on a user-defined network records the network's id,
and the daemon prefers it over the name
([container_operations.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container_operations.go#L1170-L1177)).

## 4. Daemon behaviour a client depends on

### a. Attach on a stopping or stopped container

`ContainerAttach` refuses only a paused container and a restarting one, both
with 409 ("wait until the container is running")
([attach.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/attach.go#L33-L42)).
With `stream`, it copies the container's stream pipes until the container's
attach context is cancelled or the copy ends
([attach.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/attach.go#L174-L216),
[attach.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/internal/stream/attach.go#L134-L171)).
The attach context is created on first use and set back to nil when it is
cancelled
([attach_context.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/attach_context.go#L15-L35)).
Its only canceller is `Cleanup`, which runs after a container's process
exits ([start.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/start.go#L276-L318),
[monitor.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/monitor.go#L83-L122)).
Before that, `Reset` closes the stream pipes and the log driver
([monitor.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/monitor.go#L17-L60)).
Inference: an attach to a stopped container waits for the next start and ends
at that run's exit, and an attach made before `start` sees the run's first
output. An attach made after `start`, without `logs`, misses whatever was
written before it.

Observations:

- Attach to an `exited` container, then `docker start` 2 seconds later: HTTP
  200, the new run's output (`first-run`), and the stream ended when the
  container exited.
- Attach to an `exited` container with no start: still open after 5 seconds.
- Attach to an `exited` container, then `docker rm` 2 seconds later: still
  open 6 seconds after the removal. Removal does not end it.
- Attach to a running container, then `docker stop`: the stream ended within
  the same second as the stop.

### b. When `logs --follow` ends

`ContainerLogs` refuses a container that is dead or marked for removal, with
409
([logs.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/logs.go#L46-L48)).
It follows only if it is reading the running container's own log driver.
When the container isn't running, `getLogger` starts a fresh driver, and
`follow := config.Follow && !cLogCreated` turns following off: the call
returns the history and ends
([logs.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/logs.go#L78-L84),
[logs.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/logs.go#L148-L167)).
A follower of a file-backed driver stops once the log file is closed and it
has read to the end
([follow.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/logger/loggerutils/follow.go#L101-L134)).
The driver is closed in `Reset` at every exit, and `handleContainerExit`
reaches the restart decision only after that
([monitor.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/monitor.go#L83-L140)).
So a follow ends at each exit, and a restart starts a new driver that the
old follower doesn't see. Compose works around this by starting a new
`ContainerLogs` follow, with `Since` set to the start time, on every start
event of a restarted container
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L250-L275),
[up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L305-L316)).

Observation: with `--restart always` and a command that exits after
2 seconds, both an attach stream and `docker logs -f` ended at the first
exit, 2 seconds after start, while the container went on restarting.

### c. `wait` conditions

`State.Wait` returns at once if the condition is already met. `not-running`
is met when `Running` is unset, and `removed` once removal has finished.
`next-exit` is never already met. A waiter for `removed` goes on the
remove-only list, and every other waiter on the stop list
([state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L157-L211)).
`SetStopped` and `SetRestarting` wake the stop list, so a restart-policy exit
wakes a `next-exit` or `not-running` waiter even though the container reads
as running again
([state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L279-L311)).
`SetRemovalError` wakes both lists, and `SetRemoved` is `SetRemovalError(nil)`,
so a failed removal also releases a `removed` waiter, with the error set
([state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L396-L409)).
`ContainerWait` looks the container up first, so a container that is already
gone gives an error before any wait
([wait.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/wait.go#L17-L24)).

Observation, on an `exited` container (last exit code 137): `not-running`
returned 200 `{"StatusCode":137}` at once. `next-exit` and `removed` were
still blocked after 3 seconds. With both of those pending, `docker rm`
returned each of them 200 `{"StatusCode":137}`. Once the container had been
removed, `wait` returned 404 `No such container`.

### d. Network removal and containers on the network

**What blocks a removal is a live endpoint, not a container's
configuration.** `Network.delete` refuses with `ActiveEndpointsError` when
the network has endpoints
([network.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/libnetwork/network.go#L999-L1039)).
That error's `Forbidden()` makes it HTTP 403, not 409
([error.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/libnetwork/error.go#L28-L41)).
On exit, `Cleanup` calls `releaseNetwork`, which deletes the container's
sandbox
([start.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/start.go#L276-L286),
[container_operations.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container_operations.go#L989-L1043)).
Inference: a stopped container holds no endpoint, and neither does a
`created` one, which has never had a sandbox. This settles the
"network whose containers are all stopped" item that
[the earlier file](leftover-selection-and-instance-identity.md#not-verified)
left unverified, in the direction this paragraph gives.

Observations:

- A network with one `created` and one `exited` container on it:
  `docker network inspect` listed 0 containers, and `docker network rm`
  succeeded. Starting the `created` container then failed with `network
  research-net1 not found`.
- With one running container: `network rm` failed with `network research-net2
  has active endpoints (name:"research-n-run" id:"…")`, HTTP 403 through the
  API.
- `docker rm -f` of a running container, with `docker network rm` started
  after it at a delay: refused (active endpoints) at 0 and 20 ms, and
  succeeded at 50, 100, 150, 200 and 300 ms.

Compose handles this on its side. Its best-effort network removal logs and
continues on a conflict, and `down`'s `removeResource` turns a conflict into
"Resource is still in use"
([executor_ops.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/executor_ops.go#L39-L49),
[down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L264-L282)).
Inference: the daemon returns 403 for active endpoints, not a conflict, so
neither `IsConflict` branch matches that error. The earlier file records that
`down` skips a network whose inspect lists containers before trying.

### e. Duplicate network names

`NewNetwork` takes a per-name lock and returns `NetworkNameError` ("network
with name %s already exists", a conflict) if a network of that name exists:
"Make sure two concurrent calls to this method won't create conflicting
networks"
([controller.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/libnetwork/controller.go#L517-L526),
[error.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/libnetwork/error.go#L18-L26)).
The API changelog for v1.44: "`POST /networks/create` with a duplicated name
now fails systematically. As such, the `CheckDuplicate` field is now
deprecated. Note that this change is _unversioned_ and applied to all API
versions on daemon that support version 1.44"
([CHANGELOG.md](https://github.com/moby/moby/blob/docker-v29.8.2/api/docs/CHANGELOG.md?plain=1#L368),
[CHANGELOG.md](https://github.com/moby/moby/blob/docker-v29.8.2/api/docs/CHANGELOG.md?plain=1#L399-L402)).
API 1.44 is Docker 25.0. Inference from the entry's wording: before it,
a duplicate was refused only when the client set `CheckDuplicate`. The older
daemons' code was not read.

## The probe

Run on 2026-11-15 on macOS (Darwin, arm64) against OrbStack's Docker daemon:
Docker client and server `29.8.2`, API `1.56`, kernel
`7.2.12-orbstack-00394-gbecdd6130068`, Docker Compose `v5.5.0`, over the
Unix socket. Raw API calls used `curl --unix-socket` against `/v1.52`. Scratch
files were under `/tmp/research-cc`, every project name began `research-`,
and everything created was removed afterwards. A final check found no
container or network named `research*`.

- **Concurrent `up -d`, from nothing (section 1).** Two services, `alpine`,
  `sleep 600`, `b` depending on `a`. Two `docker compose up -d` started in
  the background back to back, five times, each followed by `down -t 0`.
- **`up -d` mid-create (section 1).** `a` became healthy after 4 seconds and
  `b` depended on `service_healthy`. A second `up -d` started 1.5 seconds
  after the first, three times.
- **Foreground `up` versus external actions (section 2).** Two services
  trapping `SIGTERM` to exit 0. `docker compose up` (no TTY) ran in the
  background. Four seconds later the external action ran, and the `up` was
  given 10 more seconds to end before it was sent `SIGINT`. For the
  two-`up` case, `SIGINT` went to the second `up`'s `docker` process and its
  plugin child.
- **`up` on stopped, partly stopped, created and network-less projects
  (section 3).** `compose stop`, `docker stop` of one container,
  `compose create`, and `docker network rm` of the default network, each
  followed by `up -d`, comparing container ids.
- **`up` during `down` (section 3).** One service ignoring `SIGTERM`, with
  `stop_grace_period: 6s`. `compose down` ran in the background and
  `up -d` started after delays from 5.6 to 6.2 seconds. Two concurrent
  `compose down`s, three times.
- **Daemon calls (section 4).** Attach with `stream=1&stdout=1&stderr=1` and
  `logs=0`, under `timeout`, against exited, removed, stopped and
  restart-policy containers. `docker logs -f`. `wait` with each condition.
  `docker network rm` against created, exited and running containers, and
  against `docker rm -f` at delays from 0 to 300 ms.

Not varied: the daemon (only OrbStack: no Docker Desktop, no rootful or
rootless Linux engine), its version, the transport (no TCP or SSH), the
host, a TTY for `up` (none, so its keyboard menu was off), more than two
concurrent clients, services with more than one replica, a config change
between the concurrent clients (which would make one recreate what the other
adopts), log drivers other than the default, and an attach with `logs=1`. The
timing probes depend on process start-up times on this machine. "Never hit"
means not hit in the runs listed, not impossible.

## Synthesis

Where a concurrent client meets another, and who absorbs it:

| Collision | Compose `v5.5.0` | Daemon `29.8.2` |
| --- | --- | --- |
| Two creates of one network name | Absorbed: conflict treated as created | Per-name lock and 409 since API 1.44 |
| Two creates of one container name | Not absorbed: the loser's `up` exits 1 | 409 name conflict, held until a removal completes |
| Two starts of one container | Absorbed | 304 for the second |
| Start of a container being removed or dead | Not absorbed: the error fails `up` (inference) | 409 "marked for removal" |
| Start of a stopped container whose network was recreated | Not absorbed: observed, `up` exits 1 | Error `network <old id> not found` |
| `up` adopting during another's `down` grace period | Reports `Running`, exits 0; `down` then removes everything (observed) | Container reads `running` until killed |
| Two removals of one container | Absorbed: conflict or not-found treated as removed | 409 "already in progress" for the second |
| Network removal with a running container on it | `down` skips networks with containers; conflict branches don't match a 403 (inference) | 403 "has active endpoints"; a stopped container doesn't block |
| Foreground `up` whose containers someone else stops | Notices `die` events and ends with 0, or the first exit code under `--abort-on-container-exit` | Attach and log streams end at each exit |

## Not verified

- **Hitting the narrow windows.** No probe caught `up` observing a container
  in the `removing` state, `up` planning no network create just before
  `down` removed the network, or `up`'s container create racing `down`'s
  removal of the same name. The outcomes in the
  [section 3 table](#3-compose-up-on-a-partly-running-or-stopped-project)
  for `dead`, `removing` and `paused` come from source, not a probe.
- **Network-conflict absorption against a foreign network.** That Compose
  also accepts a same-named network with a different config hash, or one
  made by another tool, after a 409 is an inference from the code. It was
  not probed.
- **`IsConflict` against a 403.** That `down`'s and the executor's conflict
  branches don't match the active-endpoints error depends on how the client
  maps 403. `errdefs` mapping in the client module was not read.
- **Attach after removal.** Why the stream stays open after `docker rm` was
  not traced. Inference: nothing in `cleanupContainer` cancels the attach
  context or closes the stream pipes of a container that isn't running. How
  long it stays open (until the client disconnects, or the daemon restarts)
  was not checked.
- **Log drivers other than `json-file`/`local`.** The follow-ends-at-exit
  reading is of `loggerutils`' file follower. Journald, syslog and remote
  drivers, and the `dual logging` cache, were not read.
- **Docker 20.10 to 24 network creation.** Only the v1.44 changelog entry was
  read. The older daemons' code, and whether Compose of that era set
  `CheckDuplicate`, were not.
- **Compose `stop` and `kill` internals.** How `up`'s own stop on abort
  interacts with a concurrent `down` was observed once (both exited 0), not
  traced.
- **Other daemons and transports.** Everything observed ran on OrbStack over
  a Unix socket. See [The probe](#the-probe).
