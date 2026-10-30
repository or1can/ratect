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

use std::collections::HashSet;

use bollard::moby::buildkit::v1::StatusResponse;
use indexmap::IndexMap;

use crate::ui::interleaved::LineBuffer;

/// Runs `chunk` through `buffer`, collecting the lines it completes.
fn push_lines(buffer: &mut LineBuffer, chunk: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    buffer.push(chunk, |line| lines.push(line.to_string()));
    lines
}

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

/// The BuildKit builder's output: structured status updates.
pub(super) struct BuildKitOutput {
    transcript: String,
    /// One per vertex, in first-seen order, since BuildKit interleaves
    /// different steps' chunks: a partial line is only ever completed by
    /// its own step's next chunk.
    logs: IndexMap<String, LineBuffer>,
    seen_vertexes: HashSet<String>,
    seen_vertex_errors: HashSet<String>,
}

impl BuildKitOutput {
    pub(super) fn new() -> Self {
        Self {
            transcript: String::new(),
            logs: IndexMap::new(),
            seen_vertexes: HashSet::new(),
            seen_vertex_errors: HashSet::new(),
        }
    }

    /// Takes one status update, returning the lines it completes.
    pub(super) fn push(&mut self, status: &StatusResponse) -> Vec<String> {
        let mut lines = Vec::new();
        for vertex in &status.vertexes {
            // Vertexes are re-sent on every state change — and the very
            // first status message announces the *whole* build graph
            // upfront, before anything runs, in graph (not execution)
            // order. So a step's name is recorded when it first reports
            // *started* — execution order, matching the docker CLI — not on
            // first sight, which would dump the entire plan at once (and
            // reversed). Errors are recorded once, when they appear.
            if vertex.started.is_some()
                && self.seen_vertexes.insert(vertex.digest.clone())
                && !vertex.name.is_empty()
            {
                let cached_suffix = if vertex.cached { " CACHED" } else { "" };
                let line = format!("{}{}", vertex.name, cached_suffix);
                self.transcript.push_str(&line);
                self.transcript.push('\n');
                lines.push(line);
            }
            if !vertex.error.is_empty() && self.seen_vertex_errors.insert(vertex.digest.clone()) {
                self.transcript.push_str(&vertex.error);
                self.transcript.push('\n');
            }
        }
        for log in &status.logs {
            self.transcript.push_str(&String::from_utf8_lossy(&log.msg));
            let buffer = self
                .logs
                .entry(log.vertex.clone())
                .or_insert_with(LineBuffer::new);
            lines.extend(push_lines(buffer, &log.msg));
        }
        lines
    }

    /// Ends the stream, returning any line still unterminated.
    pub(super) fn finish(&mut self) -> Vec<String> {
        self.logs.values_mut().flat_map(flush_lines).collect()
    }

    pub(super) fn transcript(&self) -> &str {
        &self.transcript
    }
}

#[cfg(test)]
#[path = "build_output_tests.rs"]
mod tests;
