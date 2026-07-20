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

//! Ratect's user-facing output layer: the task-event model and the sinks
//! ("event loggers") that turn execution milestones into what the user
//! actually sees on stdout.
//!
//! Ported from Batect's own design (its `TaskEventSink`/`EventLogger`
//! interfaces and per-style logger implementations): execution code
//! (`engine.rs`, `docker.rs`) posts typed [`TaskEvent`]s to an injected
//! [`EventSink`] instead of printing directly, and the selected logger
//! decides what — if anything — each event renders as. This indirection is
//! what makes Batect's `--output` modes possible at all (one event stream,
//! rendered four different ways), and it's also what keeps concurrent
//! container startup (0.15.0) coherent on screen: events arrive from
//! concurrent branches of the dependency graph, and each logger serializes
//! its rendering internally rather than every call site racing to print.
//!
//! `tracing` remains separate and unchanged by this layer: diagnostics and
//! breadcrumbs to stderr, controlled by `RUST_LOG`. Events here are the
//! product's actual output, on stdout — see docs/how-it-works.md.

pub mod fancy;
pub mod interleaved;
pub mod simple;

use std::io::IsTerminal;
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

/// One milestone (or fine-grained progress line) in a task's execution.
///
/// Posted by `engine.rs` (milestones — it's the component that knows
/// container/task names and when each phase begins) and `docker.rs`
/// (fine-grained pull/build progress — the streamed detail only it can
/// see). Loggers are free to ignore any variant; the progress variants
/// exist solely for the richer output modes and carry the image/tag
/// reference `docker.rs` has, not a container name (a logger constructed
/// with the config can map one to the other).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEvent {
    /// A task (the requested one or a prerequisite) is about to run its own
    /// container — posted after its prerequisites completed. A task with no
    /// `run` of its own never posts this.
    TaskStarting {
        task: String,
    },
    /// The task's own container dependency graph, resolved and proven
    /// acyclic — posted right after [`TaskEvent::TaskStarting`], before any
    /// container work begins. Carries what a per-container progress display
    /// needs to draw one line per container from the very start (Batect's
    /// `StartupProgressDisplayProvider` gets the same graph at logger
    /// construction; Ratect's loggers outlive one task, so it arrives as an
    /// event instead).
    TaskGraphResolved {
        containers: Vec<TaskContainerInfo>,
    },
    /// The task's own container ran to completion — with a zero *or*
    /// non-zero `exit_code`: a command exiting non-zero still "finishes"
    /// (Batect prints the same summary line either way). An infrastructure
    /// failure (image build failed, dependency never became healthy, ...)
    /// never posts this. Posted after cleanup, so it's the last event of a
    /// successful task execution.
    TaskFinished {
        task: String,
        exit_code: i64,
        duration: Duration,
    },
    /// An image pull is actually happening — not posted when
    /// `image_pull_policy: IfNotPresent` (the default) skips the pull
    /// because the image already exists locally, nor when this session
    /// already pulled (or decided not to pull) the same image.
    ImagePullStarting {
        image: String,
    },
    /// One status line from the Docker daemon's pull stream.
    ImagePullProgress {
        image: String,
        message: String,
    },
    ImagePullCompleted {
        image: String,
    },
    /// `container`'s `build_directory` is being built — keyed by container
    /// name (what the user declared), not the derived image tag.
    ImageBuildStarting {
        container: String,
    },
    /// One line of build output — a classic builder stream line, or a
    /// BuildKit step name/log chunk. `tag` is the image tag being built
    /// (`<project>-<container>`), the reference `docker.rs` has at this
    /// depth.
    ImageBuildProgress {
        tag: String,
        message: String,
    },
    ImageBuildCompleted {
        container: String,
    },
    /// `container`'s image is resolved and ready to run — posted exactly
    /// once per container per task execution, regardless of whether a pull
    /// or build actually happened this time. Unlike
    /// [`TaskEvent::ImagePullCompleted`]/[`TaskEvent::ImageBuildCompleted`]
    /// (which only post the *first* time a given image/container is
    /// resolved in this whole invocation — `resolve_image`'s dedup applies
    /// across tasks, not just within one), this is the reliable per-task
    /// "this container's image is ready" signal a display needs: without
    /// it, a container whose image was already local (`IfNotPresent`, the
    /// default) or already resolved by an earlier task would never emit
    /// *any* image-related event, leaving a per-container progress display
    /// stuck showing "ready to pull/build" for that container's entire
    /// dependency wait.
    ImageResolved {
        container: String,
    },
    /// A dependency/sidecar container is about to start. The task's own
    /// container instead posts [`TaskEvent::RunningTaskContainer`].
    DependencyStarting {
        container: String,
    },
    DependencyStarted {
        container: String,
    },
    /// The dependency reported healthy (immediately, for a container with
    /// no health check at all — the event still posts, matching Batect).
    ContainerBecameHealthy {
        container: String,
    },
    /// One of a dependency's `setup_commands` is about to run. `index` is
    /// 1-based, for rendering as "(n of total)".
    RunningSetupCommand {
        container: String,
        command: String,
        index: usize,
        total: usize,
    },
    /// Every one of the dependency's `setup_commands` succeeded — only
    /// posted when it had some.
    SetupCommandsCompleted {
        container: String,
    },
    /// The task's own container is about to run. `command` is the resolved
    /// command (`run.command` falling back to the container's own), `None`
    /// when the image's default `CMD` runs instead.
    RunningTaskContainer {
        container: String,
        command: Option<String>,
    },
    /// Teardown (stopping dependency containers, removing the task
    /// network) is about to begin. Not posted when there's nothing to tear
    /// down (`--use-network` and no dependencies started).
    CleanupStarting,
    /// A dependency container was stopped and removed during cleanup.
    ContainerRemoved {
        container: String,
    },
    /// The task's own network is about to be removed — the last cleanup
    /// step. Never posted under `--use-network` (Ratect didn't create that
    /// network, so it never removes it).
    RemovingNetwork,
    /// The task failed for an infrastructure reason (image build failed, a
    /// dependency never became healthy, ...) — the counterpart of
    /// [`TaskEvent::TaskFinished`], which covers the task's own command
    /// completing (with any exit code). The error itself still propagates
    /// to stderr through the normal error chain; this event only lets a
    /// live display stop repainting cleanly before that error prints.
    TaskFailed {
        task: String,
    },
    /// One line of a container's own stdout/stderr — only posted under
    /// [`ContainerIoStreaming::Interleaved`] (see that type: in every other
    /// mode the task container's output goes straight to the real stdout
    /// and dependency output isn't captured at all), line-buffered with
    /// trailing `\r` stripped by `docker.rs`.
    ContainerOutput {
        container: String,
        line: String,
    },
    /// One line of a setup command's output — posted by the engine after
    /// the command completes (its output arrives collected, not streamed).
    /// Only the interleaved logger renders these; every other mode
    /// discards setup-command output, matching Batect.
    SetupCommandOutput {
        container: String,
        /// 1-based, matching [`TaskEvent::RunningSetupCommand`].
        index: usize,
        line: String,
    },
}

/// One container in a task's dependency graph, as carried by
/// [`TaskEvent::TaskGraphResolved`] — everything a per-container progress
/// line needs to describe that container's journey without access to the
/// `Config` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContainerInfo {
    pub name: String,
    /// `Some` for an `image` container — the reference
    /// [`TaskEvent::ImagePullProgress`] events carry.
    pub image: Option<String>,
    /// `Some` for a `build_directory` container — the tag
    /// (`<project>-<name>`) [`TaskEvent::ImageBuildProgress`] events carry.
    pub build_tag: Option<String>,
    /// The container's own direct dependencies, for "waiting for X to be
    /// ready" descriptions.
    pub dependencies: Vec<String>,
    /// `true` for the task's own container (the one
    /// [`TaskEvent::RunningTaskContainer`] is about, whose output streams
    /// to stdout).
    pub is_task_container: bool,
}

/// Batect's four output styles — how (and whether) [`TaskEvent`]s render.
/// `None`/unspecified on the command line means "auto-select" — see
/// [`select_output_style`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStyle {
    /// A live-updating, multi-line progress display (one line per container
    /// in the task's dependency graph), for interactive terminals.
    Fancy,
    /// Plain, append-only milestone lines with no live progress detail —
    /// the default on any console that doesn't support `Fancy`.
    Simple,
    /// Error messages only — task lifecycle stays entirely silent. Also
    /// switches `--list-tasks` to a machine-readable format.
    Quiet,
    /// Line-buffered output from *all* containers (dependencies included),
    /// each line prefixed with its container's name.
    All,
}

/// Picks the output style when `--output` wasn't given — a port of Batect's
/// `EventLoggerProvider`/`ConsoleInfo.supportsInteractivity` rule: `Fancy`
/// on a console that can actually support it (stdout is a real terminal,
/// `TERM` is set and isn't `dumb`, and the terminal's dimensions are
/// queryable — each an independent signal that live repainting would
/// misbehave), `Simple` otherwise. `--no-color` also forces `Simple` as the
/// *default* (matching Batect) — an explicit `--output fancy --no-color`
/// still gets colorless fancy (a deliberate Ratect extension; see
/// [`Console`]). `Quiet` and `All` are never auto-selected.
///
/// Batect additionally special-cases mintty (a Windows terminal that fails
/// `isatty`) and a legacy `TRAVIS` environment variable check; Ratect
/// implements neither — Windows is untested here anyway, and modern CI
/// doesn't allocate a TTY, so the terminal check already covers it.
///
/// Pure (every input injected) so the whole decision table is
/// unit-testable; `main.rs` feeds it the real terminal facts.
pub fn select_output_style(
    requested: Option<OutputStyle>,
    no_color: bool,
    stdout_is_terminal: bool,
    term: Option<&str>,
    console_dimensions_available: bool,
) -> OutputStyle {
    if let Some(style) = requested {
        return style;
    }
    if supports_interactivity(stdout_is_terminal, term, console_dimensions_available) && !no_color {
        OutputStyle::Fancy
    } else {
        OutputStyle::Simple
    }
}

/// Whether the console can support a live-repainting display — the shared
/// half of [`select_output_style`]'s auto-selection rule, also used to
/// validate an *explicit* `--output fancy` up front (with a clear error)
/// instead of Batect's behavior of accepting it and crashing on the first
/// repaint. Deliberately excludes `--no-color`: that only influences the
/// *default*, since colorless fancy works fine (see [`Console`]).
pub fn supports_interactivity(
    stdout_is_terminal: bool,
    term: Option<&str>,
    console_dimensions_available: bool,
) -> bool {
    stdout_is_terminal && term.is_some_and(|term| term != "dumb") && console_dimensions_available
}

/// Whether the terminal's dimensions are actually queryable — the
/// "am I really attached to a console" signal [`select_output_style`]
/// wants beyond plain `isatty` (Batect's `ConsoleDimensions.current !=
/// null` check).
pub fn console_dimensions_available() -> bool {
    crossterm::terminal::size().is_ok()
}

/// How container stdout/stderr reaches the user — decided by the selected
/// output style, not configured separately, exactly like Batect (whose
/// `EventLogger` carries its `ioStreamingOptions`): picking the logger *is*
/// picking the I/O policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerIoStreaming {
    /// The task's own container streams raw to the real stdout (with
    /// interactive TTY/stdin eligibility as usual); dependency output isn't
    /// captured at all. Batect's `TaskContainerOnlyIOStreamingOptions` —
    /// what `fancy`/`simple`/`quiet` all use.
    TaskContainerOnly,
    /// Every container's output (dependencies included) is line-buffered
    /// and posted as [`TaskEvent::ContainerOutput`] events instead of
    /// reaching stdout directly; no container gets a TTY or stdin, and
    /// every container gets `TERM=dumb`. Batect's
    /// `InterleavedContainerIOStreamingOptions` — what `all` uses.
    Interleaved,
}

/// Receives every [`TaskEvent`] a task execution produces. Implementations
/// must be safe to call from concurrent branches of the dependency graph —
/// serialize any rendering internally.
pub trait EventSink: Send + Sync {
    fn post(&self, event: TaskEvent);

    /// The I/O streaming policy this logger needs — see
    /// [`ContainerIoStreaming`]. `engine.rs` and `docker.rs` consult this
    /// rather than being configured separately, so the policy can never
    /// disagree with the selected output style.
    fn container_io_streaming(&self) -> ContainerIoStreaming {
        ContainerIoStreaming::TaskContainerOnly
    }
}

/// Discards every event — the default sink wired into
/// `TaskEngine`/`DockerClient` at construction, so unit tests (and any
/// embedding that doesn't want UI output) stay silent unless a real logger
/// is injected.
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn post(&self, _event: TaskEvent) {}
}

/// The colors loggers actually use — grown as needed, not a full ANSI
/// palette up front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

impl Color {
    fn ansi_code(self) -> &'static str {
        match self {
            Color::Red => "31",
            Color::Green => "32",
            Color::Yellow => "33",
            Color::Blue => "34",
            Color::Magenta => "35",
            Color::Cyan => "36",
            Color::White => "37",
        }
    }
}

/// Where a logger writes, plus whether ANSI color is applied when it does —
/// a minimal port of Batect's `Console`. Color and (future) cursor movement
/// are deliberately independent axes here, unlike Batect's single
/// `enableComplexOutput` flag — that coupling is the only reason Batect has
/// to reject `fancy` + `--no-color` at parse time, and Ratect supports that
/// combination instead.
///
/// Writes are serialized by an internal lock so loggers can render from
/// concurrent event posts without interleaving partial lines.
pub struct Console {
    writer: Mutex<Box<dyn Write + Send>>,
    color_enabled: bool,
}

impl Console {
    /// A console on the real stdout, with color enabled iff stdout is a
    /// terminal *and* `--no-color` wasn't given — colors are never emitted
    /// into a pipe or redirection regardless of the flag, matching Batect's
    /// `enableComplexOutput = !disableColorOutput && stdoutIsTTY`.
    pub fn stdout(no_color: bool) -> Self {
        Self::new(
            Box::new(std::io::stdout()),
            !no_color && std::io::stdout().is_terminal(),
        )
    }

    pub fn new(writer: Box<dyn Write + Send>, color_enabled: bool) -> Self {
        Self {
            writer: Mutex::new(writer),
            color_enabled,
        }
    }

    /// Writes `line` plus a newline, flushed immediately — output must
    /// appear as it happens, not whenever the buffer next fills, since
    /// container output written elsewhere shares the same underlying
    /// stdout.
    pub fn println(&self, line: &str) {
        let mut writer = self.writer.lock().unwrap();
        // Deliberately ignore write errors (e.g. a closed pipe on the far
        // end of `ratect ... | head`) rather than panicking or threading a
        // Result through every logger — matching what `println!` would have
        // aborted on anyway, minus the abort.
        let _ = writeln!(writer, "{line}");
        let _ = writer.flush();
    }

    /// `text` wrapped in `color`'s ANSI escape when color is enabled,
    /// unchanged otherwise.
    pub fn colored(&self, color: Color, text: &str) -> String {
        if self.color_enabled {
            format!("\x1b[{}m{}\x1b[0m", color.ansi_code(), text)
        } else {
            text.to_string()
        }
    }

    /// `text` wrapped in the ANSI bold escape when color (SGR styling
    /// generally — `--no-color` suppresses bold too, matching Batect) is
    /// enabled, unchanged otherwise.
    pub fn bold(&self, text: &str) -> String {
        if self.color_enabled {
            format!("\x1b[1m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    /// Writes `text` exactly as given (no trailing newline appended),
    /// flushed immediately, as one atomic write under the console's lock —
    /// how the fancy display emits a whole repaint (cursor movement plus
    /// rewritten lines) without another writer's line landing mid-repaint.
    /// Cursor-movement escapes are deliberately *not* gated on
    /// `color_enabled` — cursor movement and SGR styling are independent
    /// axes here (see the type-level docs).
    pub fn write_raw(&self, text: &str) {
        let mut writer = self.writer.lock().unwrap();
        let _ = write!(writer, "{text}");
        let _ = writer.flush();
    }
}

/// `duration` as a short human-readable string for the task summary line —
/// "3.4s", or "2m 3.4s" from a minute up.
pub(crate) fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs_f64();
    if total_seconds < 60.0 {
        format!("{total_seconds:.1}s")
    } else {
        let minutes = (total_seconds / 60.0).floor();
        format!("{}m {:.1}s", minutes as u64, total_seconds - minutes * 60.0)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// A cloneable in-memory writer, so a test can hand one clone to a
    /// [`super::Console`] and keep the other to read back what was written.
    #[derive(Clone, Default)]
    pub(crate) struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        pub(crate) fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_below_a_minute_is_seconds_with_one_decimal() {
        assert_eq!(format_duration(Duration::from_millis(3450)), "3.5s");
        assert_eq!(format_duration(Duration::from_millis(120)), "0.1s");
    }

    #[test]
    fn format_duration_from_a_minute_up_includes_minutes() {
        assert_eq!(format_duration(Duration::from_secs(63)), "1m 3.0s");
        assert_eq!(format_duration(Duration::from_millis(150_500)), "2m 30.5s");
    }

    #[test]
    fn console_colors_only_when_enabled() {
        let colored = Console::new(Box::new(std::io::sink()), true);
        assert_eq!(colored.colored(Color::Green, "0"), "\x1b[32m0\x1b[0m");
        let plain = Console::new(Box::new(std::io::sink()), false);
        assert_eq!(plain.colored(Color::Red, "1"), "1");
    }

    #[test]
    fn console_println_writes_line_with_newline() {
        let buffer = test_support::SharedBuffer::default();
        let console = Console::new(Box::new(buffer.clone()), false);
        console.println("hello");
        console.println("world");
        assert_eq!(buffer.contents(), "hello\nworld\n");
    }

    /// `select_output_style(requested, no_color, stdout_is_terminal, term,
    /// console_dimensions_available)` shorthand for the decision-table
    /// tests below.
    fn auto(no_color: bool, tty: bool, term: Option<&str>, dimensions: bool) -> OutputStyle {
        select_output_style(None, no_color, tty, term, dimensions)
    }

    #[test]
    fn an_explicit_request_always_wins() {
        // Even on a console that couldn't support it — an explicitly
        // requested style is never second-guessed here (fancy's own
        // interactive-console requirement is enforced at wiring time, with
        // a clear error, not silently overridden).
        for style in [
            OutputStyle::Fancy,
            OutputStyle::Simple,
            OutputStyle::Quiet,
            OutputStyle::All,
        ] {
            assert_eq!(
                select_output_style(Some(style), true, false, None, false),
                style
            );
        }
    }

    #[test]
    fn interactive_console_defaults_to_fancy() {
        assert_eq!(
            auto(false, true, Some("xterm-256color"), true),
            OutputStyle::Fancy
        );
    }

    #[test]
    fn each_non_interactive_signal_alone_forces_simple() {
        // stdout isn't a terminal (piped/redirected/CI).
        assert_eq!(auto(false, false, Some("xterm"), true), OutputStyle::Simple);
        // TERM unset.
        assert_eq!(auto(false, true, None, true), OutputStyle::Simple);
        // TERM=dumb.
        assert_eq!(auto(false, true, Some("dumb"), true), OutputStyle::Simple);
        // Terminal dimensions unavailable.
        assert_eq!(auto(false, true, Some("xterm"), false), OutputStyle::Simple);
    }

    #[test]
    fn no_color_forces_the_default_to_simple_even_on_an_interactive_console() {
        assert_eq!(auto(true, true, Some("xterm"), true), OutputStyle::Simple);
    }

    #[test]
    fn quiet_and_all_are_never_auto_selected() {
        // Exhaustively: the default is only ever Fancy or Simple.
        for no_color in [false, true] {
            for tty in [false, true] {
                for term in [None, Some("dumb"), Some("xterm")] {
                    for dimensions in [false, true] {
                        let style = auto(no_color, tty, term, dimensions);
                        assert!(
                            style == OutputStyle::Fancy || style == OutputStyle::Simple,
                            "auto({no_color}, {tty}, {term:?}, {dimensions}) = {style:?}"
                        );
                    }
                }
            }
        }
    }
}
