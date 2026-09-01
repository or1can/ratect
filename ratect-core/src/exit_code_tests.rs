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

use super::*;

use crate::interrupt::{TaskInterrupted, TerminationSignal};

/// The task's own command wins over everything: its code is what a script
/// wants, and collapsing it to a generic failure is what `docker run` itself
/// declines to do.
#[test]
fn a_task_command_that_failed_keeps_its_own_code() {
    let error = anyhow::Error::new(crate::docker::ContainerExitedNonZero { exit_code: 42 });

    assert_eq!(for_error(&error), 42);
}

/// A run ended by a signal exits 128 + that signal's own number, so a script
/// or CI job can tell a cancelled run apart from a failed one *and* tell what
/// cancelled it — Ctrl+C and the `SIGTERM` an editor or init system sends are
/// not the same event, and one code for both would say they were.
#[test]
fn a_signalled_run_exits_with_that_signals_code() {
    let code = |signal| for_error(&anyhow::Error::new(TaskInterrupted::new(signal)));

    assert_eq!(code(TerminationSignal::Interrupt), 130);
    assert_eq!(code(TerminationSignal::Terminate), 143);
    assert_eq!(code(TerminationSignal::Hangup), 129);
}

/// Everything else is one code. Batect returns 255 for every failure
/// including these; 1 is the deliberate divergence.
#[test]
fn any_other_failure_exits_1() {
    assert_eq!(for_error(&anyhow::anyhow!("the config file is empty")), 1);
}

/// The context an `anyhow` error accumulates on its way up must not hide the
/// cause underneath — a container failure wrapped in "task 'build' failed" is
/// still that container's exit code. `downcast_ref` searches the whole cause
/// chain, and the engine adds context at several points on the way out, so
/// this pins the property the mapping rests on rather than the shape of any
/// one path through it.
#[test]
fn context_added_on_the_way_up_does_not_hide_the_cause() {
    use anyhow::Context;

    let error = Err::<(), _>(anyhow::Error::new(crate::docker::ContainerExitedNonZero {
        exit_code: 3,
    }))
    .context("task 'build' failed")
    .unwrap_err();

    assert_eq!(for_error(&error), 3);
}
