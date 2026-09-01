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
