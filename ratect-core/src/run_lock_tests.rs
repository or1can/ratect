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

//! **A test that expects a task run lock to read free as soon as its holder
//! closes the file, rather than unlocking it, keeps that holder in a process
//! of its own** — as [`a_killed_holder_process_reads_dead`] does. In this
//! test binary, any test on another thread that starts a process creates a
//! child sharing every open file, the holder's included, until its `exec`:
//! for that moment the lock outlives the close, and a reader finds it held
//! (#886). [`RunLock`]'s drop and [`RunLock::finish`] unlock explicitly,
//! which frees the lock for every process sharing the file, so a test that
//! drops or finishes one needs no such process (#888).

use super::*;
use std::io::Write;

fn unique_temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!("ratect-run-lock-test-{}", uuid::Uuid::new_v4()))
}

fn run_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A lock file under its final name holding `content`, whose lock nobody
/// holds.
fn write_lock_file(directory: &Path, run: &str, content: &str) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join(format!("{run}.lock")), content).unwrap();
}

#[cfg(unix)]
#[test]
fn a_taken_lock_is_under_its_final_name_and_reads_alive() {
    let directory = unique_temp_dir();
    let locks = RunLocks::at(directory.clone());
    let run = run_id();

    let lock = locks.take(&run).expect("taken");

    assert_eq!(lock.path(), directory.join(format!("{run}.lock")));
    assert!(!directory.join(format!("{run}.tmp")).exists());
    let content = std::fs::read_to_string(lock.path()).unwrap();
    assert_eq!(
        content,
        format!("{FORMAT}\nboot {}\n", boot_identity().unwrap())
    );
    assert_eq!(locks.state(&run), RunState::Alive);
    drop(lock);
    std::fs::remove_dir_all(&directory).ok();
}

/// A task run that left nothing behind deletes its file, which then reads
/// unknown, never dead.
#[cfg(unix)]
#[test]
fn a_lock_finished_having_left_nothing_is_gone() {
    let directory = unique_temp_dir();
    let locks = RunLocks::at(directory.clone());
    let run = run_id();

    let lock = locks.take(&run).unwrap();
    let path = lock.path().to_path_buf();
    lock.finish(true);

    assert!(!path.exists());
    assert_eq!(locks.state(&run), RunState::Unknown);
    std::fs::remove_dir_all(&directory).ok();
}

/// A task run that left something keeps its file, which reads dead once it
/// is unlocked — by `finish`, or by a drop, as a panic would.
#[cfg(unix)]
#[test]
fn a_lock_finished_or_dropped_having_left_something_reads_dead() {
    let directory = unique_temp_dir();
    let locks = RunLocks::at(directory.clone());

    let finished = run_id();
    locks.take(&finished).unwrap().finish(false);
    assert_eq!(locks.state(&finished), RunState::Dead);

    let dropped = run_id();
    drop(locks.take(&dropped).unwrap());
    assert_eq!(locks.state(&dropped), RunState::Dead);
    std::fs::remove_dir_all(&directory).ok();
}

/// Decision 2: a free lock is proof only to a reader on the kernel that
/// wrote it.
#[cfg(unix)]
#[test]
fn a_free_lock_from_another_boot_reads_unknown() {
    let directory = unique_temp_dir();
    let run = run_id();
    write_lock_file(&directory, &run, &format!("{FORMAT}\nboot another-boot\n"));

    assert_eq!(
        RunLocks::at(directory.clone()).state(&run),
        RunState::Unknown
    );
    std::fs::remove_dir_all(&directory).ok();
}

/// Each way of reaching unknown besides another boot and the lock's own
/// error, which no local filesystem here produces.
#[test]
fn what_cant_be_read_as_this_formats_free_lock_reads_unknown() {
    let directory = unique_temp_dir();
    let locks = RunLocks::at(directory.clone());
    let boot = boot_identity().unwrap_or_default();

    let absent = run_id();
    assert_eq!(locks.state(&absent), RunState::Unknown, "no file");

    let other_format = run_id();
    write_lock_file(
        &directory,
        &other_format,
        &format!("ratect-run-lock 2\nboot {boot}\n"),
    );
    assert_eq!(
        locks.state(&other_format),
        RunState::Unknown,
        "another format"
    );

    let empty = run_id();
    write_lock_file(&directory, &empty, "");
    assert_eq!(locks.state(&empty), RunState::Unknown, "no format line");

    assert_eq!(
        RunLocks::default().state(&run_id()),
        RunState::Unknown,
        "nowhere"
    );
    std::fs::remove_dir_all(&directory).ok();
}

/// A `run` label comes from the daemon, so one that isn't a plain id names
/// no file: it reads unknown, and is neither taken nor deleted.
#[cfg(unix)]
#[test]
fn a_run_label_that_isnt_a_plain_id_names_no_file() {
    let directory = unique_temp_dir();
    let locks = RunLocks::at(directory.join("runs"));
    let outside = directory.join("outside.lock");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(&outside, "").unwrap();

    let real = run_id();
    let lock = locks.take(&real).unwrap();
    for run in [
        "../outside",
        "",
        "a/b",
        "a.b",
        "not-hex",
        &real.to_uppercase(),
    ] {
        assert_eq!(locks.state(run), RunState::Unknown, "{run:?}");
        assert!(locks.take(run).is_none(), "{run:?}");
        locks.forget(run).unwrap();
    }
    assert!(outside.exists());
    assert!(
        lock.path().exists(),
        "an upper-cased id is another task run's"
    );
    drop(lock);
    std::fs::remove_dir_all(&directory).ok();
}

/// A reader probes with a shared lock: one holding its probe never makes
/// another read alive, so two readers at once both read dead.
#[cfg(unix)]
#[test]
fn a_reader_holding_its_probe_doesnt_make_another_read_alive() {
    let directory = unique_temp_dir();
    let locks = RunLocks::at(directory.clone());
    let run = run_id();
    locks.take(&run).unwrap().finish(false);

    let other_reader = File::open(directory.join(format!("{run}.lock"))).unwrap();
    other_reader.try_lock_shared().unwrap();
    assert_eq!(locks.state(&run), RunState::Dead);
    other_reader.unlock().unwrap();
    std::fs::remove_dir_all(&directory).ok();
}

/// `resources clean`'s deletion, and one of a file already gone.
#[test]
fn forgetting_deletes_the_file_and_tolerates_one_gone() {
    let directory = unique_temp_dir();
    let locks = RunLocks::at(directory.clone());
    let run = run_id();
    write_lock_file(&directory, &run, "");

    locks.forget(&run).unwrap();
    assert!(!directory.join(format!("{run}.lock")).exists());
    locks.forget(&run).unwrap();
    std::fs::remove_dir_all(&directory).ok();
}

#[test]
fn nowhere_takes_nothing() {
    assert!(RunLocks::default().take(&run_id()).is_none());
}

/// Set to a directory and run id, makes [`a_lock_holder_process`] take that
/// task run's lock and hold it until killed.
const HOLDER_ENV: &str = "RATECT_TEST_RUN_LOCK_HOLDER";

/// What [`a_lock_holder_process`] prints once it holds the lock.
const HELD: &str = "run-lock-held";

/// Not a test on its own: does nothing unless [`HOLDER_ENV`] is set, as it
/// is when the test below runs this binary again to get a holder in a
/// process of its own.
#[test]
fn a_lock_holder_process() {
    let Some(holder) = std::env::var(HOLDER_ENV).ok() else {
        return;
    };
    let (directory, run) = holder.split_once('\n').unwrap();
    let lock = RunLocks::at(PathBuf::from(directory)).take(run).unwrap();
    // libtest has already printed `test <name> ... ` without a newline, so
    // the parent looks for this anywhere in a line.
    let mut stdout = std::io::stdout();
    stdout.write_all(format!("{HELD}\n").as_bytes()).unwrap();
    stdout.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(60));
    drop(lock);
}

/// The premise of the whole lock: the kernel frees a holder's lock when it is
/// killed, however it is killed, and its task run then reads dead.
#[cfg(unix)]
#[test]
fn a_killed_holder_process_reads_dead() {
    use std::io::BufRead;

    let directory = unique_temp_dir();
    let run = run_id();
    let mut holder = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "run_lock::tests::a_lock_holder_process",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HOLDER_ENV, format!("{}\n{run}", directory.display()))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = holder.stdout.take().unwrap();
    let held = std::io::BufReader::new(stdout)
        .lines()
        .any(|line| line.is_ok_and(|line| line.contains(HELD)));
    assert!(held, "the holder process never took the lock");

    let locks = RunLocks::at(directory.clone());
    assert_eq!(locks.state(&run), RunState::Alive);
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(locks.state(&run), RunState::Dead);
    std::fs::remove_dir_all(&directory).ok();
}
