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

//! The `all` output mode — a port of Batect's `InterleavedEventLogger`/
//! `InterleavedOutput`: every line of output, Ratect's own milestones and
//! *all* containers' stdout/stderr alike, prefixed with the container it
//! belongs to (`name    | `), padded to a common column and colored
//! round-robin so concurrent containers stay tellable-apart. The only mode
//! that shows dependency output, setup-command output, and full image-build
//! output at all. Its I/O policy ([`ContainerIoStreaming::Interleaved`])
//! means no container gets a TTY or stdin and every container gets
//! `TERM=dumb`, matching Batect.
//!
//! Differences from Batect's implementation, all deliberate:
//! - Batect's own status lines carry a second inner `Batect | ` prefix
//!   (`build | Batect | Running build...`); Ratect drops that inner prefix —
//!   the outer one already says whose line it is, and Ratect's milestone
//!   wording is unambiguous about being Ratect's own.
//! - A container's output is split into lines on a lone `\r` as well as on
//!   `\n`, so a progress redraw prints rather than piling up — see
//!   [`LineBuffer`].
//! - A container's last line is printed even without a trailing newline;
//!   Batect loses it.
//! - A failure is an unprefixed `Error: …` line on stderr, as in every
//!   style, where Batect prints `<task> ! `/`<container> ! ` lines on
//!   stdout; and a failed setup command or network check is reported, where
//!   Batect's `all` prints nothing for either.
//! - The inner `Setup command N | ` and `Image build | ` labels are in the
//!   default foreground, where Batect paints them white (decisions/0017).
//!
//! The prefix colours also differ — see `CONTAINER_COLORS`.
//!
//! Streams (decisions/0017): every prefixed line goes to the progress
//! console — stdout under `ratect-compat` (Batect's placement, kept), stderr
//! under `ratect` — *except* the task container's own output lines, which
//! are the run's result and go to stdout in both binaries, prefixed like
//! everything else so `all` still reads as one interleaved log on a
//! terminal. So `ratect run x -o all 2>/dev/null` is the task's own lines
//! and nothing else, and `ratect-compat -o all` is unchanged.

use super::{Color, Console, ContainerIoStreaming, EventSink, OnceFlag, TaskEvent};
use std::collections::HashMap;
use std::sync::Mutex;
use unicode_width::UnicodeWidthStr;

/// The prefix colors assigned to containers, round-robin in sorted name
/// order, so a container's colour doesn't change between runs — in
/// `docker compose`'s order, which keeps blue and cyan off neighbouring
/// containers until the cycle wraps; red is reserved for errors, and the
/// task-level prefix is bold in the default foreground (decisions/0017).
/// Batect cycles six, starting at black, assigns by graph order, and paints
/// the task prefix white.
const CONTAINER_COLORS: [Color; 5] = [
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Magenta,
    Color::Blue,
];

pub struct InterleavedEventLogger {
    /// The progress console — see `create_event_sink`.
    console: Console,
    /// Errors, warnings and notices, in every style — unprefixed here
    /// (ratect#367).
    stderr: Console,
    /// The task container's own output lines — the run's result, so stdout
    /// in both binaries (the same stream as `console` under `ratect-compat`).
    result: Console,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The current task's name — the prefix for task-level lines (bold,
    /// default foreground).
    task: Option<String>,
    /// The task's own container's name, from `TaskGraphResolved` — whose
    /// `ContainerOutput` lines go to `result` rather than `console`.
    task_container: Option<String>,
    /// Deferred `Running <task>...` preamble: printed once the graph
    /// arrives, so it aligns with the container prefixes (the padding width
    /// isn't known until then).
    preamble_pending: bool,
    /// Container name -> its assigned prefix color.
    colors: HashMap<String, Color>,
    /// Container name -> its image reference, for fanning a pull line out
    /// to every container that uses that image (Batect does the same).
    images: HashMap<String, String>,
    /// Container name -> its build tag, mapping `ImageBuildProgress` (keyed
    /// by tag) back to a container.
    build_tags: HashMap<String, String>,
    /// The common prefix column width, in terminal display columns (not
    /// bytes or `char`s — see [`UnicodeWidthStr`]): the longest of the
    /// container names and the task's own name.
    prefix_width: usize,
    /// Guards "Cleaning up..." printing once per task.
    started_cleanup: OnceFlag,
}

impl InterleavedEventLogger {
    pub fn new(console: Console, stderr: Console, result: Console) -> Self {
        Self {
            console,
            stderr,
            result,
            state: Mutex::new(State::default()),
        }
    }

    /// `line` under `name`'s prefix, padded to the common column and
    /// styled — bold always, in `color` when there is one, the default
    /// foreground otherwise (never white: decisions/0017) — written to
    /// `console`.
    fn print_prefixed(
        &self,
        console: &Console,
        state: &State,
        name: &str,
        color: Option<Color>,
        line: &str,
    ) {
        // Padded by hand (not `format!("{name:width$}")`, which pads by
        // `char` count) — `prefix_width` is a display-column measurement,
        // and `char` count agrees with that only for single-width
        // characters. A CJK container name is 2 display columns per
        // character but 1 `char` each, so `{:width$}` would under-pad it
        // and break alignment with every other column.
        let padding = " ".repeat(state.prefix_width.saturating_sub(name.width()));
        let padded = format!("{name}{padding}");
        // One span, one reset; the separator outside it, unstyled
        // (decisions/0017) where Batect bolds and colours it too.
        let prefix = match color {
            Some(color) => console.bold_colored(color, &padded),
            None => console.bold(&padded),
        };
        console.println(&format!("{prefix} | {line}"));
    }

    /// A milestone line belonging to `container`, on the progress console.
    fn print_for_container(&self, state: &State, container: &str, line: &str) {
        let color = state.colors.get(container).copied();
        self.print_prefixed(&self.console, state, container, color, line);
    }

    /// A task-level line (the task's own prefix), on the progress console.
    fn print_for_task(&self, state: &State, line: &str) {
        let task = state.task.clone().unwrap_or_default();
        self.print_prefixed(&self.console, state, &task, None, line);
    }

    /// Prints the deferred "Running `<task>`..." preamble if it hasn't
    /// already gone out — called both once the graph normally arrives
    /// (`TaskGraphResolved`) and, as a fallback, on `RunEnded`, so an
    /// infrastructure failure early enough that the graph never resolved
    /// still gets this task's own line out before the error.
    fn flush_preamble_if_pending(&self, state: &mut State) {
        if !state.preamble_pending {
            return;
        }
        state.preamble_pending = false;
        let task = state.task.clone().unwrap_or_default();
        self.print_for_task(state, &format!("Running {}...", self.console.bold(&task)));
    }

    /// Every container using `image`, sorted — a pull belongs to all of
    /// them at once.
    fn containers_for_image(state: &State, image: &str) -> Vec<String> {
        let mut names: Vec<String> = state
            .images
            .iter()
            .filter(|(_, container_image)| container_image.as_str() == image)
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    fn container_for_build_tag(state: &State, tag: &str) -> Option<String> {
        state
            .build_tags
            .iter()
            .find(|(_, build_tag)| build_tag.as_str() == tag)
            .map(|(name, _)| name.clone())
    }
}

impl EventSink for InterleavedEventLogger {
    fn container_io_streaming(&self) -> ContainerIoStreaming {
        ContainerIoStreaming::Interleaved
    }

    fn wants_progress_detail(&self) -> bool {
        true
    }

    /// The `result` console's alone: the task container's lines are the only
    /// writes whose failure ends a run, and a closed stderr never does
    /// (ratect#669).
    fn result_reader_gone(&self) -> bool {
        self.result.reader_gone()
    }

    fn post(&self, event: TaskEvent) {
        if self.stderr.print_stderr_class(&event) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        match event {
            TaskEvent::TaskStarting { task } => {
                *state = State {
                    task: Some(task),
                    preamble_pending: true,
                    ..State::default()
                };
            }
            // On this task's own prefix, never the last task's: it has no
            // containers, so its own name is the whole column.
            TaskEvent::PrerequisitesOnly { task } => {
                *state = State {
                    prefix_width: task.width(),
                    task: Some(task.clone()),
                    ..State::default()
                };
                self.print_for_task(&state, &super::nothing_more_to_do(&self.console, &task));
            }
            TaskEvent::TaskGraphResolved { containers } => {
                state.task_container = containers
                    .iter()
                    .find(|info| info.is_task_container)
                    .map(|info| info.name.clone());
                let task_width = state.task.as_deref().map_or(0, UnicodeWidthStr::width);
                state.prefix_width = containers
                    .iter()
                    .map(|info| info.name.width())
                    .chain([task_width])
                    .max()
                    .unwrap_or(0);
                let mut names: Vec<&str> =
                    containers.iter().map(|info| info.name.as_str()).collect();
                names.sort_unstable();
                state.colors = names
                    .iter()
                    .zip(CONTAINER_COLORS.iter().cycle())
                    .map(|(name, color)| (name.to_string(), *color))
                    .collect();
                state.images = containers
                    .iter()
                    .filter_map(|info| {
                        info.image
                            .as_ref()
                            .map(|image| (info.name.clone(), image.clone()))
                    })
                    .collect();
                state.build_tags = containers
                    .iter()
                    .filter_map(|info| {
                        info.build_tag
                            .as_ref()
                            .map(|tag| (info.name.clone(), tag.clone()))
                    })
                    .collect();
                self.flush_preamble_if_pending(&mut state);
            }
            TaskEvent::ImagePullStarting { image } => {
                for container in Self::containers_for_image(&state, &image) {
                    self.print_for_container(
                        &state,
                        &container,
                        &format!("Pulling {}...", self.console.bold(&image)),
                    );
                }
            }
            // Pull progress detail stays off even here, matching Batect
            // (`all` shows pull start/finish lines only — build output is
            // the streamed detail it adds).
            TaskEvent::ImagePullProgress { .. } => {}
            // No line of its own — this container's own Pulling/Pulled or
            // Building/Built lines above already cover the "image ready"
            // moment when a pull/build actually happened; when neither did
            // (an already-local image, or resolved earlier this
            // invocation), there was never anything to announce here in
            // the first place.
            TaskEvent::ImageResolved { .. } => {}
            // Only the fancy logger's cleanup countdown needs to know a
            // container exists before it is started; here the container's
            // own "Container removed." line during cleanup is enough, and
            // Batect's own interleaved logger reports no creation line
            // either — for the create's start or its end.
            TaskEvent::ContainerCreating { .. } | TaskEvent::ContainerCreated { .. } => {}
            TaskEvent::ImagePullCompleted { image } => {
                for container in Self::containers_for_image(&state, &image) {
                    self.print_for_container(
                        &state,
                        &container,
                        &format!("Pulled {}.", self.console.bold(&image)),
                    );
                }
            }
            TaskEvent::ImageBuildStarting { container } => {
                self.print_for_container(&state, &container, "Building image...");
            }
            TaskEvent::ImageBuildProgress { tag, message } => {
                if let Some(container) = Self::container_for_build_tag(&state, &tag) {
                    self.print_for_container(
                        &state,
                        &container,
                        &format!("Image build | {message}"),
                    );
                }
            }
            TaskEvent::ImageBuildCompleted { container } => {
                self.print_for_container(&state, &container, "Image built.");
            }
            TaskEvent::DependencyStarting { container } => {
                self.print_for_container(&state, &container, "Starting container...");
            }
            TaskEvent::DependencyStarted { container } => {
                self.print_for_container(&state, &container, "Container started.");
            }
            TaskEvent::ContainerBecameHealthy { container } => {
                self.print_for_container(&state, &container, "Container became healthy.");
            }
            TaskEvent::DependencyCompleted { container } => {
                self.print_for_container(&state, &container, "Container ran to completion.");
            }
            TaskEvent::RunningSetupCommand {
                container,
                command,
                index,
                total,
                run_in,
            } => {
                // The line is already prefixed with `container`, so only the
                // container it runs *in* needs naming, and only when that
                // isn't the same one.
                let elsewhere = run_in.map_or(String::new(), |run_in| format!(" in {run_in}"));
                self.print_for_container(
                    &state,
                    &container,
                    &format!(
                        "Running setup command {} ({index} of {total}){elsewhere}...",
                        self.console.bold(&command)
                    ),
                );
            }
            TaskEvent::SetupCommandOutput {
                container,
                index,
                line,
            } => {
                self.print_for_container(
                    &state,
                    &container,
                    &format!("Setup command {index} | {line}"),
                );
            }
            TaskEvent::SetupCommandsCompleted { container } => {
                self.print_for_container(
                    &state,
                    &container,
                    "Container has completed all setup commands.",
                );
            }
            TaskEvent::DependencyExitedUnexpectedly {
                container,
                exit_code,
            } => {
                self.print_for_container(
                    &state,
                    &container,
                    &format!("{}.", super::exited_unexpectedly(exit_code)),
                );
            }
            TaskEvent::RunningTaskContainer { container, command } => {
                let line = match command {
                    Some(command) => format!("Running {}...", self.console.bold(&command)),
                    None => "Running...".to_string(),
                };
                self.print_for_container(&state, &container, &line);
            }
            TaskEvent::ContainerOutput { container, line } => {
                // The task container's lines are the result — stdout, in
                // both binaries (see the module docs); a dependency's go
                // with the progress.
                if state.task_container.as_deref() == Some(container.as_str()) {
                    let color = state.colors.get(&container).copied();
                    self.print_prefixed(&self.result, &state, &container, color, &line);
                } else {
                    self.print_for_container(&state, &container, &line);
                }
            }
            TaskEvent::CleanupStarting => {
                if state.started_cleanup.fire_once() {
                    self.print_for_task(&state, "Cleaning up...");
                }
            }
            TaskEvent::ContainerRemoved { container } => {
                self.print_for_container(&state, &container, "Container removed.");
            }
            TaskEvent::RemovingNetwork => {
                self.print_for_task(&state, "Removing task network...");
            }
            TaskEvent::TaskFinished {
                task,
                exit_code,
                duration,
            } => {
                self.print_for_task(
                    &state,
                    &super::format_task_summary(
                        &self.console,
                        &self.console.bold(&task),
                        exit_code,
                        duration,
                    ),
                );
            }
            // Nothing to show of its own — `all` has no live display to
            // stop — but an infrastructure failure early enough that
            // TaskGraphResolved never posted (e.g. `--use-network` naming a
            // nonexistent network) would otherwise leave the deferred
            // preamble (see TaskStarting/TaskGraphResolved above) stuck
            // unprinted forever, so this task's prefix never appeared at
            // all. Flushed here, the moment the run ends, so it still lands
            // before the `Error` event that reports such a failure —
            // `prefix_width` may still be 0 (no graph arrived to size it
            // against), which just means no padding beyond the task name
            // itself, same as a single-container task would get anyway.
            TaskEvent::RunEnded { .. } => {
                self.flush_preamble_if_pending(&mut state);
            }
            // The error itself was an `Error` event, before cleanup.
            TaskEvent::TaskFailed { task, duration } => {
                self.print_for_task(
                    &state,
                    &super::format_task_failed_summary(&self.console, &task, duration),
                );
            }
            // `CleanupStarting`'s line already said cleanup began.
            TaskEvent::CleanupFinished => {}
            // Printed on stderr before the match (`print_stderr_class`).
            TaskEvent::Error { .. } | TaskEvent::Warning { .. } | TaskEvent::Notice { .. } => {}
        }
    }
}

/// Turns a container's raw output chunks into whole lines: buffers until a
/// `\n` *or* a lone `\r` (one not immediately followed by `\n` — a CRLF pair
/// still folds to one line break, not two), and hands each complete line to
/// `emit`. [`LineBuffer::flush`] emits any unterminated tail when the stream
/// ends. Deliberately **not** a port of Batect's own
/// `InterleavedContainerOutputSink`, which splits on `\n` alone: a
/// carriage-return progress redraw (pip/curl/apt-style) would otherwise
/// produce no output until the next `\n`, then dump one giant concatenated
/// line. Nor does Batect's sink emit an unterminated tail: only its `close`
/// does, and nothing in production closes it, so that last line is lost —
/// see `docs/differences-from-batect.md`'s own entries for both. `docker.rs`
/// drives one of these per streamed container.
pub struct LineBuffer {
    pending: Vec<u8>,
    /// Set on seeing a `\r` whose successor isn't known yet — resolved by
    /// the very next byte, whichever call it arrives on: `\n` folds into
    /// one line break (CRLF), anything else means the `\r` was a lone
    /// flush boundary of its own and that next byte starts fresh content.
    /// A field rather than a `push`-local, since the two bytes of a CRLF
    /// pair can land in different chunks (and therefore different `push`
    /// calls) when Docker's stream happens to split there.
    pending_cr: bool,
}

impl LineBuffer {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            pending_cr: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8], mut emit: impl FnMut(&str)) {
        for &byte in chunk {
            if self.pending_cr {
                self.pending_cr = false;
                if byte == b'\n' {
                    // CRLF: one line break, not two.
                    self.flush_line(&mut emit);
                    continue;
                }
                // A lone `\r` — its own flush boundary. `byte` wasn't part
                // of it, so it still falls through to the match below.
                self.flush_line(&mut emit);
            }
            match byte {
                b'\r' => self.pending_cr = true,
                b'\n' => self.flush_line(&mut emit),
                _ => self.pending.push(byte),
            }
        }
    }

    pub fn flush(&mut self, mut emit: impl FnMut(&str)) {
        // Nothing will ever arrive to resolve a still-pending `\r` at this
        // point (the stream ended) — the content buffered before it is the
        // whole story either way, so it's flushed the same as any other
        // unterminated tail; the `\r` itself is simply dropped, never
        // emitted.
        self.pending_cr = false;
        if !self.pending.is_empty() {
            self.flush_line(&mut emit);
        }
    }

    fn flush_line(&mut self, emit: &mut impl FnMut(&str)) {
        let line = std::mem::take(&mut self.pending);
        emit(&String::from_utf8_lossy(&line));
    }
}

#[cfg(test)]
#[path = "interleaved_tests.rs"]
mod tests;
