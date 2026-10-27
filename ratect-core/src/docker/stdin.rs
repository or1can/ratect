//! The process's one reader of its own stdin, lent to each task container
//! attached to it in turn.
//!
//! A run can attach several task containers to stdin one after another — a
//! prerequisite's, then the task after it (ratect#486). A blocking read on
//! stdin can't be cancelled: when a container exits, its pump is aborted, but
//! a read already in progress runs on regardless. So the pump of one
//! container must never read stdin itself, or the read it leaves behind takes
//! the next container's first input and drops it. [`StdinReader`] does the
//! reading on a thread of its own and hands each chunk over a channel to
//! whichever pump holds it at the time; a read that completes once that
//! container's session has ended waits in the channel for the next one.
//!
//! What is read ahead is bounded: the thread reads only once the channel is
//! empty, so at most one chunk is ever held that no container has taken — as
//! many as a single abandoned read would have taken before.
//!
//! End of input is the source's own: a pipe's is final, so every pump after
//! it sees end of input at once; a terminal's is temporary, ending the stdin
//! of the container attached at the time (or, read between containers, of
//! the next), after which the terminal is read again. A terminal only
//! reports one outside raw mode — Ctrl+D at the start of a line, when no TTY
//! was allocated or between sessions — since in a TTY session Ctrl+D is a
//! byte for the container's own terminal like any other.

use std::io::Read;
use std::pin::Pin;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex};

/// One read from the source: what it returned, or that it reached end of
/// input.
enum Input {
    Data(Vec<u8>),
    End,
}

pub(super) struct StdinReader {
    inputs: Mutex<mpsc::Receiver<Input>>,
}

impl StdinReader {
    /// The reader of this process's own stdin — started the first time a
    /// container is attached, so a run with no interactive-eligible
    /// container never reads stdin at all.
    pub(super) fn process() -> &'static StdinReader {
        static PROCESS: std::sync::OnceLock<StdinReader> = std::sync::OnceLock::new();
        PROCESS.get_or_init(|| {
            use std::io::IsTerminal;
            let stdin = std::io::stdin();
            let terminal = stdin.is_terminal();
            StdinReader::spawn(stdin, terminal)
        })
    }

    /// Starts the thread reading `source`. `terminal` says whether its end
    /// of input is temporary (a terminal's Ctrl+D) rather than final.
    pub(super) fn spawn(mut source: impl Read + Send + 'static, terminal: bool) -> StdinReader {
        let (sender, receiver) = mpsc::channel(1);
        std::thread::spawn(move || {
            let mut buffer = vec![0; 8192];
            // Waiting for room before reading, not after, is what bounds
            // the read-ahead to the one chunk the channel holds.
            while let Ok(permit) = futures::executor::block_on(sender.reserve()) {
                match source.read(&mut buffer) {
                    Ok(0) => {
                        permit.send(Input::End);
                        if !terminal {
                            break;
                        }
                    }
                    Ok(read) => permit.send(Input::Data(buffer[..read].to_vec())),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    // A descriptor another process shares can be left
                    // non-blocking; that's no reason to end stdin for good.
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => {
                        tracing::debug!(%error, "stdin unreadable; ending it for this run");
                        permit.send(Input::End);
                        break;
                    }
                }
            }
        });
        StdinReader {
            inputs: Mutex::new(receiver),
        }
    }

    /// Copies this reader's input into a container's attach input until end
    /// of input, then shuts that input down — the half-close (Batect's own
    /// `CloseWrite()`) that tells Docker the input has ended, so a
    /// `stdin_once` container gets its own stdin closed in turn (ratect#220).
    /// Explicit rather than left to dropping `attach_input`: that is a write
    /// half split from the same connection as the output stream, and
    /// dropping it closes nothing while the output half is still being read.
    ///
    /// Holds the reader for as long as it runs, so the caller aborts it once
    /// its container's session ends. A chunk taken before then goes with
    /// this container — including one typed in the moment between the
    /// container exiting and its session ending. Write errors end the copy —
    /// the pump is best-effort, and the container's exit is what ends the
    /// session either way.
    pub(super) async fn forward(&self, mut attach_input: Pin<Box<dyn AsyncWrite + Send>>) {
        let mut inputs = self.inputs.lock().await;
        while let Some(Input::Data(chunk)) = inputs.recv().await {
            if attach_input.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = attach_input.shutdown().await;
    }
}

#[cfg(test)]
#[path = "stdin_tests.rs"]
mod tests;
