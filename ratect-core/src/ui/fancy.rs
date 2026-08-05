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

use super::{Console, EventSink, TaskContainerInfo, TaskEvent};
use std::collections::BTreeSet;
use std::sync::Mutex;
use unicode_width::UnicodeWidthChar;

pub struct FancyEventLogger {
    console: Console,
    /// A fixed terminal width, overriding live detection — `None` in
    /// production (`stdout`), which queries the real terminal live on
    /// every repaint via [`FancyEventLogger::current_width`] instead
    /// (see its own docs); `Some` only in tests, which need a pinned
    /// value to make assertions on rendered output deterministic.
    fixed_width: Option<u16>,
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
    /// The lines' own rendered content (post-clip, pre-cursor-movement)
    /// from the last repaint that actually wrote anything — lets
    /// `repaint_startup` skip the write (and its width-query/lock/syscall
    /// cost) entirely when nothing visible would actually change. Common in
    /// practice: Docker resends the same coarse pull/build status text many
    /// times per layer while only the byte-progress detail Ratect doesn't
    /// render keeps changing underneath it.
    last_rendered: Option<String>,
    /// `false` once the block froze (task container running, cleanup
    /// started, or the task failed) — no more startup repaints after that.
    keep_updating_startup: bool,
    /// Containers started and not yet removed — the cleanup countdown.
    /// The task's own container counts (it is removed during the cleanup
    /// stage like any other, since 0.25.0), matching Batect's own
    /// `CleanupProgressDisplayLine`, which counts
    /// `containersCreated - containersRemoved` with no special case for it.
    /// `BTreeSet` so the rendered list is stable.
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

/// Truncates `text` to `width` *display columns*, appending `...` when it
/// had to cut — ANSI escape sequences (bold container names) count for
/// zero, and a truncation that severed one gets a trailing reset so the
/// styling can't bleed into the next line. Uses each character's real
/// Unicode display width (wide CJK characters count as 2, zero-width/
/// combining marks count as 0) rather than approximating with a plain
/// character count — a plain count under-measures exactly the characters
/// most likely to appear in a container name or streamed build output,
/// which would otherwise let a rendered line wrap onto more terminal rows
/// than this logger accounts for, desyncing its own repaint math (see
/// [`display_width`]) from what's actually on screen.
fn clip_to_width(text: &str, width: Option<u16>) -> String {
    let Some(width) = width else {
        return text.to_string();
    };
    let width = width as usize;
    if display_width(text) <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(3);
    let mut out = String::new();
    let mut kept_width = 0;
    let mut in_escape = false;
    let mut saw_escape = false;
    for c in text.chars() {
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
        let char_width = c.width().unwrap_or(0);
        if kept_width + char_width > keep {
            break;
        }
        out.push(c);
        kept_width += char_width;
    }
    out.push_str("...");
    if saw_escape {
        out.push_str("\x1b[0m");
    }
    out
}

/// How many terminal display columns `text` occupies — each character's
/// real Unicode width (0, 1, or 2; see [`UnicodeWidthChar`]), counting ANSI
/// escape sequences as zero.
fn display_width(text: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for c in text.chars() {
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            width += c.width().unwrap_or(0);
        }
    }
    width
}

const CURSOR_UP_ONE_AND_CLEAR: &str = "\x1b[1A\r\x1b[2K";

impl FancyEventLogger {
    pub fn new(console: Console) -> Self {
        Self {
            console,
            fixed_width: None,
            state: Mutex::new(State::default()),
        }
    }

    /// The logger `main.rs` actually wires up: real stdout (color iff it's
    /// a terminal and `--no-color` wasn't given — colorless fancy keeps the
    /// live repaint, only dropping bold/color), width queried live from the
    /// terminal.
    pub fn stdout(no_color: bool) -> Self {
        Self::new(Console::stdout(no_color))
    }

    /// The terminal's current display width, or `None` if it can't be
    /// determined at all — `fixed_width` when set (tests only), otherwise
    /// queried live via crossterm on every call (which is also what keeps
    /// a resized terminal rendering correctly, with no resize-signal
    /// listener needed). A reported width of `0` (some pseudo-terminals
    /// with no size set, e.g. `script`'s) means "unknown", not "zero
    /// columns" — clipping to it would reduce every line to bare `"..."`.
    fn current_width(&self) -> Option<u16> {
        self.fixed_width.or_else(|| {
            crossterm::terminal::size()
                .ok()
                .and_then(|(width, _)| (width > 0).then_some(width))
        })
    }

    /// Repaints the whole startup block in place: cursor up over the
    /// previous frame, then clear-and-rewrite every line — emitted as one
    /// atomic `write_raw` so nothing can interleave mid-frame.
    fn repaint_startup(&self, state: &mut State) {
        if state.lines.is_empty() {
            return;
        }
        let width = self.current_width();
        let mut rendered = String::new();
        for line in &state.lines {
            rendered.push_str("\r\x1b[2K");
            rendered.push_str(&clip_to_width(&line.render(&self.console), width));
            rendered.push('\n');
        }
        if state.last_rendered.as_deref() == Some(rendered.as_str()) {
            // Nothing visible would actually change — skip the write
            // entirely rather than repainting identical content (see
            // `last_rendered`'s own docs for why this is the common case,
            // not a rare one).
            return;
        }
        let mut frame = String::new();
        if state.painted_lines > 0 {
            frame.push_str(&format!("\x1b[{}A", state.painted_lines));
        }
        frame.push_str(&rendered);
        state.painted_lines = state.lines.len();
        state.last_rendered = Some(rendered);
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
        let width = self.current_width();
        let mut frame = String::new();
        if state.cleanup_shown {
            frame.push_str(CURSOR_UP_ONE_AND_CLEAR);
        } else {
            // A blank separator line first, unconditionally — matching
            // simple mode's blank line before "Cleaning up...". Not just
            // cosmetic here: the task container's own output streams raw
            // (`docker.rs`, outside this logger's control) and may not end
            // in a newline, so painting cleanup directly would land on the
            // same row as that output — which the *next* repaint's
            // cursor-up-and-clear would then erase, destroying it.
            frame.push('\n');
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
    fn wants_progress_detail(&self) -> bool {
        true
    }

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
                state.last_rendered = None;
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
            TaskEvent::ImageResolved { container } => {
                if !state.keep_updating_startup {
                    return;
                }
                if let Some(line) = Self::line_mut(&mut state, &container) {
                    // Only advance a line still sitting at `Pending` — a
                    // pull/build that actually happened this task already
                    // moved it on via `ImagePullCompleted`/
                    // `ImageBuildCompleted` (posted before this event, since
                    // `resolve_image` awaits them first). This is purely the
                    // fallback for when neither ever fired at all — an
                    // already-local image under the default
                    // `IfNotPresent` policy, or an image/build this
                    // invocation already resolved for an earlier task —
                    // without it the line would otherwise sit at "ready to
                    // pull/build" for the container's entire dependency
                    // wait, never showing what it's actually waiting on.
                    if matches!(line.stage, Stage::Pending) {
                        line.stage = Stage::WaitingForDependencies(
                            line.info.dependencies.iter().cloned().collect(),
                        );
                    }
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
            TaskEvent::TaskContainerCreated { container } => {
                // Counted from *created*, not from `RunningTaskContainer` —
                // that is posted before the container exists, so counting it
                // there left the countdown stuck on a container that was
                // never created whenever creation failed. Outside the
                // `keep_updating_startup` guard deliberately: that guard is
                // about whether the startup block may still be repainted, and
                // this is cleanup bookkeeping, which has to be right either
                // way.
                state.started_containers.insert(container);
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
                self.console.println(&super::format_task_summary(
                    &self.console,
                    &self.console.bold(&task),
                    exit_code,
                    duration,
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
#[path = "fancy_tests.rs"]
mod tests;
