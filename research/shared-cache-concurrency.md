# Shared on-disk cache concurrency: how comparable tools do it

Research for [ratect#733](https://github.com/or1can/ratect/issues/733) (the Git
include cache's lock file) and [ratect#741](https://github.com/or1can/ratect/issues/741)
(`CacheStore::remove` re-listing shared storage after authorizing against an
earlier listing). It records facts and precedents only. It does not choose a
design for ratect: that is for the design round #733 asks for.

Every claim cites the source that owns it, pinned to a tag or commit:
cargo `7e1923c8` (master, 2026-10-30), git `v2.56.0`, Go `go1.27.3`, Gradle
`v9.8.2`, Nix `2.35.4`, cacache `v21.0.1`, pnpm `v12.16.1`, ccache `v4.14.2`,
Batect `67c94224` (0.85.0 plus 3 commits). Where this document reasons from a
source rather than quoting it, it says so ("inference"). Anything that could
not be checked is in [Not verified](#not-verified).

## Summary

1. **Nobody uses age alone to break a lock held by a live process.** Each tool
   that breaks locks by age does one of three things. ccache keeps the lock
   alive with a 500 ms heartbeat against a 2 s limit. git's `gc.pid` adds a
   12-hour limit to a PID and hostname check. pnpm falls back to age (24 h)
   only where the filesystem can't hold an OS lock. ccache's own source says
   breaking a stale lock is an "inherent race" in which two processes can both
   believe they hold it.
2. **Tools whose lock must survive a crash use OS locks and never delete the
   lock file.** Cargo, Go, Gradle and pnpm all do this. The OS releases the
   lock when the holder dies, however it dies, so a dead holder is never
   mistaken for a slow one. The one tool that does delete a locked file, Nix,
   needs a lock-then-verify protocol to make that safe: it writes a token into
   the file after unlinking it, and every locker checks the file's size after
   locking.
3. **git's existence locks have no stale detection, by design.** git removes
   its own lock files from `atexit` and signal handlers. A lock left behind by
   SIGKILL or a crash stays until a person deletes it. git 2.56's opt-in
   `core.lockfilePid` only improves the error message: it never removes
   anything.
4. **A lock that outlives its process needs its identity to move with it.**
   git's `daemonize()` reassigns lock ownership to the forked child. With
   flock, a lock is shared by every inherited descriptor and is only released
   when the last one closes, so a child that inherits the fd keeps the lock
   alive. Go and Cargo only treat an interrupted writer's leftovers as safe to
   delete because that writer was the lock holder itself. ratect's `git clone`
   is a separate child that doesn't hold the lock. Batect's is too, but Batect
   clones into a fresh `Files.createTempDirectory` each time rather than a
   fixed `<key>.tmp`.
5. **Mixed versions are handled by keeping what old versions do, not by
   changing it.** Cargo added new lock modes while keeping the old
   `DownloadExclusive` lock, and documents that old cargos in the build phase
   don't coordinate with GC. It accepts that, because the worst case is an
   error and a retry. Go added `.partial` markers that versions before 1.14.2
   ignore, and points users at `go mod verify`/`go clean -modcache`. Cargo's
   `.cargo-ok` history spans five versions of meaning, each read
   compatibly. Gradle freezes a cross-version lock protocol
   (`Version1LockStateSerializer`) for caches that several versions share.
6. **GC re-checks liveness at the moment of deletion.** Nix re-checks its
   temp roots for each path just before deleting it, because roots can arrive
   after its scan. pnpm deletes a store file only if its hard-link count is 1
   when it is about to remove it. Cargo's GC instead excludes every concurrent
   cargo that takes the lock (`MutateExclusive`). No tool found here deletes
   from a fresh listing without checking it against the authorization it made
   from an older one, which is the shape of #741.
7. **On network filesystems, the locking tools mostly give up locking and say
   so.** Cargo skips locking entirely on NFS, because flock can block forever
   there even with `LOCK_NB`, and on any filesystem that reports locking as
   unsupported. Gradle's docs say concurrent access needs the processes to be
   able to talk to each other, which "is usually *not the case* for
   containerized builds". pnpm keeps its locks in `$XDG_RUNTIME_DIR` or `/tmp`,
   which are host-local, so they only coordinate processes that agree on that
   directory. ccache allows NFS but says it "hasn't been tested very
   thoroughly". `O_EXCL` itself is only atomic on NFSv3 or later.

## 1. Locking primitive

| Tool | Primitive | Lock file deleted on release? |
| --- | --- | --- |
| Batect (Git includes) | `Files.createFile` existence lock, polled every 100 ms for 5 min | Yes |
| git (`index.lock` etc.) | `O_CREAT\|O_EXCL` existence lock, then `rename` to commit | Yes (renamed or removed) |
| Cargo (package cache) | OS lock (`File::lock`: `flock`, `LockFileEx`; `fcntl` on Solaris) | No |
| Go (module cache, `GOCACHE` trim) | OS lock (`flock`; `LockFileEx` on Windows; `fcntl` on AIX/Solaris) | No |
| Gradle (`~/.gradle/caches`) | Java `FileChannel` byte-range locks on two regions of one file, plus UDP contention handoff | No |
| Nix (GC, temp roots, path locks) | `flock` | GC lock: no. Path locks: optionally, with lock-then-verify |
| pnpm (store prune vs install) | OS lock (`File::lock`, `flock` on Android) in a per-user lock directory | No |
| ccache (cache entries) | `symlink` (or `O_EXCL` file on Cygwin) holding `host-pid-thread-time`, plus an `.alive` heartbeat file; Windows: `CreateFile` with no sharing and `FILE_FLAG_DELETE_ON_CLOSE` | Yes |
| npm cacache | No lock: content-addressed writes to a temp file, then a non-overwriting move | n/a |

**Batect.** `LockingRepositoryCloner.clone` creates `<destination>.lock` with
`Files.createFile`, retries every 100 ms until a 5-minute `cloneTimeout`,
re-checks `Files.exists(destination)` under the lock, and deletes the lock in a
`finally`
([LockingRepositoryCloner.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/libs/git-client/src/main/kotlin/batect/git/LockingRepositoryCloner.kt#L29-L71)).
`GitRepositoryCache.cloneRepoIfMissing` only calls it when the working copy is
missing
([GitRepositoryCache.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/config/includes/GitRepositoryCache.kt#L60-L66)).
ratect's `GitIncludeCache::acquire_lock` copies this: `create_new`, a 100 ms
poll, and a 5-minute `lock_timeout`
([git_include.rs](../ratect-core/src/git_include.rs)).

**git.** `lockfile.h` gives the rationale: create `<filename>.lock` with
`O_CREAT|O_EXCL` "so that we can notice and fail if somebody else has already
locked the file", write the new content into it, then rename it into place.
Readers don't block, and see either the old or the new content
([lockfile.h](https://github.com/git/git/blob/v2.56.0/lockfile.h#L4-L31)).
So git's lock *is* the new content, and the lock and the atomic update are the
same file. `lock_file_timeout` retries with quadratic, jittered backoff
([lockfile.c](https://github.com/git/git/blob/v2.56.0/lockfile.c#L202-L252)).

**Cargo.** `cache_lock.rs` defines three modes over two files,
`.package-cache` and `.package-cache-mutate`. `DownloadExclusive` covers
downloads and resolution. `Shared` is held by every build. `MutateExclusive`,
for GC, takes "mutate" and then "download"
([cache_lock.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/cache_lock.rs#L25-L67),
[file names](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/cache_lock.rs#L459-L462)).
They were introduced by [cargo#12706](https://github.com/rust-lang/cargo/pull/12706)
("Add new package cache lock modes", merged 2023-10-09) in preparation for GC
([cargo#12634](https://github.com/rust-lang/cargo/pull/12634)). `flock.rs`
says the locks are advisory `flock` on Unix, process-scoped `fcntl` on Solaris,
and mandatory `LockFileEx` on Windows
([flock.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/flock.rs#L160-L176)).
`FileLock::drop` unlocks and never removes the file
([flock.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/flock.rs#L134-L142)).

**Go.** `lockedfile.Mutex` "provides mutual exclusion within and across
processes by locking a well-known file"
([mutex.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/lockedfile/mutex.go#L13-L22)).
`modfetch.lockVersion` takes one per module version at
`<GOMODCACHE>/cache/download/<module>/@v/<version>.lock`
([cache.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/modfetch/cache.go#L151-L160)).
The `flock` implementation covers darwin, the BSDs, illumos and linux
([filelock_unix.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/lockedfile/internal/filelock/filelock_unix.go#L5)).
AIX and Solaris get an `fcntl` emulation that keeps its own per-inode
bookkeeping, because `fcntl` locks "attach to an (inode, process) pair rather
than a file descriptor"
([filelock_fcntl.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/lockedfile/internal/filelock/filelock_fcntl.go#L5-L13)).
On any other platform every call returns `errors.ErrUnsupported`
([filelock_other.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/lockedfile/internal/filelock/filelock_other.go)).
A maintainer confirmed that Windows uses `LockFileEx`
([golang/go#36568](https://github.com/golang/go/issues/36568)).

**Gradle.** `DefaultFileLock.lock` locks a "state region" of the lock file for
the whole operation. An exclusive holder then briefly locks an "information
region" and writes its port, lock id, PID and operation name into it, "so other
processes can read our details and ping us"
([DefaultFileLockManager.java](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/DefaultFileLockManager.java#L374-L437)).
`DefaultFileLockContentionHandler` is the reason for that write. A holder keeps
its lock open while nobody wants it, "because each lock open/close action
requires File I/O which is expensive". A requester that fails to lock sends
the holder a UDP message, and the holder releases the lock and tells it to
retry
([DefaultFileLockContentionHandler.java](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/locklistener/DefaultFileLockContentionHandler.java#L40-L76)).
So the ping is a hand-off between live processes, not a liveness check.

**Nix.** `lockFile` is plain `flock` with `LOCK_SH`/`LOCK_EX`
([unix/pathlocks.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/unix/pathlocks.cc#L37-L68)).

**pnpm.** `StoreOperationLock` is "a cross-process lock that keeps destructive
store maintenance from overlapping an operation that may still consume store
files". Installs take it shared (`lock_for_use`) and `store prune` takes it
exclusive (`lock_for_prune`). Both lock a global `all-stores.lock` and a
per-store file
([store_lock.rs](https://github.com/pnpm/pnpm/blob/v12.16.1/pnpm/crates/store-dir/src/store_lock.rs#L34-L77)),
through Rust's `File::lock` ([file_lock.rs](https://github.com/pnpm/pnpm/blob/v12.16.1/pnpm/crates/fs/src/file_lock.rs#L1-L6)).
This is the same shape as Cargo's `Shared`/`MutateExclusive` split.

**ccache.** `LockFile::do_acquire` creates `<path>.lock` as a symbolic link
whose target is `hostname-pid-threadid-sec.nsec`, or as an exclusive regular
file on Cygwin/MSYS2
([lockfile.cpp](https://github.com/ccache/ccache/blob/v4.14.2/src/ccache/util/lockfile.cpp#L226-L325)).
On Windows it opens the lock with no sharing and `FILE_FLAG_DELETE_ON_CLOSE`,
so the OS removes the file when the handle closes
([lockfile.cpp](https://github.com/ccache/ccache/blob/v4.14.2/src/ccache/util/lockfile.cpp#L389-L432)).

**npm cacache.** The README promises "Lockless, high-concurrency cache access"
and "Fault tolerance (immune to corruption, partial writes, process races,
etc)"
([README.md](https://github.com/npm/cacache/blob/v21.0.1/README.md#L86-L95)).
Content goes to a temp file opened with `wx` and is then moved to its
content-addressed path with `overwrite: false`
([write.js](https://github.com/npm/cacache/blob/v21.0.1/lib/content/write.js#L30-L45),
[moveToDestination](https://github.com/npm/cacache/blob/v21.0.1/lib/content/write.js#L165-L180)).
Inference: this needs no lock because two writers of one key write identical
bytes. A Git include's `(remote, ref)` key has no such guarantee, since a ref
can move between two clones.

**Rust's `File::lock`** (stable since 1.89.0) "currently corresponds to the
`flock` function on Unix … and the `LockFileEx` function on Windows". The lock
"may be advisory or mandatory", and is released "when this file (along with
any other file descriptors/handles duplicated or inherited from it) is closed"
([std docs](https://doc.rust-lang.org/std/fs/struct.File.html#method.lock)).

## 2. Dead holder versus slow holder

Three families appear. In the first, the OS releases the lock and no judgment
is needed. In the second, a person decides. In the third, the tool guesses
from age, a PID or a heartbeat.

**The OS releases it (Cargo, Go, Gradle, Nix, pnpm).** An OS lock is released
when the holder's descriptors close, so a crashed holder's lock is free at
once and a slow holder's never is. Gradle adds a "dirty" bit to the state
region, so the next owner can tell whether the last one unlocked cleanly
(`getUnlockedCleanly` returns `!lockState.isDirty()`,
[DefaultFileLockManager.java](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/DefaultFileLockManager.java#L235-L239)).
Nix uses OS release *as* its liveness probe. GC tries a non-blocking write lock
on each process's `temproots/<pid>` file: "This can only
succeed if the owning process has died", and it then removes the file
([gc.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/gc.cc#L192-L199)).
pnpm does the same for its private installs. It tells "a copy still in use
from one left behind by the OS lock the handle holds on the `in-use` file …
the OS releases that lock when the holding process ends, however it ends"
([private_install.rs](https://github.com/pnpm/pnpm/blob/v12.16.1/pnpm/crates/store-dir/src/private_install.rs#L1-L17)).

**A person decides (git index.lock, Batect, ratect today).** git's `tempfile`
module registers `atexit` and signal handlers that remove a process's own lock
files ([lockfile.h](https://github.com/git/git/blob/v2.56.0/lockfile.h#L17-L25)).
Each tempfile records its owner PID "to prevent a forked process from deleting
a temporary file created by its parent"
([tempfile.c](https://github.com/git/git/blob/v2.56.0/tempfile.c#L39-L42)).
Nothing removes a lock left by SIGKILL. The error says "Another git process
seems to be running in this repository, or the lock file may be stale"
([lockfile.c](https://github.com/git/git/blob/v2.56.0/lockfile.c#L254-L298)).
`core.lockfilePid` (default `false`) writes `pid <n>` to a sibling `~pid.lock`
file, and the error then reports whether that PID still runs
([core.adoc](https://github.com/git/git/blob/v2.56.0/Documentation/config/core.adoc#L351-L360),
[lockfile.c](https://github.com/git/git/blob/v2.56.0/lockfile.c#L80-L166)).
The message itself warns "(PIDs can be reused)", and nothing is ever removed
automatically. Batect has neither handler: `releaseLock` runs in a `finally`,
which a killed JVM never reaches
([LockingRepositoryCloner.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/libs/git-client/src/main/kotlin/batect/git/LockingRepositoryCloner.kt#L47-L71)).

**The tool guesses.**

- *git `gc.pid`.* `lock_repo_for_gc` takes `gc.pid.lock` as an ordinary
  existence lock, then reads the existing `gc.pid`. It defers to that holder
  only if the file's mtime is at most 12 hours old, it parses as
  `<pid> <hostname>`, and either the hostname differs ("be gentle to concurrent
  "gc" on remote hosts") or `kill(pid, 0)` succeeds or returns `EPERM`. The
  comment calls 12 hours "very generous". `--force` overrides it "after the
  user verifies that no gc is running"
  ([gc.c](https://github.com/git/git/blob/v2.56.0/builtin/gc.c#L418-L483)).
  So this is a PID check that only applies on the same host, with an age
  bound that makes a wrong answer cheap: a gc run a day late.
- *ccache.* The holder touches `<path>.alive` every 500 ms
  (`k_keep_alive_interval`,
  [longlivedlockfilemanager.cpp](https://github.com/ccache/ccache/blob/v4.14.2/src/ccache/util/longlivedlockfilemanager.cpp#L30)).
  A waiter breaks the lock once it has seen no `.alive` activity for 2 s
  (`k_staleness_limit`) *and* the lock's content still equals the content it
  first read, which guards against the lock having been re-taken in between.
  A lock that isn't made long-lived "risks being considered stale" after two
  seconds
  ([lockfile.hpp](https://github.com/ccache/ccache/blob/v4.14.2/src/ccache/util/lockfile.hpp#L30-L32),
  [lockfile.cpp](https://github.com/ccache/ccache/blob/v4.14.2/src/ccache/util/lockfile.cpp#L326-L383)).
  The source then names its own limit: "There is an inherent race condition
  here where two processes may believe they both acquired the lock after
  breaking it". It is mitigated, not removed, by sleeping a random 10–50 ms
  before retrying.
- *pnpm, as a fallback.* "Where the filesystem cannot hold such a lock, a copy
  counts as in use for `ABANDONED_AFTER` from its creation", which is 24 hours,
  "well past any command that runs one, so a copy is never removed from under
  a live process for want of an OS lock"
  ([private_install.rs](https://github.com/pnpm/pnpm/blob/v12.16.1/pnpm/crates/store-dir/src/private_install.rs#L11-L39)).

**Child processes that outlive the holder.**

- git hands the lock to the child. `daemonize()` forks. The child calls
  `reassign_tempfile_ownership(parent_pid, getpid())`, and the parent hands its
  tempfiles to the child "to prevent us from removing them upon exit"
  ([setup.c](https://github.com/git/git/blob/v2.56.0/setup.c#L2211-L2240)).
  `git gc --auto` instead releases `gc.pid` before it daemonizes and takes it
  again in the daemon, so the recorded PID is the daemon's
  ([gc.c](https://github.com/git/git/blob/v2.56.0/builtin/gc.c#L665-L705)).
  `git maintenance run` holds `objects/maintenance.lock` across its
  `daemonize()` call
  ([gc.c](https://github.com/git/git/blob/v2.56.0/builtin/gc.c#L1392-L1436)).
- flock is held by every inherited descriptor. Duplicate descriptors "created
  by, for example, fork(2) or dup(2) refer to the same lock", which is released
  only when all of them close
  ([flock(2)](https://man7.org/linux/man-pages/man2/flock.2.html)). Rust's docs
  say the same for "inherited" handles (above). Inference: an OS lock passed
  to the `git` child would last as long as the child does. git's own lock
  descriptor is the reverse: it is `O_CLOEXEC`, "so the new contents must be
  written by the current process, not a spawned one"
  ([lockfile.h](https://github.com/git/git/blob/v2.56.0/lockfile.h#L58-L60)).
- Go and Cargo only clean up leftovers whose writer was the lock holder
  itself. Go's `unzip` deletes old `.tmp-*` directories and partial extractions
  under the version lock: "This is only safe to do because the lock file
  ensures that their writers are no longer active"
  ([fetch.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/modfetch/fetch.go#L218-L234)).
  It extracts the zip in-process, so the writer *is* the lock holder.
- tokio does not kill a child when its handle is dropped: "a spawned process
  will, by default, continue to execute even after the `Child` handle has been
  dropped", unless `kill_on_drop` is set
  ([tokio `Command`](https://docs.rs/tokio/latest/tokio/process/struct.Command.html#method.kill_on_drop)).
  That is what makes ratect's orphaned clone possible.
- Batect's `GitClient.clone` clones into
  `Files.createTempDirectory(destination.parent, destination.name)`, which is a
  new randomly-suffixed directory per attempt, and registers it with
  `deleteOnExit`
  ([GitClient.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/libs/git-client/src/main/kotlin/batect/git/GitClient.kt#L28-L47)).
  ratect's `SystemGitClient` uses one fixed `<key>.tmp` and removes it first if
  it exists ([git_include.rs](../ratect-core/src/git_include.rs)). Inference:
  under Batect an orphaned `git clone` writes into a directory no later cloner
  reuses, and leaves litter rather than a mixture. Under ratect, the next
  cloner deletes and reuses the directory that orphan is still writing.

## 3. Removal and GC while the cache is in use

**Exclude every user for the duration (Cargo, pnpm, Gradle).** Cargo's GC
needs `MutateExclusive`, "to ensure no other cargo process is running". Auto-GC
uses `try_acquire` and skips if anyone holds the lock, because it "is intended
to be opportunistic"
([global_cache_tracker.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/workspace/global_cache_tracker.rs#L46-L62),
[gc.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/workspace/gc.rs#L74-L75)).
Automatic GC has been on by default since Rust 1.88.0, and the `gc`
subcommand is still unstable
([Rust 1.88.0 announcement](https://blog.rust-lang.org/2025/06/26/Rust-1.88.0/)).
pnpm's `prune_keeping` takes the store lock exclusively before it reads any
references ([prune.rs](https://github.com/pnpm/pnpm/blob/v12.16.1/pnpm/crates/store-dir/src/prune.rs#L117-L135)).
Gradle's `DefaultCacheCoordinator.cleanup` runs the cleanup action
`withOwnershipNow`, holding the cache's own file lock
([DefaultCacheCoordinator.java](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/DefaultCacheCoordinator.java#L156-L173)).

**Re-check at the point of deletion (Nix, pnpm).** Nix's GC takes the global
GC lock exclusively. It then reads the temp roots written before that point,
and anything newer reaches it through a socket that `addTempRoot` connects to
when it can't take the GC lock shared
([gc.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/gc.cc#L84-L161),
[gc.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/gc.cc#L390-L395)).
That still leaves a gap after the scan, so the delete loop re-checks each
path: "Re-check tempRoots before deleting … Between the BFS and this deletion
loop, new temproots may have been added via the GC socket by a concurrent
process"
([gc.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/gc.cc#L712-L735)).
In-progress `tmp-*` directories are deleted only if GC can lock them
exclusively ([gc.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/gc.cc#L540-L556)).
pnpm's `remove_unlinked_file` stats each content file and removes it only if
it is a regular file with hard-link count 1, treating `NotFound` as nothing to
do ([prune_cas.rs](https://github.com/pnpm/pnpm/blob/v12.16.1/pnpm/crates/store-dir/src/prune_cas.rs#L152-L167)).
The deciding fact is read from the file itself just before removal, under the
exclusive store lock.

**Grace period instead of exclusion (git prune, Go `GOCACHE`).** `git gc`
documents its race openly. Running concurrently, "there is a risk of it
deleting an object that the other process is using but hasn't created a
reference to". It mitigates this by keeping anything newer than `--prune`, and
by having writers refresh the mtime of an object that already exists. Its
docs then concede that "users who run commands concurrently have to live with
some risk of corruption (which seems to be low in practice)"
([git-gc.adoc](https://github.com/git/git/blob/v2.56.0/Documentation/git-gc.adoc#L153-L169)).
Go's build cache trims entries unused for 5 days, at most once a day.
`trim.txt` is updated under a lock before the trim starts, so only one process
trims at a time. The entries themselves are deleted without a lock, with an
extra hour of slack "to account for the imprecision of our 'last used' mtimes"
([cache.go (build cache)](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/cache/cache.go#L335-L435)).

**No coordination at all (`go clean -modcache`, cacache).** `go clean
-modcache` is `modfetch.RemoveAll(GOMODCACHE)` and takes no lock
([clean.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/clean/clean.go#L219-L240)).
`cacache.verify` is "a naive mark-and-sweep tracing garbage collector". It
reads the index, then walks content and removes anything not marked, and it
removes the whole `tmp` directory
([verify.js](https://github.com/npm/cacache/blob/v21.0.1/lib/verify.js#L84-L93),
[cleanTmp](https://github.com/npm/cacache/blob/v21.0.1/lib/verify.js#L242-L245),
[README](https://github.com/npm/cacache/blob/v21.0.1/README.md#L656-L665)).
Inference: a concurrent `put` that has written content but not yet its index
entry, or is still writing into `tmp`, can lose that content. cacache's design
tolerates this because a reader re-fetches a missing entry.

**"Delete what I listed" vs "delete what's there now".** None of the tools
read here authorizes a removal against one listing and then deletes from a
second. The patterns found are:

- authorization and deletion under one exclusive lock, so the two listings
  can't differ (Cargo, pnpm, Gradle);
- a per-item re-check that uses the same rule as the authorization (Nix's
  temp roots, pnpm's link count);
- an age cutoff evaluated at deletion time, applied to whatever is there (Go,
  git, cacache).

This is the shape of [ratect#741](https://github.com/or1can/ratect/issues/741).
`CacheStore::remove` authorizes with `CacheSelection::shared_only` against the
listing, then `clean_shared_volume_caches` calls `list_volumes` again and
deletes by name ([cache.rs](../ratect-core/src/cache.rs)).

**Does removal ever delete a lock file?**

- *Never (Cargo, Go, Gradle, pnpm's store locks).* The file persists and only
  the OS lock on it comes and goes. Gradle's `lockRepresentsStateOnFileSystem`
  still verifies, on Unix, that "the lock file hasn't been
  changed/deleted/recreated between opening a file handle and acquiring the
  lock", by re-reading the state region against its in-memory copy. Windows
  skips the check because "OS won't allow to modify the state region or delete
  the file due to lock"
  ([DefaultFileLockManager.java](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/DefaultFileLockManager.java#L135-L162),
  [DefaultFileLockManager.java](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/DefaultFileLockManager.java#L344-L367)).
- *Only while holding it, then marking it (Nix).* `deleteLockFile` unlinks the
  path and then writes a token "to indicate to other processes waiting on this
  lock that the lock is stale (deleted)". `PathLocks::lockPaths` checks
  `fstat(fd).st_size != 0` after each lock, and if so retries: "we're holding
  a lock on a deleted file. This means that other processes may create and
  acquire a lock on `lockPath`, and proceed"
  ([unix/pathlocks.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/unix/pathlocks.cc#L25-L35),
  [unix/pathlocks.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/unix/pathlocks.cc#L71-L128)).
  Temp-roots files use the same trick in reverse: GC writes `d` after
  unlinking, and the creator loops until the file it has locked is empty
  ([gc.cc](https://github.com/NixOS/nix/blob/2.35.4/src/libstore/gc.cc#L54-L82)).
  Nix calls removing a lock file "an optimisation, not a necessity".
- *Only when judged stale (ccache).* A breaker removes `.alive` and `.lock`
  after the 2 s and same-content checks, and accepts the race described
  above.
- *Never (Batect).* `GitRepositoryCache.delete` removes the working copy and
  the info file only
  ([GitRepositoryCache.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/config/includes/GitRepositoryCache.kt#L128-L136)),
  as ratect's `remove_entry` does now.

## 4. Mixed versions sharing one cache directory

- **Cargo kept the old lock and added new ones beside it.** Before 1.74, "cargo
  only acquired the `DownloadExclusive` lock". Newer cargos taking
  `MutateExclusive` "should still correctly block when an old cargo is
  downloading … but they do not properly coordinate when an old cargo is in the
  build phase (because it holds no locks)". This was accepted as "the only
  consequence is that one side or the other encounters an error and needs to
  retry"
  ([cache_lock.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/cache_lock.rs#L69-L82)).
  The GC database must stay forward- and backward-compatible: add columns and
  tables, never remove one or change what it means.
  `sync_db_with_files` reconciles with files that older cargos added without
  tracking them
  ([global_cache_tracker.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/workspace/global_cache_tracker.rs#L64-L84)).
- **Cargo's completion marker changed meaning five times, and each reading
  stays compatible.** `.cargo-ok` was created before extraction in 1.34
  (fine-grained locking), became redundant under a global lock in 1.36, was
  no longer pre-created in 1.49, was checked for symlinks in 1.64 (CVE-2022-36113),
  and became JSON `{ v: 1 }` in 1.71, with "a heavy-hammer approach that
  unpacks the `.crate` file again" on a parse failure
  ([registry mod.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/sources/registry/mod.rs#L505-L545)).
  Git checkouts use their own `.cargo-ok` "ready" marker
  ([cargo's git utils.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/sources/git/utils.rs#L358-L384)).
- **Go accepted a window where old versions misread new state.** "Go 1.14.2 and
  higher respect .partial files. Older versions may use partially extracted
  directories. 'go mod verify' can detect this, and 'go clean -modcache' can
  fix it." Go also still removes temp directories left by versions before 1.16
  ([fetch.go](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/modfetch/fetch.go#L218-L270)).
  In [golang/go#36568](https://github.com/golang/go/issues/36568), maintainer
  Bryan Mills said "extract in place" had "an awkward migration path": older
  versions "will not know to wait for" a new marker. He rejected using a file's
  *presence* to mark incompleteness, because of a check-then-create race
  between two processes.
- **Gradle separates by version and freezes the shared protocol.** "Different
  versions of Gradle will perform maintenance on only the version-specific
  caches associated with each version". Shared caches are deleted only "if no
  Gradle version still uses them". Older versions with shorter retention "may
  clean the shared caches earlier than what is configured"
  ([Gradle user home docs](https://docs.gradle.org/current/userguide/directory_layout.html)).
  Locks requested as cross-version use `Version1LockStateSerializer`, not the
  current serializer, and skip the verification above
  ([DefaultFileLockManager.java](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/DefaultFileLockManager.java#L212),
  [L346-L348](https://github.com/gradle/gradle/blob/v9.8.2/platforms/core-execution/persistent-cache/src/main/java/org/gradle/cache/internal/DefaultFileLockManager.java#L346-L348)).
- **git made its PID file additive and opt-in.** `core.lockfilePid` adds a new
  sibling file under a name refnames can't collide with. Older gits ignore it,
  and the lock itself is unchanged
  ([lockfile.c](https://github.com/git/git/blob/v2.56.0/lockfile.c#L80-L92)).

No tool found here changed an existence lock into an OS lock, or the reverse,
on a directory that older binaries keep using. Every migration above kept the
old primitive working and added a new one beside it.

## 5. Network filesystems

- **Cargo skips locking.** flock "is known to be broken on NFS". Its failure
  mode is "**blocking forever**, even if the "non-blocking" flag is passed"
  ([cargo#2615](https://github.com/rust-lang/cargo/issues/2615)), so Cargo
  skips every lock when `statfs` reports `NFS_SUPER_MAGIC` (Linux, non-musl
  only). It also treats `ENOTSUP`/`EOPNOTSUPP`/`ENOSYS` (or Windows
  `ERROR_INVALID_FUNCTION`) as granted
  ([flock.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/flock.rs#L369-L398),
  [L445-L488](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/flock.rs#L445-L488)).
  The `Filesystem` docs say this "**does not** guarantee that a lock is
  acquired … it is the user's responsibility to not run concurrent processes"
  ([flock.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/util/flock.rs#L166-L172)).
  The GC database avoids SQLite WAL "since it has shared-memory requirements
  cargo can't depend on due to things like network mounts"
  ([global_cache_tracker.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/workspace/global_cache_tracker.rs#L64-L72)).
- **Linux kernel behaviour.** Since Linux 2.6.12, NFS clients emulate flock as
  whole-file `fcntl` locks (before that, flock on NFS was local only). From
  2.6.37, the `local_lock` mount option can make them local again. SMB
  propagates flock only from Linux 5.5, where it becomes *mandatory*: "any IO
  on a locked file will always fail with EACCES when done from a separate file
  descriptor" ([flock(2)](https://man7.org/linux/man-pages/man2/flock.2.html)).
  `O_EXCL` "is supported only when using NFSv3 or later on kernel 2.6 or later".
  The page suggests a `link(2)` of a unique hostname-plus-PID file as the
  portable alternative ([open(2)](https://man7.org/linux/man-pages/man2/open.2.html)).
- **Gradle needs its processes to reach each other.** Concurrent access "is
  only supported if the different Gradle processes can communicate together",
  which "is usually *not the case* for containerized builds". Its read-only
  dependency cache "is accessed without locking" and must not be written while
  others read it
  ([dependency caching docs](https://docs.gradle.org/current/userguide/dependency_caching.html#sec:cache-locking)).
- **pnpm keeps locks host-local on purpose.** Lock files live in
  `$XDG_RUNTIME_DIR` (when it is an absolute directory the user owns) or
  `/tmp`. "Processes coordinate only while they agree on it"
  ([secure_temp_lock.rs](https://github.com/pnpm/pnpm/blob/v12.16.1/pnpm/crates/fs/src/secure_temp_lock.rs#L13-L20)).
  Inference: a store on a network share gets no cross-host exclusion.
- **ccache allows NFS, with caveats.** "It is possible to put the cache directory
  on an NFS filesystem", but "Ccache hasn't been tested very thoroughly on NFS"
  ([manual](https://ccache.dev/manual/latest.html)). Its lock code handles a
  "Lost NFS reply?" (symlink creation reporting failure although the link was
  made) and grants the lock anyway where the filesystem can't make symlinks
  ([lockfile.cpp](https://github.com/ccache/ccache/blob/v4.14.2/src/ccache/util/lockfile.cpp#L274-L325)).
- **Clock skew.** git's `gc.pid` and ccache both compare a file's mtime with
  the local clock. The mtime is set by the file server on a network mount
  ([gc.c](https://github.com/git/git/blob/v2.56.0/builtin/gc.c#L461),
  [lockfile.cpp](https://github.com/ccache/ccache/blob/v4.14.2/src/ccache/util/lockfile.cpp#L332-L342)).
  Neither source says anything about skew. Inference: ccache's 2 s window is
  much more exposed to skew than git's 12 hours. Go's `GOCACHE` trim does guard
  against one direction: a `trim.txt` timestamp more than an hour in the future
  is treated as corrupt and the trim runs anyway
  ([cache.go (build cache)](https://github.com/golang/go/blob/go1.27.3/src/cmd/go/internal/cache/cache.go#L381-L396)).
  OS locks don't read clocks at all.

## Mapping to ratect's constraints

The constraints come from #733's last comment and from #741. For each one:
which tools face it, and how they resolve it, if they do.

**Age-based staleness: a live lock looks stale after a long clone, because its
mtime is set once.** ccache faces the same problem and adds a heartbeat (an
`.alive` file touched every 500 ms). git's `gc.pid` uses a bound it calls
"very generous" (12 h) and only for a task where being wrong costs a day's
delay. pnpm uses age only where OS locks are unavailable, with a 24 h bound.
No tool uses a bare acquisition-time mtime against a short threshold.

**Age plus heartbeat: orphaned young locks, file-then-lock removal order,
check-then-act delete races, clock skew, and a first clone interrupted before
its working copy exists.**

- *Check-then-act.* ccache documents exactly this race ("two processes may
  believe they both acquired the lock after breaking it") and only reduces it
  with a random sleep. Its same-content check also keeps it from breaking a
  lock that was re-taken since its first read. Nix avoids the race by deleting
  a lock file only while holding it, then marking it, with every locker
  verifying after it acquires.
- *Clock skew.* ccache and git `gc.pid` are both exposed (see
  [section 5](#5-network-filesystems)). No tool found here handles skew
  explicitly, except Go's future-timestamp guard on `trim.txt`.
- *Interrupted first clone.* Tools with OS locks don't have this problem: the
  lock goes with the process. git's own cleanup handlers cover die and signals
  but not SIGKILL, which leaves the lock for a person to remove. Go and Cargo
  use a completion marker (`.partial`, `.cargo-ok`) rather than the lock to
  record an incomplete entry, and clean up under the next lock.

**OS advisory lock: mixed versions with released binaries that take an
existence lock on every load.** The tools that changed their locking kept the
older primitive working: Cargo's `DownloadExclusive`, Go's `.partial`, git's
`~pid.lock`. Cargo and Go each accepted a documented window of degraded safety
against old binaries. Gradle avoids the question by giving each version its
own directory, and freezing a cross-version protocol for shared caches. None
of them faced an old binary that would *hang* on the new scheme. The closest
precedent for that is Go's concern in #36568 that old versions "will not know
to wait for" a new marker.

**Old and new binaries not excluding each other; an old `release_lock`
deleting a file the new binary has locked.** Cargo's "do not properly
coordinate" is the same gap, and it was accepted because the worst outcome is
a retryable error. Gradle's `lockRepresentsStateOnFileSystem` and Nix's
`st_size` check are the two lock-then-verify designs that detect "my locked
file was deleted or recreated under me" and retry. That is the case where an
old binary deletes a file the new one has locked. Inference: neither check
stops an old binary that never takes the OS lock from writing alongside a new
one. They only stop the new binary from believing it holds a lock it doesn't.

**Orphaned `git clone` that outlives the ratect process.** git (moving
ownership with `reassign_tempfile_ownership`) and flock (inherited descriptors
share the lock) are the precedents for keeping a lock alive as long as the
process that actually writes. Go and Cargo avoid the issue because their
writer is the lock holder. Batect's unique `createTempDirectory` per attempt
means an orphan never shares a directory with a later cloner. tokio's default
of not killing a dropped child, and the fact that SIGKILL can't be caught, are
both confirmed above.

**Network filesystems (NFS, SMB, FUSE homes): flock unsupported or host-local,
while `create_new` works.** Cargo gives up locking on NFS and on "unsupported",
and says so in its docs. pnpm keeps locks deliberately host-local. Gradle
needs its processes to be able to talk to each other. ccache tolerates NFS
with symlink locks and a heartbeat. On Linux, `O_EXCL` is atomic on NFSv3 and
later, and `link(2)` is the documented fallback. No tool here claims both
cross-host exclusion and crash safety on a network filesystem. FUSE was not
addressed by any primary source found (see [Not verified](#not-verified)).

**Deleting a flocked file needs lock-then-verify.** Nix (write a token after
unlinking; check `fstat` size after locking) and Gradle (re-read the state
region on Unix) are the two worked examples. Cargo, Go and pnpm avoid the
question by never deleting the lock file.

**Should removal ever touch a lock?** Batect's `delete` never does. Cargo, Go,
Gradle and pnpm never delete lock files at all. Nix deletes them only while
holding them. ccache's stale-breaking is the only removal by a non-holder, and
its source calls the result racy.

**#741: authorizing against one listing and then deleting from a fresh one.**
No tool read here does this. The precedents are
[section 3](#3-removal-and-gc-while-the-cache-is-in-use)'s three shapes:
authorize and delete under one exclusive lock (Cargo, pnpm, Gradle), re-check
each item at deletion against the same rule (Nix's temp roots re-check, pnpm's
link count), or delete by an age rule evaluated on what is actually there (Go,
git). The "possible shape" in #741 itself, never reaching shared storage
without `--scope shared`, removes the second listing rather than reconciling
it. That is closest to the first shape. Docker volumes, the storage #741 is
about, are a daemon-side namespace that ratect cannot lock. That was not
researched (see below).

## Not verified

- **Cargo and its `git` CLI child.** Whether Cargo kills a `git fetch` child
  (`net.git-fetch-with-cli`) when it is interrupted, or what the child can do
  to the git database after Cargo's lock is released. `fetch_with_cli` builds
  the command
  ([cargo's git utils.rs](https://github.com/rust-lang/cargo/blob/7e1923c8092d0be52f818eb1a12b592a4c6024bc/src/sources/git/utils.rs#L1256-L1330)),
  but its process handling wasn't traced.
- **Go's VCS subprocesses.** Whether Go's VCS code (`cmd/go/internal/modfetch/codehost`)
  kills `git` children when the `go` command dies, and how its `vcs` directory
  locks relate to them. Not read.
- **Go and NFS.** Whether Go documents any NFS behaviour for the module cache.
  A search found nothing primary. [golang/go#36568](https://github.com/golang/go/issues/36568)
  is about Windows renames, not NFS.
- **Nix on NFS.** Whether Nix supports a store or state directory on NFS, and
  how its flock-based GC protocol behaves there. Not found.
- **Gradle cleanup in detail.** How a cleanup avoids deleting an entry another
  Gradle process is about to use, beyond holding the cache's own lock
  (`withOwnershipNow`). Also which of Gradle's caches use the cross-version
  lock. The cleanup actions and `LockOptions` callers weren't traced.
- **Gradle's UDP ping across hosts.** Only the docs' containerized-builds
  caveat was read.
- **pnpm's index-database and content-write ordering.** Whether an install can
  add a content file between prune's reference scan and its link-count check.
  Prune holds the store lock exclusively, so it shouldn't. The install side
  wasn't read to confirm it takes `lock_for_use` on every path.
- **cacache's put ordering.** The inference that a concurrent `put` can lose
  content to `verify` assumes content is written before the index entry. This
  wasn't confirmed in cacache's `put.js`.
- **FUSE home directories.** No primary source found addresses flock or
  `O_EXCL` on FUSE generally. The behaviour depends on each filesystem.
- **Rust std and `O_CLOEXEC`.** Rust's std docs say an *inherited* handle keeps
  a lock. Whether std opens files with `O_CLOEXEC` by default (which would stop
  a `Command` child inheriting one unless arranged) wasn't checked in std's
  source.
- **Bazel's disk and repository caches, and Docker's content store.** Not
  researched. Docker's volume namespace matters to #741, since ratect can't
  lock it.
- **Cargo master's directory layout.** cargo's sources moved from
  `src/cargo/...` to `src/...` at the commit cited. The module docs quoted
  match those in the paths the brief named, but the commit that moved them
  wasn't identified.
