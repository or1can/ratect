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

//! Termination-signal (Ctrl+C, `SIGTERM`, `SIGHUP`) tracking, so a cancelled
//! run still cleans up after itself.
//!
//! Without this, the signal kills the process outright and every container the
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
//! # Every terminating signal, not only `SIGINT`
//!
//! Batect traps `SIGINT` alone, and so did Ratect until 0.26.0 — which left
//! the very leak the trap exists to prevent reachable by another route.
//! `ratect-compat` run as a long-lived subprocess by an editor is stopped
//! with `SIGTERM` when that editor closes or restarts it, and one developer
//! machine reached 29 abandoned networks against Docker's default pool of
//! roughly 31, at which point *every* Ratect run on it failed with `all
//! predefined address pools have been fully subnetted`. Networks leak faster
//! than containers, because a run that fails during startup leaks one too:
//! the rate is per process launch rather than per session.
//!
//! So [`TerminationSignal::ALL`] is trapped, all three down the one path —
//! the engine's meaning of "the user wants out" doesn't depend on how they
//! said it. What the signal *does* decide is the process's exit code
//! ([`TerminationSignal::exit_code`], the shell's own convention) and how
//! the failure describes itself, since "interrupted" and "press Ctrl+C again"
//! are both untrue of a process an init system stopped.
//!
//! `SIGKILL` cannot be trapped by anything, so the leak stays possible and
//! [`crate::labels`] remain the backstop — read by `ratect resources clean`,
//! which is a `ratect` verb with no `ratect-compat` equivalent, so from that
//! binary the sweep is `docker` filtering on the same labels. The docs say so
//! rather than implying a guarantee this can't make.
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
//! The engine's rule is *relative*: it compares against the count when
//! cleanup started, not a fixed `>= 2`. Arming the handler replaces the
//! process's default behaviour for all three signals for the whole run, so a
//! signal the engine doesn't act on is one it has silently swallowed — and a fixed
//! threshold swallows the first Ctrl+C during the cleanup of a run that was
//! never interrupted, which is the common case rather than an exotic one.
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
//!
//! # Two invariants to preserve
//!
//! [`Interrupt::wait_for`] calls `notified()` *before* checking the
//! count. `Notify::notify_waiters` wakes only waiters that already exist, so
//! checking first would let an interrupt land in the gap and hang the caller
//! forever; it has its own regression test.
//!
//! [`Interrupt::listen`] spawns, so it can only be called from inside a runtime. That
//! is why both binaries arm it from their async path rather than from the
//! synchronous `engine_settings` their flag-mapping tests call directly.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

/// A signal Ratect traps so a run cleans up after itself instead of being
/// killed where it stands.
///
/// Three of them, and the set is closed: these are the terminating signals a
/// task runner is actually sent by a shell, an editor, an init system or a CI
/// agent. `SIGQUIT` is deliberately left alone — its documented job is to
/// dump core *now*, so trapping it to do several seconds of cleanup would
/// take away the one signal that is meant to skip exactly that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationSignal {
    /// `SIGINT` — Ctrl+C in a non-raw terminal.
    Interrupt,
    /// `SIGTERM` — the polite stop an editor, init system or `kill` sends.
    Terminate,
    /// `SIGHUP` — the controlling terminal went away.
    Hangup,
}

impl TerminationSignal {
    /// Every signal trapped, in the order [`Interrupt::listen`] registers
    /// them. Iterated rather than written out at each site, so adding a
    /// fourth is one edit rather than three.
    pub const ALL: [TerminationSignal; 3] = [Self::Interrupt, Self::Terminate, Self::Hangup];

    /// The signal's own number.
    ///
    /// Written out rather than taken from `SignalKind::as_raw_value`, which
    /// is Unix-only — [`TerminationSignal::exit_code`] is not, and these
    /// three numbers are fixed by POSIX rather than varying by platform.
    ///
    /// Private: what a caller outside this module wants is the exit code or
    /// the name, and the raw number is only how those two are derived and
    /// how [`Interrupt`] stores the last signal seen.
    fn number(self) -> i32 {
        match self {
            Self::Interrupt => 2,
            Self::Terminate => 15,
            Self::Hangup => 1,
        }
    }

    /// The process exit code for a run this signal ended: 128 + its number,
    /// the shell's own convention for "killed by this signal" — 130
    /// interrupted, 143 terminated, 129 hung up on.
    ///
    /// This signal's half of the mapping in [`crate::exit_code`], which is
    /// where the whole of it lives and why.
    pub fn exit_code(self) -> u8 {
        // Every signal here is numbered well below 128, so the sum is a
        // valid exit code and the cast cannot truncate.
        128 + self.number() as u8
    }

    /// The signal's name, for a message that has to say which one arrived.
    pub fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Hangup => "SIGHUP",
        }
    }

    /// How a run ended by this signal describes itself. `SIGINT` keeps the
    /// bare "Interrupted" it has always printed — that is what the user did,
    /// and naming the signal there would explain a keystroke to the person
    /// who pressed it. The other two name themselves, because nothing was
    /// interrupted and nobody pressed anything.
    pub fn ended_run(self) -> &'static str {
        match self {
            Self::Interrupt => "Interrupted",
            Self::Terminate => "Terminated by SIGTERM",
            Self::Hangup => "Terminated by SIGHUP",
        }
    }

    /// How to ask a second time — the press or signal that abandons the
    /// cleanup as well as the run.
    pub fn send_again(self) -> &'static str {
        match self {
            Self::Interrupt => "Press Ctrl+C again",
            Self::Terminate => "Send SIGTERM again",
            Self::Hangup => "Send SIGHUP again",
        }
    }

    #[cfg(unix)]
    fn kind(self) -> tokio::signal::unix::SignalKind {
        use tokio::signal::unix::SignalKind;
        match self {
            Self::Interrupt => SignalKind::interrupt(),
            Self::Terminate => SignalKind::terminate(),
            Self::Hangup => SignalKind::hangup(),
        }
    }

    /// The signal [`TerminationSignal::number`] names, or
    /// [`TerminationSignal::Interrupt`] for anything else.
    ///
    /// [`Interrupt`] stores the last signal seen as its number in a single
    /// atomic, and this reads it back. The "anything else" case is not
    /// defensive padding: zero is what that atomic holds before any signal
    /// arrives (it is [`Default`]'s), zero is not a signal number, and a run
    /// that was never signalled is reported exactly as an interrupted one
    /// would be. Total rather than fallible for that reason — there is no
    /// caller that could act on the difference.
    fn from_number(number: usize) -> Self {
        Self::ALL
            .into_iter()
            .find(|signal| signal.number() as usize == number)
            .unwrap_or(Self::Interrupt)
    }
}

/// The error a task fails with when a termination signal ends it.
///
/// A distinct type rather than a plain message so the binaries can map it to
/// their own exit code, the way they already do for
/// [`crate::docker::ContainerExitedNonZero`] — and it carries the signal
/// because that code is 128 + the signal's own number, so "which one" is not
/// something the binaries can recover any other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskInterrupted {
    /// The signal that ended the run.
    pub signal: TerminationSignal,
}

impl TaskInterrupted {
    /// The failure for a run ended by `signal`.
    pub fn new(signal: TerminationSignal) -> Self {
        Self { signal }
    }
}

impl std::fmt::Display for TaskInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.signal.ended_run())
    }
}

impl std::error::Error for TaskInterrupted {}

/// Tracks termination signals for one invocation.
///
/// Cheap to clone as an [`Arc`]; every holder observes the same count.
#[derive(Debug, Default)]
pub struct Interrupt {
    count: AtomicUsize,
    /// The most recent signal's [`TerminationSignal::number`], read back
    /// through [`TerminationSignal::from_number`]. Zero — which is what
    /// [`Default`] gives and what a run that was never signalled keeps — is
    /// no signal's number and reads as [`TerminationSignal::Interrupt`], so
    /// the exit-code path can read this unconditionally.
    last_signal: AtomicUsize,
    notify: Notify,
}

impl Interrupt {
    /// A new tracker with nothing recorded, listening for nothing —
    /// call [`Interrupt::listen`] to attach it to the real signals.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Starts listening for every [`TerminationSignal`], recording each one
    /// via [`Interrupt::record_signal`] until the process exits.
    ///
    /// One spawned task per signal rather than one task selecting over all
    /// three: each stream is then subscribed *once* and received from in a
    /// loop, which is what matters here. Re-subscribing per iteration
    /// (`tokio::signal::ctrl_c()` in a loop) drops and rebuilds the
    /// subscription each time, and a signal landing in that gap is
    /// discarded — precisely the gap this feature cares about, since the
    /// interesting *second* signal often follows hard on the first.
    ///
    /// Signals coalesce regardless (the OS and tokio both collapse several
    /// pending `SIGINT`s into one notification), so two presses in the same
    /// instant can still count once. That is inherent to signal delivery
    /// rather than something a listener can fix, and it is harmless here:
    /// the presses this feature cares about are seconds apart, on either
    /// side of a decision the user is watching happen.
    ///
    /// Off Unix there are no `SignalKind` streams at all, so this keeps the
    /// portable `tokio::signal::ctrl_c` and the rebuild gap along with it —
    /// unlike the `SIGWINCH` listener in [`crate::docker`], which has no
    /// Windows equivalent and is deliberately Unix-only. The spawned tasks
    /// are never awaited or cancelled: they live as long as the process,
    /// matching the way Batect's own trap stays armed for the whole run so a
    /// second signal is still seen.
    #[cfg(unix)]
    pub fn listen(self: &Arc<Self>) {
        for signal in TerminationSignal::ALL {
            let interrupt = Arc::clone(self);

            tokio::spawn(async move {
                let mut stream = match tokio::signal::unix::signal(signal.kind()) {
                    Ok(stream) => stream,
                    Err(error) => {
                        // Nothing to report to the user: a signal they
                        // haven't sent yet isn't a problem they can act on,
                        // and the run itself is unaffected.
                        tracing::debug!(
                            ?error,
                            signal = signal.name(),
                            "Could not listen for this signal; it will not clean up"
                        );
                        return;
                    }
                };

                while stream.recv().await.is_some() {
                    interrupt.record_signal(signal);
                }

                tracing::debug!(
                    signal = signal.name(),
                    "Signal listener closed; it will not clean up"
                );
            });
        }
    }

    /// Starts listening for Ctrl+C, recording every one via
    /// [`Interrupt::record`] until the process exits — see the Unix version
    /// of this method for the whole story.
    #[cfg(not(unix))]
    pub fn listen(self: &Arc<Self>) {
        let interrupt = Arc::clone(self);

        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                interrupt.record();
            }

            tracing::debug!("Interrupt listener closed; Ctrl+C will not clean up");
        });
    }

    /// Records an interrupt (`SIGINT`), waking everything waiting on
    /// [`Interrupt::interrupted`].
    ///
    /// Public so a caller can inject one without a real signal — which is
    /// what makes the engine's own behaviour testable.
    pub fn record(&self) {
        self.record_signal(TerminationSignal::Interrupt);
    }

    /// Records `signal`, waking everything waiting on
    /// [`Interrupt::interrupted`].
    ///
    /// The signal is stored *before* the count is raised, so anything that
    /// sees the new count also sees what caused it.
    pub fn record_signal(&self, signal: TerminationSignal) {
        self.last_signal
            .store(signal.number() as usize, Ordering::SeqCst);
        self.count.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// The most recent signal recorded — [`TerminationSignal::Interrupt`]
    /// when none has been, so a caller need not handle "no signal yet"
    /// separately from the case it would report identically anyway.
    pub fn last_signal(&self) -> TerminationSignal {
        TerminationSignal::from_number(self.last_signal.load(Ordering::SeqCst))
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
    /// `Notify::notify_waiters` wakes only the waiters that already exist,
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
