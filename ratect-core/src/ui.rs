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
//! actually sees.
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
//! **Streams** (decisions/0017). A logger writes to two consoles and never
//! chooses either: the *progress console* — stdout for `ratect-compat`,
//! stderr for `ratect`, the one file descriptor the two binaries differ in,
//! which each `main` names as a [`ProgressStream`] — carries milestones,
//! status and the summary line, the only class `-o` shapes; stderr carries
//! the other three classes in every style, `quiet` included: an
//! [`TaskEvent::Error`] (`Error: ` in bold red), a [`TaskEvent::Warning`]
//! (`Warning: ` in bold yellow) and a [`TaskEvent::Notice`] (unmarked). A
//! line a user with `RUST_LOG` unset would be worse off without is one of
//! those three, posted as an event, never a `tracing::warn!` — `tracing`
//! is for breadcrumbs and for failures of no consequence to the run, and a
//! printed line may have a `debug!` beside it but never a `warn!` twin.
//! Each [`Console`] decides colour from *its own* stream's `is_terminal()`,
//! and `fancy`'s auto-selection asks the progress console's stream, so
//! `ratect run x > out.txt` on a terminal still gets `fancy`. The task
//! container's own output is the run's *result*, streamed to the real
//! stdout by `docker.rs` in every style and both binaries; it never passes
//! through here except under `all`, whose line-buffered
//! [`TaskEvent::ContainerOutput`] lines for the task container go to
//! stdout (prefixed) while a dependency's follow the progress console —
//! so `ratect run x -o all 2>/dev/null` is still the task's own lines.
//!
//! **Escaping** (#752). A name or value from configuration is untrusted
//! text — a Git bundle's above all — so whatever a [`Console`] prints is a
//! [`Text`], and a plain string becomes one only by having its control
//! characters escaped ([`printable`]): an ESC, BEL or `\r` from config
//! prints as `\u{1b}`, `\u{7}`, `\r`, never as a command to the terminal.
//! Ratect's own styling and cursor movement are `Text` already, so they
//! survive; a line is composed with [`text!`](crate::text), which escapes
//! each argument and keeps the `Text`s it is given. Composing with
//! `format!` instead still prints nothing unescaped — it escapes the styling
//! too, visibly. Only [`Text::raw`] skips escaping: for Ratect's own
//! sequences, and for what passes through by design — a container's own
//! output, which is the user's program's. Image build output is escaped
//! like configuration, since a Git bundle's `Dockerfile` writes the `RUN`
//! lines BuildKit echoes. A warning or notice is one line, so a plain string
//! handed to one is escaped whole, line breaks included. An error keeps its
//! line breaks and tabs — a chain's layout, and the build output, health
//! check output, git stderr or TOML source snippet it may quote — so each
//! value from configuration an error quotes is escaped where the message
//! quotes it, with [`printable`] (#770), as are the YAML and TOML parsers'
//! own messages, which quote keys and values as written. A command keeps
//! its line breaks wherever it is quoted: a multi-line script is ordinary.
//! A site that misses a value can only add a line, never send the terminal
//! an escape sequence, since `Text` still escapes the rest.
//!
//! The user-facing output layer (0.16.0's output-modes
//! work) — a port of Batect's `TaskEventSink`/`EventLogger` design: `engine.rs`
//! posts typed `TaskEvent` milestones and `docker.rs` posts fine-grained
//! pull/build progress to an injected `EventSink` (both default to the silent
//! `NullEventSink`; `main.rs` wires the real logger into both so one sink sees
//! the whole stream), and the selected logger decides what each event renders
//! as — never `println!` from `engine.rs`/`docker.rs` directly. Loggers must
//! serialize rendering internally (events arrive concurrently since 0.15.0);
//! `Console` keeps color and cursor movement as *independent* axes,
//! deliberately unlike Batect's single `enableComplexOutput` flag — that
//! coupling is the only reason Batect rejects `fancy` + `--no-color`, a
//! combination Ratect supports instead (colorless fancy). Milestone events are
//! keyed by container/task name (engine's vocabulary); progress events by
//! image/tag (all `docker.rs` knows) — a logger maps one to the other via the
//! `TaskGraphResolved` event's `TaskContainerInfo`s. The logger also *owns the
//! container I/O policy* (`EventSink::container_io_streaming`, mirroring
//! Batect's `EventLogger.ioStreamingOptions`): `engine.rs` and `docker.rs`
//! consult it rather than being configured separately, which is how `all` mode
//! line-buffers every container's output into `ContainerOutput` events (no
//! TTY/stdin, `TERM=dumb` everywhere) while the other three modes stream the
//! task container raw to stdout — add any future per-mode I/O behavior through
//! that method, not a new engine/docker setting. That raw-streaming half is
//! also why `simple.rs` *drops* the task container's own
//! `ContainerBecameHealthy`/`RunningSetupCommand`/`SetupCommandsCompleted`
//! lines (0.21.0, `is_task_container` — the same three `if (container ==
//! taskContainer) return` guards Batect's own `SimpleEventLogger` has): since
//! the task container's readiness gate, where it has one (the
//! Batect-compatible dialect; a native project's has none), runs
//! *concurrently* with its command (`engine.rs`), printing them would drop a line into the middle of
//! that command's unframed output. `all` mode has no such collision (prefixed,
//! line-buffered) and reports them for every container, matching Batect's own
//! `InterleavedEventLogger`; `fancy` never sees them, since its block freezes
//! the moment the task container starts. Any *new* milestone event for the
//! task's own container needs the same decision made for it. Style selection and logger
//! construction (including the explicit-`fancy`-without-an-interactive-console
//! error) live in `ui::create_event_sink`, not `main.rs` — deliberately, so the
//! `ratect` binary (see `ROADMAP.md`'s two-binaries section) gets this for free
//! once it needs it, instead of reimplementing the style→logger match itself;
//! `ratect-compat/src/main.rs` only gathers the terminal facts once and hands them
//! to it (and to `select_output_style`, for `--list-tasks`'s own quiet-format
//! decision).

pub mod fancy;
pub mod interleaved;
pub mod simple;

use std::io::IsTerminal;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    /// A task with no `run` of its own has had its prerequisites run, and
    /// there is nothing more to do — posted in place of
    /// [`TaskEvent::TaskStarting`], which such a task never posts, each time
    /// one is reached: requested, or itself a prerequisite. Progress, so
    /// `quiet` prints nothing for it (a divergence from Batect, which prints
    /// it in every style).
    PrerequisitesOnly {
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
    /// never posts this, nor does a command that exited `0` and whose
    /// cleanup then failed — [`TaskEvent::TaskFailed`] covers both. Posted
    /// after cleanup, so it's the last event of a successful task execution.
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
    /// BuildKit step's header, `CACHED`, `ERROR:` or log line, labelled with
    /// the step's `#N` (`docker::build_output`) — with no line terminator:
    /// possibly blank, possibly indented. `tag` is the image tag being built
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
    /// `container` is about to be created — its image is resolved and the
    /// task network exists, which is all creation waits for: it does not
    /// wait for the containers it depends on to be ready (decisions/0019).
    /// Posted for a dependency and for the task's own container alike, so
    /// a per-container display can show Batect's `creating container...`;
    /// `simple` and `all` print nothing for it, as Batect's own simple and
    /// interleaved loggers don't.
    ContainerCreating {
        container: String,
    },
    /// Docker has created `container` — it exists, but is not started.
    /// From here it is the engine's to remove, so this is the point a
    /// display may start counting it as outstanding cleanup; Batect's
    /// `ContainerCreatedEvent` is what its cleanup progress display counts
    /// too. Posted for every narrated container, the task's own included.
    ///
    /// Distinct from [`TaskEvent::DependencyStarting`]/
    /// [`TaskEvent::RunningTaskContainer`], which are posted when the
    /// container's *start* begins — after every container it depends on is
    /// ready, which may be long after this.
    ContainerCreated {
        container: String,
    },
    /// A dependency/sidecar container is about to start — every container
    /// it depends on is ready. The task's own container instead posts
    /// [`TaskEvent::RunningTaskContainer`].
    DependencyStarting {
        container: String,
    },
    DependencyStarted {
        container: String,
    },
    /// A container reported healthy — a dependency, or under the
    /// Batect-compatible dialect the task's own container (decisions/0012).
    /// Immediate for a container with no health check at all (the event
    /// still posts, matching Batect); for an externally checked container,
    /// posted by its companion when the check passes (ratect#98).
    ContainerBecameHealthy {
        container: String,
    },
    /// A `run_to_completion` dependency exited with status 0 and is now
    /// ready — what such a container posts in place of
    /// [`TaskEvent::ContainerBecameHealthy`], since it has no health check
    /// and runs to completion instead of staying detached. `ratect`-native
    /// only, like the field itself.
    DependencyCompleted {
        container: String,
    },
    /// One of a container's `setup_commands` is about to run — a
    /// dependency's, or under the Batect-compatible dialect the task's own
    /// container's (decisions/0012). `index` is 1-based, for rendering as
    /// "(n of total)".
    ///
    /// `container` is always the container whose readiness gate this is —
    /// the one that *declared* the command. `run_in` (ratect#111) is the
    /// container it actually runs inside, and is `Some` only when the two
    /// differ; every renderer has to name it, since otherwise the line
    /// reads as though the command ran somewhere it didn't.
    RunningSetupCommand {
        container: String,
        command: String,
        index: usize,
        total: usize,
        run_in: Option<String>,
    },
    /// Every one of a container's `setup_commands` succeeded — a
    /// dependency's, or under the Batect-compatible dialect the task's own
    /// container's. Only posted when it had some.
    SetupCommandsCompleted {
        container: String,
    },
    /// A dependency that had already become ready (healthy, setup commands
    /// done) exited on its own — while the task's own command, or a later
    /// dependency's own health/setup wait, was still going. Never posted
    /// for a container that cleanup itself stops: the engine stops
    /// watching a dependency for this the moment the task's own run
    /// finishes, strictly before cleanup ever touches a container, so a
    /// deliberate stop can never be misreported as this.
    DependencyExitedUnexpectedly {
        container: String,
        exit_code: i64,
    },
    /// The task's own container — already created, see
    /// [`TaskEvent::ContainerCreated`] — is about to be started: every
    /// container it depends on is ready. `command` is the resolved command
    /// (`run.command` falling back to the container's own), `None` when no
    /// command is configured (the image's default `CMD`, or additional args
    /// replacing it, runs instead).
    RunningTaskContainer {
        container: String,
        command: Option<String>,
    },
    /// Teardown (stopping dependency containers, removing the task
    /// network) is about to begin. Not posted when there's nothing to tear
    /// down (`--use-network`, no dependencies started, and no task
    /// container created). `cause` is what cleanup follows when that isn't
    /// the task simply finishing — `None` for a clean exit, and for a signal
    /// or a closed stdout, whose notice already said why.
    CleanupStarting {
        cause: Option<CleanupCause>,
    },
    /// Cleanup has ended, however it went — posted after the last removal
    /// it attempted (or abandoned), and before any line reporting what it
    /// left. Only posted when [`TaskEvent::CleanupStarting`] was. A live
    /// cleanup display stops and clears its countdown here, so nothing
    /// printed afterwards sits under a countdown for work that is over.
    CleanupFinished,
    /// A container was stopped during cleanup and is about to be removed — a
    /// dependency, or the task's own, whether it stopped now or had already
    /// exited, as Batect reports it. Not posted for one created but never
    /// started, nor for one whose stop the cleanup ladder's escalation rung
    /// cut short; one whose stop finished just before that has posted it,
    /// and its removal is then the forced one.
    ContainerStopped {
        container: String,
    },
    /// A container was removed during cleanup — a dependency, or the task's
    /// own.
    ContainerRemoved {
        container: String,
    },
    /// The task's own network is about to be removed — the last cleanup
    /// step. Never posted under `--use-network` (Ratect didn't create that
    /// network, so it never removes it).
    RemovingNetwork,
    /// The task's own network was removed — posted only after a
    /// [`TaskEvent::RemovingNetwork`] whose removal succeeded.
    NetworkRemoved,
    /// The task's run has ended — its command finished, a failure stopped
    /// it, or a signal did — and nothing more of its startup will happen.
    /// Posted the moment the run resolves, before cleanup and before any
    /// notice about how the run ended, so a live display can stop updating
    /// first: the engine knows the run is over, and the display shouldn't
    /// have to infer it from what is printed next.
    RunEnded {
        task: String,
    },
    /// The task failed to run for an infrastructure reason (image build
    /// failed, a dependency never became healthy, ...), or its command
    /// exited `0` and cleanup then failed — the counterpart of
    /// [`TaskEvent::TaskFinished`], which covers the task's own command
    /// completing (with any exit code), and what every style but `quiet`
    /// renders the failed summary line from (`<task> failed after <t>.`).
    /// Posted after cleanup, like `TaskFinished`, with `duration` measured
    /// the same way: from `TaskStarting` to the end of cleanup. It carries
    /// no error: that was reported when it happened, as a
    /// [`TaskEvent::Error`] — before cleanup, or at its end for a cleanup
    /// failure. Never posted for a run a termination signal or
    /// stdout's reader going ended — its notice says so instead — nor for a
    /// task that never reached `TaskStarting`, such as one whose
    /// prerequisite failed.
    TaskFailed {
        task: String,
        duration: Duration,
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
    /// Something about the run the user reads but need not act on — the
    /// interrupt notice, what `--no-cleanup*` left in place. Printed on
    /// stderr, unmarked, in every style including `quiet`; never `tracing`
    /// (decisions/0017: a user with `RUST_LOG` unset is worse off without
    /// it). The three stderr classes below are events rather than direct
    /// `eprintln!`s from the engine so the loggers own every byte that
    /// reaches a stream — `fancy` has to freeze its live block before any
    /// of them lands, or the next repaint counts rows that aren't there.
    Notice {
        message: Text,
    },
    /// Something the user should change, though the run proceeds — a proxy
    /// bound to loopback, a credential helper that failed. `Warning: ` in
    /// bold yellow, then the message in the default colour, on stderr in
    /// every style.
    Warning {
        message: Text,
    },
    /// Ratect failed to do something it set out to do, reported here rather
    /// than by the error the run returns because the run's own error is a
    /// different one (a task that exited `3` whose cleanup then failed) or
    /// carries none of the detail (what the daemon refused, what was left
    /// behind, how to remove it). `Error: ` in bold red, then the message in
    /// the default colour, on stderr in every style. A failure reported this
    /// way is already reported: `main` prints no second `Error:` for the
    /// error it turns into (see `exit_code::already_reported`).
    Error {
        message: Text,
    },
}

/// One container in a task's dependency graph, as carried by
/// [`TaskEvent::TaskGraphResolved`] — everything a per-container progress
/// line needs to describe that container's journey without access to the
/// `Config` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContainerInfo {
    pub name: String,
    /// `Some` for a container whose image is pulled — the reference
    /// [`TaskEvent::ImagePullProgress`] events carry: its `image`, or the
    /// `--override-image` that replaces it (or its `build_directory`).
    pub image: Option<String>,
    /// `Some` for a `build_directory` container that is built — the tag
    /// (`<project>-<name>`) [`TaskEvent::ImageBuildProgress`] events carry.
    /// `None` under `--override-image`, which pulls instead.
    pub build_tag: Option<String>,
    /// The container's own direct dependencies, in the order they were
    /// declared — for the task's own container, the task's `dependencies`
    /// first — each once: what a display orders its lines by, and what it
    /// shows a container waiting for. (Sorted instead for a container the
    /// configuration doesn't declare, which a loaded one never has.)
    pub dependencies: Vec<String>,
    /// Whether the container has `setup_commands` to run once it is
    /// healthy — so whether becoming healthy makes it ready, or only
    /// starts its setup commands, which then end in
    /// [`TaskEvent::SetupCommandsCompleted`].
    pub has_setup_commands: bool,
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

/// The terminal facts every output decision is made from, read once per
/// invocation — [`select_output_style`] and [`create_event_sink`] both want
/// them, and querying twice risks answering differently. `pub` so both
/// binaries construct their own via [`TerminalFacts::gather`] rather than
/// each duplicating the fields and the reasoning above by hand.
#[derive(Debug, Clone)]
pub struct TerminalFacts {
    /// The `TERM` environment variable, unparsed — `None` when unset, and
    /// distinct from `Some("dumb")`; see [`supports_interactivity`].
    pub term: Option<String>,
    /// Whether stdout is a real terminal, not piped/redirected.
    pub stdout_is_terminal: bool,
    /// Whether stderr is a real terminal — asked separately, because the
    /// two are redirected separately (`> out.txt` takes stdout alone) and
    /// each stream's colour, and `fancy`'s eligibility, is decided from the
    /// stream actually being written to (decisions/0017).
    pub stderr_is_terminal: bool,
    /// Whether the terminal's dimensions are actually queryable — see
    /// [`console_dimensions_available`].
    pub console_dimensions_available: bool,
}

impl TerminalFacts {
    /// Collects the signals below — independent, already-existing facts
    /// about this process's own environment, not an inference about what
    /// they mean (that's [`supports_interactivity`]'s job).
    pub fn gather() -> Self {
        Self {
            term: std::env::var("TERM").ok(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            stderr_is_terminal: std::io::stderr().is_terminal(),
            console_dimensions_available: console_dimensions_available(),
        }
    }

    /// Whether `stream` is a real terminal.
    pub fn is_terminal(&self, stream: ProgressStream) -> bool {
        match stream {
            ProgressStream::Stdout => self.stdout_is_terminal,
            ProgressStream::Stderr => self.stderr_is_terminal,
        }
    }
}

/// Which stream a binary's *progress console* is — the stream its own
/// progress (milestones, status, the summary line) is written to. The one
/// thing the two binaries differ in here (decisions/0017): `ratect-compat`
/// keeps Batect's stdout, which a Batect user's scripts may read;
/// `ratect`'s is stderr, so its stdout is the task container's output and
/// nothing else. Each `main` names its own; nothing in this module decides
/// it, and no dialect gate inside a logger ever should — that would be the
/// next place a third difference crept in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressStream {
    Stdout,
    Stderr,
}

impl ProgressStream {
    /// How the stream is named in a message to the user.
    pub fn name(self) -> &'static str {
        match self {
            ProgressStream::Stdout => "stdout",
            ProgressStream::Stderr => "stderr",
        }
    }
}

/// Folds the `NO_COLOR` environment variable (<https://no-color.org> — its
/// spec: present, regardless of value, means no color) into the
/// `--no-color` flag's own boolean, so an unset flag with `NO_COLOR` in the
/// environment behaves exactly as if `--no-color` had been passed —
/// disables color *and* pushes the default output style toward `Simple`
/// the same way the flag already does (see [`select_output_style`]).
/// Deliberately not narrowed to "color only", even though that's closer to
/// the letter of the NO_COLOR spec: `--no-color` already couples the two,
/// and diverging only for the environment variable would make the flag and
/// the variable disagree for no reason a user would expect.
///
/// `env` is `impl Fn(&str) -> Option<String>` rather than calling
/// `std::env::var` directly, matching `proxy.rs`'s own `HostEnv`
/// convention — real `std::env::var` in production, a fixed closure in
/// tests.
pub fn resolve_no_color(cli_flag: bool, env: impl Fn(&str) -> Option<String>) -> bool {
    cli_flag || env("NO_COLOR").is_some()
}

/// Whether [`Console`]'s own ANSI output is suppressed, left to the real
/// terminal, or forced on regardless of the terminal — a plain `bool` can't
/// express "force past a non-terminal", which is exactly what
/// `CLICOLOR_FORCE` needs (see [`resolve_color_mode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Color iff the real console turns out to be a terminal — [`Console::stdout`]'s
    /// own `is_terminal()` check decides.
    Auto,
    /// Never color, regardless of the terminal — `--no-color`/`NO_COLOR`.
    Off,
    /// Always color, even when the terminal check would otherwise say no —
    /// `CLICOLOR_FORCE`.
    ForcedOn,
}

/// Folds `CLICOLOR_FORCE` in on top of [`resolve_no_color`]'s own verdict —
/// takes that verdict directly (`no_color`) rather than re-deriving
/// `--no-color`/`NO_COLOR` itself, since a caller has always already
/// resolved it by the time this runs. `CLICOLOR_FORCE` never gets a say
/// once `no_color` is `true`: an explicit "no color" always wins over a
/// forced "yes color". Deliberately narrower than `NO_COLOR`'s "any value,
/// even empty, counts" rule: `CLICOLOR_FORCE=0` explicitly means "don't
/// force", matching the convention other tools (`ripgrep`, `bat`) already
/// use for it — don't "fix" this to match `NO_COLOR`'s own rule; the two
/// variables have different conventions on purpose.
///
/// Deliberately returns [`ColorMode`], not a second `bool`: `Auto` and
/// `ForcedOn` must never push [`select_output_style`]'s default toward
/// `Simple` the way `Off` does — forcing `fancy`'s cursor-repaint into a
/// non-terminal pipe would corrupt the output outright, so callers pass
/// `no_color` (never this) to that function.
pub fn resolve_color_mode(no_color: bool, env: impl Fn(&str) -> Option<String>) -> ColorMode {
    if no_color {
        return ColorMode::Off;
    }
    match env("CLICOLOR_FORCE") {
        Some(value) if value != "0" => ColorMode::ForcedOn,
        _ => ColorMode::Auto,
    }
}

impl ColorMode {
    /// Whether a stream is coloured under this mode: never for `Off`, always
    /// for `ForcedOn`, and otherwise whatever `is_terminal` says of *that*
    /// stream — asked lazily, so `Off` and `ForcedOn` never probe it.
    pub fn enables_color(self, is_terminal: impl FnOnce() -> bool) -> bool {
        match self {
            ColorMode::Off => false,
            ColorMode::ForcedOn => true,
            ColorMode::Auto => is_terminal(),
        }
    }
}

/// Picks the output style when `--output` wasn't given — a port of Batect's
/// `EventLoggerProvider`/`ConsoleInfo.supportsInteractivity` rule: `Fancy`
/// on a console that can actually support it (the progress console's stream
/// is a real terminal, `TERM` is set and isn't `dumb`, and the terminal's
/// dimensions are queryable — each an independent signal that live
/// repainting would misbehave), `Simple` otherwise. `--no-color` also forces
/// `Simple` as the *default* (matching Batect) — an explicit `--output fancy
/// --no-color` still gets colorless fancy (a deliberate Ratect extension;
/// see [`Console`]). `Quiet` and `All` are never auto-selected.
///
/// Batect additionally special-cases mintty (a Windows terminal that fails
/// `isatty`) and a legacy `TRAVIS` environment variable check; Ratect
/// implements neither — Windows is untested here anyway, and modern CI
/// doesn't allocate a TTY, so the terminal check already covers it.
///
/// Pure (every input injected) so the whole decision table is
/// unit-testable; `main.rs` feeds it the real [`TerminalFacts`].
pub fn select_output_style(
    requested: Option<OutputStyle>,
    no_color: bool,
    terminal: &TerminalFacts,
    progress: ProgressStream,
) -> OutputStyle {
    if let Some(style) = requested {
        return style;
    }
    if supports_interactivity(terminal, progress) && !no_color {
        OutputStyle::Fancy
    } else {
        OutputStyle::Simple
    }
}

/// Whether the progress console can support a live-repainting display — the
/// shared half of [`select_output_style`]'s auto-selection rule, Batect's
/// `ConsoleInfo.supportsInteractivity`: everything an *explicit* `--output
/// fancy` needs (see `explicit_fancy_refusal`), and `TERM` set as well.
/// Asked of `progress`'s stream, not stdout regardless: under `ratect` the
/// block repaints on stderr, so `ratect run x > out.txt` on a terminal is
/// as interactive as it ever was (decisions/0017).
/// Deliberately excludes `--no-color`: that only influences the *default*,
/// since colorless fancy works fine (see [`Console`]).
///
/// **Staying with this heuristic rather than a terminfo lookup, deliberately**
/// (Batect's own roadmap wanted the latter, and this ported the approach it
/// was dissatisfied with): terminfo is Unix-only, so it would sit *beside*
/// this heuristic rather than replace it, and Windows is precisely where
/// detection is hardest; it distinguishes terminals that move the cursor but
/// can't do colour, a combination that's effectively extinct in developer
/// environments today; Batect needed the distinction more than Ratect does,
/// since its `enableComplexOutput` flag coupled colour and cursor movement
/// into one on/off switch, where Ratect keeps them independent axes (a wrong
/// guess here degrades one, not both); it costs a new dependency (a terminfo
/// parser or an ncurses binding) for that narrow benefit; and it doesn't
/// cover what modern terminals actually signal — truecolor is advertised via
/// `COLORTERM`, which terminfo handles poorly, and which Ratect has no use
/// for regardless: [`Color`] only ever emits basic 8-color SGR codes, so
/// there's no truecolor escape anywhere to gate on `COLORTERM` in the first
/// place — not a gap, just nothing to honour. `NO_COLOR`/`CLICOLOR_FORCE`
/// *are* honoured (see [`resolve_no_color`]/[`resolve_color_mode`]) — that
/// was the genuinely useful gap, not a terminfo integration.
pub fn supports_interactivity(terminal: &TerminalFacts, progress: ProgressStream) -> bool {
    terminal.term.is_some() && explicit_fancy_refusal(terminal, progress).is_none()
}

/// Why an explicit `--output fancy` can't run on `progress`'s console, if
/// it can't. Less than [`supports_interactivity`] asks: `TERM` may be unset,
/// since a real terminal without it is very likely capable and Batect, which
/// never reads `TERM` for an explicit style, renders fancy there. Everything
/// refused here is a documented divergence (docs/differences-from-batect.md):
/// Batect accepts an explicit `fancy` unconditionally, then crashes on the
/// first repaint when stdout isn't a terminal, or garbles a `TERM=dumb` one
/// with cursor movements it can't perform. An unqueryable terminal size
/// crashes Batect too.
fn explicit_fancy_refusal(terminal: &TerminalFacts, progress: ProgressStream) -> Option<String> {
    if !terminal.is_terminal(progress) {
        Some(format!("{} isn't a terminal", progress.name()))
    } else if terminal.term.as_deref() == Some("dumb") {
        Some("TERM is 'dumb'".to_string())
    } else if !terminal.console_dimensions_available {
        Some("the terminal size can't be determined".to_string())
    } else {
        None
    }
}

/// Whether the terminal's dimensions are actually queryable — the
/// "am I really attached to a console" signal [`select_output_style`]
/// wants beyond plain `isatty` (Batect's `ConsoleDimensions.current !=
/// null` check). Every lookup error counts as "not queryable", so the style
/// falls back to `simple`. Batect tells "no console" (`null`) apart from a
/// failed native lookup, which it rethrows when choosing a style
/// automatically, failing the run.
pub fn console_dimensions_available() -> bool {
    crossterm::terminal::size().is_ok()
}

/// Constructs the output-mode logger for a task-running invocation: selects
/// the style (see [`select_output_style`]) from the given terminal facts,
/// then builds — and, for an explicit `fancy`, validates — the concrete
/// logger for it. The one place this selection-plus-construction logic
/// lives, so a second CLI binary (see ROADMAP.md's two-binaries section)
/// gets it for free instead of reimplementing the match; callers pass a
/// [`TerminalFacts`] in once (typically already gathered for their own
/// `select_output_style` call, e.g. for a `--list-tasks` quiet-format
/// decision that has no logger to construct) rather than this function
/// gathering them again on top of that.
///
/// Errors only for an explicit `fancy` on a console that can't support live
/// repainting — see `explicit_fancy_refusal` for what that means and how
/// Batect differs. Auto-selected fancy already implies an interactive
/// console, so this can only actually fire for an explicit `-o fancy`.
///
/// `progress` is which stream the binary's progress console is (see
/// [`ProgressStream`]); every logger gets that console plus a stderr one for
/// the three classes every style prints, each coloured by its own stream.
pub fn create_event_sink(
    requested: Option<OutputStyle>,
    color_mode: ColorMode,
    terminal: &TerminalFacts,
    progress: ProgressStream,
) -> anyhow::Result<Arc<dyn EventSink>> {
    // Style selection only ever means "off or not" — `ForcedOn` must never
    // push the default toward `Simple` the way `Off` does (see
    // `resolve_color_mode`'s own doc comment for why).
    let no_color = color_mode == ColorMode::Off;
    let style = select_output_style(requested, no_color, terminal, progress);
    let progress_console = Console::for_stream(progress, color_mode);
    let stderr = Console::stderr(color_mode);
    Ok(match style {
        OutputStyle::Simple => Arc::new(simple::SimpleEventLogger::new(progress_console, stderr)),
        OutputStyle::Quiet => Arc::new(QuietEventLogger::new(stderr)),
        OutputStyle::Fancy => {
            if let Some(reason) = explicit_fancy_refusal(terminal, progress) {
                anyhow::bail!(
                    "Fancy output requires an interactive console ({reason}) — use \
                     --output simple instead."
                );
            }
            Arc::new(fancy::FancyEventLogger::new(
                progress_console,
                stderr,
                terminal.stderr_is_terminal,
            ))
        }
        OutputStyle::All => Arc::new(interleaved::InterleavedEventLogger::new(
            progress_console,
            stderr,
            Console::stdout(color_mode),
        )),
    })
}

/// How container stdout/stderr reaches the user — decided by the selected
/// output style, not configured separately, exactly like Batect (whose
/// `EventLogger` carries its `ioStreamingOptions`): picking the logger *is*
/// picking the I/O policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerIoStreaming {
    /// The task's own container streams raw to the real stdout (with
    /// interactive TTY/stdin eligibility as usual); dependency output isn't
    /// captured at all, and every container gets the host's `TERM`. Batect's
    /// `TaskContainerOnlyIOStreamingOptions` — what `fancy`/`simple`/`quiet`
    /// all use.
    TaskContainerOnly,
    /// Every container's output (dependencies included) is line-buffered
    /// and posted as [`TaskEvent::ContainerOutput`] events instead of
    /// reaching stdout directly; no container gets a TTY or stdin, and
    /// every container gets `TERM=dumb`. Batect's
    /// `InterleavedContainerIOStreamingOptions` — what `all` uses.
    Interleaved,
}

impl ContainerIoStreaming {
    /// Whether this policy ever allows a container to be interactive (a
    /// real TTY, stdin forwarding) at all — `false` under `Interleaved`:
    /// the logger owns stdout line by line there, and a raw TTY stream
    /// can't be split into prefixed lines. The single source of truth for
    /// that gate: `engine.rs` consults it when deciding whether to treat
    /// the task's own container as interactive-eligible in the first
    /// place, and `docker.rs` consults it again, independently, right
    /// before actually attaching — both call this instead of hand-rolling
    /// their own `== `/`!= ` comparison against a specific variant, so a
    /// future third policy can't leave the two checks disagreeing about
    /// which containers it applies to (the exhaustive `match` below forces
    /// an explicit answer for it, at the definition site, the moment it's
    /// added — unlike a comparison against one already-named variant,
    /// which would silently keep compiling with the wrong answer).
    pub fn allows_interactive(self) -> bool {
        match self {
            ContainerIoStreaming::TaskContainerOnly => true,
            ContainerIoStreaming::Interleaved => false,
        }
    }
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

    /// Whether this logger renders any of the fine-grained progress events
    /// (`ImagePullProgress`/`ImageBuildProgress`/`SetupCommandOutput`) at
    /// all. `false` by default, matching `simple`/`quiet`/`NullEventSink` —
    /// none of which render any of the three — so `docker.rs`/`engine.rs`
    /// can skip constructing (and posting) those events entirely in the
    /// common case, instead of paying a `String` allocation per pull/build
    /// status line or setup-command output line only for `post` to
    /// immediately discard it.
    ///
    /// Deliberately coarse — one flag for all three event kinds, not a
    /// separate query per kind — even though `fancy` and `all` each render
    /// only some of them (`fancy`: pull+build progress, not setup-command
    /// output; `all`: build progress and setup-command output, not pull
    /// progress). The residual cost of occasionally posting an event a
    /// "rich" mode itself still no-ops on is negligible next to skipping
    /// construction entirely for every `simple`/`quiet` run, and keeps this
    /// trait's surface from growing one method per event kind.
    fn wants_progress_detail(&self) -> bool {
        false
    }

    /// Whether the stream this logger writes the task container's own output
    /// to has refused a write because its reader has gone — see
    /// [`crate::docker::StdoutClosed`]. Only a logger that writes that output
    /// itself (the [`ContainerIoStreaming::Interleaved`] one) can know;
    /// otherwise `docker.rs` writes it and sees the failure directly.
    fn result_reader_gone(&self) -> bool {
        false
    }
}

/// Discards every event — the default sink wired into
/// `TaskEngine`/`DockerClient` at construction, so unit tests (and any
/// embedding that doesn't want UI output) stay silent unless a real logger
/// is injected. Not what `-o quiet` gets: that is [`QuietEventLogger`],
/// which still prints the stderr classes.
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn post(&self, _event: TaskEvent) {}
}

/// The `quiet` style: no progress at all — but errors, warnings and notices
/// still reach stderr, as in every style. `quiet` means "no progress", not
/// "nothing" (decisions/0017): the interrupt notice and the `--no-cleanup*`
/// reminder are exactly the lines a scripted, quiet run can't do without.
/// Batect's quiet logger renders only task-failure events; a run's failure
/// is one of those three classes here (an `Error` event, posted when it
/// happens), so the three classes are all this one renders.
pub struct QuietEventLogger {
    stderr: Console,
}

impl QuietEventLogger {
    pub fn new(stderr: Console) -> Self {
        Self { stderr }
    }
}

impl EventSink for QuietEventLogger {
    fn post(&self, event: TaskEvent) {
        self.stderr.print_stderr_class(&event);
    }
}

/// What a config load prints itself. A load runs before either binary has
/// created its event sink — the sink is built from the loaded project — so
/// the load can't post [`TaskEvent`]s, and gets this instead. Same streams
/// and styles as the sink (decisions/0017): a notice goes to stderr in every
/// style, `-o quiet` included; progress goes to the binary's progress
/// console, and nowhere under `quiet`. Waiting on a Git include's entry lock
/// (decisions/0023) is a notice; cloning one is progress. Cheap to clone:
/// every clone writes through the same two consoles.
#[derive(Clone)]
pub struct LoadOutput {
    stderr: Arc<Console>,
    progress: Option<Arc<Console>>,
}

impl LoadOutput {
    /// `style` as the binary selected it, `progress` its progress stream.
    pub fn new(style: OutputStyle, color_mode: ColorMode, progress: ProgressStream) -> Self {
        Self::from_consoles(style, Console::stderr(color_mode), || {
            Console::for_stream(progress, color_mode)
        })
    }

    /// `progress` is made, and used, only for a style that shows progress.
    fn from_consoles(
        style: OutputStyle,
        stderr: Console,
        progress: impl FnOnce() -> Console,
    ) -> Self {
        Self {
            stderr: Arc::new(stderr),
            progress: (style != OutputStyle::Quiet).then(|| Arc::new(progress())),
        }
    }

    pub fn notice(&self, message: impl ToText) {
        self.stderr.notice(message);
    }

    pub fn progress(&self, line: impl Into<Text>) {
        if let Some(progress) = &self.progress {
            progress.println(line);
        }
    }

    /// `text` as [`Console::bold`] renders it on the progress console, for
    /// the user's own names in a [`progress`](Self::progress) line
    /// (decisions/0017).
    pub fn progress_bold(&self, text: &str) -> Text {
        match &self.progress {
            Some(progress) => progress.bold(text),
            None => text.to_text(),
        }
    }

    /// Writes to the two buffers it returns, stderr's then progress's.
    #[cfg(test)]
    pub(crate) fn capturing(
        style: OutputStyle,
    ) -> (Self, test_support::SharedBuffer, test_support::SharedBuffer) {
        let stderr = test_support::SharedBuffer::default();
        let progress = test_support::SharedBuffer::default();
        let output =
            Self::from_consoles(style, Console::new(Box::new(stderr.clone()), false), || {
                Console::new(Box::new(progress.clone()), false)
            });
        (output, stderr, progress)
    }
}

/// For a load no binary chose a style for —
/// [`Config::load_from_file`](crate::config::Config::load_from_file) and the
/// Git include verbs: notices on stderr, no progress.
impl Default for LoadOutput {
    fn default() -> Self {
        Self {
            stderr: Arc::new(Console::stderr(ColorMode::Auto)),
            progress: None,
        }
    }
}

/// Why cleanup began, when it isn't the task simply finishing — what the
/// opening cleanup line names (decisions/0017): the task's own non-zero
/// exit, or the step that failed and the container it failed for. The
/// `Error:` line before cleanup says how; this says what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupCause {
    /// The task's own container exited with a non-zero code.
    Exited {
        task: String,
        exit_code: i64,
    },
    ImagePullFailed {
        container: String,
    },
    ImageBuildFailed {
        container: String,
    },
    CreateFailed {
        container: String,
    },
    StartFailed {
        container: String,
    },
    NotHealthy {
        container: String,
    },
    /// `index` is 1-based, as `Running setup command` numbers it.
    SetupCommandExited {
        container: String,
        index: usize,
        exit_code: i64,
    },
    SetupCommandNotRun {
        container: String,
        index: usize,
    },
    /// A `run_to_completion` dependency exited with a non-zero code.
    DependencyExited {
        container: String,
        exit_code: i64,
    },
    /// A failure no step claimed — named by the task it ended.
    Failed {
        task: String,
    },
}

/// The colors loggers actually use — grown as needed, not a full ANSI
/// palette up front. Never white or black (SGR 37/30, bright or not): those
/// are the theme's own foreground and background, light grey on a dark theme
/// and near-invisible on Solarized Light, so plain text is the default
/// foreground (no SGR at all) and emphasis is [`Console::bold`]
/// (decisions/0017). Only the plain 31–36 range, for the same reason — the
/// bright variants are remapped to greys by some themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
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
        }
    }
}

/// `value` with each control character escaped as Rust would (`\u{1b}`,
/// `\n`, `\0`), for printing a value from configuration: a raw ESC, BEL or
/// carriage return would otherwise reach the terminal as a command —
/// retitling it, or overwriting the line it is printed on (#752) — and a raw
/// NUL prints as nothing, so a path refused for holding one would look fine
/// (#365). Nothing else is escaped, so an ordinary value, Unicode included,
/// reads exactly as it was written. The one escaping rule: [`Text`] applies
/// it to everything a [`Console`] prints, and an error message to each value
/// from configuration it quotes (#770).
pub fn printable(value: &str) -> String {
    escape_controls(value, |_| true)
}

/// `value` with each control character `escaped` says to escape written as
/// [`char::escape_debug`] writes it — [`printable`]'s rule, for a caller that
/// must keep some control characters as they are.
pub fn escape_controls(value: &str, escaped: impl Fn(char) -> bool) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c.is_control() && escaped(c) {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

/// Text that is safe to print: every control character in it is either
/// escaped ([`printable`]) or one Ratect wrote itself — a style from
/// [`Console::bold`] and its siblings, a line break or tab in a message, the
/// literal part of a [`text!`](crate::text) line. What every [`Console`]
/// entry point takes (#752), so a string from configuration can't reach a
/// terminal unescaped by default: a plain `&str` or `String` converts by
/// escaping, keeping only its line breaks and tabs — a message's layout,
/// and the layout of the program output an error can quote (a failed build's
/// output, a health check's), neither of which can hide or overwrite
/// anything — and only [`Text::raw`] skips escaping, for the few things
/// that must pass through as written.
///
/// Composing a line by `format!` and handing it over as a `String` is
/// therefore *safe but wrong*: the styles in it get escaped too, which shows.
/// Compose with [`text!`](crate::text) instead, which escapes each argument
/// (a value from configuration, line breaks and all) and keeps the
/// [`Text`]s it is given as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text(String);

impl Text {
    /// `text` as written, control characters included — for Ratect's own
    /// escape sequences, and for what passes through by design: a
    /// container's own output, which is the user's program's (#752's scope),
    /// and generated files printed whole.
    /// Never for anything from configuration.
    pub fn raw(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `items` with `separator`, Ratect's own, between each — the [`Text`]
    /// form of `[..].join(..)`.
    pub fn join(items: impl IntoIterator<Item = Text>, separator: &'static str) -> Self {
        let parts: Vec<String> = items.into_iter().map(|text| text.0).collect();
        Self(parts.join(separator))
    }
}

impl std::fmt::Display for Text {
    /// Pads like a `str`, so `{:>8}` aligns a [`Text`] by its characters.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(&self.0)
    }
}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for Text {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// A whole line or message: every control character escaped except a line
/// break or a tab.
impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self(escape_controls(text, |c| c != '\n' && c != '\t'))
    }
}

impl From<String> for Text {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&String> for Text {
    fn from(text: &String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&Text> for Text {
    fn from(text: &Text) -> Self {
        text.clone()
    }
}

/// One value as it goes into a [`text!`](crate::text) line or a
/// [`Console::bold`]-style span: a string from configuration escaped whole,
/// line breaks included, since a name or a value has no lines of its own; a
/// [`Text`] as it is; a number as it prints.
pub trait ToText {
    fn to_text(&self) -> Text;
}

impl ToText for str {
    fn to_text(&self) -> Text {
        Text(printable(self))
    }
}

impl ToText for String {
    fn to_text(&self) -> Text {
        self.as_str().to_text()
    }
}

/// A path, as `Path::display` writes it — escaped like any string, since a
/// path can come from configuration too.
impl ToText for std::path::Display<'_> {
    fn to_text(&self) -> Text {
        self.to_string().to_text()
    }
}

impl ToText for Text {
    fn to_text(&self) -> Text {
        self.clone()
    }
}

impl<T: ToText + ?Sized> ToText for &T {
    fn to_text(&self) -> Text {
        (**self).to_text()
    }
}

macro_rules! number_to_text {
    ($($number:ty),*) => {
        $(impl ToText for $number {
            fn to_text(&self) -> Text {
                Text(self.to_string())
            }
        })*
    };
}

number_to_text!(i32, i64, u16, u32, u64, usize);

/// `format!` for a [`Text`] line: the format string is Ratect's own and kept
/// as written, and each argument goes in through [`ToText`] — escaped if it
/// is a string, as it is if it is already a [`Text`]. A name captured inline
/// (`{name}`) would be formatted by `format!` itself, bypassing the escaping,
/// so the format string may not hold one: that is a compile error.
#[macro_export]
macro_rules! text {
    ($format:literal $(, $argument:expr)* $(,)?) => {{
        const _: () = assert!(
            !$crate::ui::captures_inline($format),
            "text! takes every value as an argument, never captured inline"
        );
        $crate::ui::Text::raw(format!(
            $format
            $(, $crate::ui::ToText::to_text(&$argument))*
        ))
    }};
}

/// Whether `format` names a variable inline (`{name}`, `{名前:>4}`) — what
/// [`text!`](crate::text) refuses. `{{` is a literal brace, and `{}`, `{:>4}`
/// and `{0}` take an argument: anything else after a `{` starts a name, which
/// may begin with any Unicode letter.
#[doc(hidden)]
pub const fn captures_inline(format: &str) -> bool {
    let bytes = format.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                i += 2;
                continue;
            }
            if i + 1 < bytes.len()
                && !matches!(bytes[i + 1], b'}' | b':')
                && !bytes[i + 1].is_ascii_digit()
            {
                return true;
            }
        }
        i += 1;
    }
    false
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
///
/// **A failed write is ignored, and the command carries on to completion** —
/// a pipe whose reader has gone (`| head -c 0`), a closed stderr (`2>&-`) —
/// as Batect's `PrintStream` does. That's why every line either binary
/// prints itself goes through a `Console`, never `println!`/`eprintln!`,
/// which panic there: a `clean` verb would stop before or partway through
/// its removal and exit `101` (ratect#444, ratect#668, ratect#669). The
/// crates deny `clippy::print_stdout`/`print_stderr` to keep it that way;
/// the panic hook is the one allowed exception. A run's own output is the
/// one thing that must *not* carry on, so a console also remembers a reader
/// that has gone ([`Console::reader_gone`]) for the run to ask about
/// (ratect#670).
pub struct Console {
    writer: Mutex<Box<dyn Write + Send>>,
    color_enabled: bool,
    reader_gone: AtomicBool,
}

impl Console {
    /// A console on the real stdout, colored according to `color_mode`
    /// (see [`ColorMode`]): never for `Off`, always for `ForcedOn`
    /// (`CLICOLOR_FORCE` — colors reach a pipe or redirection on purpose),
    /// and otherwise iff stdout is actually a terminal — matching Batect's
    /// `enableComplexOutput = !disableColorOutput && stdoutIsTTY` for the
    /// `Auto`/`Off` cases, which is all Batect itself has.
    pub fn stdout(color_mode: ColorMode) -> Self {
        let color_enabled = color_mode.enables_color(|| std::io::stdout().is_terminal());
        Self::new(Box::new(std::io::stdout()), color_enabled)
    }

    /// A console on the real stderr, colored by the same rule as
    /// [`Console::stdout`] but asked of *stderr's* own `is_terminal()` —
    /// Batect builds its two consoles the same way (`stdoutIsTTY` and
    /// `stderrIsTTY` separately). What `ratect`'s progress and both
    /// binaries' errors, warnings and notices are written to; the `tracing`
    /// subscriber follows the same decision via [`Console::color_enabled`].
    pub fn stderr(color_mode: ColorMode) -> Self {
        let color_enabled = color_mode.enables_color(|| std::io::stderr().is_terminal());
        Self::new(Box::new(std::io::stderr()), color_enabled)
    }

    /// [`Console::stdout`] or [`Console::stderr`], whichever `stream` names.
    pub fn for_stream(stream: ProgressStream, color_mode: ColorMode) -> Self {
        match stream {
            ProgressStream::Stdout => Self::stdout(color_mode),
            ProgressStream::Stderr => Self::stderr(color_mode),
        }
    }

    pub fn new(writer: Box<dyn Write + Send>, color_enabled: bool) -> Self {
        Self {
            writer: Mutex::new(writer),
            color_enabled,
            reader_gone: AtomicBool::new(false),
        }
    }

    /// Whether a write has failed because this console's reader has gone —
    /// a closed pipe. Any other failed write doesn't count.
    pub fn reader_gone(&self) -> bool {
        self.reader_gone.load(Ordering::Relaxed)
    }

    /// Ignores `result`'s failure, as every write here does, but remembers a
    /// reader that has gone.
    fn note_failure(&self, result: std::io::Result<()>) {
        if result.is_err_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe) {
            self.reader_gone.store(true, Ordering::Relaxed);
        }
    }

    /// Whether this console applies SGR styling — decided once at
    /// construction from its [`ColorMode`] and its own stream.
    pub fn color_enabled(&self) -> bool {
        self.color_enabled
    }

    /// `message` as an error line: `Error: ` in bold red, the message in the
    /// default colour (decisions/0017's marker). The marker says "act on
    /// this"; a message that is read, not acted on, is a [`Console::notice`].
    pub fn error(&self, message: impl Into<Text>) {
        let message: Text = message.into();
        self.println(text!(
            "{} {}",
            self.bold_colored(Color::Red, "Error:"),
            message
        ));
    }

    /// `message` as a warning line: `Warning: ` in bold yellow, the message
    /// in the default colour.
    ///
    /// One line, so a plain string has every control character escaped, a
    /// line break included (#752) — only [`Console::error`] lays out a
    /// message over several lines, an error chain's.
    pub fn warning(&self, message: impl ToText) {
        let message = message.to_text();
        self.println(text!(
            "{} {}",
            self.bold_colored(Color::Yellow, "Warning:"),
            message
        ));
    }

    /// `message` as a notice: unmarked, since it asks for nothing. One
    /// line, escaped as [`Console::warning`]'s is.
    pub fn notice(&self, message: impl ToText) {
        self.println(message.to_text());
    }

    /// Prints `event` if it is one of the three stderr classes
    /// ([`TaskEvent::Error`]/[`TaskEvent::Warning`]/[`TaskEvent::Notice`])
    /// and returns whether it was — the one rendering every logger shares,
    /// so the markers can't drift between styles. Call on the stderr
    /// console; a logger with a live display freezes it first.
    pub fn print_stderr_class(&self, event: &TaskEvent) -> bool {
        match event {
            TaskEvent::Error { message } => self.error(message),
            TaskEvent::Warning { message } => self.warning(message),
            TaskEvent::Notice { message } => self.notice(message),
            _ => return false,
        }
        true
    }

    /// Writes `line` plus a newline, flushed immediately — output must
    /// appear as it happens, not whenever the buffer next fills, since
    /// container output written elsewhere shares the same underlying
    /// stdout. `line` escaped as [`Text`] says.
    pub fn println(&self, line: impl Into<Text>) {
        let line = line.into();
        let mut writer = self.writer.lock().unwrap();
        // Deliberately ignore write errors (e.g. a closed pipe on the far
        // end of `ratect ... | head`) rather than panicking or threading a
        // Result through every logger — matching what `println!` would have
        // aborted on anyway, minus the abort. No explicit `flush()` here
        // (unlike `print`, below): the real production writer is
        // `std::io::stdout()`, whose internal `LineWriter` already flushes
        // on every `\n` this always writes — an extra flush call would just
        // be a second, redundant syscall per line.
        self.note_failure(writeln!(writer, "{line}"));
    }

    /// `text` wrapped in `color`'s ANSI escape when color is enabled,
    /// unchanged otherwise — escaped as [`ToText`] says either way.
    pub fn colored(&self, color: Color, text: &(impl ToText + ?Sized)) -> Text {
        self.styled(color.ansi_code(), text)
    }

    /// `text` wrapped in the ANSI bold escape when color (SGR styling
    /// generally — `--no-color` suppresses bold too, matching Batect) is
    /// enabled, unchanged otherwise.
    pub fn bold(&self, text: &(impl ToText + ?Sized)) -> Text {
        self.styled("1", text)
    }

    /// `text` both bold and in `color`, as one SGR sequence (`1;31`) rather
    /// than one nested in the other — what the `Error:`/`Warning:` markers
    /// are, and what a test pins byte for byte.
    pub fn bold_colored(&self, color: Color, text: &(impl ToText + ?Sized)) -> Text {
        self.styled(&format!("1;{}", color.ansi_code()), text)
    }

    /// `text` in SGR `code` when color is enabled — one span per line, each
    /// reset before its newline, so `all` can split styled text into
    /// prefixed lines without a prefix inheriting the style or a line
    /// losing it (Batect's `TextRun.lines` keeps each piece's style the
    /// same way). An empty line gets no escape.
    fn styled(&self, code: &str, text: &(impl ToText + ?Sized)) -> Text {
        let text = text.to_text();
        if !self.color_enabled {
            return text;
        }
        let styled = text
            .as_str()
            .split('\n')
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("\x1b[{code}m{line}\x1b[0m")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        Text::raw(styled)
    }

    /// Writes `text` (escaped as [`Text`] says) with no trailing newline
    /// appended, flushed immediately, as one atomic write under the
    /// console's lock — how the fancy display emits a whole repaint (cursor
    /// movement plus rewritten lines, as one [`Text::raw`]) without another
    /// writer's line landing mid-repaint. Cursor-movement escapes are
    /// deliberately *not* gated on `color_enabled` — cursor movement and SGR
    /// styling are independent axes here (see the type-level docs).
    pub fn print(&self, text: impl Into<Text>) {
        let text = text.into();
        let mut writer = self.writer.lock().unwrap();
        let result = write!(writer, "{text}").and_then(|()| writer.flush());
        self.note_failure(result);
    }
}

/// A "do this exactly once, until reset" guard — the `Cleaning up...`
/// once-per-task pattern `simple.rs`/`interleaved.rs` both need (several
/// cleanup-worthy events can arrive for one task, but the line prints only
/// for the first).
#[derive(Default)]
pub(crate) struct OnceFlag(bool);

impl OnceFlag {
    /// `true` (and marks itself fired) only the first call since
    /// construction or the last [`OnceFlag::reset`]; every call after that
    /// returns `false` and does nothing.
    pub(crate) fn fire_once(&mut self) -> bool {
        if self.0 {
            false
        } else {
            self.0 = true;
            true
        }
    }

    pub(crate) fn reset(&mut self) {
        self.0 = false;
    }
}

/// The opening cleanup line `simple` and `all` print: `Cleaning up...`, or
/// `Cleaning up after <what failed>...` when cleanup follows a failure,
/// subject first and the user's own names and values bold (decisions/0017).
pub(crate) fn cleaning_up(console: &Console, cause: Option<&CleanupCause>) -> Text {
    let bold = |text: &str| console.bold(text);
    let after = match cause {
        None => return Text::raw("Cleaning up..."),
        Some(CleanupCause::Exited { task, exit_code }) => text!(
            "{} exited with code {}",
            bold(task),
            bold(&exit_code.to_string())
        ),
        Some(CleanupCause::ImagePullFailed { container }) => {
            text!("{}'s image failed to pull", bold(container))
        }
        Some(CleanupCause::ImageBuildFailed { container }) => {
            text!("{}'s image failed to build", bold(container))
        }
        Some(CleanupCause::CreateFailed { container }) => {
            text!("{} failed to be created", bold(container))
        }
        Some(CleanupCause::StartFailed { container }) => {
            text!("{} failed to start", bold(container))
        }
        Some(CleanupCause::NotHealthy { container }) => {
            text!("{} did not become healthy", bold(container))
        }
        Some(CleanupCause::SetupCommandExited {
            container,
            index,
            exit_code,
        }) => text!(
            "{}'s setup command {} exited with code {}",
            bold(container),
            index,
            bold(&exit_code.to_string())
        ),
        Some(CleanupCause::SetupCommandNotRun { container, index }) => {
            text!(
                "{}'s setup command {} could not be run",
                bold(container),
                index
            )
        }
        Some(CleanupCause::DependencyExited {
            container,
            exit_code,
        }) => text!(
            "{} exited with code {}",
            bold(container),
            bold(&exit_code.to_string())
        ),
        Some(CleanupCause::Failed { task }) => text!("{} failed", bold(task)),
    };
    text!("Cleaning up after {}...", after)
}

/// A container's command as every style shows it: bold, the user's own
/// value (decisions/0017), with every control character escaped except a
/// line break or a tab. A multi-line command is ordinary — a script — and each style
/// lays its lines out itself (`all` prefixes each one, `fancy` keeps its row
/// to one), where a name has no lines of its own and escapes them too.
pub(crate) fn command(console: &Console, command: &str) -> Text {
    console.bold(&Text::from(command))
}

/// What every non-quiet style prints for [`TaskEvent::PrerequisitesOnly`]:
/// Batect's own sentence, the task's name in bold.
pub(crate) fn nothing_more_to_do(console: &Console, task: &str) -> Text {
    text!(
        "The task {} only defines prerequisite tasks, nothing more to do.",
        console.bold(task)
    )
}

/// The summary line every non-quiet style ends a task that failed to run
/// with — `<task> failed after <duration>.`, the name bold and nothing
/// coloured. It says the run ended, never why: the error already said that,
/// wherever it was printed. One phrase, here, so a test pins the wording
/// once for all three styles.
pub(crate) fn format_task_failed_summary(
    console: &Console,
    task: &str,
    duration: Duration,
) -> Text {
    text!(
        "{} failed after {}.",
        console.bold(task),
        format_duration(duration)
    )
}

/// The task summary line every output mode ends a task with —
/// `<task> finished with exit code <n> in <duration>.`, the name bold and
/// the exit code bold, green for `0` or red otherwise.
pub(crate) fn format_task_summary(
    console: &Console,
    task: &str,
    exit_code: i64,
    duration: Duration,
) -> Text {
    let color = if exit_code == 0 {
        Color::Green
    } else {
        Color::Red
    };
    let exit_code = console.bold_colored(color, &exit_code.to_string());
    text!(
        "{} finished with exit code {} in {}.",
        console.bold(task),
        exit_code,
        format_duration(duration)
    )
}

/// What a dependency exiting on its own says — `exited unexpectedly with
/// exit code <n>` — in `simple`'s milestone line and `fancy`'s status line
/// alike. A milestone, not a warning (decisions/0017): a dependency exiting
/// is the event the run's failure, if one follows, will explain, so it
/// wears no `Warning:` and goes to the progress console like any other.
/// One phrase, here, so a test pins the wording once; the same shape every
/// other event the two styles share will take as ratect#505/#507 move them
/// in. The exit code is bold, the user's own value (decisions/0017), and
/// uncoloured: a palette colour on an exit code is the summary line's.
pub(crate) fn exited_unexpectedly(console: &Console, exit_code: i64) -> Text {
    text!(
        "exited unexpectedly with exit code {}",
        console.bold(&exit_code.to_string())
    )
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

    /// A writer whose every write and flush fails with its error kind —
    /// `BrokenPipe` for a reader that has gone.
    pub(crate) struct FailingWriter(pub(crate) std::io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(self.0.into())
        }
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
