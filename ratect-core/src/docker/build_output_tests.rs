use super::*;
use bollard::moby::buildkit::v1::{Vertex, VertexLog};

fn classic(chunks: &[&str]) -> (Vec<String>, ClassicBuildOutput) {
    let mut output = ClassicBuildOutput::new();
    let mut lines = Vec::new();
    for chunk in chunks {
        lines.extend(output.push(chunk));
    }
    lines.extend(output.finish());
    (lines, output)
}

#[test]
fn classic_chunk_with_several_lines_gives_one_line_each() {
    let (lines, _) = classic(&["Step 1/2 : FROM alpine\n ---> abc\n"]);
    assert_eq!(lines, ["Step 1/2 : FROM alpine", " ---> abc"]);
}

#[test]
fn classic_line_split_across_chunks_is_reassembled() {
    let (lines, _) = classic(&["hel", "lo\n"]);
    assert_eq!(lines, ["hello"]);
}

#[test]
fn classic_keeps_indentation_and_blank_lines() {
    let (lines, _) = classic(&["first\n", "  --> src/main.rs:3:5\n", "\n", "after-blank\n"]);
    assert_eq!(lines, ["first", "  --> src/main.rs:3:5", "", "after-blank"]);
}

#[test]
fn classic_crlf_is_one_terminator() {
    let (lines, _) = classic(&["a\r", "\nb\r\n"]);
    assert_eq!(lines, ["a", "b"]);
}

/// The same rule as a container's output (`LineBuffer`, ratect#716): a
/// progress redraw is two lines, never one the terminal overwrites.
#[test]
fn classic_lone_carriage_return_ends_a_line() {
    let (lines, _) = classic(&["a\rb\n"]);
    assert_eq!(lines, ["a", "b"]);
}

#[test]
fn classic_final_unterminated_line_is_returned_on_finish() {
    let mut output = ClassicBuildOutput::new();
    assert_eq!(output.push("done\nlast"), ["done"]);
    assert_eq!(output.finish(), ["last"]);
}

#[test]
fn classic_transcript_is_the_stream_verbatim() {
    let (_, output) = classic(&["first\n", "  --> src/main.rs:3:5\n", "\n", "after-blank\n"]);
    assert_eq!(
        output.transcript(),
        "first\n  --> src/main.rs:3:5\n\nafter-blank\n"
    );
}

fn vertex(digest: &str, name: &str) -> Vertex {
    Vertex {
        digest: digest.into(),
        name: name.into(),
        ..Default::default()
    }
}

fn started(digest: &str, name: &str) -> Vertex {
    Vertex {
        started: Some(Default::default()),
        ..vertex(digest, name)
    }
}

fn log(digest: &str, msg: &str) -> VertexLog {
    VertexLog {
        vertex: digest.into(),
        msg: msg.as_bytes().to_vec(),
        ..Default::default()
    }
}

fn logs(logs: Vec<VertexLog>) -> StatusResponse {
    StatusResponse {
        logs,
        ..Default::default()
    }
}

#[test]
fn buildkit_log_lines_keep_indentation_and_blank_lines() {
    let mut output = BuildKitOutput::new();
    let mut lines = output.push(&logs(vec![
        log("a", "first\n  --> src/main.rs:3:5\n"),
        log("a", "\n"),
        log("a", "after-"),
    ]));
    lines.extend(output.push(&logs(vec![log("a", "blank\n")])));
    assert_eq!(lines, ["first", "  --> src/main.rs:3:5", "", "after-blank"]);
}

#[test]
fn buildkit_interleaved_partial_lines_are_not_joined_across_vertexes() {
    let mut output = BuildKitOutput::new();
    let mut lines = output.push(&logs(vec![log("a", "from-"), log("b", "other-")]));
    lines.extend(output.push(&logs(vec![log("b", "b\n"), log("a", "a\n")])));
    assert_eq!(lines, ["other-b", "from-a"]);
}

#[test]
fn buildkit_final_unterminated_lines_are_returned_on_finish() {
    let mut output = BuildKitOutput::new();
    assert!(output
        .push(&logs(vec![log("a", "last-a"), log("b", "last-b")]))
        .is_empty());
    assert_eq!(output.finish(), ["last-a", "last-b"]);
}

#[test]
fn buildkit_step_names_are_lines_of_their_own() {
    let mut output = BuildKitOutput::new();
    let lines = output.push(&StatusResponse {
        vertexes: vec![
            started("a", "[1/2] FROM alpine"),
            vertex("b", "[2/2] RUN x"),
        ],
        ..Default::default()
    });
    assert_eq!(lines, ["[1/2] FROM alpine"]);
}
