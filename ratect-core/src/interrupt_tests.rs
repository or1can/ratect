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

use std::time::Duration;

#[test]
fn starts_with_no_interrupts_recorded() {
    let interrupt = Interrupt::new();

    assert_eq!(interrupt.count(), 0);
    assert!(!interrupt.is_interrupted());
}

#[test]
fn counts_every_interrupt_rather_than_latching() {
    let interrupt = Interrupt::new();

    interrupt.record();
    assert_eq!(interrupt.count(), 1);
    assert!(interrupt.is_interrupted());

    interrupt.record();
    assert_eq!(interrupt.count(), 2);
}

/// Which signal ended the run decides the process's exit code (128 + the
/// signal's own number), so the tracker has to say *which* one arrived, not
/// only that one did.
#[test]
fn records_which_signal_arrived() {
    let interrupt = Interrupt::new();

    interrupt.record_signal(TerminationSignal::Terminate);

    assert_eq!(interrupt.count(), 1);
    assert_eq!(interrupt.last_signal(), TerminationSignal::Terminate);
}

/// A run interrupted and then hung up on is reported as hung up on: the
/// signal that ended it is the last one, not the first.
#[test]
fn the_most_recent_signal_wins() {
    let interrupt = Interrupt::new();

    interrupt.record_signal(TerminationSignal::Interrupt);
    interrupt.record_signal(TerminationSignal::Hangup);

    assert_eq!(interrupt.count(), 2);
    assert_eq!(interrupt.last_signal(), TerminationSignal::Hangup);
}

/// `record` is what the engine's own tests inject with and what a non-Unix
/// Ctrl+C arrives as, so it has to mean `SIGINT` specifically — including
/// before anything has been recorded at all, since the exit-code path reads
/// it regardless.
#[test]
fn recording_without_a_signal_means_an_interrupt() {
    let interrupt = Interrupt::new();
    assert_eq!(interrupt.last_signal(), TerminationSignal::Interrupt);

    interrupt.record();
    assert_eq!(interrupt.last_signal(), TerminationSignal::Interrupt);
}

/// POSIX's own numbers, which the exit code below is built from.
#[test]
fn signal_numbers_are_the_posix_ones() {
    assert_eq!(TerminationSignal::Interrupt.number(), 2);
    assert_eq!(TerminationSignal::Terminate.number(), 15);
    assert_eq!(TerminationSignal::Hangup.number(), 1);
}

/// 128 + the number, the shell's convention for "killed by this signal", so
/// an interrupted run exits 130, a terminated one 143 and a hung-up one 129.
/// One contract shared by both binaries, which is why it lives here.
#[test]
fn the_exit_code_is_128_plus_the_signal() {
    assert_eq!(TerminationSignal::Interrupt.exit_code(), 130);
    assert_eq!(TerminationSignal::Terminate.exit_code(), 143);
    assert_eq!(TerminationSignal::Hangup.exit_code(), 129);
}

/// The tracker stores a signal as its number and reads it back, so the
/// round trip has to be exact for every signal — and a value that is no
/// signal's number, which is what the atomic holds before anything arrives,
/// has to read as an interrupt rather than panic.
#[test]
fn a_signal_survives_the_round_trip_through_its_number() {
    for signal in TerminationSignal::ALL {
        assert_eq!(
            TerminationSignal::from_number(signal.number() as usize),
            signal
        );
    }

    assert_eq!(
        TerminationSignal::from_number(0),
        TerminationSignal::Interrupt
    );
}

/// The failure a run ends with is printed to stderr in every output style,
/// so it has to name what actually happened: "interrupted" is wrong for a
/// process an editor or an init system terminated, and nobody pressed
/// anything.
#[test]
fn the_failure_names_the_signal_that_ended_the_run() {
    assert_eq!(
        TaskInterrupted::new(TerminationSignal::Interrupt).to_string(),
        "Interrupted"
    );
    assert_eq!(
        TaskInterrupted::new(TerminationSignal::Terminate).to_string(),
        "Terminated by SIGTERM"
    );
    assert_eq!(
        TaskInterrupted::new(TerminationSignal::Hangup).to_string(),
        "Terminated by SIGHUP"
    );
}

#[tokio::test]
async fn interrupted_resolves_immediately_when_one_already_happened() {
    let interrupt = Interrupt::new();
    interrupt.record();

    // Would hang rather than fail if the already-interrupted case were
    // missed, so bound it.
    tokio::time::timeout(Duration::from_secs(5), interrupt.interrupted())
        .await
        .expect("interrupted() should resolve immediately");
}

#[tokio::test]
async fn interrupted_resolves_when_one_arrives_later() {
    let interrupt = Interrupt::new();
    let waiter = Arc::clone(&interrupt);

    let handle = tokio::spawn(async move { waiter.interrupted().await });

    // Let the spawned task reach its await point before interrupting, so
    // this exercises the arrives-while-waiting path rather than the
    // already-interrupted one above.
    tokio::task::yield_now().await;
    interrupt.record();

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("interrupted() should resolve once an interrupt arrives")
        .expect("waiter should not panic");
}

#[tokio::test]
async fn wait_for_a_second_interrupt_ignores_the_first() {
    let interrupt = Interrupt::new();
    let waiter = Arc::clone(&interrupt);

    let handle = tokio::spawn(async move { waiter.wait_for(2).await });

    tokio::task::yield_now().await;
    interrupt.record();
    tokio::task::yield_now().await;
    assert!(
        !handle.is_finished(),
        "one interrupt should not satisfy two"
    );

    interrupt.record();

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("wait_for(2) should resolve on the second interrupt")
        .expect("waiter should not panic");
}

#[tokio::test]
async fn an_interrupt_racing_registration_is_not_missed() {
    // The bug this guards: checking the count *before* registering
    // interest lets an interrupt land in between, so the waiter sleeps
    // through the very thing it is waiting for. Recording from another
    // task while this one is between those two steps is the closest a
    // test can get to that window without reaching inside.
    let interrupt = Interrupt::new();
    let recorder = Arc::clone(&interrupt);

    let handle = tokio::spawn(async move {
        tokio::task::yield_now().await;
        recorder.record();
    });

    tokio::time::timeout(Duration::from_secs(5), interrupt.interrupted())
        .await
        .expect("a concurrently-recorded interrupt should still wake the waiter");
    handle.await.expect("recorder should not panic");
}
