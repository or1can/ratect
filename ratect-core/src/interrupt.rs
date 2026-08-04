//! Interrupt (Ctrl+C) tracking, so a cancelled run still cleans up after
//! itself.
//!
//! Without this, `SIGINT` kills the process outright and every container the
//! task started — plus its network — is left behind, since nothing gets the
//! chance to remove them. Batect traps the signal instead
//! (`InterruptionTrap`, wrapped around its whole execution run in
//! `TaskRunner`) and posts a `UserInterruptedExecutionEvent`, which is a
//! `TaskFailedEvent` — so an interrupt takes the *ordinary failure* path
//! rather than a bespoke one, and `--no-cleanup-after-failure` suppresses
//! cleanup for it exactly as it does for a build or health-check failure
//! (confirmed in Batect's `TaskStateMachine`: any `TaskFailedEvent` sets
//! `taskHasFailed`, and `startCleanupStage` then selects
//! `behaviourAfterFailure`). [`crate::engine::TaskEngine`] does the same.
//!
//! This module is deliberately only the *signal* half: it counts interrupts
//! and lets callers await one. What to do about it — abandon the run, clean
//! up, give up on cleaning up — belongs to the engine, which is the only
//! thing that knows what has been created so far.
//!
//! # Counting rather than latching
//!
//! A single flag would be enough to abandon a run, but not to answer the
//! second question an interrupted user asks, which is "stop *now*". Cleanup
//! talks to the daemon and can itself be slow (a container ignoring
//! `SIGTERM` waits out Docker's ten-second kill timeout), so a second Ctrl+C
//! during cleanup has to mean something. Batect answers this too, if
//! indirectly: its signal handler stays registered for the whole run, so a
//! second interrupt posts a second failure event, and
//! `TaskStateMachine.handleTaskFailedEvent` sees it is already in the
//! cleanup stage and switches to `PostTaskManualCleanup.Required`, printing
//! the commands to remove things by hand. Counting is what lets the engine
//! distinguish the two.
//!
//! # Interactive tasks
//!
//! A task attached to a real TTY puts the terminal in raw mode, and a raw
//! terminal does not turn Ctrl+C into a signal at all — the `0x03` byte is
//! forwarded to the container's stdin, for the container to interpret. That
//! is correct, and matches `docker run -it`: the keystroke belongs to the
//! program you are talking to. So this handler is what covers every
//! *non*-interactive run, which is all of CI and most local task runs, and
//! an interactive session is served by the container receiving the keystroke
//! itself.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

/// The error a task fails with when the user interrupts it.
///
/// A distinct type rather than a plain message so the binaries can map it to
/// their own exit code, the way they already do for
/// [`crate::docker::ContainerExitedNonZero`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskInterrupted;

impl std::fmt::Display for TaskInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Interrupted")
    }
}

impl std::error::Error for TaskInterrupted {}

/// Tracks interrupts (Ctrl+C) for one invocation.
///
/// Cheap to clone as an [`Arc`]; every holder observes the same count.
#[derive(Debug, Default)]
pub struct Interrupt {
    count: AtomicUsize,
    notify: Notify,
}

impl Interrupt {
    /// A new tracker with no interrupts recorded, listening for nothing —
    /// call [`Interrupt::listen`] to attach it to the real signal.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Starts listening for Ctrl+C, recording every one via
    /// [`Interrupt::record`] until the process exits.
    ///
    /// Uses `tokio::signal::ctrl_c` rather than a Unix-specific `SIGINT`
    /// stream so this works on Windows too — unlike the `SIGWINCH` listener
    /// in [`crate::docker`], which has no Windows equivalent and is
    /// deliberately Unix-only. The spawned task is never awaited or
    /// cancelled: it lives as long as the process, matching the way Batect's
    /// own trap stays armed for the whole run so a *second* interrupt is
    /// still seen.
    pub fn listen(self: &Arc<Self>) {
        let interrupt = Arc::clone(self);

        tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    // Installing the handler failed, and retrying in a tight
                    // loop would spin. Nothing to report to the user: an
                    // interrupt they haven't sent yet isn't a problem they
                    // can act on, and the run itself is unaffected.
                    tracing::debug!("Could not listen for interrupts; Ctrl+C will not clean up");
                    return;
                }

                interrupt.record();
            }
        });
    }

    /// Records an interrupt, waking everything waiting on
    /// [`Interrupt::interrupted`].
    ///
    /// Public so a caller can inject one without a real signal — which is
    /// what makes the engine's own behaviour testable.
    pub fn record(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// How many interrupts have been recorded so far.
    pub fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    /// Whether at least one interrupt has been recorded.
    pub fn is_interrupted(&self) -> bool {
        self.count() > 0
    }

    /// Resolves once at least one interrupt has been recorded — immediately
    /// if one already has.
    pub async fn interrupted(&self) {
        self.wait_for(1).await;
    }

    /// Resolves once at least `count` interrupts have been recorded —
    /// immediately if that many already have.
    ///
    /// The `notified()`-before-check ordering matters and isn't incidental:
    /// [`Notify::notify_waiters`] wakes only the waiters that already exist,
    /// so registering interest *first* and checking the count *second* is
    /// what stops an interrupt arriving between the two from being missed
    /// and hanging the caller forever.
    pub async fn wait_for(&self, count: usize) {
        loop {
            let notified = self.notify.notified();

            if self.count() >= count {
                return;
            }

            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
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
}
