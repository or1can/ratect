use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, DuplexStream};

/// What one read of a [`Scripted`] source returns.
enum Step {
    Data(&'static [u8]),
    End,
    Fail,
    WouldBlock,
    Interrupted,
}

/// A source the test feeds by hand, one [`Step`] per read. Counts the reads
/// it has been asked for.
struct Scripted {
    reads: mpsc::UnboundedReceiver<Step>,
    started: Arc<AtomicUsize>,
}

impl Read for Scripted {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.started.fetch_add(1, Ordering::SeqCst);
        match self.reads.blocking_recv().unwrap_or(Step::End) {
            Step::Data(data) => {
                buffer[..data.len()].copy_from_slice(data);
                Ok(data.len())
            }
            Step::End => Ok(0),
            Step::Fail => Err(std::io::Error::other("unreadable")),
            Step::WouldBlock => Err(std::io::ErrorKind::WouldBlock.into()),
            Step::Interrupted => Err(std::io::ErrorKind::Interrupted.into()),
        }
    }
}

fn scripted(
    terminal: bool,
) -> (
    &'static StdinReader,
    mpsc::UnboundedSender<Step>,
    Arc<AtomicUsize>,
) {
    let (feed, reads) = mpsc::unbounded_channel();
    let started = Arc::new(AtomicUsize::new(0));
    let source = Scripted {
        reads,
        started: started.clone(),
    };
    let reader = Box::leak(Box::new(StdinReader::spawn(source, terminal)));
    (reader, feed, started)
}

/// Attaches a container: its pump, and the container's end of its stdin.
fn attach(reader: &'static StdinReader) -> (tokio::task::JoinHandle<()>, DuplexStream) {
    let (attach_input, container_stdin) = tokio::io::duplex(1024);
    let pump = tokio::spawn(reader.forward(Box::pin(attach_input)));
    (pump, container_stdin)
}

async fn read_some(container_stdin: &mut DuplexStream) -> Vec<u8> {
    let mut buffer = [0; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), container_stdin.read(&mut buffer))
        .await
        .expect("nothing reached the container")
        .unwrap();
    buffer[..read].to_vec()
}

async fn detach(pump: tokio::task::JoinHandle<()>) {
    pump.abort();
    let _ = pump.await;
}

#[tokio::test]
async fn input_read_after_a_container_detaches_reaches_the_next_one() {
    let (reader, feed, _) = scripted(true);

    let (pump, mut first) = attach(reader);
    feed.send(Step::Data(b"first")).unwrap();
    assert_eq!(read_some(&mut first).await, b"first");
    // The reader is now blocked in a read nobody is waiting for — the
    // one an aborted pump used to leave behind.
    detach(pump).await;

    feed.send(Step::Data(b"second")).unwrap();
    let (_pump, mut second) = attach(reader);
    assert_eq!(read_some(&mut second).await, b"second");
}

#[tokio::test]
async fn a_terminals_end_of_input_ends_only_the_attached_containers_stdin() {
    let (reader, feed, _) = scripted(true);

    let (pump, mut first) = attach(reader);
    feed.send(Step::End).unwrap();
    assert_eq!(
        read_some(&mut first).await,
        b"",
        "its stdin should be closed"
    );
    pump.await.unwrap();

    feed.send(Step::Data(b"more")).unwrap();
    let (_pump, mut second) = attach(reader);
    assert_eq!(read_some(&mut second).await, b"more");
}

#[tokio::test]
async fn a_pipes_end_of_input_ends_every_later_containers_stdin_at_once() {
    let (reader, feed, _) = scripted(false);

    let (pump, mut first) = attach(reader);
    feed.send(Step::Data(b"data")).unwrap();
    feed.send(Step::End).unwrap();
    assert_eq!(read_some(&mut first).await, b"data");
    assert_eq!(read_some(&mut first).await, b"");
    pump.await.unwrap();

    let (pump, mut second) = attach(reader);
    assert_eq!(read_some(&mut second).await, b"");
    pump.await.unwrap();
}

#[tokio::test]
async fn no_more_than_one_chunk_is_read_ahead_of_the_containers() {
    let (_reader, feed, started) = scripted(false);
    for chunk in [b"a", b"b", b"c"] {
        feed.send(Step::Data(chunk)).unwrap();
    }

    // The first read fills the channel; the second may not start until a
    // container takes that chunk. Nothing attaches, so it never does —
    // given time to, which a broken bound would take straight away.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while started.load(Ordering::SeqCst) < 2 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_read_that_would_block_or_was_interrupted_is_retried() {
    for step in [Step::WouldBlock, Step::Interrupted] {
        let (reader, feed, _) = scripted(false);

        let (_pump, mut container) = attach(reader);
        feed.send(step).unwrap();
        feed.send(Step::Data(b"after")).unwrap();
        assert_eq!(read_some(&mut container).await, b"after");
    }
}

#[tokio::test]
async fn a_container_whose_stdin_cant_be_written_lets_the_next_one_read() {
    let (reader, feed, _) = scripted(true);

    let (pump, first) = attach(reader);
    drop(first);
    feed.send(Step::Data(b"lost")).unwrap();
    tokio::time::timeout(Duration::from_secs(5), pump)
        .await
        .expect("the pump should end once its write fails")
        .unwrap();

    feed.send(Step::Data(b"next")).unwrap();
    let (_pump, mut second) = attach(reader);
    assert_eq!(read_some(&mut second).await, b"next");
}

#[tokio::test]
async fn a_read_error_ends_every_containers_stdin_even_from_a_terminal() {
    let (reader, feed, _) = scripted(true);

    let (pump, mut first) = attach(reader);
    feed.send(Step::Fail).unwrap();
    assert_eq!(read_some(&mut first).await, b"");
    pump.await.unwrap();

    // The thread has stopped reading, so this may not even be accepted.
    let _ = feed.send(Step::Data(b"too late"));
    let (pump, mut second) = attach(reader);
    assert_eq!(read_some(&mut second).await, b"");
    pump.await.unwrap();
}

/// A session dropped mid-way — a cancelled run drops the task container's —
/// drops the guard [`crate::docker::spawn_stdin_pump`] returned, which must
/// let go of the reader, or every later container's pump waits on it
/// forever.
#[tokio::test]
async fn dropping_a_sessions_pump_lets_the_next_container_read() {
    let (reader, feed, _) = scripted(true);

    let (attach_input, _first) = tokio::io::duplex(1024);
    let pump = crate::docker::spawn_stdin_pump(reader, Box::pin(attach_input));
    // Only a pump that holds the reader proves anything by being dropped.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while reader.inputs.try_lock().is_ok() && tokio::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    assert!(
        reader.inputs.try_lock().is_err(),
        "the pump never took the reader"
    );
    drop(pump);

    feed.send(Step::Data(b"next")).unwrap();
    let (_pump, mut second) = attach(reader);
    assert_eq!(read_some(&mut second).await, b"next");
}
