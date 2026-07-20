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
//! One simplification against Batect: its own status lines carry a second
//! inner `Batect | ` prefix (`build | Batect | Running build...`); Ratect
//! drops that inner prefix — the outer one already says whose line it is,
//! and Ratect's milestone wording is unambiguous about being Ratect's own.

use super::{Color, Console, ContainerIoStreaming, EventSink, OnceFlag, TaskEvent};
use std::collections::HashMap;
use std::sync::Mutex;
use unicode_width::UnicodeWidthStr;

/// The prefix colors assigned to containers, round-robin in (sorted) name
/// order — deliberately excluding white (task-level lines) and red
/// (errors), matching Batect.
const CONTAINER_COLORS: [Color; 5] = [
    Color::Blue,
    Color::Cyan,
    Color::Green,
    Color::Magenta,
    Color::Yellow,
];

pub struct InterleavedEventLogger {
    console: Console,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The current task's name — the prefix for task-level lines (white).
    task: Option<String>,
    /// Deferred "Running <task>..." preamble: printed once the graph
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
    pub fn new(console: Console) -> Self {
        Self {
            console,
            state: Mutex::new(State::default()),
        }
    }

    /// The logger `main.rs` actually wires up: real stdout, color iff
    /// stdout is a terminal and `--no-color` wasn't given.
    pub fn stdout(no_color: bool) -> Self {
        Self::new(Console::stdout(no_color))
    }

    fn print_prefixed(&self, state: &State, name: &str, color: Color, line: &str) {
        // Padded by hand (not `format!("{name:width$}")`, which pads by
        // `char` count) — `prefix_width` is a display-column measurement,
        // and `char` count agrees with that only for single-width
        // characters. A CJK container name is 2 display columns per
        // character but 1 `char` each, so `{:width$}` would under-pad it
        // and break alignment with every other column.
        let padding = " ".repeat(state.prefix_width.saturating_sub(name.width()));
        let padded = format!("{name}{padding}");
        let prefix = self.console.colored(color, &self.console.bold(&padded));
        self.console.println(&format!("{prefix} | {line}"));
    }

    /// A milestone/output line belonging to `container`.
    fn print_for_container(&self, state: &State, container: &str, line: &str) {
        let color = state.colors.get(container).copied().unwrap_or(Color::White);
        self.print_prefixed(state, container, color, line);
    }

    /// A task-level line (the task's own prefix, white).
    fn print_for_task(&self, state: &State, line: &str) {
        let task = state.task.clone().unwrap_or_default();
        self.print_prefixed(state, &task, Color::White, line);
    }

    /// Prints the deferred "Running `<task>`..." preamble if it hasn't
    /// already gone out — called both once the graph normally arrives
    /// (`TaskGraphResolved`) and, as a fallback, on `TaskFailed`, so an
    /// infrastructure failure early enough that the graph never resolved
    /// still gets this task's own line onto stdout before the error.
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

    fn post(&self, event: TaskEvent) {
        let mut state = self.state.lock().unwrap();
        match event {
            TaskEvent::TaskStarting { task } => {
                *state = State {
                    task: Some(task),
                    preamble_pending: true,
                    ..State::default()
                };
            }
            TaskEvent::TaskGraphResolved { containers } => {
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
                    self.print_for_container(&state, &container, &format!("Pulling {image}..."));
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
            TaskEvent::ImagePullCompleted { image } => {
                for container in Self::containers_for_image(&state, &image) {
                    self.print_for_container(&state, &container, &format!("Pulled {image}."));
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
            TaskEvent::RunningSetupCommand {
                container,
                command,
                index,
                total,
            } => {
                self.print_for_container(
                    &state,
                    &container,
                    &format!("Running setup command {command} ({index} of {total})..."),
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
            TaskEvent::RunningTaskContainer { container, command } => {
                let line = match command {
                    Some(command) => format!("Running {command}..."),
                    None => "Running...".to_string(),
                };
                self.print_for_container(&state, &container, &line);
            }
            TaskEvent::ContainerOutput { container, line } => {
                self.print_for_container(&state, &container, &line);
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
                    &super::format_task_summary(&self.console, &task, exit_code, duration),
                );
            }
            // The error itself reaches stderr through the normal error
            // chain — but an infrastructure failure early enough that
            // TaskGraphResolved never posted (e.g. `--use-network` naming a
            // nonexistent network) would otherwise leave the deferred
            // preamble (see TaskStarting/TaskGraphResolved above) stuck
            // unprinted forever, so this task's prefix never appeared on
            // stdout at all before the error. Flush it here if so —
            // `prefix_width` may still be 0 (no graph arrived to size it
            // against), which just means no padding beyond the task name
            // itself, same as a single-container task would get anyway.
            TaskEvent::TaskFailed { .. } => {
                self.flush_preamble_if_pending(&mut state);
            }
        }
    }
}

/// Turns a container's raw output chunks into whole lines: buffers until a
/// `\n`, strips a trailing `\r` (a carriage-return progress spinner
/// collapses to plain lines), and hands each complete line to `emit`.
/// [`LineBuffer::flush`] emits any unterminated tail when the stream ends.
/// A port of Batect's `InterleavedContainerOutputSink`'s line splitting;
/// `docker.rs` drives one of these per streamed container.
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8], mut emit: impl FnMut(&str)) {
        for byte in chunk {
            if *byte == b'\n' {
                let mut line = std::mem::take(&mut self.pending);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                emit(&String::from_utf8_lossy(&line));
            } else {
                self.pending.push(*byte);
            }
        }
    }

    pub fn flush(&mut self, mut emit: impl FnMut(&str)) {
        if !self.pending.is_empty() {
            let mut line = std::mem::take(&mut self.pending);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            emit(&String::from_utf8_lossy(&line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::SharedBuffer;
    use super::super::TaskContainerInfo;
    use super::*;
    use std::time::Duration;

    fn logger() -> (InterleavedEventLogger, SharedBuffer) {
        let buffer = SharedBuffer::default();
        // Color disabled: expectations stay readable, and prefix padding is
        // the structural thing worth asserting.
        let console = Console::new(Box::new(buffer.clone()), false);
        (InterleavedEventLogger::new(console), buffer)
    }

    fn info(name: &str, image: Option<&str>, build_tag: Option<&str>) -> TaskContainerInfo {
        TaskContainerInfo {
            name: name.to_string(),
            image: image.map(str::to_string),
            build_tag: build_tag.map(str::to_string),
            dependencies: Vec::new(),
            is_task_container: false,
        }
    }

    fn start_task(logger: &InterleavedEventLogger, containers: Vec<TaskContainerInfo>) {
        logger.post(TaskEvent::TaskStarting {
            task: "test".into(),
        });
        logger.post(TaskEvent::TaskGraphResolved { containers });
    }

    #[test]
    fn declares_the_interleaved_io_policy() {
        let (logger, _) = logger();
        assert_eq!(
            logger.container_io_streaming(),
            ContainerIoStreaming::Interleaved
        );
    }

    #[test]
    fn prefixes_are_padded_to_the_longest_name() {
        let (logger, buffer) = logger();
        start_task(
            &logger,
            vec![
                info("db", Some("postgres:15"), None),
                info("app-server", Some("app:1"), None),
            ],
        );
        logger.post(TaskEvent::DependencyStarting {
            container: "db".into(),
        });
        logger.post(TaskEvent::ContainerOutput {
            container: "app-server".into(),
            line: "hello".into(),
        });
        assert_eq!(
            buffer.contents(),
            "test       | Running test...\n\
             db         | Starting container...\n\
             app-server | hello\n"
        );
    }

    #[test]
    fn prefix_padding_aligns_by_display_width_not_char_count() {
        // "数据库" is 3 `char`s but 6 terminal columns (2 per CJK
        // character) — padding by `char` count (Rust's own `{:width$}`)
        // would under-pad it by 3 columns and misalign every other
        // column; padding by display width keeps them lined up.
        let (logger, buffer) = logger();
        start_task(
            &logger,
            vec![
                info("数据库", Some("postgres:15"), None),
                info("db", Some("redis:7"), None),
            ],
        );
        logger.post(TaskEvent::DependencyStarting {
            container: "数据库".into(),
        });
        logger.post(TaskEvent::DependencyStarting {
            container: "db".into(),
        });
        assert_eq!(
            buffer.contents(),
            "test   | Running test...\n\
             数据库 | Starting container...\n\
             db     | Starting container...\n"
        );
    }

    #[test]
    fn a_pull_fans_out_to_every_container_using_that_image() {
        let (logger, buffer) = logger();
        start_task(
            &logger,
            vec![
                info("a", Some("shared:1"), None),
                info("b", Some("shared:1"), None),
                info("c", Some("other:1"), None),
            ],
        );
        logger.post(TaskEvent::ImagePullStarting {
            image: "shared:1".into(),
        });
        logger.post(TaskEvent::ImagePullCompleted {
            image: "shared:1".into(),
        });
        assert_eq!(
            buffer.contents(),
            "test | Running test...\n\
             a    | Pulling shared:1...\n\
             b    | Pulling shared:1...\n\
             a    | Pulled shared:1.\n\
             b    | Pulled shared:1.\n"
        );
    }

    #[test]
    fn build_output_gets_an_inner_image_build_prefix() {
        let (logger, buffer) = logger();
        start_task(&logger, vec![info("app", None, Some("proj-app"))]);
        logger.post(TaskEvent::ImageBuildProgress {
            tag: "proj-app".into(),
            message: "Step 1/3 : FROM alpine".into(),
        });
        assert_eq!(
            buffer.contents(),
            "test | Running test...\n\
             app  | Image build | Step 1/3 : FROM alpine\n"
        );
    }

    #[test]
    fn setup_command_output_gets_an_inner_numbered_prefix() {
        let (logger, buffer) = logger();
        start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
        logger.post(TaskEvent::SetupCommandOutput {
            container: "db".into(),
            index: 2,
            line: "initialised".into(),
        });
        assert_eq!(
            buffer.contents(),
            "test | Running test...\n\
             db   | Setup command 2 | initialised\n"
        );
    }

    #[test]
    fn task_failed_before_the_graph_resolves_still_flushes_the_preamble() {
        // An infrastructure failure early enough that TaskGraphResolved
        // never posts (e.g. a `--use-network` validation failure) must not
        // leave this task's line unprinted forever — TaskFailed flushes the
        // deferred preamble itself in that case.
        let (logger, buffer) = logger();
        logger.post(TaskEvent::TaskStarting {
            task: "test".into(),
        });
        logger.post(TaskEvent::TaskFailed {
            task: "test".into(),
        });
        assert_eq!(buffer.contents(), "test | Running test...\n");

        // A second TaskFailed (shouldn't happen in practice, but the guard
        // must not double-print).
        logger.post(TaskEvent::TaskFailed {
            task: "test".into(),
        });
        assert_eq!(buffer.contents(), "test | Running test...\n");
    }

    #[test]
    fn task_level_lines_use_the_task_name_prefix() {
        let (logger, buffer) = logger();
        start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
        logger.post(TaskEvent::CleanupStarting);
        logger.post(TaskEvent::CleanupStarting);
        logger.post(TaskEvent::RemovingNetwork);
        logger.post(TaskEvent::TaskFinished {
            task: "test".into(),
            exit_code: 0,
            duration: Duration::from_millis(2100),
        });
        assert_eq!(
            buffer.contents(),
            "test | Running test...\n\
             test | Cleaning up...\n\
             test | Removing task network...\n\
             test | test finished with exit code 0 in 2.1s.\n"
        );
    }

    #[test]
    fn line_buffer_splits_on_newlines_and_strips_carriage_returns() {
        let mut buffer = LineBuffer::new();
        let mut lines: Vec<String> = Vec::new();
        buffer.push(b"partial", &mut |line: &str| lines.push(line.to_string()));
        assert!(lines.is_empty());
        buffer.push(b" line\r\nsecond\nthird", &mut |line: &str| {
            lines.push(line.to_string())
        });
        assert_eq!(lines, vec!["partial line", "second"]);
        buffer.flush(&mut |line: &str| lines.push(line.to_string()));
        assert_eq!(lines, vec!["partial line", "second", "third"]);
        // Nothing pending — flush again emits nothing.
        buffer.flush(&mut |_line: &str| panic!("nothing should be pending"));
    }
}
