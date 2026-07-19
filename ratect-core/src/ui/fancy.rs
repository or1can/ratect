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

//! The `fancy` output mode — a port of Batect's `FancyEventLogger`/
//! `StartupProgressDisplay`/`CleanupProgressDisplay`: a live status block
//! with one line per container in the task's dependency graph, repainted in
//! place via cursor movement (no spinner — the "animation" is purely
//! rewriting changed lines, exactly like Batect), then *frozen* (after a
//! separating blank line) the moment the task's own container starts, so
//! the container's raw output streams below it untouched. Cleanup gets a
//! single live countdown line after the task exits, cleared before the
//! final summary line.
//!
//! Differences from Batect's implementation, both deliberate:
//! - Batect repaints only lines that changed (a diff against the previous
//!   frame); Ratect rewrites the whole block each time — between two
//!   flushes of one atomic write, so there's no visible flicker, and every
//!   repaint re-clips against the *current* terminal width for free.
//! - Colorless fancy works (`--no-color` suppresses bold/color but not
//!   cursor movement) — see [`Console`]'s independent-axes design.

use super::{Color, Console, EventSink, TaskContainerInfo, TaskEvent};
use std::collections::BTreeSet;
use std::sync::Mutex;

/// Where the terminal's current width comes from — injected so unit tests
/// can pin it; the real logger queries crossterm on every repaint (which is
/// also what keeps a resized terminal rendering correctly).
pub type WidthSource = Box<dyn Fn() -> Option<u16> + Send + Sync>;

pub struct FancyEventLogger {
    console: Console,
    width_source: WidthSource,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// One display line per container in the current task's graph —
    /// alphabetical, task container last (a deterministic Ratect choice;
    /// Batect's own order falls out of its graph's node set).
    lines: Vec<ContainerLine>,
    /// How many block lines are currently painted on screen (0 = nothing
    /// painted yet, so the first paint doesn't cursor-up).
    painted_lines: usize,
    /// `false` once the block froze (task container running, cleanup
    /// started, or the task failed) — no more startup repaints after that.
    keep_updating_startup: bool,
    /// Dependency containers started and not yet removed — the cleanup
    /// countdown. `BTreeSet` so the rendered list is stable.
    started_containers: BTreeSet<String>,
    /// The task's own network is being removed (the last cleanup step).
    removing_network: bool,
    /// The live cleanup line is currently on screen.
    cleanup_shown: bool,
    /// Whether any task has rendered yet — a blank separator line goes
    /// between one task's output and the next's, matching the simple
    /// logger.
    printed_a_task: bool,
}

/// One container's progress line: `<bold name>: <stage description>`.
struct ContainerLine {
    info: TaskContainerInfo,
    stage: Stage,
}

/// Where a container currently is in its startup journey. Every transition
/// is event-driven and unconditional — a stage that "should" come next but
/// whose event never fires (e.g. no pull happens because the image is
/// already local) is simply skipped when a later event arrives.
enum Stage {
    /// Nothing has happened yet.
    Pending,
    /// `ImagePullStarting`/`ImagePullProgress` — the latest status line.
    Pulling(Option<String>),
    /// `ImageBuildStarting`/`ImageBuildProgress` — the latest build line.
    Building(Option<String>),
    /// Image resolved; waiting on the named dependencies (drained as their
    /// `ContainerBecameHealthy` events arrive — an approximation of full
    /// readiness that at worst under-reports the wait by a dependency's own
    /// setup-command time).
    WaitingForDependencies(BTreeSet<String>),
    /// `DependencyStarting`.
    StartingContainer,
    /// `DependencyStarted` — waiting for the health verdict.
    WaitingToBecomeHealthy,
    /// `RunningSetupCommand`.
    RunningSetupCommand {
        command: String,
        index: usize,
        total: usize,
    },
    /// A dependency's terminal state: healthy, setup commands done.
    Ready,
    /// The task container's terminal state: its command is running.
    RunningCommand(Option<String>),
}

impl ContainerLine {
    fn description(&self) -> String {
        match &self.stage {
            Stage::Pending => match (&self.info.image, &self.info.build_tag) {
                (Some(image), _) => format!("ready to pull image {image}"),
                (None, Some(_)) => "ready to build image".to_string(),
                (None, None) => "ready".to_string(),
            },
            Stage::Pulling(None) => match &self.info.image {
                Some(image) => format!("pulling image {image}..."),
                None => "pulling image...".to_string(),
            },
            Stage::Pulling(Some(status)) => match &self.info.image {
                Some(image) => format!("pulling {image}: {status}"),
                None => format!("pulling image: {status}"),
            },
            Stage::Building(None) => "building image...".to_string(),
            Stage::Building(Some(line)) => format!("building image: {line}"),
            Stage::WaitingForDependencies(remaining) if remaining.is_empty() => {
                "waiting to start...".to_string()
            }
            Stage::WaitingForDependencies(remaining) => {
                let names: Vec<&str> = remaining.iter().map(String::as_str).collect();
                format!(
                    "waiting for {} {} to be ready...",
                    if names.len() == 1 {
                        "dependency"
                    } else {
                        "dependencies"
                    },
                    names.join(", ")
                )
            }
            Stage::StartingContainer => "starting container...".to_string(),
            Stage::WaitingToBecomeHealthy => {
                "container started, waiting for it to become healthy...".to_string()
            }
            Stage::RunningSetupCommand {
                command,
                index,
                total,
            } => format!("running setup command {command} ({index} of {total})..."),
            Stage::Ready => "ready".to_string(),
            Stage::RunningCommand(Some(command)) => format!("running {command}"),
            Stage::RunningCommand(None) => "running".to_string(),
        }
    }

    fn render(&self, console: &Console) -> String {
        format!("{}: {}", console.bold(&self.info.name), self.description())
    }
}

/// Truncates `text` to `width` *visible* columns, appending `...` when it
/// had to cut — ANSI escape sequences (bold container names) count for
/// zero, and a truncation that severed one gets a trailing reset so the
/// styling can't bleed into the next line. Character count approximates
/// column count, same as Batect's own `TextRun.limitToLength`.
fn clip_to_width(text: &str, width: Option<u16>) -> String {
    let Some(width) = width else {
        return text.to_string();
    };
    let width = width as usize;
    let visible_count = visible_chars(text);
    if visible_count <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(3);
    let mut out = String::new();
    let mut kept = 0;
    let mut chars = text.chars().peekable();
    let mut in_escape = false;
    let mut saw_escape = false;
    for c in chars.by_ref() {
        if in_escape {
            out.push(c);
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_escape = true;
            saw_escape = true;
            out.push(c);
            continue;
        }
        if kept == keep {
            break;
        }
        out.push(c);
        kept += 1;
    }
    out.push_str("...");
    if saw_escape {
        out.push_str("\x1b[0m");
    }
    out
}

/// How many columns `text` occupies, counting ANSI escape sequences as
/// zero.
fn visible_chars(text: &str) -> usize {
    let mut count = 0;
    let mut in_escape = false;
    for c in text.chars() {
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            count += 1;
        }
    }
    count
}

const CURSOR_UP_ONE_AND_CLEAR: &str = "\x1b[1A\r\x1b[2K";

impl FancyEventLogger {
    pub fn new(console: Console, width_source: WidthSource) -> Self {
        Self {
            console,
            width_source,
            state: Mutex::new(State::default()),
        }
    }

    /// The logger `main.rs` actually wires up: real stdout (color iff it's
    /// a terminal and `--no-color` wasn't given — colorless fancy keeps the
    /// live repaint, only dropping bold/color), width queried live from the
    /// terminal.
    pub fn stdout(no_color: bool) -> Self {
        Self::new(
            Console::stdout(no_color),
            // A reported width of 0 (some pseudo-terminals with no size
            // set, e.g. `script`'s) means "unknown", not "zero columns" —
            // clipping to it would reduce every line to bare "...".
            Box::new(|| {
                crossterm::terminal::size()
                    .ok()
                    .and_then(|(width, _)| (width > 0).then_some(width))
            }),
        )
    }

    /// Repaints the whole startup block in place: cursor up over the
    /// previous frame, then clear-and-rewrite every line — emitted as one
    /// atomic `write_raw` so nothing can interleave mid-frame.
    fn repaint_startup(&self, state: &mut State) {
        if state.lines.is_empty() {
            return;
        }
        let width = (self.width_source)();
        let mut frame = String::new();
        if state.painted_lines > 0 {
            frame.push_str(&format!("\x1b[{}A", state.painted_lines));
        }
        for line in &state.lines {
            frame.push_str("\r\x1b[2K");
            frame.push_str(&clip_to_width(&line.render(&self.console), width));
            frame.push('\n');
        }
        state.painted_lines = state.lines.len();
        self.console.write_raw(&frame);
    }

    fn cleanup_text(&self, state: &State) -> String {
        if !state.started_containers.is_empty() {
            let names: Vec<&str> = state
                .started_containers
                .iter()
                .map(String::as_str)
                .collect();
            format!(
                "Cleaning up: {} container{} ({}) left to remove...",
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                names.join(", ")
            )
        } else if state.removing_network {
            "Cleaning up: removing task network...".to_string()
        } else {
            "Cleaning up...".to_string()
        }
    }

    /// Paints (or repaints, in place) the single live cleanup line.
    fn repaint_cleanup(&self, state: &mut State) {
        let width = (self.width_source)();
        let mut frame = String::new();
        if state.cleanup_shown {
            frame.push_str(CURSOR_UP_ONE_AND_CLEAR);
        }
        frame.push_str(&clip_to_width(&self.cleanup_text(state), width));
        frame.push('\n');
        state.cleanup_shown = true;
        self.console.write_raw(&frame);
    }

    fn line_mut<'state>(
        state: &'state mut State,
        container: &str,
    ) -> Option<&'state mut ContainerLine> {
        state
            .lines
            .iter_mut()
            .find(|line| line.info.name == container)
    }
}

impl EventSink for FancyEventLogger {
    fn post(&self, event: TaskEvent) {
        let mut state = self.state.lock().unwrap();
        match event {
            TaskEvent::TaskStarting { task } => {
                let printed_a_task = state.printed_a_task;
                *state = State {
                    printed_a_task: true,
                    keep_updating_startup: true,
                    ..State::default()
                };
                if printed_a_task {
                    self.console.println("");
                }
                self.console
                    .println(&format!("Running {}...", self.console.bold(&task)));
            }
            TaskEvent::TaskGraphResolved { containers } => {
                let mut containers = containers;
                // Alphabetical, task container last — its line is the one
                // the eye follows into the streamed output below the block.
                containers.sort_by(|a, b| {
                    (a.is_task_container, &a.name).cmp(&(b.is_task_container, &b.name))
                });
                state.lines = containers
                    .into_iter()
                    .map(|info| ContainerLine {
                        info,
                        stage: Stage::Pending,
                    })
                    .collect();
                state.painted_lines = 0;
                // A freshly resolved graph (re)starts the live display —
                // not just `TaskStarting` — so the block updates even for
                // an event stream that skips the task-level preamble.
                state.keep_updating_startup = true;
                self.repaint_startup(&mut state);
            }
            TaskEvent::ImagePullStarting { image } => {
                if !state.keep_updating_startup {
                    return;
                }
                for line in &mut state.lines {
                    if line.info.image.as_deref() == Some(image.as_str()) {
                        line.stage = Stage::Pulling(None);
                    }
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::ImagePullProgress { image, message } => {
                if !state.keep_updating_startup {
                    return;
                }
                for line in &mut state.lines {
                    if line.info.image.as_deref() == Some(image.as_str()) {
                        line.stage = Stage::Pulling(Some(message.clone()));
                    }
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::ImagePullCompleted { image } => {
                if !state.keep_updating_startup {
                    return;
                }
                for line in &mut state.lines {
                    if line.info.image.as_deref() == Some(image.as_str()) {
                        line.stage = Stage::WaitingForDependencies(
                            line.info.dependencies.iter().cloned().collect(),
                        );
                    }
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::ImageBuildStarting { container } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::Building(None);
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::ImageBuildProgress { tag, message } => {
                if !state.keep_updating_startup {
                    return;
                }
                for line in &mut state.lines {
                    if line.info.build_tag.as_deref() == Some(tag.as_str()) {
                        line.stage = Stage::Building(Some(message.clone()));
                    }
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::ImageBuildCompleted { container } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::WaitingForDependencies(
                        line.info.dependencies.iter().cloned().collect(),
                    );
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::DependencyStarting { container } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::StartingContainer;
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::DependencyStarted { container } => {
                if !state.keep_updating_startup {
                    return;
                }
                state.started_containers.insert(container.clone());
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::WaitingToBecomeHealthy;
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::ContainerBecameHealthy { container } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::Ready;
                }
                // The now-healthy container stops being waited on by
                // anything else in the graph (see `WaitingForDependencies`'
                // approximation note).
                for line in &mut state.lines {
                    if let Stage::WaitingForDependencies(remaining) = &mut line.stage {
                        remaining.remove(&container);
                    }
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::RunningSetupCommand {
                container,
                command,
                index,
                total,
            } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::RunningSetupCommand {
                        command,
                        index,
                        total,
                    };
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::SetupCommandsCompleted { container } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::Ready;
                }
                self.repaint_startup(&mut state);
            }
            TaskEvent::RunningTaskContainer { container, command } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    line.stage = Stage::RunningCommand(command);
                }
                // One final frame, then freeze the block behind a blank
                // separator — the task container's raw output streams below
                // from here on, and must never fight a repaint (Batect's
                // `keepUpdatingStartupProgress` mechanism).
                self.repaint_startup(&mut state);
                state.keep_updating_startup = false;
                self.console.println("");
            }
            TaskEvent::CleanupStarting => {
                // On an infrastructure failure the block may still be live
                // — cleanup freezes it, exactly like the task starting
                // does (Batect stops startup updates on the first
                // CleanupStep).
                state.keep_updating_startup = false;
                self.repaint_cleanup(&mut state);
            }
            TaskEvent::ContainerRemoved { container } => {
                state.started_containers.remove(&container);
                if state.cleanup_shown {
                    self.repaint_cleanup(&mut state);
                }
            }
            TaskEvent::RemovingNetwork => {
                state.removing_network = true;
                if state.cleanup_shown {
                    self.repaint_cleanup(&mut state);
                }
            }
            TaskEvent::TaskFinished {
                task,
                exit_code,
                duration,
            } => {
                // The live cleanup line makes way for the permanent
                // summary, matching Batect's `onTaskFinished`.
                if state.cleanup_shown {
                    self.console.write_raw(CURSOR_UP_ONE_AND_CLEAR);
                    state.cleanup_shown = false;
                }
                let color = if exit_code == 0 {
                    Color::Green
                } else {
                    Color::Red
                };
                let exit_code = self.console.colored(color, &exit_code.to_string());
                self.console.println(&format!(
                    "{} finished with exit code {exit_code} in {}.",
                    self.console.bold(&task),
                    super::format_duration(duration)
                ));
            }
            TaskEvent::TaskFailed { .. } => {
                // Freeze everything; the error itself reaches stderr via
                // the normal error chain after cleanup. The cleanup line
                // (if shown) stays as the last thing on stdout.
                state.keep_updating_startup = false;
            }
            // Interleaved-mode events — never posted under this logger's
            // (default) TaskContainerOnly streaming policy, and setup
            // command output has no place in the live block regardless.
            TaskEvent::ContainerOutput { .. } | TaskEvent::SetupCommandOutput { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::SharedBuffer;
    use super::*;
    use std::time::Duration;

    fn logger_with_width(width: u16) -> (FancyEventLogger, SharedBuffer) {
        let buffer = SharedBuffer::default();
        // Color disabled so expectations stay readable — bold/color are
        // covered by Console's own tests; cursor movement is emitted
        // regardless (the independent-axes design).
        let console = Console::new(Box::new(buffer.clone()), false);
        (
            FancyEventLogger::new(console, Box::new(move || Some(width))),
            buffer,
        )
    }

    fn info(name: &str, image: Option<&str>, deps: &[&str], is_task: bool) -> TaskContainerInfo {
        TaskContainerInfo {
            name: name.to_string(),
            image: image.map(str::to_string),
            build_tag: None,
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
            is_task_container: is_task,
        }
    }

    #[test]
    fn clip_to_width_truncates_visible_chars_only() {
        assert_eq!(clip_to_width("hello", Some(10)), "hello");
        assert_eq!(clip_to_width("hello world", Some(8)), "hello...");
        // Escape sequences don't count toward the width, and a clipped
        // styled string gets a trailing reset.
        let styled = "\x1b[1mapp\x1b[0m: something quite long";
        assert_eq!(clip_to_width(styled, Some(30)), styled);
        assert_eq!(
            clip_to_width(styled, Some(10)),
            "\x1b[1mapp\x1b[0m: so...\x1b[0m"
        );
    }

    #[test]
    fn graph_resolution_paints_one_line_per_container_task_container_last() {
        let (logger, buffer) = logger_with_width(120);
        logger.post(TaskEvent::TaskStarting {
            task: "test".into(),
        });
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![
                info("app", Some("app:1"), &["db"], true),
                info("db", Some("postgres:15"), &[], false),
            ],
        });
        assert_eq!(
            buffer.contents(),
            "Running test...\n\
             \r\x1b[2Kdb: ready to pull image postgres:15\n\
             \r\x1b[2Kapp: ready to pull image app:1\n"
        );
    }

    #[test]
    fn a_progress_event_repaints_the_block_in_place() {
        let (logger, buffer) = logger_with_width(120);
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![
                info("app", Some("app:1"), &["db"], true),
                info("db", Some("postgres:15"), &[], false),
            ],
        });
        logger.post(TaskEvent::ImagePullStarting {
            image: "postgres:15".into(),
        });
        assert_eq!(
            buffer.contents(),
            "\r\x1b[2Kdb: ready to pull image postgres:15\n\
             \r\x1b[2Kapp: ready to pull image app:1\n\
             \x1b[2A\
             \r\x1b[2Kdb: pulling image postgres:15...\n\
             \r\x1b[2Kapp: ready to pull image app:1\n"
        );
    }

    #[test]
    fn task_container_start_freezes_the_block_behind_a_blank_line() {
        let (logger, buffer) = logger_with_width(120);
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![info("app", Some("app:1"), &[], true)],
        });
        logger.post(TaskEvent::RunningTaskContainer {
            container: "app".into(),
            command: Some("cargo test".into()),
        });
        let after_freeze = buffer.contents();
        assert!(after_freeze.ends_with(
            "\x1b[1A\
             \r\x1b[2Kapp: running cargo test\n\
             \n"
        ));

        // Once frozen, further progress events must not repaint.
        logger.post(TaskEvent::ImagePullProgress {
            image: "app:1".into(),
            message: "late".into(),
        });
        assert_eq!(buffer.contents(), after_freeze);
    }

    #[test]
    fn dependency_becoming_healthy_unblocks_waiting_lines() {
        let (logger, buffer) = logger_with_width(120);
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![
                info("app", Some("app:1"), &["db"], true),
                info("db", Some("postgres:15"), &[], false),
            ],
        });
        logger.post(TaskEvent::ImagePullCompleted {
            image: "app:1".into(),
        });
        assert!(buffer
            .contents()
            .contains("app: waiting for dependency db to be ready..."));
        logger.post(TaskEvent::ContainerBecameHealthy {
            container: "db".into(),
        });
        let contents = buffer.contents();
        assert!(contents.contains("db: ready"), "{contents}");
        assert!(contents.contains("app: waiting to start..."), "{contents}");
    }

    #[test]
    fn cleanup_line_counts_down_then_summary_replaces_it() {
        let (logger, buffer) = logger_with_width(120);
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![
                info("app", Some("app:1"), &["db"], true),
                info("db", Some("postgres:15"), &[], false),
            ],
        });
        logger.post(TaskEvent::DependencyStarted {
            container: "db".into(),
        });
        logger.post(TaskEvent::RunningTaskContainer {
            container: "app".into(),
            command: None,
        });
        logger.post(TaskEvent::CleanupStarting);
        assert!(buffer
            .contents()
            .ends_with("Cleaning up: 1 container (db) left to remove...\n"));

        logger.post(TaskEvent::ContainerRemoved {
            container: "db".into(),
        });
        logger.post(TaskEvent::RemovingNetwork);
        assert!(buffer.contents().ends_with(
            "\x1b[1A\r\x1b[2K\
             Cleaning up: removing task network...\n"
        ));

        logger.post(TaskEvent::TaskFinished {
            task: "test".into(),
            exit_code: 0,
            duration: Duration::from_millis(1500),
        });
        assert!(buffer.contents().ends_with(
            "\x1b[1A\r\x1b[2K\
             test finished with exit code 0 in 1.5s.\n"
        ));
    }

    #[test]
    fn a_second_task_resets_the_display_after_a_blank_separator() {
        let (logger, buffer) = logger_with_width(120);
        logger.post(TaskEvent::TaskStarting {
            task: "prereq".into(),
        });
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![info("app", Some("app:1"), &[], true)],
        });
        logger.post(TaskEvent::TaskStarting {
            task: "main".into(),
        });
        let contents = buffer.contents();
        assert!(
            contents.ends_with("\n\nRunning main...\n"),
            "expected a blank separator before the second task: {contents:?}"
        );
        // The new task starts with no lines — nothing repaints until its
        // own graph resolves.
        logger.post(TaskEvent::ImagePullProgress {
            image: "app:1".into(),
            message: "late".into(),
        });
        assert_eq!(buffer.contents(), contents);
    }

    #[test]
    fn lines_are_clipped_to_the_terminal_width() {
        let (logger, buffer) = logger_with_width(20);
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![info(
                "a-container-with-a-really-long-name",
                Some("some-image:1"),
                &[],
                true,
            )],
        });
        assert_eq!(buffer.contents(), "\r\x1b[2Ka-container-with-...\n");
    }
}
