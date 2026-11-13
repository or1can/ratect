# 0027 — No holder touches another clone's staging directory, so two holders of an entry lock cost only a duplicate clone

**Status:** Accepted — planned (ratect-compat 0.35.0 · ratect 0.14.0):
[#880](https://github.com/or1can/ratect/issues/880). Supersedes
[0023](0023-git-include-entry-lock.md)'s Decisions 5 and 7; the rest of 0023
stands.

## Context

[0023](0023-git-include-entry-lock.md) makes a Git include's clone correct by
staging it, and uses the entry lock only to prevent duplicate work. Its
Consequences bound what two holders of one lock can cost: "a duplicate clone,
never a corrupt entry", at worst a live clone failing with an error. Two
holders were thought rare, but
[#877](https://github.com/or1can/ratect/issues/877) found an OS lock that
reaches only one kernel: a macOS host and an OrbStack container sharing
`~/.ratect/incl` each took an exclusive `flock` the other held. A waiter there
sees a live holder's lock as abandoned and takes it over.

Two places in 0023 quietly depended on the lock being exclusive, and with two
holders each can exceed that bound:

- **Decision 7** has the holder delete every other `<key>.tmp-*` in place,
  since under an exclusive lock only an interrupted clone can have left one.
  With two holders, one may be a live clone. Deleting it in place
  (`remove_dir_all`) can leave part of the tree behind for that clone to
  rename into place as `<key>`, if its `git` keeps writing into directories
  the delete has not reached, or the delete fails partway. And `remove_dir_all`
  works through an open handle on the directory, so if the live clone's rename
  into `<key>` lands while the delete is under way, the delete goes on emptying
  `<key>` itself. Either way an incomplete entry is in place, and every later
  load accepts it, since a load checks only that `<key>` exists.
- **Decision 5** has release delete `<key>.lock` by path. With two holders of
  one file, the first release deletes it, a third process creates and takes a
  new one, and the second release deletes that one too. So the duplicates can
  chain past two.

Decision 7's problem is that a holder can't tell a staging directory an
interrupted clone left from one a live clone is still writing, when the
writer may be on another kernel. No marker in the directory would tell them
apart there any better than the entry lock's own OS lock does, and recording
a boot identity was declined (#877's note on 0023).

## Decision

**The staging thesis is kept and finished: a clone writes only to its own
staging directory, so it stays correct with any number of holders, and the
lock still only prevents duplicate work.**

1. **A holder never removes a staging directory it didn't make**
   (replacing 0023's Decision 7). It clones into a fresh `<key>.tmp-<unique>`
   and renames it into place, and deletes that one only if its own clone
   fails or loses the rename. A holder leaves every other `<key>.tmp-*`
   alone.
2. **The 30-day sweep, and `ratect includes clean`, remove a staging
   directory last changed more than 30 days ago,** of any entry, whether or
   not that entry is still cached. It uses the same `STALE_AFTER` as the
   sweep's removal of unused entries, and the directory's own modification
   time. No clone runs that long, so such a directory was left by one that
   was interrupted. `includes clean` applies those 30 days whatever age it is
   given, `--all` included, since a younger directory may be a clone in
   progress; `ratect` runs no sweep of its own, so this is how its users
   reclaim the space. `includes refresh` leaves staging directories alone.
3. **Release deletes the lock file unless it is known to be another file**
   (replacing 0023's Decision 5). It makes the device-and-inode comparison
   Decision 4 makes on a takeover, still while holding the OS lock. When the
   path names another file, or none, it leaves it alone and only drops its
   handle. When it can't look, it tries the delete as before, so whatever
   stopped it is reported. Off Unix, where that comparison can't be made,
   this version takes nothing over (0023's Decision 3), so release deletes
   by path as before.

With two holders, each clones into its own staging directory; the first
rename into `<key>` wins, and the other discards its copy and uses the
winner's, as 0023's Decision 1 already has it.

## Alternatives considered

- **Move another holder's staging directory aside before deleting it,** so
  the two renames decide the race. Tried first, under #880. `git clone` names
  its destination by absolute path and creates missing directories as it
  writes, so it can recreate a moved-aside directory at its old path; that
  needed a check of the directory's identity before the rename, and three
  review passes kept finding cases the check got wrong: a lone holder blamed
  for another's interference when an older `git` removed the directory on a
  failed clone, a Windows holder sharing the cache with a Linux container
  left unchecked, and two retrying processes clearing each other's clones in
  turn. Each was a consequence of deleting work whose owner can't be told
  alive, which Decision 1 stops doing.
- **Make the lock exclusive across kernels by recording the holder's boot
  identity in it,** as [0025](0025-task-run-lock.md) plans for task runs.
  Declined in #877's note on 0023, for the reasons given there: released
  binaries would rewrite the marker over it, an older holder's lock carries
  none, and after a reboot every lock left before it would bring back
  #733's 5-minute wait. And it would leave correctness depending on the lock,
  which 0023 set out not to do.
- **Have `includes clean --all` and `refresh` remove every staging
  directory.** One racing a live clone would delete it in place, the hazard
  0023's Decision 7 already had, which its "Nothing outside the lock deletes
  one" kept removal clear of. Waiting 30 days leaves only the cases
  Consequences accepts: a clock wrong by that much, or a clone suspended
  that long.
- **Delete the lock file through its handle rather than its path.** Unix has
  no call that unlinks an open file by descriptor.

## Consequences

- **The worst case of two holders is a duplicate clone,** when both run this
  version or later on Unix, never a corrupt entry and never a failed load,
  outside the age rule's cases below. A holder
  on a binary that takes locks over but predates this record (ratect-compat
  0.33.0 and 0.34.x, ratect 0.12.0 and 0.13.x) still deletes another
  holder's staging directory in place, so with one of those as a holder the
  bound doesn't hold. It goes as those binaries go out of use, as 0023's
  older-binary gaps do.
- **An interrupted clone's staging directory takes disk space for at least
  30 days,** rather than until the next load of that entry: under
  `ratect-compat`, until its next run after that, and under `ratect`, which
  runs no sweep, until `ratect includes clean` or a `ratect-compat` run. A
  removal that stops partway may have changed its modification time, and it
  then waits another 30 days. That matters only for large repositories interrupted often.
- **The sweep's age rule is the first on a staging directory.** 0023 rejected
  age for locks because a slow clone could outlive a short threshold; 30
  days is longer than any clone runs. A directory's modification time
  changes when an entry directly inside it is created, removed or renamed,
  so a clone's moves at least when `git clone` creates `.git` and when `git
  checkout` writes the top-level files, and never more than a clone's length
  before now. It is set by the clock of whichever kernel wrote it and read
  against the sweeper's, so the rule also assumes those clocks agree to
  within 30 days; one that jumps forward by more during a clone, as one
  without a battery-backed clock can at boot, would see a live clone as
  abandoned and delete it in place. So would a clone whose process is
  suspended for 30 days, by job control or a paused container, and then
  resumes.
- **Release's check and its delete are two steps.** If another holder of the
  same file releases it, and a third process creates a new lock, between the
  check and the delete, the third's lock is still deleted. That window is the
  length of one `stat`; before, it was the whole of the other holder's clone.
  The first release of a file two holders share still deletes it, so a third
  process may start another duplicate clone.
- **Off Unix the release fix does nothing.** No release targets Windows,
  but a Windows build sharing the cache with a Linux container would have
  its lock taken over by the container, whose `flock` can't see Windows'
  lock, and would still release by path.
- **What was run, and what wasn't.** Unit tests on a local APFS volume with
  a fake `git`, with modification times set by the test; `config`'s tests
  run real-`git` clones, one holder at a time. Not run: two holders on two
  kernels (a macOS host and an OrbStack or Docker Desktop container sharing
  `~/.ratect/incl`), a real interrupted clone's leftover being swept, clocks
  that disagree, Linux filesystems, and a network filesystem, where a
  directory's modification time and rename atomicity between clients may
  differ.
