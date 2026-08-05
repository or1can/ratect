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
            // Subscribed *once*, then received from in a loop. Calling
            // `tokio::signal::ctrl_c()` per iteration would instead drop the
            // subscription and rebuild it each time, and a signal landing in
            // that gap is discarded — which is precisely the gap that
            // matters here, since the interesting second interrupt often
            // follows hard on the first.
            //
            // Signals coalesce regardless of this (the OS and tokio both
            // collapse several pending `SIGINT`s into one notification), so
            // two presses in the same instant can still count once. That's
            // inherent to signal delivery rather than something a listener
            // can fix, and it's harmless here: the presses this feature
            // cares about are seconds apart, on either side of a decision
            // the user is watching happen.
            #[cfg(unix)]
            let mut signal =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                    Ok(signal) => signal,
                    Err(error) => {
                        // Nothing to report to the user: an interrupt they
                        // haven't sent yet isn't a problem they can act on,
                        // and the run itself is unaffected.
                        tracing::debug!(
                            ?error,
                            "Could not listen for interrupts; Ctrl+C will not clean up"
                        );
                        return;
                    }
                };

            loop {
                #[cfg(unix)]
                let received = signal.recv().await.is_some();
                // No `SignalKind` equivalent off Unix, so this keeps the
                // portable `ctrl_c` and the rebuild gap along with it.
                #[cfg(not(unix))]
                let received = tokio::signal::ctrl_c().await.is_ok();

                if !received {
                    tracing::debug!("Interrupt listener closed; Ctrl+C will not clean up");
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
#[path = "interrupt_tests.rs"]
mod tests;
