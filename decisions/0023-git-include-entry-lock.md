# 0023 — A Git include's entry lock proves its holder dead, and correctness doesn't depend on it

**Status:** Accepted — planned
([#733](https://github.com/or1can/ratect/issues/733), ratect-compat 0.33.0 ·
ratect 0.12.0).

## Context

A Git include is cloned once into `~/.ratect/incl/<key>/` and reused. Both
binaries share that cache, and so does every version of either one installed
on the machine. Cloning an entry is guarded by an **entry lock**
(CONTEXT.md's **Caches**), `<key>.lock`: an existence lock, taken by creating
the file exclusively, polled every 100 ms, given up after `lock_timeout`
(5 minutes) and released by deleting the file. That is Batect's
`LockingRepositoryCloner`.

Two fixes in this release changed who meets that lock.
[#389](https://github.com/or1can/ratect/issues/389) made a load take it only
when the entry's working copy is missing, as Batect does, so a lock left
beside a complete clone no longer blocks every load.
[#631](https://github.com/or1can/ratect/issues/631) made removal (`ratect
includes clean`, `refresh`, and the 30-day sweep) stop taking it, so a
leftover lock no longer stalls removal for 5 minutes. Together they produce
[#733](https://github.com/or1can/ratect/issues/733): removal now removes an
entry beside a leftover lock, and the next load to need it waits 5 minutes and
fails. Nothing ever removes the lock except a person. A first clone stopped by
Ctrl+C has always left one the same way.

The obvious fix, telling a lock whose holder has gone from one whose holder is
slow, was designed three times during #631 and failed review each time.
#733's last comment has the detail. In short:

- **Age-based.** A lock's mtime is set when it's taken, so a clone slower than
  the threshold holds a lock that already looks abandoned.
- **Age plus a heartbeat.** Deleting a lock that looks stale is check-then-act:
  two removers that read the same mtime can delete a live cloner's new lock.
  It compares a file server's clock with the local one. And it does nothing
  for a lock left before any working copy exists.
- **An OS advisory lock on a file never deleted.** Released binaries
  (ratect-compat 0.32.0, ratect 0.11.0) take the existence lock on every load,
  so a lock file that never goes away makes each of their loads wait and fail,
  for good. Old and new binaries would not exclude each other either.

One more fact sat under all three. `SystemGitClient::clone_repo` clones into a
fixed sibling `<key>.tmp`, wiping it first. A `git` that outlived the ratect
process that started it, which nothing prevents (`git` isn't killed with its
parent, and SIGKILL can't be caught), would then share a directory with the
next cloner, and a mixture of the two could be renamed into place and cached
for good. So every design had to prove the holder *and its `git`* had gone,
which is what made proof so hard. Batect doesn't have this problem: its
`GitClient` clones into a fresh `Files.createTempDirectory` each time.

The constraints, agreed while designing this:

- **Mixed versions** may cost no more than an older binary on its own already
  does: an occasional 5-minute wait, never a permanent hang, and never two
  binaries writing one entry.
- **Filesystems.** It must be fully correct on a local filesystem. On a
  network home (NFS, SMB, FUSE) it must never corrupt or hang permanently, but
  may lose automatic recovery.
- **Unix only**, matching the release targets.
- **A death ratect can't observe is not proof.** A home directory
  bind-mounted into a container, or seen from another PID namespace, shares the
  cache without sharing the process table.
- **Batect parity** ([0013](0013-when-ratect-compat-copies-batect.md)) binds
  outcomes, not the lock format. Batect uses its own `~/.batect/incl`, so no
  Batect process meets these files. Batect's own outcome for a lock whose
  holder has gone, waiting and then failing, is the kind of bug rule 3 says
  not to copy.
- **A wait must say what it is waiting for.** No timeout value is better than
  another. What matters is that the user isn't left watching ratect hang.

How comparable tools coordinate a shared cache was researched against their
sources, in
[`research/shared-cache-concurrency.md`](../research/shared-cache-concurrency.md):
Cargo, git, Go, npm and pnpm, Gradle, Nix and ccache.

## Decision

**Correctness comes from how a clone is staged, so the entry lock only
prevents duplicate work. The lock is still an existence lock, so older
binaries stay excluded, and it also carries an OS lock, so a holder's death
can be proved.**

1. **Every clone attempt stages into a fresh temporary directory.** It's
   named `<key>.tmp-<unique>` and renamed into place atomically when the clone
   and checkout have finished. A clone that loses the rename because another
   finished first discards its copy. So two clones of one entry, at once,
   can't produce a corrupt or mixed working copy, and a clone that was
   interrupted is never renamed into place, whatever happens to the lock. This
   returns to Batect's behaviour.
2. **A new holder takes the existence lock exactly as before,** creating
   `<key>.lock` exclusively, so every older binary still waits on it. Through
   the same open file it then takes an exclusive OS advisory lock (`flock`)
   and writes a format marker into the file: a line naming the format and its
   version. The file is opened close-on-exec, so the `git` it starts doesn't
   inherit the OS lock. Under Decision 1, a `git` that outlives its holder
   writes only into a directory nobody will rename, so the **holder** is the
   ratect process alone.
3. **A lock is abandoned when it carries the marker and its OS lock is free.**
   A waiter that finds `<key>.lock` already there tries a non-blocking OS lock
   on it. The kernel releases a holder's OS lock when the holder exits, even
   on SIGKILL. It does so for every process on the machine, whatever PID
   namespace or bind mount it sees the file through, which satisfies "a death
   ratect can't observe is not proof". A lock with no marker belongs to an
   older binary, or to a new holder that hasn't written its marker yet. Its
   holder can't be proved dead, so it is never recovered. Nor is any lock
   where the OS lock itself returns an error, as it can on a network
   filesystem.
4. **An abandoned lock is taken over in place, never by deleting it.** The
   waiter whose non-blocking OS lock succeeded checks that `<key>.lock` still
   refers to the file it opened, by device and inode. If it does, the waiter
   writes its own marker and is the holder. If not, it starts again. Nix and
   Gradle make the same check after locking, by other means. The file exists throughout, so older
   binaries stay excluded, and only one waiter can hold the OS lock, so two
   can't both take over. The check-then-act delete that sank the heartbeat
   design never happens.
5. **Release still deletes the file,** while still holding the OS lock, because
   older binaries take the file's absence to mean the lock is free. A waiter
   that had opened the file before it was deleted fails Decision 4's check and
   starts again.
6. **Removal never touches the lock.** `clean`, `refresh` and the sweep remove
   the working copy and the sidecar, as #631 left them. #733 is fixed by
   Decisions 3 and 4: the next load to meet a lock whose new-format holder
   has gone takes it over and clones.
7. **The holder removes the entry's leftover temporary directories,** every
   `<key>.tmp-*` but its own, before it clones. Under the lock, any other is
   left by an interrupted clone. Nothing outside the lock deletes one, and
   no rule here is ever based on a file's age.
8. **Every wait on an entry lock says so when it starts.** The first attempt
   that finds the lock held prints a notice on stderr, in every output style,
   as [0017](0017-output-streams-and-styles.md) requires of notices. It names
   the Git include as written (its remote and ref), says whether the holder is
   running or ratect can't tell (Decision 3), and when the wait will give up.
   A wait that ends because the lock was released prints nothing more.
9. **A wait that can't recover still gives up after `lock_timeout`, still 5
   minutes.** The failure says what it found. If the holder was running, it
   says to wait or retry. If the holder couldn't be proved dead, it names the
   lock file and says it is safe to delete once no ratect process is cloning
   that include.

## Alternatives considered

- **Age-based staleness, or age plus a heartbeat.** Rejected for the reasons
  in Context. The research found no tool that breaks a lock on a short
  threshold measured from when the lock was taken. ccache, the tool closest to
  the heartbeat design, describes the race where two processes break the same
  lock as inherent. Both also compare a file server's clock with the local
  one.
- **An OS lock alone, on a file never deleted** (Cargo, Go, Gradle and pnpm
  all do this). It's the right model with no older binaries to support. Here
  it makes every load by a released binary wait and fail, and the two
  binaries wouldn't exclude each other. Decisions 2 and 5 keep the existence
  lock so that the OS lock can be added beside it.
- **A process ID and hostname in the lock** (git's `gc.pid`). A process ID
  means nothing to a process in another PID namespace, so a container that
  sees no such process would break a live holder on the host. The kernel's OS
  lock gives the same proof without that gap.
- **Removal deletes the lock** when the working copy it removes was complete
  (#733's option 1). It's a lock removal doesn't hold, deleted on a guess
  about its holder. The research found no tool doing this except ccache, whose
  source calls it racy. Batect's `GitRepositoryCache.delete` doesn't.
- **Only warn about a leftover lock** (#733's option 3). Decision 9 keeps that
  as the floor for locks that can't be recovered, rather than the whole fix.
- **Count a holder's `git` as part of the holder,** by passing the OS lock to
  it. Needed only while clones shared `<key>.tmp`. Decision 1 removes the
  shared directory, so a surviving `git` can do no harm, and waiting for it
  would only delay recovery.

## Consequences

- **The marker is an on-disk format that several versions share.** A later
  version may change it only in a way every version still supported can read,
  and a version that finds a marker it doesn't recognise treats the lock as
  one it can't prove abandoned.
- **Release keeps deleting the lock file for as long as any supported binary
  takes the existence lock.** Once none does, release could keep the file and
  drop the existence half. That change needs its own record.
- **Some locks are never recovered.** An empty lock left by an older binary
  beside a removed entry still costs a 5-minute wait and a failure, now one
  that says what to do. This gap shrinks as older binaries go out of use.
- **An OS lock that works per host only** (some NFS mounts and some FUSE
  filesystems, or a home directory shared between macOS and a Docker Desktop
  VM) can let a waiter on another kernel take over a live holder's lock.
  Under Decision 1 that costs a duplicate clone, never a corrupt entry. At
  worst the new holder's Decision 7 removes the live clone's temporary
  directory, and that clone fails with an error.
- **`ratect-compat` matches Batect in one more way, and differs in one.**
  Staging into a fresh directory per attempt is parity. Recovering an
  abandoned lock and printing a notice while it waits are divergences, and get
  a row in [`docs/differences-from-batect.md`](../docs/differences-from-batect.md).
- **The wait notice is printed while the config is still loading,** before
  either binary has created its event sink, so the load needs its own way to
  get a notice onto stderr.
- **Close-on-exec is pinned by a test,** not assumed. The research couldn't
  confirm Rust std's default.
- **A clone's own progress is not covered here.** A first clone of a large
  include is still silent until it finishes. That has its own issue, since it
  has nothing to do with concurrency.
