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

//! Line endings while an interactive session has the terminal in raw mode
//! (ratect#613).
//!
//! Raw mode (`cfmakeraw`, which `crossterm` uses on Unix) clears `OPOST`, so
//! a line feed no longer returns the cursor to the first column, and a line
//! printed with a bare `\n` leaves the next one starting where it ended.
//! So, while raw mode is on, every line Ratect writes to a terminal ends
//! `\r\n` instead — and only then: outside it the terminal adds the `\r`
//! itself, and a file or pipe should never get one.
//!
//! One process-wide flag records whether raw mode is on. The interactive
//! session's guard in `docker.rs` is the only thing that sets or clears it;
//! [`Console`](super::Console) and the `tracing` writer [`stderr`] read it
//! at each write, each also asking whether *its own* stream is a terminal,
//! since raw mode needs only stdin and stdout to be one — stderr may be
//! `2> log`. The translation runs on bytes about to be written, after
//! [`Text`](super::Text) escaping, so a `\r` from configuration is already
//! the two characters `\` `r` by then and stays that way; a `\r\n` Ratect
//! wrote itself is left alone rather than doubled.
//!
//! Turning `OPOST` back on after `cfmakeraw` would fix every writer at
//! once, but it would also translate the task container's own output,
//! which passes through the same terminal: a full-screen program in it
//! that moves the cursor down with a bare `\n` (ncurses does, for xterm's
//! `cud1`) would land in the first column. So the terminal stays as
//! `docker run -it` leaves it, and Ratect's own lines carry their `\r`.
//!
//! Windows gets the same translation without needing it: `crossterm`'s raw
//! mode there changes input modes only, and an extra `\r` before a line
//! feed is harmless.

use std::borrow::Cow;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether an interactive session currently has the terminal in raw mode.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// Records whether raw mode is on — for the interactive session's guard
/// only.
pub(crate) fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::SeqCst);
}

/// `bytes` as they should reach a writer that is a terminal iff
/// `is_terminal`: every `\n` not already preceded by `\r` gets one, while
/// raw mode is on and the writer is a terminal; unchanged otherwise.
pub(crate) fn line_endings(bytes: &[u8], is_terminal: bool) -> Cow<'_, [u8]> {
    if !(is_terminal && is_active()) || !bytes.contains(&b'\n') {
        return Cow::Borrowed(bytes);
    }
    let mut translated = Vec::with_capacity(bytes.len() + bytes.len() / 16 + 1);
    let mut previous = None;
    for &byte in bytes {
        if byte == b'\n' && previous != Some(b'\r') {
            translated.push(b'\r');
        }
        translated.push(byte);
        previous = Some(byte);
    }
    Cow::Owned(translated)
}

/// A writer that ends lines `\r\n` by the same rule as a
/// [`Console`](super::Console): while raw mode is on, and only if it writes
/// to a terminal — what the `tracing` fmt layer writes through, one per
/// event. Each `write` is translated on its own, which is enough for that
/// layer: it formats an event whole and writes it once.
pub struct LineEndings<W> {
    inner: W,
    is_terminal: bool,
}

impl<W: Write> LineEndings<W> {
    /// `inner`, which is a terminal iff `is_terminal`.
    pub fn new(inner: W, is_terminal: bool) -> Self {
        Self { inner, is_terminal }
    }
}

impl<W: Write> Write for LineEndings<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match line_endings(buf, self.is_terminal) {
            Cow::Borrowed(buf) => self.inner.write(buf),
            Cow::Owned(translated) => {
                self.inner.write_all(&translated)?;
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The real stderr, as a [`LineEndings`] — both binaries' `tracing` stderr
/// layer's writer (`.with_writer(ratect_core::ui::raw_mode::stderr)`), so a
/// log line printed during an interactive session starts at the first
/// column too. Not for `ratect-compat`'s `--log-file` layer: a file is never
/// a terminal.
pub fn stderr() -> LineEndings<std::io::Stderr> {
    let stderr = std::io::stderr();
    // Called once per log event, so the terminal check is skipped while it
    // can't matter.
    let is_terminal = is_active() && stderr.is_terminal();
    LineEndings::new(stderr, is_terminal)
}

/// Serializes every test that sets the flag, or reads it through a writer
/// that is a terminal — it is process-wide, and `cargo test` runs tests on
/// several threads. Survives a test that panicked holding it.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[path = "raw_mode_tests.rs"]
mod tests;
