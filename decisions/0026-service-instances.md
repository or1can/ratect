# 0026 — A service instance belongs to no process

**Status:** Accepted — planned (ratect 0.14.0):
[#103](https://github.com/or1can/ratect/issues/103) (service groups and
foreground `services up`) and
[#124](https://github.com/or1can/ratect/issues/124) (detached mode).
`ratect` only: `ratect-compat` has no `services` verb. Refined while
implementing #103: Decision 3 now says how the network serialises a
concurrent `up`, and Decision 6 records how #103 settled the points this
record had left open. A first settlement had an `up` repair a stopped or
partly-up instance from what it listed; reviews kept finding races with
another `up` changing the same instance, so Decision 6 now has an `up`
attach, wait or replace, and never repair
([`research/concurrent-clients-on-one-container-group.md`](../research/concurrent-clients-on-one-container-group.md)).

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
says of a task's network. The daemon has refused a second since API 1.44,
Docker 25 (the research above).

## Decision

### 1. A service instance is not a task run, and no process owns it

A *service instance* is one copy of a service group, brought up. Ratect
gives it no `run` label, and it takes no lock.

- **Foreground `services up`** brings the instance up, or attaches to the
  one already there, streams its output, and takes it down when the group's exit
  condition is met or on a termination signal. The process is a client of the
  instance, not its owner.
- **Detached `services up`** brings up or attaches to the instance and returns. No
  exit condition applies: nothing is watching.
- **`services down`** finds the instance by its labels and removes it. It
  needs nothing the `up` left behind but the labels.
- **Attaching** is a foreground `services up` on a running instance. A
  termination signal to a foreground `up` takes the instance down, whoever
  started it and however, and another `up` attached to it sees its members
  stop and ends.
- **A foreground `up` that is killed outright** cleans up nothing, and the
  instance keeps running as a detached one does. `services down` ends it.

**Whatever carries the instance's labels is the instance, stopped members
included.** `up` attaches to an instance with a member up and replaces one
with none (Decision 6), so two generations of one instance do not exist
side by side.

### 2. An instance is found by project, service group and instance

Its containers and network carry what 0002 puts on a container and a network
(project, version, and on a container its name in the configuration), and:

| Label | On | Value |
| --- | --- | --- |
| `eu.orican.ratect.role` | containers | `service` |
| the service group's label | containers, network | the group's name in the configuration |
| the instance's label | containers, network | the instance; a fixed value in 0.14.0 |
| the start directory's label | containers, network | the directory of the configuration file `up` was given |

Ratect gives them no `task` or `run` label. The new keys are #103's to
name; they are additions in 0002's namespace.

A service instance doesn't write every key in 0002's namespace, and a key
its owner doesn't write can still reach a container: from its configured
`labels`, or from its image, whose labels Docker copies onto a container for
each key the create doesn't set. So a member can carry a `run` label, and a
task run's container a `service` one. A container's `role`, which Ratect
always sets and nothing overrides, says which owns it; a network, which
takes neither configured nor image labels and has no `role`, is an
instance's when it has a `service` label and no `run` label.

### 3. A container's name guards a concurrent `up`

An instance's names are deterministic, made from its project, service group,
instance and, for a container, the container. Of two `up` creating the same
container name, the daemon refuses the second. That holds for whoever
reaches the daemon, though #875 ran it from one machine only.

- A name held by something without the instance's labels is refused with an
  error. It is not attached to and not removed.
- The labels remain the identity, as
  [0018](0018-built-images-named-for-people-found-by-labels.md) has it for
  built images; the name is the guard and nothing finds an instance by it.
- A network's name is no such guard on every daemon Ratect supports
  (Context), so the network serialises an `up` by a different rule: an
  `up` that finds no instance creates its network, lists the networks
  carrying the instance's labels, and has won only if its own is the only
  one. One that finds a second removes its own and starts over after a
  random pause; one whose create the daemon refuses as a name in use goes
  by the instance it then finds, as Decision 6 says. Two creators can't both see only their own, since the later lists
  after both exist, so no two `up`s both bring the instance up; on a tie,
  where each sees both, both start over.

### 4. One instance of a service group per project, for now

The instance label and the instance's place in the name are there from
0.14.0, with a fixed value, so that allowing a second later changes no label
or name on a daemon ([#873](https://github.com/or1can/ratect/issues/873)).

Two worktrees of a project share its project name, so `services up` in the
second attaches to the first's instance, whose bind mounts point into the first
worktree. `up` prints a warning
([0017](0017-output-streams-and-styles.md)) when it attaches to an instance whose
start directory is not its own configuration file's. The start directory is
for that warning and for `services list`; nothing finds an instance by it.

### 5. `resources` leaves a service instance alone, whatever its state

`resources list` and `resources clean` do not select a service instance:
running or exited, with `--all` or without. `services down` removes an
instance in any state, and `services list` is where one is seen.

An `up` that is still starting has a network and created members with
nothing running, which can't be told from an abandoned instance, and an
instance has no lock ([0025](0025-task-run-lock.md)) to say which. One whose
members have all exited is the next `up`'s to replace (Decision 6). One
command removing an instance, in every state, is also the simpler rule.

### 6. How #103 settled what this record left open

- **The network on Docker 20.10 to 24**: Decision 3's list-after-create
  rule, which holds whatever a daemon does with a second network of one
  name. The winner's containers join the network by its id, since a
  loser's twin makes the name ambiguous until it is removed. #103's
  real-daemon tests run on Docker 25 or later only (Docker 29 on OrbStack,
  and the engine on CI's Linux runner), where the daemon refuses the second
  create, so the rule's other half — removing a second network and starting
  over — is tested against a fake runtime and no daemon.
- **What an `up` does with an instance it finds** depends on the states
  its containers are listed in, the first row that applies deciding, and it
  never repairs one:

  | It finds | It does |
  | --- | --- |
  | A container only created or being removed | Waits, with a notice, and looks again: another `up` or `services down` is bringing it up or taking it down |
  | No network | Replaces it: takes down what it found, then starts over |
  | A network and no container | Waits, as above |
  | A member up: running, or paused or restarting, which an exit still lies ahead of | Attaches: follows each container running at that moment and watches each member that is up, starting nothing |
  | No member up | Replaces it, as above |
  | Nothing | Creates the network (Decision 3) |

  The winner creates every container before it starts any, so an `up`
  that finds an instance still being brought up sees a container only
  created and waits, rather than attaching to half of it; one whose winner
  fails sees the network go, and starts over. Starting over is tried up to
  five times; waiting isn't counted. Repairing — starting stopped members
  again where they stand — is what the first settlement did, and it raced
  whatever else was changing the instance: a stopped container holds no
  network endpoint, so a take-down can remove the network under it, after
  which the daemon can't start it again; and a take-down part done looked
  like a configuration the instance had outgrown.
- **Who takes an instance down**: an `up` that won the network or
  attached, and only what it won or attached to — the network and
  containers by their ids, trying for up to ten rounds until none of them
  is left — so a newer instance another `up` brings up under the same
  labels meanwhile is left alone; an `up` replacing an instance (the table
  above), and `services down`, likewise take down what they found. A container another
  `up` or `services down` is already removing counts as removed, and is
  waited for until it has gone; the network, which the daemon refuses to
  remove while a container on it is running or paused, is tried again. An `up` that ends before winning or attaching takes nothing
  down — but removes the network its own create made, when a signal ended
  it with that create still under way, since it can't have won it.
- **After a lost create**: an `up` whose network create loses
  creates nothing. The winner creates every container before it starts
  any, so the loser, finding a container only created, waits rather than
  attaching to one half set up; if the winner fails, it takes the instance
  down and the loser, finding the network gone, starts over (the table
  above). #103's acceptance criterion that an `up` "adopts or replaces
  stopped members" is met by replacing an instance with no member up; one
  with a member up is attached to, its exited members left as they are.
- **An instance won piecemeal**: can't happen, since only the `up` that won
  the network creates any container. A container's name stays the guard
  against anything else: a name held by something without the instance's
  labels stops `up` with the daemon's error, and is not attached to or removed.
- **The name's format**: `<project>..<group>..<instance>..<container>`, and
  `<project>..<group>..<instance>` for the network. Each part is an
  image-name component, which never holds `..` and never starts or ends
  with `.`, so the parts can be read back unambiguously; a group's name
  gets the rule at load that a container's has.
- **The exit options' names**: `exit_condition` (`"any_stopped"`,
  `"all_stopped"`) for when `up` takes the group down on its own, and
  `exit_code_from` (`"first_non_zero"`, `"first_to_exit"`,
  `{ container = "<member>" }`) for where its exit code comes from. A
  field named `exit_code` reads as setting the code, and no tool surveyed
  has one whose value isn't a code; `exit_code_from` takes its name from
  Compose's `--exit-code-from`, as `stop_signal` and `stop_grace_period`
  are Compose's field names. Compose's flag takes a service only, and
  implies ending the group on the first exit; `exit_code_from` adds the two
  rules and implies nothing about `exit_condition`
  ([`research/service-group-exit-option-names.md`](../research/service-group-exit-option-names.md)).
  Compose's `--abort-on-container-failure`, ending the group on the first
  non-zero exit only, would be a third `exit_condition` value
  ([#901](https://github.com/or1can/ratect/issues/901)).
- **A member whose configuration or image has changed** is not detected,
  and a container the configuration has gained is not added: a running
  instance is attached to as it is, so one generation is never replaced by
  another under an `up` attached to it. Taking the instance down is how a
  change is applied.
- **An exit condition when a member had already exited**: an attaching `up`
  neither starts nor counts it, so a condition counts only exits that `up`
  sees. A rule nominating that member takes the exit code it left, which
  the daemon still has.
- **A foreground `up` killed outright** prints nothing, and nothing is
  printed at the start to warn of it. The next `up` says the group was
  already up as it attaches, and `services list` shows it.
- **How a task reaches an instance's network**: the network's name is fixed
  (above), so `ratect run --use-network <project>..<group>..default` names
  it, and the group's containers answer there to their configured names.
  Probed by hand on Docker 29 in #103; no test runs it.

## Alternatives considered

**Detached `services up` as a Ratect process put in the background.** The
manager would keep a group's exit condition working while detached. But
`services down` would have to find the manager, which means a process ID on
disk where #124 keeps no local state, or stop the containers and let it
notice; and the manager can die (a reboot, a logout, the end of a CI step)
leaving a running instance with no owner, so the ownerless path is needed
regardless. It is a second mechanism that removes nothing of the first. It
can still be added on top of this model if a detached exit condition is wanted.

**A foreground `services up` as a task run, with detached as something
else.** Two code paths for one thing; a second `up` would give an instance
two owners; and a killed foreground instance looks the same as a detached
one.

**A lock held by a foreground `services up`**, so that `services list` could
say whether anything is attached and the next `up` could tell a detached
instance from one whose `up` was killed. Both behave the same afterwards, so
it would change a display column and no behaviour.

**Repairing an instance an `up` finds partly up**, starting its stopped
members again, dependencies first, once nothing was only created or being
removed. It was #103's first settlement. Each `up` decided from a list of
states that another `up` could change before it acted, and successive
reviews found the races that left: a container stopped between the list and
the attach, a take-down in progress read as an outgrown configuration, an
exit recorded before the `up` attached meeting its exit condition at once. Docker
Compose's `up` repairs the same way and accepts those races (the research
above). Replacing what has no member running, and waiting out what is being
changed, leaves no decision that rests on a stopped container staying as it
was listed.

**A file lock to serialise a concurrent `up`.** It works on one machine and
not through a shared daemon. A container's name guards a create in both;
how an `up` copes with what that leaves unserialised is Decision 6.

**Have `resources` select an instance whose members have all exited.** It
would clear an abandoned instance without a second command, and it would
race an `up` replacing that instance, two clients removing one instance
for different reasons.

**Identify an instance by a `run` label.** `up` attaches to an instance, so an
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
  way prints nothing. The next `up` says it found the instance up
  (Decision 6).
- A termination signal in an attached foreground `up` stops an instance
  somebody else started detached. The CLI documentation has to say it.
- A detached instance has no group exit condition and no exit code: a member
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
  one machine: five pairs of concurrent `docker create`. #103 added a test
  of two concurrent `services up`, run on that daemon and by CI's
  `docker-integration` job on its Linux runner. What neither varied: the
  daemon's version below 25 (none has run, so nothing has shown that a
  network list on Docker 20.10 to 24 includes one created a moment before,
  which Decision 3's rule relies on), its class (no rootless or
  userns-remap daemon), and its transport (no TCP, TLS or SSH daemon).
- An `up` waits with no limit while the instance has a container only
  created or being removed, or a network and no container, so one abandoned
  half brought up — by an `up` killed outright meanwhile — holds the next
  until it is ended and the instance is taken down.
- An attached `up` that ends takes the instance down even when another has
  already begun to, finishing what is left. Docker Compose's foreground
  `up`, by contrast, removes nothing when someone else stops its project
  (the research above): an `up` here can't tell a member's own exit from
  another's take-down.
- Changing a member's configuration doesn't change a running instance:
  `services down` first.
