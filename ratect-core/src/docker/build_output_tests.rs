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

fn update(vertexes: Vec<Vertex>) -> StatusResponse {
    StatusResponse {
        vertexes,
        ..Default::default()
    }
}

fn with_inputs(vertex: Vertex, inputs: &[&str]) -> Vertex {
    Vertex {
        inputs: inputs.iter().map(|input| input.to_string()).collect(),
        ..vertex
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
    assert_eq!(
        lines,
        [
            "#1 first",
            "#1   --> src/main.rs:3:5",
            "#1",
            "#1 after-blank"
        ]
    );
}

#[test]
fn buildkit_interleaved_partial_lines_are_not_joined_across_vertexes() {
    let mut output = BuildKitOutput::new();
    let mut lines = output.push(&logs(vec![log("a", "from-"), log("b", "other-")]));
    lines.extend(output.push(&logs(vec![log("b", "b\n"), log("a", "a\n")])));
    assert_eq!(lines, ["#2 other-b", "#1 from-a"]);
}

#[test]
fn buildkit_final_unterminated_lines_are_returned_on_finish() {
    let mut output = BuildKitOutput::new();
    assert!(output
        .push(&logs(vec![log("a", "last-a"), log("b", "last-b")]))
        .is_empty());
    assert_eq!(output.finish(), ["#1 last-a", "#2 last-b"]);
}

#[test]
fn buildkit_a_step_is_headed_when_it_starts_and_not_before() {
    let mut output = BuildKitOutput::new();
    let lines = output.push(&update(vec![
        started("a", "[1/2] FROM alpine"),
        vertex("b", "[2/2] RUN x"),
    ]));
    assert_eq!(lines, ["#1 [1/2] FROM alpine"]);
}

#[test]
fn buildkit_steps_of_one_stage_display_in_step_order() {
    let mut output = BuildKitOutput::new();
    let lines = output.push(&update(vec![
        started("b", "[builder 2/2] RUN b"),
        started("a", "[builder 1/2] RUN a"),
    ]));
    assert_eq!(lines, ["#1 [builder 1/2] RUN a", "#2 [builder 2/2] RUN b"]);
}

#[test]
fn buildkit_a_step_displays_after_its_own_inputs() {
    let mut output = BuildKitOutput::new();
    let lines = output.push(&update(vec![
        with_inputs(started("b", "[two 1/1] COPY"), &["a"]),
        started("a", "[one 1/1] RUN a"),
    ]));
    assert_eq!(lines, ["#1 [one 1/1] RUN a", "#2 [two 1/1] COPY"]);
}

#[test]
fn buildkit_internal_steps_display_first_and_the_build_definition_first_of_them() {
    let mut output = BuildKitOutput::new();
    let lines = output.push(&update(vec![
        started("s", "[1/2] FROM alpine"),
        started("x", "exporting to image"),
        started("m", "[internal] load metadata for alpine"),
        started("d", "[internal] load build definition from Dockerfile"),
    ]));
    assert_eq!(
        lines,
        [
            "#1 [internal] load build definition from Dockerfile",
            "#2 [internal] load metadata for alpine",
            "#3 [1/2] FROM alpine",
            "#4 exporting to image",
        ]
    );
}

/// docker-client's comparator isn't a total order (stages don't compare,
/// and inputs only order the pairs they join), which `sort_by` may panic
/// on; nor does a name without a step number parse.
#[test]
fn buildkit_ordering_mixed_stages_dependencies_and_odd_names_never_panics() {
    let batch = vec![
        started("o2", "[one 2/3] RUN b"),
        with_inputs(started("t1", "[two 1/2] RUN c"), &["o3"]),
        started("w", "[weird]"),
        started("o1", "[one 1/3] RUN a"),
        started("x", "exporting to image"),
        started("o3", "[one 3/3] RUN d"),
        started("n", "[x/y] RUN e"),
        started("t2", "[two 2/2] RUN f"),
    ];
    let position = |lines: &[String], name: &str| {
        lines
            .iter()
            .position(|line| line.ends_with(name))
            .unwrap_or_else(|| panic!("{name} should be displayed: {lines:?}"))
    };
    // Each rotation and reversal of the batch, as a cheap stand-in for
    // shuffling it.
    for reverse in [false, true] {
        for rotation in 0..batch.len() {
            let mut shuffled = batch.clone();
            if reverse {
                shuffled.reverse();
            }
            shuffled.rotate_left(rotation);
            let lines = BuildKitOutput::new().push(&update(shuffled));
            assert_eq!(lines.len(), batch.len(), "{lines:?}");
            assert!(position(&lines, "RUN a") < position(&lines, "RUN b"));
            assert!(position(&lines, "RUN b") < position(&lines, "RUN d"));
            assert!(position(&lines, "RUN d") < position(&lines, "RUN c"));
            assert!(position(&lines, "RUN c") < position(&lines, "RUN f"));
            assert_eq!(
                position(&lines, "exporting to image"),
                batch.len() - 1,
                "{lines:?}"
            );
        }
    }
}

#[test]
fn buildkit_interleaved_steps_each_label_their_own_lines() {
    let mut output = BuildKitOutput::new();
    output.push(&update(vec![
        started("a", "[one 1/1] RUN a"),
        started("b", "[two 1/1] RUN b"),
    ]));
    let lines = output.push(&logs(vec![
        log("b", "from b\n"),
        log("a", "from a\n"),
        log("b", "b again\n"),
    ]));
    assert_eq!(lines, ["#2 from b", "#1 from a", "#2 b again"]);
}

#[test]
fn buildkit_a_cached_step_says_so_on_a_line_of_its_own() {
    let mut output = BuildKitOutput::new();
    let lines = output.push(&update(vec![Vertex {
        cached: true,
        ..started("a", "[1/2] FROM alpine")
    }]));
    assert_eq!(lines, ["#1 [1/2] FROM alpine", "#1 CACHED"]);
}

#[test]
fn buildkit_a_step_error_is_reported_once() {
    let failed = Vertex {
        error: "exit code: 1".into(),
        ..started("a", "[1/1] RUN false")
    };
    let mut output = BuildKitOutput::new();
    let mut lines = output.push(&update(vec![failed.clone()]));
    lines.extend(output.push(&update(vec![failed])));
    assert_eq!(lines, ["#1 [1/1] RUN false", "#1 ERROR: exit code: 1"]);
}

#[test]
fn buildkit_a_log_line_before_its_step_starts_follows_the_steps_header() {
    let mut output = BuildKitOutput::new();
    output.push(&update(vec![vertex("a", "[1/1] RUN x")]));
    let mut lines = output.push(&logs(vec![log("a", "early\n")]));
    lines.extend(output.push(&update(vec![started("a", "[1/1] RUN x")])));
    assert_eq!(lines, ["#1 [1/1] RUN x", "#1 early"]);
}

#[test]
fn buildkit_a_step_keeps_its_number_across_updates() {
    let mut output = BuildKitOutput::new();
    output.push(&update(vec![started("a", "[1/2] RUN a")]));
    output.push(&update(vec![started("b", "[2/2] RUN b")]));
    let lines = output.push(&StatusResponse {
        vertexes: vec![started("b", "[2/2] RUN b"), started("a", "[1/2] RUN a")],
        logs: vec![log("a", "a\n"), log("b", "b\n")],
        ..Default::default()
    });
    assert_eq!(lines, ["#1 a", "#2 b"]);
}

/// The failure transcript is the lines the events carry, labels and all.
#[test]
fn buildkit_transcript_is_the_labelled_lines() {
    let mut output = BuildKitOutput::new();
    output.push(&StatusResponse {
        vertexes: vec![Vertex {
            error: "exit code: 1".into(),
            ..started("a", "[1/1] RUN x")
        }],
        logs: vec![log("a", "out\n  indented\n")],
        ..Default::default()
    });
    output.finish();
    assert_eq!(
        output.transcript(),
        "#1 [1/1] RUN x\n#1 out\n#1   indented\n#1 ERROR: exit code: 1\n"
    );
}

/// Each event is one line, so a step name or error spanning several (a
/// heredoc `RUN`'s, say) is a labelled line per line.
#[test]
fn buildkit_a_multi_line_name_or_error_is_a_labelled_line_per_line() {
    let mut output = BuildKitOutput::new();
    let lines = output.push(&update(vec![Vertex {
        error: "failed:\nexit code: 1".into(),
        ..started("a", "[1/1] RUN <<EOF\necho hi\nEOF")
    }]));
    assert_eq!(
        lines,
        [
            "#1 [1/1] RUN <<EOF",
            "#1 echo hi",
            "#1 EOF",
            "#1 ERROR: failed:",
            "#1 exit code: 1",
        ]
    );
}

/// A completed step's unterminated last line is shown when it completes,
/// and a log BuildKit sends for it later is still that step's.
#[test]
fn buildkit_a_completed_steps_tail_shows_at_once_and_a_later_log_stays_labelled() {
    let completed = Vertex {
        completed: Some(Default::default()),
        ..started("a", "[1/2] RUN a")
    };
    let mut output = BuildKitOutput::new();
    output.push(&update(vec![started("a", "[1/2] RUN a")]));
    let mut lines = output.push(&logs(vec![log("a", "tail")]));
    lines.extend(output.push(&update(vec![completed])));
    lines.extend(output.push(&StatusResponse {
        vertexes: vec![started("b", "[2/2] RUN b")],
        logs: vec![log("a", "late\n")],
        ..Default::default()
    }));
    lines.extend(output.finish());
    assert_eq!(lines, ["#1 tail", "#2 [2/2] RUN b", "#1 late"]);
}
