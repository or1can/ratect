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
}

/// Receives every [`TaskEvent`] a task execution produces. Implementations
/// must be safe to call from concurrent branches of the dependency graph —
/// serialize any rendering internally.
pub trait EventSink: Send + Sync {
    fn post(&self, event: TaskEvent);
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
    Green,
    Red,
}

impl Color {
    fn ansi_code(self) -> &'static str {
        match self {
            Color::Green => "32",
            Color::Red => "31",
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
    /// terminal — colors are never emitted into a pipe or redirection,
    /// matching Batect's `enableComplexOutput = ... && stdoutIsTTY`.
    pub fn stdout() -> Self {
        Self::new(Box::new(std::io::stdout()), std::io::stdout().is_terminal())
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
}
