//! What an image build's output stream becomes: the lines posted as
//! [`TaskEvent::ImageBuildProgress`](crate::ui::TaskEvent::ImageBuildProgress)
//! events, and the transcript a failed build's error carries (see
//! `build_output_suffix`). One type per builder, each fed what bollard's
//! stream hands `build_image` and synchronous, so the framing is
//! unit-testable without a daemon.
//!
//! The lines are framed by [`LineBuffer`], the one rule `-o all` has for
//! prefixed output (ratect#716) — Batect sends build output through the same
//! sink as a container's. So a line keeps its indentation, a blank line is
//! an empty one, and a lone `\r` ends a line as it does for container output
//! (`docs/differences-from-batect.md`). The docker layer, not the logger,
//! splits them (ratect#518): the fancy style shows the latest line as its
//! one-line status, which a multi-line message would break.
//!
//! The transcript is the stream's own text, as Batect's failure message
//! carries it.

use std::collections::HashMap;

use bollard::moby::buildkit::v1::{StatusResponse, Vertex};

use crate::ui::interleaved::LineBuffer;

/// Runs `chunk` through `buffer`, collecting the lines it completes.
fn push_lines(buffer: &mut LineBuffer, chunk: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    buffer.push(chunk, |line| lines.push(line.to_string()));
    lines
}

/// Ends `buffer`'s stream, collecting any unterminated last line.
fn flush_lines(buffer: &mut LineBuffer) -> Vec<String> {
    let mut lines = Vec::new();
    buffer.flush(|line| lines.push(line.to_string()));
    lines
}

/// The classic builder's output: `stream` text chunks, Docker's own step
/// lines and the steps' output alike.
pub(super) struct ClassicBuildOutput {
    transcript: String,
    lines: LineBuffer,
}

impl ClassicBuildOutput {
    pub(super) fn new() -> Self {
        Self {
            transcript: String::new(),
            lines: LineBuffer::new(),
        }
    }

    /// Takes one `stream` chunk, returning the lines it completes.
    pub(super) fn push(&mut self, chunk: &str) -> Vec<String> {
        self.transcript.push_str(chunk);
        push_lines(&mut self.lines, chunk.as_bytes())
    }

    /// Ends the stream, returning any line still unterminated.
    pub(super) fn finish(&mut self) -> Vec<String> {
        flush_lines(&mut self.lines)
    }

    pub(super) fn transcript(&self) -> &str {
        &self.transcript
    }
}

/// The BuildKit builder's output: structured status updates, rendered as
/// BuildKit's plain-progress format renders them (ratect#538) — every line
/// labelled `#N` with the step it belongs to, so the output of stages
/// running in parallel can be told apart:
///
/// - A step is numbered the first time it is displayed — when it starts, or
///   when its first log line arrives, whichever is first — counting from 1,
///   and keeps its number for the rest of the build.
/// - Being numbered displays its `#N <name>` header (none for a step with no
///   name), so no `#N` line precedes its step's header.
/// - A cached step adds `#N CACHED`; a failed one `#N ERROR: <error>`, once.
/// - Steps that start in the same update display in [`display_order`].
///
/// Plain-progress's elapsed times, `DONE` lines and status lines are left
/// out, so the output is the same from run to run
/// (`docs/differences-from-batect.md`).
///
/// The transcript is the same labelled lines, not the raw log chunks: in
/// it, too, a line needs its step's label to be read.
pub(super) struct BuildKitOutput {
    transcript: String,
    /// Keyed by digest, since BuildKit re-sends a vertex on every change.
    steps: HashMap<String, Step>,
    /// The number the last step to be numbered got.
    last_number: usize,
}

struct Step {
    name: String,
    number: Option<usize>,
    started: bool,
    shown_cached: bool,
    shown_error: bool,
    /// One per step, since BuildKit interleaves different steps' chunks: a
    /// partial line is only ever completed by its own step's next chunk.
    lines: LineBuffer,
}

impl BuildKitOutput {
    pub(super) fn new() -> Self {
        Self {
            transcript: String::new(),
            steps: HashMap::new(),
            last_number: 0,
        }
    }

    /// Takes one status update, returning the lines it completes.
    pub(super) fn push(&mut self, status: &StatusResponse) -> Vec<String> {
        let mut lines = Vec::new();
        for vertex in &status.vertexes {
            let step = self.entry(&vertex.digest);
            if !vertex.name.is_empty() {
                step.name.clone_from(&vertex.name);
            }
        }
        // Vertexes are re-sent on every state change — and the very first
        // status message announces the *whole* build graph upfront, before
        // anything runs, in graph (not execution) order. So a step is
        // displayed when it first reports *started* — execution order,
        // matching the docker CLI — not on first sight, which would dump the
        // entire plan at once (and reversed).
        let starting = status
            .vertexes
            .iter()
            .filter(|vertex| vertex.started.is_some() && !self.steps[&vertex.digest].started)
            .collect();
        for vertex in display_order(starting) {
            let step = self.entry(&vertex.digest);
            step.started = true;
            let show_cached = vertex.cached && !std::mem::replace(&mut step.shown_cached, true);
            self.show(&vertex.digest, &mut lines);
            if show_cached {
                self.emit(&vertex.digest, "CACHED", &mut lines);
            }
        }
        for log in &status.logs {
            self.show(&log.vertex, &mut lines);
            let completed = push_lines(&mut self.entry(&log.vertex).lines, &log.msg);
            for line in completed {
                self.emit(&log.vertex, &line, &mut lines);
            }
        }
        for vertex in &status.vertexes {
            // A finished step's unterminated last line can complete no
            // further, so it is shown now, before the step's error.
            if vertex.completed.is_some() || !vertex.error.is_empty() {
                let rest = flush_lines(&mut self.entry(&vertex.digest).lines);
                for line in rest {
                    self.emit(&vertex.digest, &line, &mut lines);
                }
            }
            if !vertex.error.is_empty()
                && !std::mem::replace(&mut self.entry(&vertex.digest).shown_error, true)
            {
                self.show(&vertex.digest, &mut lines);
                self.emit(
                    &vertex.digest,
                    &format!("ERROR: {}", vertex.error),
                    &mut lines,
                );
            }
        }
        lines
    }

    /// Ends the stream, returning any line still unterminated, in step
    /// order.
    pub(super) fn finish(&mut self) -> Vec<String> {
        let mut unfinished: Vec<(usize, String)> = self
            .steps
            .iter()
            .filter_map(|(digest, step)| Some((step.number?, digest.clone())))
            .collect();
        unfinished.sort();
        let mut lines = Vec::new();
        for (_, digest) in unfinished {
            let rest = flush_lines(&mut self.entry(&digest).lines);
            for line in rest {
                self.emit(&digest, &line, &mut lines);
            }
        }
        lines
    }

    pub(super) fn transcript(&self) -> &str {
        &self.transcript
    }

    fn entry(&mut self, digest: &str) -> &mut Step {
        self.steps
            .entry(digest.to_string())
            .or_insert_with(|| Step {
                name: String::new(),
                number: None,
                started: false,
                shown_cached: false,
                shown_error: false,
                lines: LineBuffer::new(),
            })
    }

    /// Numbers the step if it isn't yet, displaying its header.
    fn show(&mut self, digest: &str, lines: &mut Vec<String>) {
        if self.entry(digest).number.is_some() {
            return;
        }
        self.last_number += 1;
        let number = self.last_number;
        let step = self.entry(digest);
        step.number = Some(number);
        if !step.name.is_empty() {
            let name = step.name.clone();
            self.emit(digest, &name, lines);
        }
    }

    /// Adds `text` to the output as lines of the step's, labelled — one per
    /// line of it, since a step's name or error can span several.
    fn emit(&mut self, digest: &str, text: &str, lines: &mut Vec<String>) {
        let number = self.steps[digest]
            .number
            .expect("a step is shown before any of its lines");
        for part in text.split('\n') {
            let line = if part.is_empty() {
                format!("#{number}")
            } else {
                format!("#{number} {part}")
            };
            self.transcript.push_str(&line);
            self.transcript.push('\n');
            lines.push(line);
        }
    }
}

/// The order steps that start in the same update display in — docker-client's
/// rules (`images_build_buildkit.go`'s `sortVerticesForDisplay`):
///
/// 1. A step comes after any step in the batch it lists among its `inputs`.
/// 2. Steps named `[…]` come before steps that aren't.
/// 3. Of those, `[internal] …` steps come first, and `[internal] load build
///    definition …` first of them.
/// 4. Within one stage (`[builder 2/4]`, or a bare `[2/4]`), steps come in
///    step-number order.
/// 5. Otherwise, BuildKit's own order is kept.
///
/// docker-client applies them as a `sort.Stable` comparator, but they aren't
/// a total order — rule 1 orders only the pairs it joins, and rule 4 says
/// nothing across stages — and `sort_by` may panic on one that isn't. So
/// this picks one step at a time instead: the first, in BuildKit's order,
/// whose inputs have all been placed and which no step still to place
/// precedes by rules 2–4. When rules 1 and 2–4 conflict, the inputs win.
fn display_order(mut batch: Vec<&Vertex>) -> Vec<&Vertex> {
    let mut ordered = Vec::with_capacity(batch.len());
    while !batch.is_empty() {
        let waits_for_input = |vertex: &Vertex| {
            vertex
                .inputs
                .iter()
                .any(|input| batch.iter().any(|other| &other.digest == input))
        };
        let mut ready: Vec<usize> = (0..batch.len())
            .filter(|&index| !waits_for_input(batch[index]))
            .collect();
        if ready.is_empty() {
            // An input cycle, which BuildKit never sends.
            ready = (0..batch.len()).collect();
        }
        let preceded_by_none_of = |candidates: &[usize], index: usize| {
            candidates
                .iter()
                .all(|&other| !displays_before(batch[other], batch[index]))
        };
        let all: Vec<usize> = (0..batch.len()).collect();
        let next = ready
            .iter()
            .copied()
            .find(|&index| preceded_by_none_of(&all, index))
            .or_else(|| {
                ready
                    .iter()
                    .copied()
                    .find(|&index| preceded_by_none_of(&ready, index))
            })
            .unwrap_or(ready[0]);
        ordered.push(batch.remove(next));
    }
    ordered
}

/// Whether rules 2–4 of [`display_order`] put `first` before `second` — a
/// strict partial order, so some step in any batch has nothing before it.
fn displays_before(first: &Vertex, second: &Vertex) -> bool {
    let (first_rank, second_rank) = (rank(&first.name), rank(&second.name));
    if first_rank != second_rank {
        return first_rank < second_rank;
    }
    match (stage_step(&first.name), stage_step(&second.name)) {
        (Some((first_stage, first_step)), Some((second_stage, second_step))) => {
            first_stage == second_stage && first_step < second_step
        }
        _ => false,
    }
}

/// Rules 2 and 3 of [`display_order`]: lower displays first.
fn rank(name: &str) -> u8 {
    if name.starts_with("[internal] load build definition ") {
        0
    } else if name.starts_with("[internal] ") {
        1
    } else if name.starts_with('[') {
        2
    } else {
        3
    }
}

/// The stage and step number of a `[<stage> <n>/<m>] …` or `[<n>/<m>] …`
/// step name, or `None` for one that isn't — which docker-client would
/// panic on.
fn stage_step(name: &str) -> Option<(&str, u32)> {
    let identifier = &name.strip_prefix('[')?[..name.find("] ")? - 1];
    let (stage, counter) = match identifier.rsplit_once(' ') {
        Some((stage, counter)) => (stage.trim(), counter),
        None => ("", identifier),
    };
    let (step, _) = counter.split_once('/')?;
    Some((stage, step.parse().ok()?))
}

#[cfg(test)]
#[path = "build_output_tests.rs"]
mod tests;
