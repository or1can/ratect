// Copyright 2026 Orican Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! One attempt at a Git include's **entry lock** (CONTEXT.md's **Caches**),
//! as decisions/0023's Decisions 2–4 and decisions/0027's release define
//! it: an existence lock, `<key>.lock` created exclusively — which is all an
//! older binary takes, and all it checks for — carrying, through the same open file, an OS lock (`flock`) and a format
//! marker. The OS lock is what lets a waiter prove the **holder** has exited:
//! the kernel releases it when the holder does, however it exits. Off Unix
//! that proof is never complete (see [`still_at`]), so nothing is taken over
//! there. Waiting, the notice and the timeout are
//! `GitIncludeCache::acquire_lock`'s; this module only looks, once.
//!
//! Every call here is synchronous filesystem work measured in microseconds,
//! except the blocking OS lock a new holder takes on its own new file, which
//! only a waiter's look can hold up, and only for as long as a look takes.
//! The caller runs [`attempt`] on a blocking thread all the same.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The format marker: the first line of a lock whose holder took its OS lock
/// first. A lock carrying anything else — an older binary's empty file, a
/// newer format this version doesn't know, or a new holder that hasn't
/// written it yet — can't be proved abandoned (Decision 3). It is an on-disk
/// format every supported version shares, so it changes only in a way all of
/// them can still read (0023's Consequences).
pub(super) const MARKER: &str = "ratect-entry-lock 1";

/// An entry lock this process holds. Dropping it without
/// [`release`](Self::release) leaves the file behind, as dying would — with
/// its OS lock released, so the next waiter takes it over.
#[derive(Debug)]
pub(super) struct EntryLock {
    file: File,
    path: PathBuf,
}

impl EntryLock {
    /// Deletes the lock file while still holding its OS lock (decisions/0027's
    /// Decision 3, restating 0023's Decision 5): older binaries take the
    /// file's absence to mean the lock is free, and a waiter that opened the
    /// file before it went fails [`attempt`]'s device-and-inode check and
    /// looks again. Not once a comparison like that one shows the path names
    /// another file, or none: where the OS lock doesn't reach every process,
    /// another holder of the same file may have released it already, and a
    /// third taken a new lock at that path, which isn't this one's to
    /// delete. Off Unix this version takes nothing over, so the file is
    /// taken to be this one's.
    ///
    /// Then [unlocks](unlock) the OS lock, deleted or not, before dropping
    /// the file.
    pub(super) fn release(self) -> std::io::Result<()> {
        let removed = if replaced(&self.file, &self.path) {
            Ok(())
        } else {
            std::fs::remove_file(&self.path)
        };
        unlock(&self.file, &self.path);
        drop(self.file);
        removed
    }

    #[cfg(all(test, unix))]
    pub(super) fn file(&self) -> &File {
        &self.file
    }
}

/// What one look at an entry lock found.
#[derive(Debug)]
pub(super) enum Attempt {
    /// This process is now the holder.
    Acquired(EntryLock),
    /// Its holder is running: the OS lock is held.
    HeldByRunningHolder,
    /// Its holder can't be proved to have exited: the lock carries no marker
    /// this version recognises, the OS lock itself returned an error, as
    /// it can on a network filesystem, or, off Unix, the file's identity
    /// can't be checked.
    HeldByUnknownHolder,
}

/// Takes the entry lock at `path` if it's free or abandoned; otherwise says
/// who holds it. Never deletes a lock it doesn't hold, and never waits on
/// one.
pub(super) fn attempt(path: &Path) -> std::io::Result<Attempt> {
    loop {
        // `std` opens every file close-on-exec, so the `git` a holder starts
        // doesn't keep its OS lock (Decision 2) — pinned by a test. Until a
        // process the holder's process is starting execs, it shares the file,
        // and with it the OS lock: a holder that drops its lock without
        // releasing it then keeps the lock that moment longer, and a waiter
        // waits (ratect#886). A released lock doesn't, nor does one a look
        // took and won't keep: each is unlocked for every process sharing
        // the file (ratect#888).
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => return Ok(Attempt::Acquired(created(file, path))),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            // Released between the two opens: try to create it again.
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            // One this process can't open for writing can't be taken over,
            // but may still be released by its holder, so it's waited on.
            Err(_) => return Ok(Attempt::HeldByUnknownHolder),
        };
        match take_over_if_abandoned(file, path) {
            TakeOver::Taken(lock) => return Ok(Attempt::Acquired(lock)),
            TakeOver::Running => return Ok(Attempt::HeldByRunningHolder),
            TakeOver::Unknown => return Ok(Attempt::HeldByUnknownHolder),
            TakeOver::Replaced => continue,
        }
    }
}

/// A lock file this process has just created: take its OS lock, then write
/// the marker, in that order, so that a marker is only ever seen beside an OS
/// lock that is held while its holder lives. Blocking is safe here: nothing
/// but a waiter's look can hold the OS lock on a file with no marker yet. A
/// filesystem whose OS lock errors leaves the existence lock alone in force,
/// unmarked, so no waiter will ever take it over.
fn created(mut file: File, path: &Path) -> EntryLock {
    if file.lock().is_ok() {
        if let Err(e) = writeln!(file, "{MARKER}") {
            // Unmarked, it is still an existence lock, just not recoverable.
            tracing::debug!("Failed to mark Git include lock file {path:?}: {e}");
        }
    }
    EntryLock {
        file,
        path: path.to_path_buf(),
    }
}

/// [`take_over_if_abandoned`]'s verdict.
#[derive(Debug)]
pub(super) enum TakeOver {
    Taken(EntryLock),
    Running,
    Unknown,
    /// `path` no longer refers to the file that was opened: its holder
    /// released it, and someone may have created a new one since. Look again.
    Replaced,
}

/// Decisions 3 and 4: `file`, opened from `path`, is abandoned if this process
/// can take its OS lock and it carries [`MARKER`]; it is then taken over in
/// place — its own marker written — provided `path` still refers to it. Only
/// one process can hold the OS lock, so only one can take over. The marker is
/// written over the old one, never after truncating the file: a taker killed
/// in between would leave an empty lock nobody can recover.
///
/// An OS lock taken here but not kept is [unlocked](unlock) before the file
/// is dropped.
pub(super) fn take_over_if_abandoned(mut file: File, path: &Path) -> TakeOver {
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return TakeOver::Running,
        Err(TryLockError::Error(_)) => return TakeOver::Unknown,
    }
    if let Err(verdict) = claim_if_abandoned(&mut file, path) {
        unlock(&file, path);
        return verdict;
    }
    TakeOver::Taken(EntryLock {
        file,
        path: path.to_path_buf(),
    })
}

/// The rest of [`take_over_if_abandoned`], with `file`'s OS lock held:
/// `Ok` once the lock is this process's, marked as such unless the write
/// failed, or the verdict to give up with, which is never `Taken`.
fn claim_if_abandoned(file: &mut File, path: &Path) -> Result<(), TakeOver> {
    match still_at(file, path) {
        Some(true) => {}
        Some(false) => return Err(TakeOver::Replaced),
        None => return Err(TakeOver::Unknown),
    }
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() || content.lines().next() != Some(MARKER) {
        return Err(TakeOver::Unknown);
    }
    let marked = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| writeln!(file, "{MARKER}"));
    if let Err(e) = marked {
        // Still the holder: the OS lock is what counts, and a waiter that
        // finds the marker damaged waits rather than taking over.
        tracing::debug!("Failed to re-mark Git include lock file {path:?}: {e}");
    }
    Ok(())
}

/// Frees any OS lock this process holds on `file` before it is dropped, as
/// [`EntryLock::release`] and a look that doesn't take the lock over both
/// do. A process that another thread of this one is starting shares the
/// file until it execs, and the drop alone would leave the OS lock held for
/// that moment by a process that has finished with it, so a waiter would
/// report the lock's holder running; an unlock frees it for every process
/// sharing the file (ratect#888). No run starts a process while it holds or
/// looks at an entry lock, other than a holder's own `git`, which it waits
/// for before its release; the test binary does (ratect#886).
///
/// That is `flock`'s rule. Where Linux emulates `flock` with `fcntl` locks,
/// as on NFS, a lock belongs to the process, which no child shares, and an
/// unlock frees it as the close would have. An unlock that fails is left to
/// the drop, which frees the lock once any such process has exec'd.
fn unlock(file: &File, path: &Path) {
    if let Err(e) = file.unlock() {
        tracing::debug!("Failed to unlock Git include lock file {path:?}: {e}");
    }
}

/// Whether `path` is known to name no file, or one other than `file`: only
/// then is the lock file not [`EntryLock::release`]'s to delete. Anything
/// else that stops it looking is left to the delete to report.
#[cfg(unix)]
fn replaced(file: &File, path: &Path) -> bool {
    match std::fs::metadata(path) {
        Err(e) => e.kind() == ErrorKind::NotFound,
        Ok(current) => file
            .metadata()
            .is_ok_and(|opened| opened.dev() != current.dev() || opened.ino() != current.ino()),
    }
}

/// See the Unix form: this version takes nothing over here, so the file is
/// taken to be the releasing holder's own.
#[cfg(not(unix))]
fn replaced(_file: &File, _path: &Path) -> bool {
    false
}

/// Whether `path` still names the file `file` was opened from, by device and
/// inode, or `None` where that can't be told.
#[cfg(unix)]
fn still_at(file: &File, path: &Path) -> Option<bool> {
    Some(match (file.metadata(), std::fs::metadata(path)) {
        (Ok(opened), Ok(current)) => opened.dev() == current.dev() && opened.ino() == current.ino(),
        _ => false,
    })
}

/// See the Unix form. `None`: `std` has no stable way to compare file
/// identities here, so no lock is ever proved abandoned, and a waiter treats
/// it as one with no marker: it waits until the holder releases it, or until
/// it times out if the holder died holding it (ratect#868).
#[cfg(not(unix))]
fn still_at(_file: &File, _path: &Path) -> Option<bool> {
    None
}
