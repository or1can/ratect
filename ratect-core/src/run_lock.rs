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

//! The **task run lock** (CONTEXT.md's **Task runs**): an OS file lock each
//! task run holds for as long as its process drives it, so that `ratect
//! resources` can tell a task run whose process is gone from one still going
//! — see [decisions/0025](https://github.com/or1can/ratect/blob/main/decisions/0025-task-run-lock.md).
//! Taking it is the engine's ([`RunLocks::take`], from `run_task_internal`);
//! reading it is [`crate::resources`]' ([`RunLocks::state`]).
//!
//! **The file.** `~/.ratect/runs/<run>.lock`, `<run>` being the task run's
//! `run` label value ([`crate::labels::RUN`]), so a reader needs no mapping
//! from a label to a lock. It holds two lines: [`FORMAT`], then `boot `
//! and its writer's kernel boot identity ([`boot_identity`]). It is created
//! as `<run>.tmp`, locked, written, and only then renamed to its final name,
//! so a file visible under that name is always locked, or its holder is gone:
//! an `flock` belongs to the open file, which a rename doesn't touch. A
//! reader never opens a `.tmp` file.
//!
//! **What a reader concludes** ([`RunState`]), by a *shared* lock taken
//! without waiting, so that readers never refuse each other: held is
//! **alive**; free, with this format and this kernel's boot identity, is
//! **dead**; anything else is **unknown** — no file, another kernel's boot
//! identity (Ratect in a container sharing `~/.ratect` across a VM boundary,
//! where #875 saw a container take a lock the host held), another format, an
//! error from the lock itself (a network filesystem), or a platform with no
//! boot identity: any but Linux and macOS. An absent file never reads dead.
//!
//! **Who deletes the file.** Its own task run, when it left nothing on the
//! daemon ([`RunLock::finish`]), and `resources clean`, after removing every
//! resource of a task run it read dead ([`RunLocks::forget`]) — never an
//! unknown one's, which may be another kernel's live task run's, and never
//! because a task run is
//! absent from one daemon, since the file may be a leftover's on another.
//! Nothing else does: a file from before a reboot, one left by a run whose
//! leftovers someone removed by hand or with `ratect-compat` alone, one kept
//! because a create the run stopped waiting for may have made something,
//! and a `.tmp` whose process died before the rename all stay, a few dozen
//! bytes each. Pruning them by age was rejected (decisions/0025): a reader on
//! another kernel sees a held lock as free, and an old file may be a dead
//! leftover's on another daemon, which deleting would turn unknown.
//!
//! **Unlock, then drop.** A process that another thread is starting shares
//! every open file of this one until its `exec`, `flock`s included, so a lock
//! freed only by dropping its file can stay held a moment longer (#886,
//! #888). [`RunLock`]'s `Drop` and a reader's probe both unlock explicitly,
//! which frees the lock for every process sharing the file. A test that
//! expects a lock to read free once its holder *closes* it, rather than
//! unlocking it, keeps that holder in a process of its own — see
//! `run_lock_tests.rs`.
//!
//! A task run that can take no lock — no home directory, a read-only one, a
//! filesystem that refuses the lock, no boot identity — still runs, and says
//! so at debug level only: the lock serves a later `resources clean`, and is
//! no reason to refuse a task.

use std::fs::File;
#[cfg(unix)]
use std::fs::{OpenOptions, TryLockError};
use std::io::ErrorKind;
#[cfg(unix)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The first line of every lock file. A file carrying anything else — a newer
/// format, or a file that isn't one of these — reads unknown, so this changes
/// only in a way every supported version can still read.
pub const FORMAT: &str = "ratect-run-lock 1";

/// What a reader can tell about a task run's process from its lock file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunState {
    /// Its lock is free, and was written by this kernel: the process is gone.
    Dead,
    /// Its lock is held: the process is still driving it.
    Alive,
    /// Ratect can't tell — see the module's notes for every way here.
    Unknown,
}

/// Where task run lock files live, or nowhere — a host user with no home, or a
/// test with no use for them. The default is
/// nowhere: [`take`](Self::take) takes nothing and [`state`](Self::state)
/// reads every task run unknown.
#[derive(Debug, Clone, Default)]
pub struct RunLocks {
    directory: Option<PathBuf>,
}

impl RunLocks {
    /// `~/.ratect/runs`, as both binaries use. A host user with no home, or
    /// a failed lookup, gets [nowhere](Self::default), said at debug level.
    pub fn at_home() -> Self {
        match crate::user::home() {
            Ok(Some(home)) => Self::at(home.join(".ratect").join("runs")),
            Ok(None) => {
                tracing::debug!("No home directory, so task runs take no lock");
                Self::default()
            }
            Err(e) => {
                tracing::debug!("No home directory, so task runs take no lock: {e:#}");
                Self::default()
            }
        }
    }

    /// Lock files in `directory`, created on first use.
    pub fn at(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
        }
    }

    /// Takes task run `run`'s lock, or `None` — said at debug level — where
    /// it can't be taken. Synchronous filesystem work measured in
    /// microseconds, and never waits on a lock.
    pub fn take(&self, run: &str) -> Option<RunLock> {
        let directory = self.directory.as_deref()?;
        match take(directory, run) {
            Ok(lock) => Some(lock),
            Err(e) => {
                tracing::debug!("Task run {run} takes no lock in {directory:?}: {e}");
                None
            }
        }
    }

    /// What task run `run`'s lock file says about its process.
    pub fn state(&self, run: &str) -> RunState {
        match self.directory.as_deref() {
            Some(directory) => probe(directory, run),
            None => RunState::Unknown,
        }
    }

    /// Deletes task run `run`'s lock file, as `resources clean` does once it
    /// has removed that task run's resources. One already gone is no failure.
    pub fn forget(&self, run: &str) -> std::io::Result<()> {
        let Some(path) = self.path(run, LOCK) else {
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// The directory lock files live in, if any.
    pub fn directory(&self) -> Option<&Path> {
        self.directory.as_deref()
    }

    fn path(&self, run: &str, extension: &str) -> Option<PathBuf> {
        lock_path(self.directory.as_deref()?, run, extension)
    }
}

/// The extension of a lock file under its final name, and of one being
/// created.
const LOCK: &str = "lock";
#[cfg(unix)]
const CREATING: &str = "tmp";

/// `<directory>/<run>.<extension>`, for a `run` that is safe as a file name.
/// A `run` label comes from the daemon, where anything could have set it, so
/// only the characters Ratect's own ids are made of — a lower-case UUID's —
/// name a file; any other reads unknown and is never deleted. Upper case is
/// refused too: on a case-insensitive filesystem (macOS's default) it would
/// name another task run's file.
fn lock_path(directory: &Path, run: &str, extension: &str) -> Option<PathBuf> {
    let safe = !run.is_empty()
        && run.len() <= 64
        && run
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f' | '-'));
    safe.then(|| directory.join(format!("{run}.{extension}")))
}

/// A task run lock this process holds. Dropping it leaves the file and
/// unlocks it, so the task run then reads dead; [`finish`](Self::finish)
/// deletes the file first, for a task run that left nothing behind.
#[derive(Debug)]
pub struct RunLock {
    file: File,
    path: PathBuf,
}

impl RunLock {
    /// Ends the task run's hold: deletes the file, while still holding its
    /// lock, when `left_nothing`, then unlocks it as the drop does. A file
    /// that can't be deleted is left, said at debug level: it reads dead,
    /// and `resources clean` finds nothing of that task run to remove.
    pub fn finish(self, left_nothing: bool) {
        if left_nothing {
            if let Err(e) = std::fs::remove_file(&self.path) {
                tracing::debug!("Failed to delete task run lock {:?}: {e}", self.path);
            }
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RunLock {
    /// Unlocks before the file closes — see the module's notes. An unlock
    /// that fails is left to the close.
    fn drop(&mut self) {
        unlock(&self.file, &self.path);
    }
}

#[cfg(unix)]
fn take(directory: &Path, run: &str) -> std::io::Result<RunLock> {
    let unusable = |what: &str| std::io::Error::new(ErrorKind::Unsupported, what.to_string());
    let boot = boot_identity().ok_or_else(|| unusable("no boot identity"))?;
    let (Some(creating), Some(path)) = (
        lock_path(directory, run, CREATING),
        lock_path(directory, run, LOCK),
    ) else {
        return Err(unusable("not a run id that names a file"));
    };
    std::fs::create_dir_all(directory)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&creating)?;
    let made = (|| {
        // Nothing else opens a `.tmp`, so this lock is free; `try_lock`
        // rather than `lock` all the same, so nothing here can wait.
        file.try_lock().map_err(|e| match e {
            TryLockError::Error(e) => e,
            TryLockError::WouldBlock => unusable("lock already held"),
        })?;
        write!(file, "{FORMAT}\nboot {boot}\n")?;
        std::fs::rename(&creating, &path)
    })();
    match made {
        Ok(()) => Ok(RunLock { file, path }),
        Err(e) => {
            unlock(&file, &creating);
            drop(file);
            let _ = std::fs::remove_file(&creating);
            Err(e)
        }
    }
}

/// See the Unix form: off Unix, no task run takes a lock (#876).
#[cfg(not(unix))]
fn take(_directory: &Path, _run: &str) -> std::io::Result<RunLock> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "only taken on Unix",
    ))
}

#[cfg(unix)]
fn probe(directory: &Path, run: &str) -> RunState {
    let Some(path) = lock_path(directory, run, LOCK) else {
        return RunState::Unknown;
    };
    let Some(boot) = boot_identity() else {
        return RunState::Unknown;
    };
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                tracing::debug!("Failed to open task run lock {path:?}: {e}");
            }
            return RunState::Unknown;
        }
    };
    match file.try_lock_shared() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return RunState::Alive,
        Err(TryLockError::Error(e)) => {
            tracing::debug!("Failed to probe task run lock {path:?}: {e}");
            return RunState::Unknown;
        }
    }
    let mut content = String::new();
    let read = file.read_to_string(&mut content);
    unlock(&file, &path);
    if read.is_err() {
        return RunState::Unknown;
    }
    let mut lines = content.lines();
    let written_here = lines.next() == Some(FORMAT)
        && lines.next().and_then(|line| line.strip_prefix("boot ")) == Some(boot.as_str());
    if written_here {
        RunState::Dead
    } else {
        RunState::Unknown
    }
}

/// See the Unix form: off Unix, every task run is unknown (#876).
#[cfg(not(unix))]
fn probe(_directory: &Path, _run: &str) -> RunState {
    RunState::Unknown
}

fn unlock(file: &File, path: &Path) {
    if let Err(e) = file.unlock() {
        tracing::debug!("Failed to unlock task run lock {path:?}: {e}");
    }
}

/// This kernel's boot identity — Linux's `boot_id`, macOS's
/// `kern.bootsessionuuid` — which changes on every boot, and which a
/// container shares with the kernel it runs on. `None` elsewhere, or where it
/// can't be read: then no lock is taken, and every task run reads unknown.
/// Read once per process.
pub fn boot_identity() -> Option<String> {
    static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BOOT.get_or_init(read_boot_identity).clone()
}

#[cfg(target_os = "linux")]
fn read_boot_identity() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(target_os = "macos")]
fn read_boot_identity() -> Option<String> {
    let name = c"kern.bootsessionuuid";
    let mut buffer = [0u8; 64];
    let mut length = buffer.len();
    // SAFETY: `name` is NUL-terminated, `buffer` is valid for `length` bytes
    // and `length` is its size; no new value is passed.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    let id = std::str::from_utf8(&buffer[..length.min(buffer.len())]).ok()?;
    let id = id.trim_end_matches('\0').trim();
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_boot_identity() -> Option<String> {
    None
}

#[cfg(test)]
#[path = "run_lock_tests.rs"]
mod tests;
