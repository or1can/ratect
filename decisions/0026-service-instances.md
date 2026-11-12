# 0026 — A service instance belongs to no process

**Status:** Accepted — planned (ratect 0.14.0):
[#103](https://github.com/or1can/ratect/issues/103) (service groups and
foreground `services up`) and
[#124](https://github.com/or1can/ratect/issues/124) (detached mode).
`ratect` only: `ratect-compat` has no `services` verb.

## Context

[#103](https://github.com/or1can/ratect/issues/103) adds named groups of a
project's containers brought up together, and
[#124](https://github.com/or1can/ratect/issues/124) lets one outlive the
command that started it. Both briefs were written before anyone asked what
such a group *is* once it is up, and three things depend on the answer.

**Who ends it.** A task run has a process that drives it and cleans up after
it. A detached group has none by design. A foreground one has a process, but
#103 also asks that a second `up` on a running group attach to it rather
than start another, which gives one group two processes.

**What `resources` makes of it.** `ratect resources clean` removes what task
runs left behind. [0025](0025-task-run-lock.md) has it prove a task run's
process dead before removing it by default. A detached group has no process
to prove dead, and is long-lived on purpose.

**How a later command finds it.** #124 keeps no local state: the daemon is
the only record. A task run is found by its `run` label, one value per
execution, but a group is brought up by one invocation and used, attached to
and taken down by others.

How comparable tools answer these is in
[`research/leftover-selection-and-instance-identity.md`](../research/leftover-selection-and-instance-identity.md).
In short: Docker Compose identifies a project by name alone, finds its
containers by label, and keeps no record of a foreground `up`, whose
containers kept running when it was killed; the Dev Containers CLI identifies
a container by its workspace's absolute path; Testcontainers identifies what
it made by a session, which no later invocation can recompute.

[#875](https://github.com/or1can/ratect/issues/875) asked whether the lock
0025 gives a task run was needed here too, to stop two `up` at once from
each creating the group. It is not, for containers: of two creates with one
name the daemon let exactly one win. It ran Docker 29 only, where a pair of
network creates behaved the same; Docker 20.10 to 24 create a second network
of the same name, as [`docs/task-lifecycle.md`](../docs/task-lifecycle.md)
says of a task's network.

## Decision

### 1. A service instance is not a task run, and no process owns it

A *service instance* is one copy of a service group, brought up. It carries
no `run` label and takes no lock.

- **Foreground `services up`** brings the instance up, or adopts the one
  already there, streams its output, and takes it down when the group's exit
  trigger fires or on a termination signal. The process is a client of the
  instance, not its owner.
- **Detached `services up`** brings up or adopts the instance and returns. No
  exit trigger applies: nothing is watching.
- **`services down`** finds the instance by its labels and removes it. It
  needs nothing the `up` left behind but the labels.
- **Attaching** is a foreground `services up` on a running instance. A
  termination signal to a foreground `up` takes the instance down, whoever
  started it and however, and another `up` attached to it sees its members
  stop and ends.
- **A foreground `up` that is killed outright** cleans up nothing, and the
  instance keeps running as a detached one does. `services down` ends it.

**Adopting covers whatever carries the instance's labels, stopped members
included.** `up` reuses or replaces what it finds, so two generations of one
instance do not exist side by side.

### 2. An instance is found by project, service group and instance

Its containers and network carry what 0002 puts on a container and a network
(project, version, and on a container its name in the configuration), and:

| Label | On | Value |
| --- | --- | --- |
| `eu.orican.ratect.role` | containers | `service` |
| the service group's label | containers, network | the group's name in the configuration |
| the instance's label | containers, network | the instance; a fixed value in 0.14.0 |
| the start directory's label | containers, network | the directory of the configuration file `up` was given |

They carry no `task` or `run` label. The new keys are #103's to name; they
are additions in 0002's namespace.

### 3. A container's name guards a concurrent `up`

An instance's names are deterministic, made from its project, service group,
instance and, for a container, the container. Of two `up` creating the same
container name, the daemon refuses the second. That holds for whoever
reaches the daemon, though #875 ran it from one machine only.

- A name held by something without the instance's labels is refused with an
  error. It is not adopted and not removed.
- The labels remain the identity, as
  [0018](0018-built-images-named-for-people-found-by-labels.md) has it for
  built images; the name is the guard and nothing finds an instance by it.
- A network's name is no such guard on every daemon Ratect supports
  (Context), so the instance's network is not what serialises an `up`.

### 4. One instance of a service group per project, for now

The instance label and the instance's place in the name are there from
0.14.0, with a fixed value, so that allowing a second later changes no label
or name on a daemon ([#873](https://github.com/or1can/ratect/issues/873)).

Two worktrees of a project share its project name, so `services up` in the
second adopts the first's instance, whose bind mounts point into the first
worktree. `up` prints a warning
([0017](0017-output-streams-and-styles.md)) when it adopts an instance whose
start directory is not its own configuration file's. The start directory is
for that warning and for `services list`; nothing finds an instance by it.

### 5. `resources` leaves a service instance alone, whatever its state

`resources list` and `resources clean` do not select a service instance:
running or exited, with `--all` or without. `services down` removes an
instance in any state, and `services list` is where one is seen.

An `up` that is still starting has a network and created members with
nothing running, and one bringing an instance up again adopts its exited
members (Decision 1). Neither can be told from an abandoned instance, and an
instance has no lock ([0025](0025-task-run-lock.md)) to say which. One command removing an
instance, in every state, is also the simpler rule.

### Open, for #103 to settle and fold back here

This record decides the model. These need code and a real daemon:

- **The network on Docker 20.10 to 24**, where a name does not stop a second
  one being created.
- **Adoption after a lost create.** Ratect sets a container up further after
  Docker creates it. An `up` that adopts a container another `up` just
  created must not start it half set up, and must cope with that `up` failing
  and removing it.
- **An instance won piecemeal.** The guard is per container, so two `up`
  from two worktrees could each win different members, leaving an instance
  with bind mounts into both. Either an instance is won whole, or that state
  is refused.
- **The name's format.** Parts joined by hyphens are ambiguous between
  projects (`shop` with group `api-db`, `shop-api` with group `db`), and a
  group's name is new in a Docker name and needs the load-time check a
  project's has.
- What `up` does with a member whose configuration or image has changed, and
  what an exit trigger means when a member had already exited.
- How a task reaches an instance's network, since `--use-network` takes a
  Docker network's name.

## Alternatives considered

**Detached `services up` as a Ratect process put in the background.** The
manager would keep a group's exit trigger working while detached. But
`services down` would have to find the manager, which means a process ID on
disk where #124 keeps no local state, or stop the containers and let it
notice; and the manager can die (a reboot, a logout, the end of a CI step)
leaving a running instance with no owner, so the ownerless path is needed
regardless. It is a second mechanism that removes nothing of the first. It
can still be added on top of this model if a detached exit trigger is wanted.

**A foreground `services up` as a task run, with detached as something
else.** Two code paths for one thing; a second `up` would give an instance
two owners; and a killed foreground instance looks the same as a detached
one.

**A lock held by a foreground `services up`**, so that `services list` could
say whether anything is attached and the next `up` could tell a detached
instance from one whose `up` was killed. Both behave the same afterwards, so
it would change a display column and no behaviour.

**A file lock to serialise a concurrent `up`.** It works on one machine and
not through a shared daemon. A container's name guards a create in both;
what that leaves unserialised is listed as open above.

**Have `resources` select an instance whose members have all exited.** It
would clear an abandoned instance without a second command, and it would
remove one that an `up` is adopting to bring up again.

**Identify an instance by a `run` label.** `up` adopts an instance, so an
instance outlives the invocation that created it and is used by later ones;
the `run` label would have to mean "an execution" for tasks and "a long-lived
group" for services.

**Identify an instance by its directory**, as the Dev Containers CLI does.
Worktrees would be isolated with no flag, but moving a directory orphans the
instance, and `services down` from elsewhere can't find it.

**Several instances of a service group in 0.14.0.** A second instance of a
group that publishes a port can't start, since a published port is a fixed
integer in the configuration; a task has no way to say which instance's
network it wants; and `down` and `list` would need to address one. Each is a
design of its own, so the flag is deferred to
[#873](https://github.com/or1can/ratect/issues/873) with the label and the
name already in place.

## Consequences

- **Killing a foreground `services up` outright leaves its instance
  running**, with nothing on the terminal to say so: a process killed that
  way prints nothing. Whether `up` should say at the start how the instance
  is ended, or the next `up` should say it found one running, is #103's to
  decide when the output is designed.
- A termination signal in an attached foreground `up` stops an instance
  somebody else started detached. The CLI documentation has to say it.
- A detached instance has no group exit trigger and no exit code: a member
  stopping leaves the others running.
- An instance is invisible to `resources list` and to `ratect doctor`'s
  leftover finding, so `services list` is required, and #124's reliance on
  `resources list` to show service containers no longer holds. An abandoned
  instance stays until someone runs `services down`.
- A second worktree shares the first's instance until
  [#873](https://github.com/or1can/ratect/issues/873). The warning says so;
  it doesn't prevent it.
- An instance's Docker names are fixed, where a task run's carry a suffix of
  its own. Something else on the daemon holding one of those names stops `up`
  with an error until it is removed.
- A view of a running instance's output that doesn't take it down on exit,
  and a way to detach from a foreground `up`, are not part of this.
- #875's check of Decision 3 ran against Docker 29 on OrbStack only, from
  one machine: five pairs of concurrent `docker create`, never two
  `services up`, and no second machine or remote daemon.
