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

//! The `simple` output mode — a port of Batect's `SimpleEventLogger`:
//! append-only milestone lines, one per lifecycle event, with no
//! live-updating progress detail at all (pull/build progress events are
//! deliberately ignored — only their start/finish milestones print). The
//! mode every non-interactive console gets by default.
//!
//! Under `ratect-compat` the progress console is stdout, so these milestone
//! lines interleave with the task container's own raw output on the same
//! stream, unmarked, matching Batect's own `simple` mode (a deliberate,
//! CHANGELOG-documented parity change, 0.16.0, and kept by
//! decisions/0017: a prefix would change every CI log captured from a
//! Batect project); `-o quiet` is that binary's escape for a script that
//! needs exact container-output-only stdout. Under `ratect` the progress
//! console is stderr and the question doesn't arise.

use super::{Console, EventSink, OnceFlag, TaskEvent};
use std::sync::Mutex;

pub struct SimpleEventLogger {
    /// The progress console — see `create_event_sink`.
    console: Console,
    /// Errors, warnings and notices, in every style.
    stderr: Console,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Guards the blank separator line between one task's output and the
    /// next's ("Running x..." → work → blank → "Running y..."), matching
    /// Batect's `SessionRunner` printing a separator between prerequisite
    /// tasks — no blank before the very first.
    printed_a_task: bool,
    /// Guards "Cleaning up..." printing once per task even though several
    /// cleanup-worthy events may follow.
    started_cleanup: OnceFlag,
    /// The task's own container's name, from `TaskGraphResolved` — used only
    /// to suppress its own `ContainerBecameHealthy`/`RunningSetupCommand`/
    /// `SetupCommandsCompleted` lines (see [`SimpleEventLogger::is_task_container`]).
    /// Reset per task the same way `started_cleanup` is.
    task_container: Option<String>,
}

impl SimpleEventLogger {
    pub fn new(console: Console, stderr: Console) -> Self {
        Self {
            console,
            stderr,
            state: Mutex::new(State::default()),
        }
    }
}

impl EventSink for SimpleEventLogger {
    fn post(&self, event: TaskEvent) {
        if self.stderr.print_stderr_class(&event) {
            return;
        }
        match event {
            TaskEvent::TaskStarting { task } => {
                let mut state = self.state.lock().unwrap();
                if state.printed_a_task {
                    self.console.println("");
                }
                state.printed_a_task = true;
                // A fresh task execution gets a fresh cleanup guard — each
                // task tears down its own network/dependencies (see
                // docs/task-lifecycle.md), so each prints its own
                // "Cleaning up...".
                state.started_cleanup.reset();
                state.task_container = None;
                self.console
                    .println(&format!("Running {}...", self.console.bold(&task)));
            }
            // In place of a task's `Running` line, so separated the same way.
            TaskEvent::PrerequisitesOnly { task } => {
                let mut state = self.state.lock().unwrap();
                if state.printed_a_task {
                    self.console.println("");
                }
                state.printed_a_task = true;
                self.console
                    .println(&super::nothing_more_to_do(&self.console, &task));
            }
            TaskEvent::TaskFinished {
                task,
                exit_code,
                duration,
            } => {
                self.console.println(&super::format_task_summary(
                    &self.console,
                    &self.console.bold(&task),
                    exit_code,
                    duration,
                ));
            }
            TaskEvent::TaskFailed { task, duration } => {
                self.console.println(&super::format_task_failed_summary(
                    &self.console,
                    &task,
                    duration,
                ));
            }
            TaskEvent::ImagePullStarting { image } => {
                self.console
                    .println(&format!("Pulling {}...", self.console.bold(&image)));
            }
            TaskEvent::ImagePullCompleted { image } => {
                self.console
                    .println(&format!("Pulled {}.", self.console.bold(&image)));
            }
            TaskEvent::ImageBuildStarting { container } => {
                self.console
                    .println(&format!("Building {}...", self.console.bold(&container)));
            }
            TaskEvent::ImageBuildCompleted { container } => {
                self.console
                    .println(&format!("Built {}.", self.console.bold(&container)));
            }
            TaskEvent::DependencyStarting { container } => {
                self.console
                    .println(&format!("Starting {}...", self.console.bold(&container)));
            }
            TaskEvent::DependencyStarted { container } => {
                self.console
                    .println(&format!("Started {}.", self.console.bold(&container)));
            }
            TaskEvent::ContainerBecameHealthy { container } => {
                if self.is_task_container(&container) {
                    return;
                }
                self.console.println(&format!(
                    "{} has become healthy.",
                    self.console.bold(&container)
                ));
            }
            TaskEvent::DependencyCompleted { container } => {
                self.console.println(&format!(
                    "{} ran to completion.",
                    self.console.bold(&container)
                ));
            }
            TaskEvent::RunningSetupCommand {
                container,
                command,
                index,
                total,
                run_in,
            } => {
                if self.is_task_container(&container) {
                    return;
                }
                // Both names when they differ: this line is flat, with no
                // per-container prefix to say whose gate it belongs to, so
                // dropping either one loses something.
                let bold = |text: &str| self.console.bold(text);
                let location = match run_in {
                    Some(run_in) => format!("{}, for {}", bold(&run_in), bold(&container)),
                    None => bold(&container),
                };
                self.console.println(&format!(
                    "Running setup command {} ({index} of {total}) in {location}...",
                    bold(&command)
                ));
            }
            TaskEvent::SetupCommandsCompleted { container } => {
                if self.is_task_container(&container) {
                    return;
                }
                self.console.println(&format!(
                    "{} has completed all setup commands.",
                    self.console.bold(&container)
                ));
            }
            TaskEvent::DependencyExitedUnexpectedly {
                container,
                exit_code,
            } => {
                self.console.println(&format!(
                    "{} {}.",
                    self.console.bold(&container),
                    super::exited_unexpectedly(exit_code)
                ));
            }
            TaskEvent::RunningTaskContainer { container, command } => {
                let bold = |text: &str| self.console.bold(text);
                let line = match command {
                    Some(command) => {
                        format!("Running {} in {}...", bold(&command), bold(&container))
                    }
                    None => format!("Running {}...", bold(&container)),
                };
                self.console.println(&line);
            }
            TaskEvent::CleanupStarting { cause } => {
                let mut state = self.state.lock().unwrap();
                if state.started_cleanup.fire_once() {
                    self.console.println("");
                    self.console
                        .println(&super::cleaning_up(&self.console, cause.as_ref()));
                }
            }
            // Recorded only so `is_task_container` can recognize the task's
            // own container above — simple mode otherwise has no use for the
            // graph itself (unlike fancy/interleaved, which use it for
            // layout).
            TaskEvent::TaskGraphResolved { containers } => {
                self.state.lock().unwrap().task_container = containers
                    .into_iter()
                    .find(|c| c.is_task_container)
                    .map(|c| c.name);
            }
            // No live progress detail in simple mode — the whole point of
            // the mode (matching Batect: `ImagePullProgressEvent`/
            // `ImageBuildProgressEvent` are unhandled there too). The
            // creation and per-step cleanup events only feed fancy's live
            // displays (Batect's simple logger prints nothing for a create
            // either), and a failure's error was an `Error` event, before
            // cleanup, so none of them get a line here either.
            TaskEvent::ImagePullProgress { .. }
            | TaskEvent::ImageBuildProgress { .. }
            | TaskEvent::ImageResolved { .. }
            | TaskEvent::ContainerCreating { .. }
            | TaskEvent::ContainerCreated { .. }
            | TaskEvent::ContainerStopped { .. }
            | TaskEvent::ContainerRemoved { .. }
            | TaskEvent::RemovingNetwork
            | TaskEvent::NetworkRemoved
            | TaskEvent::RunEnded { .. }
            | TaskEvent::CleanupFinished
            | TaskEvent::ContainerOutput { .. }
            | TaskEvent::SetupCommandOutput { .. } => {}
            // Printed on stderr before the match (`print_stderr_class`).
            TaskEvent::Error { .. } | TaskEvent::Warning { .. } | TaskEvent::Notice { .. } => {}
        }
    }
}

impl SimpleEventLogger {
    /// Whether `container` is the task's own — the three readiness
    /// milestones (`ContainerBecameHealthy`/`RunningSetupCommand`/
    /// `SetupCommandsCompleted`) are silently dropped for it, matching
    /// Batect's own `SimpleEventLogger` (each of its three equivalent
    /// handlers opens with `if (container == taskContainer) return`).
    ///
    /// Since 0.21.0 those events fire for the task's own container too (see
    /// `engine.rs`'s `run_task_container_readiness`), and they'd print
    /// *while* its raw command output is streaming to this same stdout —
    /// output that has no framing of its own (no prefix, and often no
    /// trailing newline), so a milestone line can land glued onto the tail
    /// of whatever the command last wrote, on the very same terminal row,
    /// at a nondeterministic point. Simple mode's whole contract is that it
    /// never touches the task's own output, so the milestone is what gives
    /// way. Nothing is lost that matters: a readiness *failure* is still
    /// reported on stderr, as an `Error` event, and `all` mode still
    /// reports every one of these for every container (its output is
    /// line-buffered and prefixed, so it has no collision to avoid — same
    /// split Batect's own `InterleavedEventLogger` makes).
    ///
    /// A dependency container is unaffected: its own readiness always
    /// completes before the task container starts, so its lines can't
    /// collide with anything.
    fn is_task_container(&self, container: &str) -> bool {
        self.state.lock().unwrap().task_container.as_deref() == Some(container)
    }
}

#[cfg(test)]
#[path = "simple_tests.rs"]
mod tests;
