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

//! What an invocation exits with — the one mapping both binaries honour.
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
//! [`crate::interrupt::TaskInterrupted`], [`crate::docker::StdoutClosed`],
//! [`crate::engine::CleanupFailed`] and [`InvalidOptionValue`] are all `ratect-core` types, and neither binary
//! defines an error of its own. This is the mapping's only home, and adding
//! a third binary would not give it a second.
//!
//! The design is [decisions/0016](../../decisions/0016-exit-codes.md), and
//! its two paths are this module's shape:
//!
//! - **Path 1, a run** — `ratect run` and a `ratect-compat` line meant to run
//!   a task. The code answers whether the task's own container ran to an exit
//!   code of its own: [`for_error`] is that mapping, and every failure of
//!   Ratect's own to run the task is [`RATECT_FAILED`], a usage error or an
//!   invalid option value included.
//! - **Path 2, every other verb** — it runs nothing, so [`RATECT_FAILED`]
//!   would say something false of it: [`VERB_FAILED`] when the command
//!   failed, [`INVOCATION_ERROR`] when it was invoked wrong.
//!
//! [`for_failure`] is that split for a command that started, and
//! [`for_invocation_error`] for an argument `clap` rejected; each `main` says
//! only which path it is on, which [`command_line_words`] helps it find when
//! the arguments don't parse. Two codes stay outside this module because
//! something else produces them: `0` for `--help` and `--version` (`clap`'s),
//! and `101` for a crash (Rust's panic default).
//!
//! How a failure is printed is not here either — each `main` prints its own
//! `Error:` line — but *whether* it needs printing is, as [`already_reported`]:
//! the types [`for_error`] dispatches over, plus the engine's own two for a
//! failure it has reported as an `Error:` line itself, are the ones a run
//! has already reported by another route (decisions/0017) — as
//! [`VerbFailuresReported`] is for a verb that printed its own failures — and two
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

/// A verb that isn't a run was invoked wrong: an argument `clap` rejects, or
/// an [`InvalidOptionValue`]. The POSIX utilities' `2`. A run never exits
/// this — it would collide with a task's own `2` — so it gets
/// [`RATECT_FAILED`] instead.
pub const INVOCATION_ERROR: u8 = 2;

/// A value an option got that fails a check on the value itself: a path that
/// doesn't exist or isn't the kind of thing the option needs, or, for a path
/// the option creates, one whose directory doesn't exist. Whatever its
/// source — the command line, the environment, a default file, or the
/// option's built-in default. A failure found once the command reads or uses
/// a valid value (a config file that doesn't parse, a log file that can't be
/// opened) is the command's own, and is not this.
///
/// Its code is its path's [`for_invocation_error`], whenever it is found;
/// *when* each value is checked is that option's own business
/// (decisions/0016). It reads as its message alone, so a verb that reports
/// one as a finding (`doctor`, `config validate`) quotes it unchanged.
#[derive(Debug)]
pub struct InvalidOptionValue {
    message: String,
}

impl InvalidOptionValue {
    /// `message` is the whole of what the user reads: name the option and
    /// the value, and what is wrong with it.
    pub fn new(message: impl Into<String>) -> Self {
        InvalidOptionValue {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for InvalidOptionValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InvalidOptionValue {}

/// A verb's failure it has already printed itself, as one `Error:` line per
/// thing that failed: `ratect includes clean`/`refresh`, which carry on past
/// an entry they couldn't remove or re-clone and report each one (#755).
/// [`already_reported`] knows it, so `main` prints nothing more, and
/// [`for_failure`] exits it as any verb's failure, [`VERB_FAILED`].
#[derive(Debug)]
pub struct VerbFailuresReported;

impl std::fmt::Display for VerbFailuresReported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failures already reported")
    }
}

impl std::error::Error for VerbFailuresReported {}

/// The process exit code for a failed run.
///
/// Four cases, in the order they are asked — the order is the precedence:
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
/// - **Stdout's reader went** — 141, 128 + `SIGPIPE`, as though that signal
///   had ended it: see [`crate::docker::StdoutClosed`].
/// - **Anything else** — [`RATECT_FAILED`], including
///   [`crate::engine::CleanupFailed`] after a command that exited `0`.
///
/// A divergence from Batect, which passes the task's own exit code through
/// too (except under `--no-cleanup-after-success`, where it always returns
/// `-1`, 255) but returns `-1` (255) for a signal or any other failure alike,
/// and so says nothing about which it was.
pub fn for_error(error: &anyhow::Error) -> u8 {
    // Looked through, since the wrapper hides the type it wraps from
    // `anyhow`'s downcasts: whatever the engine reported early still exits
    // as what it is.
    let error = error
        .downcast_ref::<crate::engine::FailureReported>()
        .map_or(error, |reported| &reported.0);
    if let Some(failure) = error.downcast_ref::<crate::docker::ContainerExitedNonZero>() {
        return failure.exit_code as u8;
    }
    if let Some(interrupted) = error.downcast_ref::<crate::interrupt::TaskInterrupted>() {
        return interrupted.signal.exit_code();
    }
    if error.is::<crate::docker::StdoutClosed>() {
        return crate::docker::StdoutClosed::EXIT_CODE;
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
/// - **Stdout's reader went** — reported, like a signal, by the engine's
///   notice and by the exit code.
/// - **Cleanup failed after a passing run** — reported by the engine's own
///   `Error:` line at the time, which names what was left behind and how to
///   remove it; a second `Error:` from `main` would say less, twice.
/// - **The task failed to run once it had started** — a failed pull or
///   build, a container that couldn't be created or started, a dependency
///   that never became ready: reported by the engine's own `Error:` line the
///   moment it happened, before cleanup ([#334](https://github.com/or1can/ratect/issues/334)),
///   as a [`crate::engine::FailureReported`].
///
/// And one that isn't a run's: **a verb's failures it printed itself** — a
/// [`VerbFailuresReported`].
///
/// Every other failure — a config that doesn't load, an unknown task, a
/// prerequisite list that names a task that doesn't exist, anything else
/// before a task starts — is reported by nothing else, so `main` prints it,
/// in every style and whatever `RUST_LOG` says. Downcasts through `anyhow`
/// context like [`for_error`].
pub fn already_reported(error: &anyhow::Error) -> bool {
    error.is::<crate::docker::ContainerExitedNonZero>()
        || error.is::<crate::interrupt::TaskInterrupted>()
        || error.is::<crate::docker::StdoutClosed>()
        || error.is::<crate::engine::CleanupFailed>()
        || error.is::<crate::engine::FailureReported>()
        || error.is::<VerbFailuresReported>()
}

/// The process exit code for a failed invocation: [`for_error`]'s for a run;
/// for anything else, [`INVOCATION_ERROR`] for an [`InvalidOptionValue`]
/// (through any context) and [`VERB_FAILED`] for every other error. The
/// path split lives here so both binaries make it the same way; each `main`
/// only says which path it is on.
pub fn for_failure(error: &anyhow::Error, is_run: bool) -> u8 {
    if is_run {
        for_error(error)
    } else if error.is::<InvalidOptionValue>() {
        INVOCATION_ERROR
    } else {
        VERB_FAILED
    }
}

/// The process exit code for an argument `clap` rejected: [`RATECT_FAILED`]
/// on a run, [`INVOCATION_ERROR`] otherwise. `clap` itself would exit `2` on
/// either path, which on a run collides with a task's own code, so both
/// `main`s parse with `try_parse` and exit with this instead. A `--help` or
/// `--version` that `clap` reached is not a rejection, and still exits `0`.
pub fn for_invocation_error(is_run: bool) -> u8 {
    if is_run {
        RATECT_FAILED
    } else {
        INVOCATION_ERROR
    }
}

/// One word of a command line, as `clap` would read it: a long option's name
/// (without `--` or any `=value`), one short option, or a positional — a
/// subcommand, a task name, or a subcommand's own argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word<'a> {
    Long(&'a str),
    Short(char),
    Positional(&'a str),
}

/// The options and positionals in `args` (the arguments, without the
/// program name), as `clap` would read them — what deciding an invocation's
/// path needs when the arguments don't parse, so there is no parsed result to
/// ask (decisions/0016's "Which path an invocation is on").
///
/// Nothing after `--` is read: it belongs to the task. An option's value is
/// skipped, whatever it looks like, so it never counts as a subcommand or a
/// flag: `--name=value`, a short option's attached value (`-oquiet`), or,
/// where `takes_value` says the option takes one, the next word. An option
/// `takes_value` doesn't recognise must answer `false` — taken to take no
/// value, the one reading that doesn't guess at an unknown flag. Each binary
/// answers `takes_value` from its own `clap` definition, which `ratect-core`
/// doesn't depend on.
pub fn command_line_words<'a, S: AsRef<str>>(
    args: &'a [S],
    takes_value: impl Fn(Word<'_>) -> bool,
) -> Vec<Word<'a>> {
    let mut words = Vec::new();
    let mut args = args.iter().map(AsRef::as_ref);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            match long.split_once('=') {
                Some((name, _)) => words.push(Word::Long(name)),
                None => {
                    words.push(Word::Long(long));
                    if takes_value(Word::Long(long)) {
                        args.next();
                    }
                }
            }
        } else if let Some(shorts) = arg.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
            for (index, short) in shorts.char_indices() {
                words.push(Word::Short(short));
                if takes_value(Word::Short(short)) {
                    if index + short.len_utf8() == shorts.len() {
                        args.next();
                    }
                    break;
                }
            }
        } else {
            words.push(Word::Positional(arg));
        }
    }
    words
}

#[cfg(test)]
#[path = "exit_code_tests.rs"]
mod tests;
