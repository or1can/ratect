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

/// An invocation error — an argument `clap` rejects — takes its path's code:
/// `125` on a run, where `2` would collide with a task's own code, and `2`
/// everywhere else (decisions/0016).
#[test]
fn a_usage_error_exits_125_on_a_run_and_2_otherwise() {
    assert_eq!(for_invocation_error(true), RATECT_FAILED);
    assert_eq!(for_invocation_error(false), INVOCATION_ERROR);
    assert_eq!(INVOCATION_ERROR, 2);
}

/// An invalid option value takes the same code as a usage error on either
/// path, through whatever context the caller wrapped it in — the code
/// depends on the kind of failure, not on which library checked the value.
#[test]
fn an_invalid_option_value_exits_125_on_a_run_and_2_otherwise() {
    use anyhow::Context;

    let invalid = || anyhow::Error::new(InvalidOptionValue::new("-f \"x\" is not a file."));
    let wrapped = Err::<(), _>(invalid())
        .context("Failed to load x")
        .unwrap_err();

    assert_eq!(for_failure(&invalid(), true), RATECT_FAILED);
    assert_eq!(for_failure(&wrapped, true), RATECT_FAILED);
    assert_eq!(for_failure(&invalid(), false), INVOCATION_ERROR);
    assert_eq!(for_failure(&wrapped, false), INVOCATION_ERROR);
}

/// The value's own message is what the user reads — the type adds nothing
/// to it, so a diagnosing verb that reports one as a finding quotes it as is.
#[test]
fn an_invalid_option_value_reads_as_its_message() {
    let error = InvalidOptionValue::new("Configuration file \"nope.toml\" not found.");

    assert_eq!(
        error.to_string(),
        "Configuration file \"nope.toml\" not found."
    );
}

/// A tiny option table standing in for a binary's own `clap` definition:
/// `-f`/`--config-file`, `-o`/`--output` and `--clean-cache` take a value,
/// every other option doesn't.
fn takes_value(word: Word<'_>) -> bool {
    matches!(
        word,
        Word::Short('f' | 'o') | Word::Long("config-file" | "output" | "clean-cache")
    )
}

fn words_of<'a>(line: &'a [&'a str]) -> Vec<Word<'a>> {
    command_line_words(line, takes_value)
}

/// The words `clap` would read as options or positionals — what deciding a
/// path needs when the arguments don't parse.
#[test]
fn command_line_words_reads_options_and_positionals_as_clap_would() {
    assert_eq!(
        words_of(&["--bogus", "run", "test"]),
        [
            Word::Long("bogus"),
            Word::Positional("run"),
            Word::Positional("test")
        ]
    );
    assert_eq!(
        words_of(&["-T", "--clean-cache=a", "build"]),
        [
            Word::Short('T'),
            Word::Long("clean-cache"),
            Word::Positional("build")
        ]
    );
}

/// Nothing after `--` is an option or a subcommand: it belongs to the task.
#[test]
fn command_line_words_stops_at_a_double_dash() {
    assert_eq!(
        words_of(&["build", "--bogus", "--", "--clean"]),
        [Word::Positional("build"), Word::Long("bogus")]
    );
}

/// An option that takes a value consumes the next word, whatever it looks
/// like, so a value never counts as a subcommand or a flag.
#[test]
fn command_line_words_skips_an_options_value() {
    assert_eq!(
        words_of(&["-o", "bogus", "run", "test"]),
        [
            Word::Short('o'),
            Word::Positional("run"),
            Word::Positional("test")
        ]
    );
    assert_eq!(
        words_of(&["--config-file", "--clean", "tasks"]),
        [Word::Long("config-file"), Word::Positional("tasks")]
    );
    assert_eq!(
        words_of(&["--clean-cache", "run"]),
        [Word::Long("clean-cache")]
    );
}

/// A value-taking short option takes the rest of its cluster as its value,
/// or the next word when nothing follows it; flags before it in the cluster
/// still count.
#[test]
fn command_line_words_reads_a_short_cluster() {
    assert_eq!(
        words_of(&["-oT", "run"]),
        [Word::Short('o'), Word::Positional("run")]
    );
    assert_eq!(
        words_of(&["-Tf", "run", "build"]),
        [
            Word::Short('T'),
            Word::Short('f'),
            Word::Positional("build")
        ]
    );
}

/// An option nobody recognises is taken to take no value — the one reading
/// that doesn't guess — so the word after it still counts.
#[test]
fn command_line_words_takes_an_unknown_option_to_take_no_value() {
    assert_eq!(
        words_of(&["--bogus", "--clean"]),
        [Word::Long("bogus"), Word::Long("clean")]
    );
}
