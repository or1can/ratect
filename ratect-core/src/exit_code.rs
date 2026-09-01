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

//! What a failed run exits with — the one mapping both binaries honour.
//!
//! Most of what a binary presents is its own interface, and the two are
//! deliberately allowed to differ: each spells its own `--output` values, its
//! own flags, its own subcommands. Exit codes are the exception, and
//! [`docs/ratect-cli.md`](../../../docs/ratect-cli.md) says so out loud —
//! `ratect`'s are "Identical to `ratect-compat`". A promise like that is not
//! kept by writing the same `match` twice; two spellings of one contract can
//! only ever differ by one of them being wrong.
//!
//! It lives here rather than in either binary because the errors it dispatches
//! over live here too: [`crate::docker::ContainerExitedNonZero`] and
//! [`crate::interrupt::TaskInterrupted`] are both `ratect-core` types, and
//! neither binary defines an error of its own. This is the mapping's only
//! home, and adding a third binary would not give it a second.
//!
//! What is *not* here is anything about how a failure is printed. That stays
//! in each `main`, which is also the reason this takes an `anyhow::Error`
//! rather than returning one: by the time a binary asks this question it has
//! already reported the failure and only needs a number.

/// The process exit code for a failed run.
///
/// Three cases, in the order they are asked:
///
/// - **The task's own command exited non-zero** — that exact code is
///   propagated as Ratect's own, matching `docker run`'s convention rather
///   than collapsing to a generic failure, so a script can inspect what
///   actually happened.
/// - **A termination signal ended the run** — 128 + that signal's number, so
///   130 for Ctrl+C, 143 for `SIGTERM`, 129 for `SIGHUP`. See
///   [`crate::interrupt::TerminationSignal::exit_code`].
/// - **Anything else** — `1`.
///
/// A divergence from Batect, which returns `-1` (255) for every failure alike
/// and so says nothing about which it was; Ratect already diverges by using 1
/// rather than 255 for an ordinary failure.
pub fn for_error(error: &anyhow::Error) -> u8 {
    if let Some(failure) = error.downcast_ref::<crate::docker::ContainerExitedNonZero>() {
        return failure.exit_code as u8;
    }
    if let Some(interrupted) = error.downcast_ref::<crate::interrupt::TaskInterrupted>() {
        return interrupted.signal.exit_code();
    }
    1
}

#[cfg(test)]
#[path = "exit_code_tests.rs"]
mod tests;
