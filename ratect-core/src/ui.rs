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
    /// container created).
    CleanupStarting,
    /// Cleanup has ended, however it went — posted after the last removal
    /// it attempted (or abandoned), and before any line reporting what it
    /// left. Only posted when [`TaskEvent::CleanupStarting`] was. A live
    /// cleanup display stops and clears its countdown here, so nothing
    /// printed afterwards sits under a countdown for work that is over.
    CleanupFinished,
    /// A container was stopped and removed during cleanup — a dependency,
    /// or the task's own.
    ContainerRemoved {
        container: String,
    },
    /// The task's own network is about to be removed — the last cleanup
    /// step. Never posted under `--use-network` (Ratect didn't create that
    /// network, so it never removes it).
    RemovingNetwork,
    /// The task's run has ended — its command finished, a failure stopped
    /// it, or a signal did — and nothing more of its startup will happen.
    /// Posted the moment the run resolves, before cleanup and before any
    /// notice about how the run ended, so a live display can stop updating
    /// first: the engine knows the run is over, and the display shouldn't
    /// have to infer it from what is printed next.
    RunEnded {
        task: String,
    },
    /// The task failed for an infrastructure reason (image build failed, a
    /// dependency never became healthy, ...) — the counterpart of
    /// [`TaskEvent::TaskFinished`], which covers the task's own command
    /// completing (with any exit code). Posted after cleanup, like
    /// `TaskFinished`. It carries no error: that was reported when it
    /// happened, before cleanup, as a [`TaskEvent::Error`] (or, for a
    /// signal, by the interrupt notice).
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
    /// Something about the run the user reads but need not act on — the
    /// interrupt notice, what `--no-cleanup*` left in place. Printed on
    /// stderr, unmarked, in every style including `quiet`; never `tracing`
    /// (decisions/0017: a user with `RUST_LOG` unset is worse off without
    /// it). The three stderr classes below are events rather than direct
    /// `eprintln!`s from the engine so the loggers own every byte that
    /// reaches a stream — `fancy` has to freeze its live block before any
    /// of them lands, or the next repaint counts rows that aren't there.
    Notice {
        message: String,
    },
    /// Something the user should change, though the run proceeds — a proxy
    /// bound to loopback, a credential helper that failed. `Warning: ` in
    /// bold yellow, then the message in the default colour, on stderr in
    /// every style.
    Warning {
        message: String,
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
        message: String,
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
/// shared half of [`select_output_style`]'s auto-selection rule, also used
/// to validate an *explicit* `--output fancy` up front (with a clear error).
/// Asked of `progress`'s stream, not stdout regardless: under `ratect` the
/// block repaints on stderr, so `ratect run x > out.txt` on a terminal is
/// as interactive as it ever was (decisions/0017).
/// Batect never checks an explicit `fancy`: it crashes on the first repaint
/// when stdout isn't a terminal, and with `TERM=dumb` writes cursor
/// movements the terminal can't perform. Deliberately excludes
/// `--no-color`: that only influences the *default*, since colorless fancy
/// works fine (see [`Console`]).
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
    terminal.is_terminal(progress)
        && terminal.term.as_deref().is_some_and(|term| term != "dumb")
        && terminal.console_dimensions_available
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
/// repainting — Batect instead accepts it, crashing on the first repaint
/// when stdout isn't a terminal and garbling a `TERM=dumb` one (a
/// documented divergence, see docs/differences-from-batect.md).
/// Auto-selected fancy already implies an interactive console, so this can
/// only actually fire for an explicit `-o fancy`.
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
            if !supports_interactivity(terminal, progress) {
                anyhow::bail!(
                    "Fancy output requires an interactive console ({} isn't a \
                     terminal, TERM is unset or 'dumb', or the terminal size can't \
                     be determined) — use --output simple instead.",
                    progress.name()
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
    pub fn error(&self, message: &str) {
        self.println(&format!(
            "{} {message}",
            self.bold_colored(Color::Red, "Error:")
        ));
    }

    /// `message` as a warning line: `Warning: ` in bold yellow, the message
    /// in the default colour.
    pub fn warning(&self, message: &str) {
        self.println(&format!(
            "{} {message}",
            self.bold_colored(Color::Yellow, "Warning:")
        ));
    }

    /// `message` as a notice: unmarked, since it asks for nothing.
    pub fn notice(&self, message: &str) {
        self.println(message);
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
    /// stdout.
    pub fn println(&self, line: &str) {
        let mut writer = self.writer.lock().unwrap();
        // Deliberately ignore write errors (e.g. a closed pipe on the far
        // end of `ratect ... | head`) rather than panicking or threading a
        // Result through every logger — matching what `println!` would have
        // aborted on anyway, minus the abort. No explicit `flush()` here
        // (unlike `write_raw`, below): the real production writer is
        // `std::io::stdout()`, whose internal `LineWriter` already flushes
        // on every `\n` this always writes — an extra flush call would just
        // be a second, redundant syscall per line.
        self.note_failure(writeln!(writer, "{line}"));
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

    /// `text` both bold and in `color`, as one SGR sequence (`1;31`) rather
    /// than one nested in the other — what the `Error:`/`Warning:` markers
    /// are, and what a test pins byte for byte.
    pub fn bold_colored(&self, color: Color, text: &str) -> String {
        if self.color_enabled {
            format!("\x1b[1;{}m{}\x1b[0m", color.ansi_code(), text)
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

/// What every non-quiet style prints for [`TaskEvent::PrerequisitesOnly`]:
/// Batect's own sentence, the task's name in bold.
pub(crate) fn nothing_more_to_do(console: &Console, task: &str) -> String {
    format!(
        "The task {} only defines prerequisite tasks, nothing more to do.",
        console.bold(task)
    )
}

/// The task summary line every output mode ends a task with —
/// `<task_label> finished with exit code <n> in <duration>.`, the exit code
/// colored green for `0` or red otherwise. `task_label` is the already-
/// formatted task name (fancy passes a bolded one; simple/interleaved pass
/// it plain) rather than a bare `&str`, so this stays agnostic to which
/// styling, if any, the caller wants around the name itself.
pub(crate) fn format_task_summary(
    console: &Console,
    task_label: &str,
    exit_code: i64,
    duration: Duration,
) -> String {
    let color = if exit_code == 0 {
        Color::Green
    } else {
        Color::Red
    };
    let exit_code = console.colored(color, &exit_code.to_string());
    format!(
        "{task_label} finished with exit code {exit_code} in {}.",
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
/// in.
pub(crate) fn exited_unexpectedly(exit_code: i64) -> String {
    format!("exited unexpectedly with exit code {exit_code}")
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
