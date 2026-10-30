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
//! as decisions/0023's Decisions 2–5 define it: an existence lock,
//! `<key>.lock` created exclusively — which is all an older binary takes, and
//! all it checks for — carrying, through the same open file, an OS lock (`flock`) and a format
//! marker. The OS lock is what lets a waiter prove the **holder** has exited:
//! the kernel releases it when the holder does, however it exits. Waiting,
//! the notice and the timeout are `GitIncludeCache::acquire_lock`'s; this
//! module only looks, once.
//!
//! Every call here is synchronous filesystem work measured in microseconds,
//! except the blocking OS lock a new holder takes on its own new file, which
//! only a waiter's look can hold up, and only for as long as a look takes.
//! The caller runs [`attempt`] on a blocking thread all the same.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
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
    /// Deletes the lock file while still holding its OS lock (Decision 5):
    /// older binaries take the file's absence to mean the lock is free, and a
    /// waiter that opened the file before it went fails [`attempt`]'s
    /// device-and-inode check and looks again.
    pub(super) fn release(self) -> std::io::Result<()> {
        let removed = std::fs::remove_file(&self.path);
        drop(self.file);
        removed
    }

    #[cfg(test)]
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
    /// this version recognises, or the OS lock itself returned an error, as
    /// it can on a network filesystem.
    HeldByUnknownHolder,
}

/// Takes the entry lock at `path` if it's free or abandoned; otherwise says
/// who holds it. Never deletes a lock it doesn't hold, and never waits on
/// one.
pub(super) fn attempt(path: &Path) -> std::io::Result<Attempt> {
    loop {
        // `std` opens every file close-on-exec, so the `git` a holder starts
        // doesn't inherit its OS lock (Decision 2) — pinned by a test.
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
pub(super) fn take_over_if_abandoned(mut file: File, path: &Path) -> TakeOver {
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return TakeOver::Running,
        Err(TryLockError::Error(_)) => return TakeOver::Unknown,
    }
    if !still_at(&file, path) {
        return TakeOver::Replaced;
    }
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() || content.lines().next() != Some(MARKER) {
        return TakeOver::Unknown;
    }
    let marked = file
        .seek(SeekFrom::Start(0))
        .and_then(|_| writeln!(file, "{MARKER}"));
    if let Err(e) = marked {
        // Still the holder: the OS lock is what counts, and a waiter that
        // finds the marker damaged waits rather than taking over.
        tracing::debug!("Failed to re-mark Git include lock file {path:?}: {e}");
    }
    TakeOver::Taken(EntryLock {
        file,
        path: path.to_path_buf(),
    })
}

/// Whether `path` still names the file `file` was opened from, by device and
/// inode.
fn still_at(file: &File, path: &Path) -> bool {
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(opened), Ok(current)) => opened.dev() == current.dev() && opened.ino() == current.ino(),
        _ => false,
    }
}
