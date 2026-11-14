# 0025 — A task run holds a lock, and `resources` removes what the lock proves dead

**Status:** Accepted — planned (ratect-compat 0.35.0 · ratect 0.14.0):
[#265](https://github.com/or1can/ratect/issues/265). Both binaries take the
lock; only `ratect` has the `resources` verbs that read it. Refined while
implementing #265: the points this record left open are settled in
[Settled while implementing](#settled-while-implementing), which replaces
the list that stood there.

## Context

[0002](0002-runtime-ownership-labels.md) labels everything Ratect creates and
says what the labels can't settle: a task run still going carries the same
labels as a leftover, and the daemon can't say whether some Ratect process
still cares about a container. `ratect resources clean` therefore selected
every labelled container and network, with `--older-than` as the only guard.

That guard assumes a live task run is young. A task run as a stdio server
lives as long as the session that started it, so the live ones are the oldest
([#265](https://github.com/or1can/ratect/issues/265)), and a supervisor that
restarts such a session with `SIGKILL` gives Ratect no chance to clean up.
Reproduced on 2026-11-12 with a task running `cat` and one `redis`
dependency: eight seconds after `kill -9` of the Ratect process, the task
container had exited (its stdin closed), the dependency was still running,
and the network remained. Nothing stops that dependency afterwards, and the
only command that removed it also removed every live task run of the project.
That was one probe: `ratect-compat` with `-o simple`, stdin piped with no
terminal, OrbStack on macOS. A task container exits on the kill only because
its stdin closes, so one holding no stdin keeps running.

The first answer tried was container state, as `docker container prune` uses
it. Three rules were drafted and each failed against the code:

- **The task container is not running.** A task container is created once
  its image and network are ready and waits in Docker's `created` state until its
  dependencies are ready ([0019](0019-readiness-gates-starting-only.md)), so
  this describes a task run that is starting.
- **Nothing in the task run is running.** This describes one that is still
  pulling or building.
- **The task container has exited.** Cleanup removes the task container
  before its dependencies, so a kill during cleanup leaves running
  dependencies and no task container, which this never selects. And the exit
  code is read only after the output stream drains, so a live task run with a
  slow consumer (`ratect run build | less`) can have an exited task
  container, which this does select.

Container state says what the daemon knows. The question is about a process
the daemon has never heard of.

[#875](https://github.com/or1can/ratect/issues/875) then tried proving the
process dead: each task run holding an OS file lock, which the kernel
releases when the process holding it dies, however it dies. Its results comment is the
evidence for this record. In short, against OrbStack on macOS: the lock read
alive while the process lived and dead after `SIGKILL` in each case tried — a
kill mid-task as the control, and two that container state got wrong, a kill
during cleanup and one while a dependency's setup command ran. It read alive
for the third such case, a task run whose task container had exited while
its output drained. It found one way the lock is wrong: through a bind mount
across a VM boundary, a container took a lock the macOS host was holding.
The file was visible; the lock was not.

[0023](0023-git-include-entry-lock.md) already relies on the same kernel
behaviour for the Git include cache, and
[`research/shared-cache-concurrency.md`](../research/shared-cache-concurrency.md)
records which other tools do. How comparable tools decide what is safe to
remove is in
[`research/leftover-selection-and-instance-identity.md`](../research/leftover-selection-and-instance-identity.md):
none read there asks whether a supervising process is alive before removing
a long-lived group, and the one scheme found for removing what a dead process
left, Testcontainers' Ryuk, holds a connection from a container that has the
Docker socket.

## Decision

### 1. A task run holds a lock for as long as its process drives it

A task run takes an exclusive OS file lock, without waiting, on a file named
for its `run` label's value, in a runs directory under `~/.ratect`. It takes
it before creating anything on the daemon and holds it until the task run
ends, cleanup included. The kernel releases it if the process dies first,
however it dies.

**One lock per task run, not per invocation.** `resources` then needs no
mapping from a run label to a lock: the file's name is the label's value. A
lock per invocation would need a second label on every container.

A task run that cannot take its lock — no home directory, a read-only one, a
filesystem that refuses the lock, a platform that isn't Unix — still runs,
and says so at debug level only. The lock serves a later `resources clean`;
it is no reason to refuse a task.

### 2. A free lock is proof only to a reader that would have seen a held one

The lock file carries a format line and its writer's kernel boot identity:
Linux's `boot_id`, macOS's `kern.bootsessionuuid`. A reader trusts a free
lock only when the file's boot identity is its own. That is what makes the
bind-mount case in Context read unknown instead of dead: the same file, seen
from another kernel.

### 3. `resources` reads three states, and removes one by default

| State | How it is reached | Selected |
| --- | --- | --- |
| **Dead** | the lock is free and the file's boot identity is the reader's | by default |
| **Alive** | the lock is held | never |
| **Unknown** | no file, another boot identity, no recognised format line, an error from the lock, or a platform that isn't Unix | only with `--all` |

A reader probes with a shared lock, so that readers do not refuse each other
and a second `resources` or `doctor` at the same moment cannot read a dead
task run as alive.

An alive task run is not selected with any flag: to remove it, end its
process, and it then reads dead.

Unknown is 0002's position, unchanged: Ratect does not know, and says so.
`--all` selects unknown task runs, and `--older-than` is the guard there that
it was everywhere before.

### 4. An absent lock file never means dead

- A task run that cleaned up fully deletes its file. One that leaves anything
  on the daemon leaves its file.
- `resources clean` deletes a dead task run's file after removing that task
  run's resources. It does not delete one because its task run is absent
  from this daemon: the file may belong to a leftover on another.

So an absent file has many causes — another machine or user, a binary from
before this record, a task run that could take no lock, a file deleted by
hand — and each reads unknown.

### 5. A task run is selected, or skipped, whole

`resources list` and `resources clean` decide per task run: the containers
and network sharing a `run` label value go together, and `--older-than`
measures the task run's age, which is its oldest resource's. A network can
only be removed once its containers are.

What carries Ratect's labels and no `run` label is not a task run: it is not
selected, and the notice does not count it. That is a service instance
([0026](0026-service-instances.md)), which `resources` leaves alone.

`resources list` stays `clean`'s dry run: it shows what `clean` would remove
and nothing else. When a selection leaves task runs out, both verbs say so in
a notice ([0017](0017-output-streams-and-styles.md)), counting the alive and
the unknown separately and naming `--all` for the unknown.

### 6. `ratect doctor` warns about what `clean` would fix

`doctor`'s leftover finding warns about dead task runs, and adds a line
counting unknown ones and pointing at `resources list --all`. It reports
nothing for alive ones. It reads the same selection `resources list` does.

### 7. Ratect never sweeps on its own

Nothing removes a dead task run except `resources clean`. A sweep at the
start of the next run would delete what `--no-cleanup` kept for inspection.

### Settled while implementing

[`decisions/README.md`](README.md) has a planned record take refinements
found while building it. #265 settled each point this record left open:

- **Decision 2 holds where it was doubted.** On OrbStack (Docker 29.8.2,
  2026-11-14), a container holding an exclusive `flock` on a file in a
  directory bind-mounted from macOS, through the VM's file share, was seen
  holding it by a second container mounting the same directory: the shared
  probe was refused. Both reported one boot identity, so a free lock between
  them is proof. The macOS host saw the same lock free, and its boot identity
  differs, so it reads unknown, as Decision 2 intends. A named volume
  behaved like the bind mount. Not varied: both containers mounted the
  directory at the same path from the same host path, so a share reached
  through two different mounts or paths was not run; nor was Docker
  Desktop.
- **A native Linux daemon** (Docker 29.1.3 in an OrbStack Ubuntu machine,
  which shares a kernel with its containers): a process on the machine and
  a container bind-mounting the machine's own directory each saw the
  other's lock held, and a `docker kill` freed the container's. But the
  machine and its containers reported different `boot_id`s, so each reads
  the other's free lock unknown, not dead: safe, at the cost of `--all`
  between them. On a Linux host that shares its `boot_id` with its
  containers, a free lock between them is proof, since the kernel and the
  inode are the same. This probe was not run on a Linux host outside
  OrbStack.
- **A slow consumer leaves an exited task container only while its unread
  output fits the buffers between them.** On that native daemon, with
  `docker run`'s output piped to a process that never read it, a container
  writing 800 KB had exited when looked at 10 seconds on, and one writing
  1.5 MB was still running then. A Ratect task run writing 4 MB to a reader
  that never read still had its task container running 60 seconds on there,
  as the real-daemon test first found on a GitHub-hosted runner. On
  OrbStack's macOS-side daemon (Docker 29.8.2) the same 4 MB task container
  had exited. Past the limit a live task run's task container is still
  running, which container state reads correctly, and the lock reads alive
  either way. So the real-daemon test holds its live task run with `SIGSTOP`
  rather than a stalled pipe, and an engine test with a fake daemon reads
  the lock held as cleanup removes the network, the last thing it removes,
  after the task container's run has returned. None of this record's tests
  drains output into a reader that has stalled, so that the lock is held
  while `docker.rs` drains it rests on the lock being held across the task
  container's whole run. Not varied: where Ratect's own limit lies, not
  found (200 KB and 500 KB had exited on both daemons, 4 MB only on
  OrbStack's); kernel socket buffer settings and page sizes; Docker versions
  besides 29.1.3 and 29.8.2, the GitHub-hosted runner's not recorded; output
  styles other than `-o simple`; a terminal as the reader; and a remote
  daemon.
- **Nothing removes the lock files the two deletion rules never do.** No
  rule found is safe: age can't tell an old file of a live task run from
  one of a dead leftover, a reader on another kernel sees a held lock as
  free, and a file of a dead leftover on another daemon would turn unknown
  if deleted. So a file from before a reboot, one left by a task run whose
  leftovers went another way (`docker rm`, or `ratect-compat` alone), one
  kept by a task run that stopped waiting for a create the daemon never
  answered (which may have made something, and if it did nothing else
  could make it read dead), and a `.tmp` whose process died before its
  rename all stay, a few dozen bytes each. The CLI documentation says they
  can be deleted by hand only when nothing is running and nothing is left
  over, since deleting a leftover's file makes it unknown. `resources clean` also leaves an **unknown** task
  run's file when it removes that task run under `--all`, since it may be a
  live one's on another kernel.
- **Creation**, as #875 designed it and `ratect_core::run_lock` builds it:
  the file is created exclusively as `<run>.tmp`, locked with a
  non-blocking exclusive lock, written (`ratect-run-lock 1`, then `boot`
  and the boot identity), and renamed to `<run>.lock`. An `flock` belongs to
  the open file, which a rename leaves alone, and a reader opens only final
  names, so a file visible under its final name is always locked or its
  holder is gone. A `run` value made of anything but a lower-case UUID's
  characters (`0`–`9`, `a`–`f` and `-`) names no file and reads unknown,
  since a label can be set by anyone with the daemon, and an upper-case one
  would name another task run's file on a case-insensitive filesystem. The directory is **`~/.ratect/runs`**. A run lacking a
  boot identity (a Unix other than Linux or macOS) takes no lock.
- **Releasing** follows #888's order for the Git include entry lock: delete
  the file while the lock is held, then unlock explicitly, then close. A
  reader's probe unlocks explicitly too. A child process that another thread
  is starting shares the file until it execs, and only an explicit unlock
  frees the lock for it.
- **What a task run that kept something is.** `--no-cleanup*` and a cleanup
  that failed both leave the file, so the task run reads dead once its
  process ends. So does a run that panics: the panic ends the process, which
  frees the lock, and the file stays.
- **What the notice and `doctor` call the states**: *ended* (dead), *still
  going* (alive) and *can't tell has ended* (unknown). The notice reads
  `Left out 2 task runs still going, and 1 task run Ratect can't tell has
  ended (pass --all to include it).`, and is counted only among task runs
  old enough for `--older-than`, since a task run it excludes for its age
  was asked not to be considered at all. `doctor` counts task runs, where it
  counted resources.

## Alternatives considered

**Container state.** Context gives the three rules tried and how each failed.

**A process ID and host in a label.** A process ID is reused, means nothing
to a remote or shared daemon, and means nothing when Ratect itself runs in a
container.

**A reaper holding a connection**, as Ryuk does, removing a task run when its
process disconnects. It needs a container with the Docker socket, privileged
in some environments and disabled where that is forbidden, and it removes on
a timer after a disconnect rather than when asked.

**Select unknown task runs whose containers have all stopped, by default.**
It would make an upgrade painless, since every leftover from an older binary
is unknown. But such a task run may be another machine's live one draining
its output, which is the case that sank "the task container has exited".

**Trust a free lock without a boot identity.** Ratect in a container sharing
the host's `~/.ratect` would then read every live host task run as dead.

**A machine identity instead of a boot identity**, which would keep a dead
task run reading dead across a reboot. The one image checked (Alpine) has no
`/etc/machine-id`, and unknown is the safe answer.

**Keep `resources clean` selecting everything, and add an opt-in flag for the
safe selection.** Not a breaking change, but the command in the documentation
stays the one that can tear down a live task run.

**Sweep dead task runs automatically.** Decision 7.

**Delete a lock file whose task run is no longer on the daemon.** With more
than one daemon, a `clean` against one would delete the file of a leftover on
another, which would then read unknown.

**Probe with the exclusive lock a task run takes.** Two readers at once
would refuse each other, and the second would read a dead task run as alive.

## Consequences

- **Breaking for `ratect`:** `resources list` and `resources clean` select
  less by default, and `--older-than` measures a task run's age where it
  measured each resource's. No `ratect.toml` changes meaning, so there is no
  written-for row.
- **On upgrade, every leftover from an older binary is unknown** and needs
  `--all`, whatever its containers' states.
- **After a reboot, every earlier leftover is unknown**, though each is
  certainly dead.
- Both binaries write under `~/.ratect` on every task run, where
  `ratect-compat` wrote there only for a Git include.
- `resources clean --all` composes with `--older-than`, where
  `includes clean --all` conflicts with it, and sits beside `--all-projects`,
  which widens the scope and not the states. The CLI documentation has to
  keep the three apart.
- On a platform other than Linux and macOS every task run is unknown, since
  none has a boot identity Ratect reads, so a bare `clean` removes nothing
  there ([#876](https://github.com/or1can/ratect/issues/876)).
- #875 did not vary: a native Linux daemon, where a container and the host
  share a kernel; Docker Desktop; a remote daemon, a second machine, another
  user's home, a home directory on a network filesystem; Windows; and the
  `ratect` binary, which shares the engine `ratect-compat` was run through.
  The file's creation, removal and pruning were not built there. #265 then
  ran the first, in an OrbStack machine, and both binaries, against
  OrbStack; the rest remain unrun.
- Lock files nothing deletes accumulate in `~/.ratect/runs`; see
  [Settled while implementing](#settled-while-implementing).
- #875 also found that 0023's Decision 3 overstates what a bind mount
  preserves ([#877](https://github.com/or1can/ratect/issues/877)).
