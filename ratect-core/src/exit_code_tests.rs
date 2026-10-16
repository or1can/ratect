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

fn cleanup_failed() -> crate::engine::CleanupFailed {
    crate::engine::CleanupFailed {
        task: "build".to_string(),
        run: "run-id".to_string(),
    }
}

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

/// Everything else is one code: the task's command never ran, or never
/// finished, and Ratect is what failed. `125` is `docker run`'s own code for
/// that, and coreutils' for any command-running wrapper — decisions/0016's
/// rule 3. Batect returns 255; that divergence is on the differences page.
#[test]
fn any_other_failure_exits_125() {
    assert_eq!(for_error(&anyhow::anyhow!("the config file is empty")), 125);
    assert_eq!(RATECT_FAILED, 125);
}

/// A run whose command exited `0` but whose cleanup then failed exits `125`
/// rather than the `0` it would otherwise have earned: a leaked container or
/// network is Ratect failing at its own job, and CI should see it.
#[test]
fn a_passing_run_whose_cleanup_failed_exits_125() {
    let error = anyhow::Error::new(cleanup_failed());

    assert_eq!(for_error(&error), RATECT_FAILED);
}

/// The task's own non-zero code is never overridden by a cleanup failure —
/// the engine keeps returning the container's failure, so that's all the
/// mapping ever sees. Pinned here on an error carrying both, in case a later
/// change starts wrapping one in the other.
#[test]
fn a_tasks_own_code_wins_over_a_cleanup_failure() {
    use anyhow::Context;

    let error = Err::<(), _>(anyhow::Error::new(cleanup_failed()))
        .context(crate::docker::ContainerExitedNonZero { exit_code: 3 })
        .unwrap_err();
    assert_eq!(for_error(&error), 3);

    let error = Err::<(), _>(anyhow::Error::new(crate::docker::ContainerExitedNonZero {
        exit_code: 3,
    }))
    .context(cleanup_failed())
    .unwrap_err();
    assert_eq!(for_error(&error), 3);
}

/// A signal that ended the command wins over a cleanup failure after it,
/// whichever wraps the other.
#[test]
fn a_signal_wins_over_a_cleanup_failure() {
    use anyhow::Context;

    let error = Err::<(), _>(anyhow::Error::new(cleanup_failed()))
        .context(TaskInterrupted::new(TerminationSignal::Terminate))
        .unwrap_err();
    assert_eq!(for_error(&error), 143);

    let error = Err::<(), _>(anyhow::Error::new(TaskInterrupted::new(
        TerminationSignal::Terminate,
    )))
    .context(cleanup_failed())
    .unwrap_err();
    assert_eq!(for_error(&error), 143);
}

/// A task's own command may itself exit with one of the codes Ratect or
/// Docker reserve; it passes through all the same, as `docker run` lets it.
#[test]
fn a_tasks_own_125_126_and_127_pass_through() {
    for exit_code in [125, 126, 127] {
        let error = anyhow::Error::new(crate::docker::ContainerExitedNonZero { exit_code });
        assert_eq!(i64::from(for_error(&error)), exit_code);
    }
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

/// A verb that isn't a run exits `1` whatever failed — even an error the run
/// mapping would give a code of its own — and a run's failure goes through
/// the mapping (decisions/0016).
#[test]
fn only_a_runs_failure_goes_through_the_mapping() {
    let container = anyhow::Error::new(crate::docker::ContainerExitedNonZero { exit_code: 3 });
    let other = anyhow::anyhow!("the daemon is unreachable");

    assert_eq!(for_failure(&container, true), 3);
    assert_eq!(for_failure(&other, true), RATECT_FAILED);
    assert_eq!(for_failure(&container, false), VERB_FAILED);
    assert_eq!(for_failure(&other, false), VERB_FAILED);
}

/// The three failures a run has already reported by another route — the
/// task's own exit (its code and the summary line), a signal (the interrupt
/// notice), a cleanup failure (the engine's own `Error:` line) — need no
/// `Error:` from `main`; everything else does. Through `anyhow` context too,
/// since the binaries see the error after the engine's own wrapping.
#[test]
fn already_reported_names_exactly_the_three_failures_reported_elsewhere() {
    use anyhow::Context;

    assert!(already_reported(&anyhow::Error::new(
        crate::docker::ContainerExitedNonZero { exit_code: 42 }
    )));
    assert!(already_reported(&anyhow::Error::new(TaskInterrupted::new(
        TerminationSignal::Terminate
    ))));
    assert!(already_reported(&anyhow::Error::new(cleanup_failed())));
    let wrapped = Err::<(), _>(anyhow::Error::new(crate::docker::ContainerExitedNonZero {
        exit_code: 3,
    }))
    .context("while running 'build'")
    .unwrap_err();
    assert!(already_reported(&wrapped));

    assert!(!already_reported(&anyhow::anyhow!(
        "the config file is empty"
    )));
    assert!(!already_reported(
        &Err::<(), _>(anyhow::anyhow!("no such image"))
            .context("Failed to pull image alpine")
            .unwrap_err()
    ));
}
