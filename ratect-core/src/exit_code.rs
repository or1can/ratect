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

//! What a run exits with — the one mapping both binaries honour.
//!
//! Most of what a binary presents is its own interface, and the two are
//! deliberately allowed to differ: each spells its own `--output` values, its
//! own flags, its own subcommands. Exit codes are the exception, and
//! [`docs/ratect-cli.md`](../../docs/ratect-cli.md) says so out loud —
//! `ratect`'s are "Identical to `ratect-compat`". A promise like that is not
//! kept by writing the same `match` twice; two spellings of one contract can
//! only ever differ by one of them being wrong.
//!
//! It lives here rather than in either binary because the errors it dispatches
//! over live here too: [`crate::docker::ContainerExitedNonZero`],
//! [`crate::interrupt::TaskInterrupted`] and [`crate::engine::CleanupFailed`]
//! are all `ratect-core` types, and neither binary defines an error of its
//! own. This is the mapping's only home, and adding a third binary would not
//! give it a second.
//!
//! The design is [decisions/0016](../../decisions/0016-exit-codes.md): the
//! code answers whether the task's own container ran to an exit code of its
//! own. [`for_error`] is a *run's* mapping only — `ratect run` and
//! `ratect-compat <task>`. Every other verb runs nothing, so [`RATECT_FAILED`]
//! would say something false of it; each exits [`VERB_FAILED`] on its own
//! failure and never consults [`for_error`]. [`for_failure`] is that split,
//! which both `main`s call. Three codes stay outside this
//! module because something else produces them: `2` for a usage error and
//! `0` for `--help` (both `clap`'s), and `101` for a crash (Rust's panic
//! default).
//!
//! How a failure is printed is not here either — each `main` prints its own
//! `Error:` line — but *whether* it needs printing is, as [`already_reported`]:
//! the same three error types [`for_error`] dispatches over are the ones a
//! run has already reported by another route (decisions/0017), and two
//! `main`s deciding that by hand could only ever differ by one being wrong.
//! This takes an `anyhow::Error` rather than returning one for the same
//! reason as before: by the time a binary asks, the run is over and it only
//! needs a number and a yes or no.

/// Everything asked for succeeded — for a run, the task's own command exited
/// `0` and Ratect cleaned up after it.
pub const SUCCESS: u8 = 0;

/// A verb that isn't a run failed — `doctor`, `config validate`, `caches`,
/// `ratect-compat --list-tasks`, and every other verb that runs no task.
/// Docker's own split: `docker ps` with the daemon down exits `1`.
pub const VERB_FAILED: u8 = 1;

/// Ratect itself failed a run: the task's command never ran, or never
/// finished, for any reason but a signal — or it exited `0` and Ratect then
/// failed to clean up after it. `docker run`'s own code for "the `docker`
/// command itself failed", and coreutils' for any command-running wrapper
/// (`env`, `nohup`, `timeout`).
pub const RATECT_FAILED: u8 = 125;

/// The process exit code for a failed run.
///
/// Three cases, in the order they are asked — the order is the precedence:
///
/// - **The task's own command exited non-zero** — that exact code is
///   propagated as Ratect's own, matching `docker run`'s convention rather
///   than collapsing to a generic failure, so a script can inspect what
///   actually happened. A cleanup failure after it doesn't change it.
/// - **A termination signal ended the run** — 128 + that signal's number, so
///   130 for Ctrl+C, 143 for `SIGTERM`, 129 for `SIGHUP`, whatever cleanup
///   did afterwards. See [`crate::interrupt::TerminationSignal::exit_code`].
///   A signal that arrived only during cleanup ended nothing, and the engine
///   doesn't report one.
/// - **Anything else** — [`RATECT_FAILED`], including
///   [`crate::engine::CleanupFailed`] after a command that exited `0`.
///
/// A divergence from Batect, which passes the task's own exit code through
/// too (except under `--no-cleanup-after-success`, where it always returns
/// `-1`, 255) but returns `-1` (255) for a signal or any other failure alike,
/// and so says nothing about which it was.
pub fn for_error(error: &anyhow::Error) -> u8 {
    if let Some(failure) = error.downcast_ref::<crate::docker::ContainerExitedNonZero>() {
        return failure.exit_code as u8;
    }
    if let Some(interrupted) = error.downcast_ref::<crate::interrupt::TaskInterrupted>() {
        return interrupted.signal.exit_code();
    }
    RATECT_FAILED
}

/// Whether a run's failure has already reached the user by another route, so
/// that `main` prints no `Error:` line for it — decisions/0017's rule that
/// each class reports an event at most once, and `Error:` is reserved for
/// Ratect's own failures:
///
/// - **The task's own command exited non-zero** — reported by the exit code,
///   and in the non-quiet styles by the summary line; the container's own
///   output said why. Never an `Error:` ([#454](https://github.com/or1can/ratect/issues/454)).
/// - **A termination signal ended the run** — reported by the interrupt
///   notice when it arrived and by the exit code. Never a trailing
///   `Error: Interrupted`.
/// - **Cleanup failed after a passing run** — reported by the engine's own
///   `Error:` line at the time, which names what was left behind and how to
///   remove it; a second `Error:` from `main` would say less, twice.
///
/// Every other failure — a config that doesn't load, an unknown task, a
/// failed pull or build, a dependency that never became ready — is reported
/// by nothing else, so `main` prints it, in every style and whatever
/// `RUST_LOG` says. Downcasts through `anyhow` context like [`for_error`].
pub fn already_reported(error: &anyhow::Error) -> bool {
    error.is::<crate::docker::ContainerExitedNonZero>()
        || error.is::<crate::interrupt::TaskInterrupted>()
        || error.is::<crate::engine::CleanupFailed>()
}

/// The process exit code for a failed invocation: [`for_error`]'s for a run,
/// [`VERB_FAILED`] for anything else, whatever the error. The run/verb split
/// lives here so both binaries make it the same way; each `main` only says
/// which kind of invocation it was.
pub fn for_failure(error: &anyhow::Error, is_run: bool) -> u8 {
    if is_run {
        for_error(error)
    } else {
        VERB_FAILED
    }
}

#[cfg(test)]
#[path = "exit_code_tests.rs"]
mod tests;
