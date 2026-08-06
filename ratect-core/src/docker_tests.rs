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

use super::*;

/// A fresh, unique scratch directory — same pattern as
/// `config.rs`'s `unique_temp_dir`. Caller cleans up.
fn unique_temp_dir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let dir = std::env::temp_dir().join(format!(
        "ratect-docker-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        count
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Records every posted `ContainerOutput` event's line, in order — the
/// only variant `drain_interleaved_log_stream`'s tests need.
#[derive(Default)]
struct RecordingEventSink {
    lines: std::sync::Mutex<Vec<String>>,
}

impl EventSink for RecordingEventSink {
    fn post(&self, event: TaskEvent) {
        if let TaskEvent::ContainerOutput { line, .. } = event {
            self.lines.lock().unwrap().push(line);
        }
    }
}

#[tokio::test]
async fn drain_interleaved_log_stream_flushes_a_buffered_partial_line_before_a_stream_error() {
    // The bug this proves fixed: an unterminated final line (no
    // trailing newline) followed by the log stream itself erroring
    // (e.g. the daemon restarting mid-stream) used to be silently
    // dropped — the early `return Err(...)` on the error skipped the
    // trailing flush that would have emitted it.
    let chunks = vec![
        Ok(bollard::container::LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"first line\n"),
        }),
        Ok(bollard::container::LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"unterminated final line"),
        }),
        Err(bollard::errors::Error::NoHomePathError),
    ];
    let stream = futures::stream::iter(chunks);
    // Kept as the concrete type so its recorded lines are inspectable
    // after the call, alongside the `Arc<dyn EventSink>` the function
    // itself needs — both point at the same underlying instance.
    let sink = std::sync::Arc::new(RecordingEventSink::default());
    let dyn_sink: std::sync::Arc<dyn EventSink> = sink.clone();

    let result = drain_interleaved_log_stream(stream, &dyn_sink, "app").await;

    assert!(result.is_err(), "the stream error should still propagate");
    assert_eq!(
        *sink.lines.lock().unwrap(),
        vec!["first line", "unterminated final line"],
        "the unterminated final line must still be flushed despite the stream error"
    );
}

#[tokio::test]
async fn await_log_follower_waits_for_the_spawned_task_to_finish() {
    // `DockerClient::new` only builds a lazily-connecting client (no
    // handshake), so this doesn't need a live daemon.
    let client = DockerClient::new(&Default::default())
        .expect("DockerClient::new should not require a live daemon");
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished_in_task = std::sync::Arc::clone(&finished);
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        finished_in_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    client
        .log_followers
        .lock()
        .unwrap()
        .insert("container-1".to_string(), handle);

    client.await_log_follower("container-1").await;

    assert!(
        finished.load(std::sync::atomic::Ordering::SeqCst),
        "await_log_follower should not return before the spawned task finishes — this is \
             the exact race the fix closes: ContainerRemoved posting before a follower's final \
             flush"
    );
    assert!(
        client.log_followers.lock().unwrap().is_empty(),
        "the entry should be removed once awaited, so a later call for the same id is a no-op"
    );
}

#[tokio::test]
async fn await_log_follower_is_a_no_op_for_a_container_with_no_follower() {
    let client = DockerClient::new(&Default::default())
        .expect("DockerClient::new should not require a live daemon");
    // Every non-interleaved run (and the task's own container, always)
    // never inserts an entry at all — must return immediately, not hang.
    client.await_log_follower("no-such-container").await;
}

#[test]
fn build_health_config_is_none_without_an_override() {
    assert_eq!(build_health_config(None), None);
}

#[test]
fn build_health_config_maps_all_fields() {
    let options = HealthCheckOptions {
        command: Some("pg_isready".to_string()),
        interval: Some(Duration::from_secs(2)),
        retries: Some(5),
        start_period: Some(Duration::from_secs(90)),
        timeout: Some(Duration::from_millis(500)),
    };

    assert_eq!(
        build_health_config(Some(&options)),
        Some(HealthConfig {
            test: Some(vec!["CMD-SHELL".to_string(), "pg_isready".to_string()]),
            interval: Some(2_000_000_000),
            timeout: Some(500_000_000),
            retries: Some(5),
            start_period: Some(90_000_000_000),
            start_interval: None,
        })
    );
}

#[test]
fn build_health_config_leaves_omitted_fields_unset_to_inherit_from_the_image() {
    let options = HealthCheckOptions {
        command: None,
        interval: Some(Duration::from_secs(1)),
        retries: None,
        start_period: None,
        timeout: None,
    };

    assert_eq!(
        build_health_config(Some(&options)),
        Some(HealthConfig {
            test: None,
            interval: Some(1_000_000_000),
            timeout: None,
            retries: None,
            start_period: None,
            start_interval: None,
        })
    );
}

#[test]
fn build_extra_hosts_formats_and_sorts_name_ip_pairs() {
    let mut hosts = HashMap::new();
    hosts.insert("zeta-service".to_string(), "10.0.0.2".to_string());
    hosts.insert("alpha-service".to_string(), "10.0.0.1".to_string());

    assert_eq!(
        build_extra_hosts(Some(&hosts)),
        Some(vec![
            "alpha-service:10.0.0.1".to_string(),
            "zeta-service:10.0.0.2".to_string(),
        ])
    );
}

#[test]
fn build_extra_hosts_is_none_when_additional_hosts_is_absent() {
    assert_eq!(build_extra_hosts(None), None);
}

#[test]
fn build_devices_maps_local_container_and_options() {
    let devices = vec![
        (
            "/dev/sda".to_string(),
            "/dev/xvda".to_string(),
            Some("rwm".to_string()),
        ),
        ("/dev/sdb".to_string(), "/dev/xvdb".to_string(), None),
    ];

    assert_eq!(
        build_devices(Some(&devices)),
        Some(vec![
            DeviceMapping {
                path_on_host: Some("/dev/sda".to_string()),
                path_in_container: Some("/dev/xvda".to_string()),
                cgroup_permissions: Some("rwm".to_string()),
            },
            DeviceMapping {
                path_on_host: Some("/dev/sdb".to_string()),
                path_in_container: Some("/dev/xvdb".to_string()),
                cgroup_permissions: Some("rwm".to_string()),
            },
        ])
    );
}

#[test]
fn build_devices_defaults_missing_options_to_rwm() {
    // Docker's own API has no default for cgroup_permissions — an
    // absent value makes runc fail outright. See build_devices' doc
    // comment for why this must be filled in here.
    let devices = vec![("/dev/sda".to_string(), "/dev/xvda".to_string(), None)];

    let result = build_devices(Some(&devices)).unwrap();
    assert_eq!(result[0].cgroup_permissions, Some("rwm".to_string()));
}

#[test]
fn build_devices_is_none_when_devices_is_absent() {
    assert_eq!(build_devices(None), None);
}

/// Docker ANDs the values under one filter name, which is what makes
/// `project=x` plus `run=y` mean "both" rather than "either" — the
/// difference between finding one run's resources and finding every
/// run's.
#[test]
fn label_filters_and_every_pair_under_one_filter_name() {
    let filters = label_filters(&[
        ("eu.orican.ratect.project", Some("demo")),
        ("x.y", Some("z")),
    ]);
    assert_eq!(
        filters,
        HashMap::from([(
            "label".to_string(),
            vec![
                "eu.orican.ratect.project=demo".to_string(),
                "x.y=z".to_string()
            ]
        )])
    );
}

/// A key with no value is Docker's "has this label" filter — the form
/// that makes "every project" mean every project *Ratect* created,
/// rather than every container on the machine.
#[test]
fn a_label_with_no_value_filters_on_the_key_alone() {
    assert_eq!(
        label_filters(&[("eu.orican.ratect.project", None)]),
        HashMap::from([(
            "label".to_string(),
            vec!["eu.orican.ratect.project".to_string()]
        )])
    );
}

/// No filter at all, rather than an empty `label` entry Docker would
/// have to interpret. Callers are warned off this on the trait methods.
#[test]
fn no_labels_means_no_filter() {
    assert!(label_filters(&[]).is_empty());
}

#[test]
fn build_tmpfs_mounts_maps_container_and_options() {
    let tmpfs = vec![
        ("/tmp/a".to_string(), "size=64m".to_string()),
        ("/tmp/b".to_string(), String::new()),
    ];

    assert_eq!(
        build_tmpfs_mounts(Some(&tmpfs)),
        Some(HashMap::from([
            ("/tmp/a".to_string(), "size=64m".to_string()),
            ("/tmp/b".to_string(), String::new()),
        ]))
    );
}

#[test]
fn build_tmpfs_mounts_is_none_when_tmpfs_is_absent() {
    assert_eq!(build_tmpfs_mounts(None), None);
}

#[test]
fn build_log_config_carries_the_driver_and_options() {
    let options = HashMap::from([("max-size".to_string(), "10m".to_string())]);

    let log_config = build_log_config(Some("json-file"), Some(&options)).unwrap();

    assert_eq!(log_config.typ.as_deref(), Some("json-file"));
    assert_eq!(log_config.config, Some(options));
}

#[test]
fn build_log_config_is_none_when_log_driver_is_absent_even_with_options() {
    let options = HashMap::from([("max-size".to_string(), "10m".to_string())]);

    assert_eq!(build_log_config(None, Some(&options)), None);
}

/// A scratch directory holding real files and real Unix sockets, so
/// `classify_ssh_agent_paths` is exercised against the filesystem it
/// actually inspects rather than a stand-in for it — the socket-vs-file
/// distinction is a `stat` result, and nothing else can fake it.
struct ScratchPaths {
    directory: PathBuf,
    listeners: Vec<std::os::unix::net::UnixListener>,
}

impl ScratchPaths {
    fn new() -> Self {
        // Deliberately short: a socket path has to fit `sun_path`, and
        // macOS's per-user temporary directory already spends ~50 of
        // those characters before this name starts.
        let id = uuid::Uuid::new_v4().simple().to_string();
        let directory = std::env::temp_dir().join(format!("rt-{}", &id[..12]));
        std::fs::create_dir_all(&directory).unwrap();
        Self {
            directory,
            listeners: Vec::new(),
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.directory.join(name);
        std::fs::write(&path, b"not really a key").unwrap();
        path
    }

    fn socket(&mut self, name: &str) -> PathBuf {
        let path = self.directory.join(name);
        self.listeners
            .push(std::os::unix::net::UnixListener::bind(&path).unwrap());
        path
    }
}

impl Drop for ScratchPaths {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn an_ssh_agent_with_no_paths_forwards_the_host_agent() {
    assert_eq!(
        classify_ssh_agent_paths("default", &[]).unwrap(),
        SshAgentSource::HostAgent
    );
}

#[test]
fn ssh_agent_paths_naming_ordinary_files_are_private_keys() {
    let scratch = ScratchPaths::new();
    let paths = vec![scratch.file("id_ed25519"), scratch.file("id_rsa")];

    assert_eq!(
        classify_ssh_agent_paths("default", &paths).unwrap(),
        SshAgentSource::Keys(paths.clone())
    );
}

/// A path that doesn't exist is *not* a socket, so it takes the private
/// key route and fails there naming the file — a much better error than
/// a connection failure against a socket that was never there.
#[test]
fn a_missing_ssh_agent_path_is_treated_as_a_private_key() {
    let paths = vec![PathBuf::from("/nonexistent/ratect/id_ed25519")];

    assert_eq!(
        classify_ssh_agent_paths("default", &paths).unwrap(),
        SshAgentSource::Keys(paths.clone())
    );
}

#[test]
fn an_ssh_agent_path_naming_a_socket_forwards_that_socket() {
    let mut scratch = ScratchPaths::new();
    let socket = scratch.socket("agent.sock");

    assert_eq!(
        classify_ssh_agent_paths("default", std::slice::from_ref(&socket)).unwrap(),
        SshAgentSource::Socket(socket.clone())
    );
}

/// BuildKit's own rule, and the only sensible one: an id maps to one
/// agent, so a socket can't be combined with anything.
#[test]
fn an_ssh_agent_mixing_a_socket_with_key_files_is_rejected() {
    let mut scratch = ScratchPaths::new();
    let socket = scratch.socket("agent.sock");
    let key = scratch.file("id_ed25519");

    let error = classify_ssh_agent_paths("deploy", &[socket.clone(), key]).unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("deploy"), "unexpected error: {message}");
    assert!(
        message.contains(&socket.display().to_string()),
        "the error should name the socket, but was: {message}"
    );
}

#[test]
fn an_ssh_agent_naming_two_sockets_is_rejected() {
    let mut scratch = ScratchPaths::new();
    let paths = vec![scratch.socket("one.sock"), scratch.socket("two.sock")];

    let error = classify_ssh_agent_paths("deploy", &paths).unwrap_err();

    assert!(
        format!("{error:#}").contains("only one agent can be forwarded"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn build_port_config_is_none_when_ports_is_absent() {
    assert!(build_port_config(None).is_none());
}

#[test]
fn build_port_config_is_none_when_ports_is_empty() {
    assert!(build_port_config(Some(&vec![])).is_none());
}

#[test]
fn build_port_config_builds_exposed_ports_and_bindings() {
    let ports = vec![
        (8080, 80, "tcp".to_string()),
        (9000, 9000, "udp".to_string()),
    ];
    let (exposed, bindings) = build_port_config(Some(&ports)).unwrap();

    assert_eq!(exposed, vec!["80/tcp".to_string(), "9000/udp".to_string()]);
    assert_eq!(
        bindings["80/tcp"],
        Some(vec![PortBinding {
            host_ip: None,
            host_port: Some("8080".to_string()),
        }])
    );
    assert_eq!(
        bindings["9000/udp"],
        Some(vec![PortBinding {
            host_ip: None,
            host_port: Some("9000".to_string()),
        }])
    );
}

#[test]
fn build_cmd_with_command_and_no_additional_args_tokenizes_it() {
    let cmd = build_cmd(Some("echo hi there"), &[]).unwrap();
    assert_eq!(
        cmd,
        Some(vec![
            "echo".to_string(),
            "hi".to_string(),
            "there".to_string(),
        ])
    );
}

#[test]
fn build_cmd_with_command_and_additional_args_appends_them_as_literal_argv() {
    let additional_args = vec!["arg1".to_string(), "arg2".to_string()];
    let cmd = build_cmd(Some("echo hi"), &additional_args).unwrap();
    assert_eq!(
        cmd,
        Some(vec![
            "echo".to_string(),
            "hi".to_string(),
            "arg1".to_string(),
            "arg2".to_string(),
        ])
    );
}

#[test]
fn build_cmd_with_no_command_and_no_additional_args_lets_the_image_use_its_own_entrypoint() {
    // `None` (not an empty `Vec`) — bollard/Docker treats an unset `cmd`
    // as "use the image's own default CMD/entrypoint", which an empty
    // array wouldn't.
    assert_eq!(build_cmd(None, &[]).unwrap(), None);
}

#[test]
fn build_cmd_with_no_command_and_additional_args_passes_them_directly_as_argv() {
    let additional_args = vec!["migrate".to_string(), "--up".to_string()];
    let cmd = build_cmd(None, &additional_args).unwrap();
    assert_eq!(cmd, Some(vec!["migrate".to_string(), "--up".to_string()]));
}

#[test]
fn build_cmd_with_an_invalid_command_and_no_additional_args_fails() {
    assert!(build_cmd(Some("echo 'unbalanced"), &[]).is_err());
}

#[test]
fn tokenize_command_line_splits_on_whitespace() {
    assert_eq!(
        tokenize_command_line("echo   hi   there").unwrap(),
        vec!["echo", "hi", "there"]
    );
}

#[test]
fn tokenize_command_line_treats_single_quoted_content_as_one_literal_argument() {
    // The classic Batect idiom for forcing `sh -c`'s command string to
    // stay a single argv token: `entrypoint: /bin/sh -c`, `command:
    // 'make lint'` (the outer quotes are YAML's; the value is the
    // literal string `'make lint'`).
    assert_eq!(
        tokenize_command_line("'make lint'").unwrap(),
        vec!["make lint"]
    );
}

#[test]
fn entrypoint_and_command_combine_correctly_for_the_classic_sh_c_idiom() {
    // `entrypoint: /bin/sh -c` alongside `command: 'make lint'` is a
    // real, working Batect idiom — Docker execs `Entrypoint ++ Cmd`, so
    // this must produce exactly `/bin/sh -c "make lint"`, with neither
    // side inserting its own extra shell layer (the bug an earlier,
    // sh-c-wrapped `build_cmd` would have had once `entrypoint` support
    // landed — see CHANGELOG.md).
    let entrypoint = tokenize_command_line("/bin/sh -c").unwrap();
    assert_eq!(entrypoint, vec!["/bin/sh", "-c"]);

    let cmd = build_cmd(Some("'make lint'"), &[]).unwrap();
    assert_eq!(cmd, Some(vec!["make lint".to_string()]));
}

#[test]
fn tokenize_command_line_does_not_process_escapes_inside_single_quotes() {
    assert_eq!(
        tokenize_command_line(r"'a\b'").unwrap(),
        vec![r"a\b".to_string()]
    );
}

#[test]
fn tokenize_command_line_processes_escapes_inside_double_quotes() {
    assert_eq!(
        tokenize_command_line(r#""a\"b""#).unwrap(),
        vec![r#"a"b"#.to_string()]
    );
}

#[test]
fn tokenize_command_line_processes_a_backslash_escape_outside_any_quote() {
    assert_eq!(
        tokenize_command_line(r"hello\ world").unwrap(),
        vec!["hello world"]
    );
}

#[test]
fn tokenize_command_line_rejects_a_trailing_backslash() {
    let err = tokenize_command_line(r"echo hi\").unwrap_err();
    assert!(err.to_string().contains("ends with a backslash"));
}

#[test]
fn tokenize_command_line_rejects_an_unbalanced_single_quote() {
    let err = tokenize_command_line("echo 'hi").unwrap_err();
    assert!(err.to_string().contains("unbalanced single quote"));
}

#[test]
fn tokenize_command_line_rejects_an_unbalanced_double_quote() {
    let err = tokenize_command_line(r#"echo "hi"#).unwrap_err();
    assert!(err.to_string().contains("unbalanced double quote"));
}

#[test]
fn tokenize_command_line_of_an_empty_string_produces_no_arguments() {
    assert_eq!(tokenize_command_line("").unwrap(), Vec::<String>::new());
}

/// The `/`-joined relative paths of every entry in a tar built by
/// `build_context_tar`, for assertions.
fn tar_entry_paths(tar_bytes: &[u8]) -> Vec<String> {
    let mut archive = tar::Archive::new(tar_bytes);
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn build_context_tar_includes_everything_when_no_dockerignore() {
    let dir = unique_temp_dir();
    fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
    fs::write(dir.join("app.txt"), "hello").unwrap();
    fs::create_dir_all(dir.join("subdir")).unwrap();
    fs::write(dir.join("subdir/nested.txt"), "nested").unwrap();

    let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
    let mut entries = tar_entry_paths(&tar_bytes);
    entries.sort();

    assert_eq!(
        entries,
        vec![
            "Dockerfile".to_string(),
            "app.txt".to_string(),
            "subdir/nested.txt".to_string(),
        ]
    );

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn build_context_tar_excludes_dockerignore_matches() {
    let dir = unique_temp_dir();
    fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
    fs::write(dir.join(".dockerignore"), "secret.txt\n").unwrap();
    fs::write(dir.join("secret.txt"), "shh").unwrap();
    fs::write(dir.join("app.txt"), "hello").unwrap();

    let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
    let entries = tar_entry_paths(&tar_bytes);

    assert!(!entries.contains(&"secret.txt".to_string()));
    assert!(entries.contains(&"app.txt".to_string()));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn build_context_tar_always_includes_dockerfile_and_dockerignore_under_broad_exclusion() {
    let dir = unique_temp_dir();
    fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
    fs::write(dir.join(".dockerignore"), "*\n").unwrap();
    fs::write(dir.join("app.txt"), "hello").unwrap();

    let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
    let entries = tar_entry_paths(&tar_bytes);

    assert!(entries.contains(&"Dockerfile".to_string()));
    assert!(entries.contains(&".dockerignore".to_string()));
    assert!(!entries.contains(&"app.txt".to_string()));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn build_context_tar_force_includes_a_custom_named_dockerfile() {
    let dir = unique_temp_dir();
    fs::create_dir_all(dir.join("docker")).unwrap();
    fs::write(dir.join("docker/Dockerfile.prod"), "FROM alpine").unwrap();
    fs::write(dir.join(".dockerignore"), "*\n").unwrap();
    fs::write(dir.join("app.txt"), "hello").unwrap();

    let tar_bytes = build_context_tar(&dir, "docker/Dockerfile.prod").unwrap();
    let entries = tar_entry_paths(&tar_bytes);

    assert!(entries.contains(&"docker/Dockerfile.prod".to_string()));
    assert!(entries.contains(&".dockerignore".to_string()));
    assert!(!entries.contains(&"app.txt".to_string()));

    fs::remove_dir_all(&dir).unwrap();
}

/// Proves the root-only-for-bare-patterns behavior (see the
/// `dockerignore` crate) holds end-to-end through the tar: a bare
/// pattern only excludes a root-level match, not a nested one with the
/// same name.
#[test]
fn build_context_tar_bare_pattern_only_excludes_at_the_root() {
    let dir = unique_temp_dir();
    fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
    fs::write(dir.join(".dockerignore"), "build\n").unwrap();
    fs::create_dir_all(dir.join("build")).unwrap();
    fs::write(dir.join("build/output.txt"), "root build output").unwrap();
    fs::create_dir_all(dir.join("packages/foo/build")).unwrap();
    fs::write(
        dir.join("packages/foo/build/output.txt"),
        "nested build output",
    )
    .unwrap();

    let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
    let entries = tar_entry_paths(&tar_bytes);

    assert!(!entries.contains(&"build/output.txt".to_string()));
    assert!(entries.contains(&"packages/foo/build/output.txt".to_string()));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn build_output_suffix_is_empty_when_nothing_was_captured() {
    assert_eq!(build_output_suffix(""), "");
}

#[test]
fn build_output_suffix_includes_the_trimmed_transcript() {
    let output = "Step 1/3 : FROM alpine\nStep 2/3 : RUN false\n";
    assert_eq!(
        build_output_suffix(output),
        "\n\nBuild output:\nStep 1/3 : FROM alpine\nStep 2/3 : RUN false"
    );
}

#[test]
fn builder_selection_follows_the_daemon_advertised_default() {
    use bollard::query_parameters::BuilderVersion;
    assert_eq!(
        select_builder_version(None, Some("2")).unwrap(),
        BuilderVersion::BuilderBuildKit
    );
    assert_eq!(
        select_builder_version(None, Some("1")).unwrap(),
        BuilderVersion::BuilderV1
    );
}

#[test]
fn builder_selection_falls_back_to_classic_when_the_daemon_advertises_nothing() {
    use bollard::query_parameters::BuilderVersion;
    assert_eq!(
        select_builder_version(None, None).unwrap(),
        BuilderVersion::BuilderV1
    );
}

#[test]
fn docker_buildkit_env_var_overrides_the_daemon_advertised_default() {
    use bollard::query_parameters::BuilderVersion;
    // Forced off, even though the daemon advertises BuildKit…
    assert_eq!(
        select_builder_version(Some("0"), Some("2")).unwrap(),
        BuilderVersion::BuilderV1
    );
    assert_eq!(
        select_builder_version(Some("false"), Some("2")).unwrap(),
        BuilderVersion::BuilderV1
    );
    // …and forced on, even though it doesn't.
    assert_eq!(
        select_builder_version(Some("1"), Some("1")).unwrap(),
        BuilderVersion::BuilderBuildKit
    );
    assert_eq!(
        select_builder_version(Some("TRUE"), None).unwrap(),
        BuilderVersion::BuilderBuildKit
    );
}

#[test]
fn an_unparseable_docker_buildkit_env_var_is_a_hard_error() {
    let err = select_builder_version(Some("banana"), Some("2")).unwrap_err();
    assert!(err.to_string().contains("'banana'"));
}

#[test]
fn split_image_reference_separates_repo_and_tag() {
    assert_eq!(
        split_image_reference("myrepo/myimage:v2"),
        ("myrepo/myimage", Some("v2"))
    );
    assert_eq!(split_image_reference("myimage:v2"), ("myimage", Some("v2")));
}

#[test]
fn split_image_reference_with_no_tag_has_none() {
    assert_eq!(
        split_image_reference("myrepo/myimage"),
        ("myrepo/myimage", None)
    );
}

#[test]
fn split_image_reference_treats_a_registry_ports_colon_as_not_a_tag_separator() {
    assert_eq!(
        split_image_reference("localhost:5000/myimage"),
        ("localhost:5000/myimage", None)
    );
    assert_eq!(
        split_image_reference("localhost:5000/myimage:v2"),
        ("localhost:5000/myimage", Some("v2"))
    );
}

#[test]
fn enable_buildkit_flag_forces_buildkit_regardless_of_the_real_env_var() {
    assert_eq!(
        docker_buildkit_env_value(true, Some("0")).as_deref(),
        Some("1"),
        "the flag must win even when the real env var explicitly forces the classic builder"
    );
    assert_eq!(docker_buildkit_env_value(true, None).as_deref(), Some("1"));
}

#[test]
fn enable_buildkit_flag_off_defers_to_the_real_env_var() {
    assert_eq!(
        docker_buildkit_env_value(false, Some("0")).as_deref(),
        Some("0")
    );
    assert_eq!(docker_buildkit_env_value(false, None), None);
}

#[test]
fn docker_context_id_matches_the_docker_cli_own_hashing() {
    // Verified against a real `~/.docker/contexts/meta/<id>` entry on
    // this machine: `printf 'orbstack' | shasum -a 256`.
    assert_eq!(
        docker_context_id("orbstack"),
        "2d89b732b01a00a2d1675ed3cee9fd0f965daadf90603c989dd3afd4569c6896"
    );
}

fn write_docker_context_meta(config_directory: &Path, context_name: &str, host: &str) {
    let id = docker_context_id(context_name);
    let meta_dir = config_directory.join("contexts").join("meta").join(&id);
    fs::create_dir_all(&meta_dir).unwrap();
    fs::write(
            meta_dir.join("meta.json"),
            format!(
                r#"{{"Name":"{context_name}","Metadata":{{}},"Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":false}}}}}}"#
            ),
        )
        .unwrap();
}

#[test]
fn docker_context_host_reads_the_endpoints_docker_host_field() {
    let config_directory = unique_temp_dir();
    write_docker_context_meta(
        &config_directory,
        "orbstack",
        "unix:///Users/kevin/.orbstack/run/docker.sock",
    );

    let host = docker_context_host(&config_directory, "orbstack").unwrap();
    assert_eq!(host, "unix:///Users/kevin/.orbstack/run/docker.sock");
}

#[test]
fn docker_context_host_errors_clearly_when_the_context_does_not_exist() {
    let config_directory = unique_temp_dir();
    let err = docker_context_host(&config_directory, "no-such-context").unwrap_err();
    assert!(
        err.to_string()
            .contains("Docker context 'no-such-context' does not exist"),
        "{err}"
    );
}

#[test]
fn active_docker_context_reads_current_context_from_config_json() {
    let config_directory = unique_temp_dir();
    fs::write(
        config_directory.join("config.json"),
        r#"{"currentContext":"orbstack"}"#,
    )
    .unwrap();

    assert_eq!(
        active_docker_context(&config_directory),
        Some("orbstack".to_string())
    );
}

#[test]
fn active_docker_context_is_none_when_config_json_is_missing() {
    let config_directory = unique_temp_dir();
    assert_eq!(active_docker_context(&config_directory), None);
}

#[test]
fn active_docker_context_is_none_when_current_context_is_unset_or_empty() {
    let config_directory = unique_temp_dir();
    fs::write(config_directory.join("config.json"), r#"{}"#).unwrap();
    assert_eq!(active_docker_context(&config_directory), None);

    fs::write(
        config_directory.join("config.json"),
        r#"{"currentContext":""}"#,
    )
    .unwrap();
    assert_eq!(active_docker_context(&config_directory), None);
}

#[test]
fn resolve_context_name_prefers_an_explicit_context_over_everything_else() {
    let options = DockerConnectionOptions {
        host: None,
        context: Some("explicit".to_string()),
        config_directory: None,
        ..Default::default()
    };
    assert_eq!(
        resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
        Some("explicit".to_string())
    );
}

#[test]
fn resolve_context_name_an_explicit_host_skips_context_resolution_entirely() {
    let options = DockerConnectionOptions {
        host: Some("tcp://1.2.3.4:2375".to_string()),
        context: None,
        config_directory: None,
        ..Default::default()
    };
    assert_eq!(
        resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
        None
    );
}

#[test]
fn resolve_context_name_falls_back_to_the_env_var_then_the_active_context() {
    let options = DockerConnectionOptions::default();
    assert_eq!(
        resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
        Some("env-context".to_string())
    );
    assert_eq!(
        resolve_context_name(&options, None, Some("active".to_string())),
        Some("active".to_string())
    );
    assert_eq!(resolve_context_name(&options, None, None), None);
}

#[test]
fn resolve_context_name_treats_the_default_context_name_as_no_context() {
    let options = DockerConnectionOptions {
        host: None,
        context: Some("default".to_string()),
        config_directory: None,
        ..Default::default()
    };
    assert_eq!(resolve_context_name(&options, None, None), None);
}

#[test]
fn connect_rejects_using_both_docker_context_and_docker_host() {
    let options = DockerConnectionOptions {
        host: Some("tcp://1.2.3.4:2375".to_string()),
        context: Some("some-context".to_string()),
        config_directory: None,
        ..Default::default()
    };
    let err = connect(&options).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Cannot use both --docker-context and --docker-host."
    );
}

#[test]
fn connect_via_an_explicit_context_uses_that_contexts_stored_host() {
    let config_directory = unique_temp_dir();
    // A `tcp://` address (unlike `unix://`) only builds a
    // lazily-connecting client (no handshake, no eager socket-existence
    // check) — see `await_log_follower_waits_for_the_spawned_task_to_finish`'s
    // own comment for the same property.
    write_docker_context_meta(&config_directory, "my-context", "tcp://1.2.3.4:2375");

    let options = DockerConnectionOptions {
        host: None,
        context: Some("my-context".to_string()),
        config_directory: Some(config_directory),
        ..Default::default()
    };
    connect(&options).expect("connecting via a valid context's stored host should succeed");
}

#[test]
fn connect_via_an_explicit_context_errors_clearly_when_it_does_not_exist() {
    let config_directory = unique_temp_dir();
    let options = DockerConnectionOptions {
        host: None,
        context: Some("no-such-context".to_string()),
        config_directory: Some(config_directory),
        ..Default::default()
    };
    let err = connect(&options).unwrap_err();
    assert!(
        err.to_string()
            .contains("Docker context 'no-such-context' does not exist"),
        "{err}"
    );
}

#[test]
fn conflicting_option_with_context_names_whichever_tls_option_was_given() {
    let base = DockerConnectionOptions {
        context: Some("some-context".to_string()),
        ..Default::default()
    };

    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls: true,
            ..base.clone()
        }),
        Some("--docker-tls")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_verify: true,
            ..base.clone()
        }),
        Some("--docker-tls-verify")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            cert_path: Some(PathBuf::from("/tmp/certs")),
            ..base.clone()
        }),
        Some("--docker-cert-path")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_ca_cert: Some(PathBuf::from("/tmp/ca.pem")),
            ..base.clone()
        }),
        Some("--docker-tls-ca-cert")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_cert: Some(PathBuf::from("/tmp/cert.pem")),
            ..base.clone()
        }),
        Some("--docker-tls-cert")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_key: Some(PathBuf::from("/tmp/key.pem")),
            ..base.clone()
        }),
        Some("--docker-tls-key")
    );
    assert_eq!(conflicting_option_with_context(&base), None);
}

#[test]
fn connect_rejects_docker_tls_flags_combined_with_docker_context() {
    for options in [
        DockerConnectionOptions {
            context: Some("some-context".to_string()),
            tls: true,
            ..Default::default()
        },
        DockerConnectionOptions {
            context: Some("some-context".to_string()),
            tls_verify: true,
            ..Default::default()
        },
    ] {
        let err = connect(&options).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Cannot use both --docker-context and --docker-tls"),
            "{err}"
        );
    }
}

#[test]
fn tls_enabled_is_true_for_either_flag_or_the_real_env_var() {
    let base = DockerConnectionOptions::default();
    assert!(!tls_enabled(&base, None));
    assert!(tls_enabled(
        &DockerConnectionOptions {
            tls: true,
            ..base.clone()
        },
        None
    ));
    assert!(tls_enabled(
        &DockerConnectionOptions {
            tls_verify: true,
            ..base.clone()
        },
        None
    ));
    assert!(tls_enabled(&base, Some("1")));
    assert!(tls_enabled(&base, Some("true")));
    assert!(tls_enabled(&base, Some("TRUE")));
    assert!(!tls_enabled(&base, Some("0")));
    assert!(!tls_enabled(&base, Some("false")));
}

#[test]
fn docker_cert_directory_prefers_the_explicit_option_over_the_env_var_and_default() {
    let options = DockerConnectionOptions {
        cert_path: Some(PathBuf::from("/tmp/explicit-certs")),
        ..Default::default()
    };
    assert_eq!(
        docker_cert_directory(&options).unwrap(),
        PathBuf::from("/tmp/explicit-certs")
    );
}

/// A throwaway self-signed root CA, and a leaf certificate/key pair it
/// signs for `localhost`/`127.0.0.1` — 2048-bit RSA (`rcgen`'s
/// default), regenerated fresh every test run via `rcgen` rather than
/// a fixed PEM committed to the repo. A static embedded certificate
/// would eventually expire on its own and fail with a stale,
/// disconnected-looking failure long after the fact, unrelated to
/// whatever change actually triggered it — generating at test time
/// with an explicit `not_before`/`not_after` window sidesteps that
/// entirely, and lets the same helper produce a deliberately
/// *already-expired* leaf certificate on demand (see
/// `connect_over_tls_rejects_an_expired_certificate`).
struct GeneratedTlsMaterials {
    /// PEM text for `ca.pem` — what a real `--docker-cert-path`
    /// directory holds, and what `connect`'s `Docker::connect_with_ssl`
    /// call reads.
    ca_pem: String,
    cert_pem: String,
    key_pem: String,
    /// DER forms of the same leaf cert/key, for the in-process TLS
    /// server below (`rustls::ServerConfig` wants DER, not PEM).
    cert_der: rustls::pki_types::CertificateDer<'static>,
    key_der: rustls::pki_types::PrivateKeyDer<'static>,
}

fn generate_test_tls_materials(
    not_before: time::OffsetDateTime,
    not_after: time::OffsetDateTime,
) -> GeneratedTlsMaterials {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "ratect-test-ca");
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, ca_key);

    let mut leaf_params =
        rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    leaf_params.not_before = not_before;
    leaf_params.not_after = not_after;
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    GeneratedTlsMaterials {
        ca_pem: ca_cert.pem(),
        cert_pem: leaf_cert.pem(),
        key_pem: leaf_key.serialize_pem(),
        cert_der: leaf_cert.der().clone(),
        key_der: rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der()),
        ),
    }
}

fn write_tls_materials(dir: &Path, materials: &GeneratedTlsMaterials) {
    fs::write(dir.join("ca.pem"), &materials.ca_pem).unwrap();
    fs::write(dir.join("cert.pem"), &materials.cert_pem).unwrap();
    fs::write(dir.join("key.pem"), &materials.key_pem).unwrap();
}

/// Accepts exactly one TCP connection on `listener` and completes a
/// TLS handshake using `cert`/`key`, then responds to the first HTTP
/// request with a minimal 200 OK — just enough for `Docker::ping` to
/// succeed once the handshake itself does. No client-cert auth is
/// requested: this harness only exercises the *client's* verification
/// of the *server's* certificate (what `--docker-tls-verify` actually
/// controls), not Ratect's own client-certificate presentation.
///
/// If the handshake itself fails (e.g. the client rejects an expired
/// certificate), `TlsAcceptor::accept` returns `Err` and this simply
/// returns — there's nothing to serve, and that's the expected outcome
/// for that test.
async fn serve_one_tls_connection(
    listener: tokio::net::TcpListener,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("valid cert/key pair");
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));

    let (stream, _) = listener.accept().await.expect("accept");
    if let Ok(mut tls_stream) = acceptor.accept(stream).await {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 1024];
        let _ = tls_stream.read(&mut buf).await;
        let _ = tls_stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: text/plain\r\n\
                      Connection: close\r\n\r\nOK",
            )
            .await;
        let _ = tls_stream.shutdown().await;
    }
}

/// Every `connect_over_tls_*` test reaches `connect`'s
/// `Docker::connect_with_ssl`, which loads the OS trust store
/// (`rustls-native-certs`) on top of our throwaway test CA. On macOS that
/// native-cert load intermittently fails ("Could not load native certs")
/// when two threads hit the Security framework at once — so they take a
/// shared lock around `connect` instead of running concurrently.
/// Recovered from poisoning so a failing test reports its own assertion
/// rather than cascading a `PoisonError`.
///
/// **Every TLS-enabled `connect` in these tests needs this lock, not just
/// the ones that complete a handshake.** The racy step is the native-cert
/// load, which happens inside `connect_with_ssl` for a bad certificate
/// path just as much as a good one — so the two error-path tests below
/// take it too, even though neither ever opens a socket. Leaving them out
/// is what made the *handshake* test fail intermittently under a full
/// `--workspace` run: an unlocked error-path test would race it and the
/// wrong test would report the failure.
///
/// The lock can't help across processes, so it relies on these being the
/// only TLS `connect` callers in one test binary. A new one belongs here
/// too. (`connect` calls that fail *before* the TLS branch — the
/// context-conflict tests — don't need it, and take no lock.)
static TLS_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial_tls() -> std::sync::MutexGuard<'static, ()> {
    TLS_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tokio::test]
async fn connect_over_tls_completes_a_real_handshake_against_a_valid_certificate() {
    let now = time::OffsetDateTime::now_utc();
    let materials = generate_test_tls_materials(
        now - time::Duration::days(1),
        now + time::Duration::days(365),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_tls_connection(
        listener,
        materials.cert_der.clone(),
        materials.key_der.clone_key(),
    ));

    let cert_directory = unique_temp_dir();
    write_tls_materials(&cert_directory, &materials);
    let options = DockerConnectionOptions {
        host: Some(format!("tcp://127.0.0.1:{port}")),
        tls_verify: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    // The lock spans only `connect` — the synchronous native-cert load is
    // the racy part, and scoping it here keeps it off the `.await`s below
    // (which `clippy::await_holding_lock` would flag).
    let docker = {
        let _guard = serial_tls();
        connect(&options).expect("connecting over TLS should build a client")
    };
    let result = docker.ping().await;
    server.await.expect("server task should not panic");

    assert!(
        result.is_ok(),
        "expected a successful handshake and ping against a valid certificate, got {result:?}"
    );
}

#[tokio::test]
async fn connect_over_tls_rejects_an_expired_certificate() {
    let now = time::OffsetDateTime::now_utc();
    // Already expired, entirely in the past — not "expires during the
    // test", which would be flaky under any scheduling delay.
    let materials =
        generate_test_tls_materials(now - time::Duration::days(2), now - time::Duration::days(1));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_tls_connection(
        listener,
        materials.cert_der.clone(),
        materials.key_der.clone_key(),
    ));

    let cert_directory = unique_temp_dir();
    write_tls_materials(&cert_directory, &materials);
    let options = DockerConnectionOptions {
        host: Some(format!("tcp://127.0.0.1:{port}")),
        tls_verify: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    let docker = {
        let _guard = serial_tls();
        connect(&options).expect("connecting over TLS should build a client")
    };
    let result = docker.ping().await;
    server.await.expect("server task should not panic");

    assert!(
        result.is_err(),
        "expected an expired certificate to be rejected, got {result:?}"
    );
}

#[test]
fn connect_over_tls_errors_clearly_when_the_ca_file_is_not_valid_pem() {
    let cert_directory = unique_temp_dir();
    // Plain garbage with no `-----BEGIN CERTIFICATE-----` marker at all
    // just yields zero parsed certificates, not an error (an empty
    // trust store isn't a construction-time failure — only a later
    // handshake would ever notice) — this needs to *look* like a PEM
    // block to actually exercise the parse-failure path.
    fs::write(
        cert_directory.join("ca.pem"),
        b"-----BEGIN CERTIFICATE-----\nnot valid base64!!!\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    fs::write(cert_directory.join("cert.pem"), b"").unwrap();
    fs::write(cert_directory.join("key.pem"), b"").unwrap();

    let options = DockerConnectionOptions {
        host: Some("tcp://127.0.0.1:2376".to_string()),
        tls_verify: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    let err = {
        let _guard = serial_tls();
        connect(&options).unwrap_err()
    };
    assert!(err.to_string().contains("over TLS"), "{err}");
}

#[test]
fn connect_over_tls_errors_clearly_when_a_certificate_file_is_missing() {
    let cert_directory = unique_temp_dir();
    // No certificate files written.

    let options = DockerConnectionOptions {
        host: Some("tcp://127.0.0.1:2376".to_string()),
        tls: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    let err = {
        let _guard = serial_tls();
        connect(&options).unwrap_err()
    };
    assert!(err.to_string().contains("over TLS"), "{err}");
}

#[test]
fn require_host_for_tls_errors_clearly_when_no_host_resolved() {
    let err = require_host_for_tls(None).unwrap_err();
    assert!(
        err.to_string()
            .contains("--docker-tls/--docker-tls-verify requires --docker-host"),
        "{err}"
    );
}

#[test]
fn require_host_for_tls_passes_through_a_resolved_host() {
    assert_eq!(
        require_host_for_tls(Some("tcp://1.2.3.4:2376".to_string())).unwrap(),
        "tcp://1.2.3.4:2376"
    );
}

#[test]
fn resolve_host_prefers_the_explicit_option_then_the_injected_env_value() {
    let options = DockerConnectionOptions {
        host: Some("tcp://explicit:2375".to_string()),
        ..Default::default()
    };
    assert_eq!(
        resolve_host(&options, Some("tcp://from-env:2375")),
        Some("tcp://explicit:2375".to_string())
    );

    let options = DockerConnectionOptions::default();
    assert_eq!(
        resolve_host(&options, Some("tcp://from-env:2375")),
        Some("tcp://from-env:2375".to_string())
    );
    assert_eq!(resolve_host(&options, None), None);
}

#[test]
fn should_use_tty_requires_both_stdin_and_stdout_to_be_real_terminals() {
    assert!(should_use_tty(true, true, true));
}

#[test]
fn should_use_tty_is_false_when_not_interactive_eligible() {
    assert!(!should_use_tty(false, true, true));
}

#[test]
fn should_use_tty_is_false_when_stdin_is_not_a_terminal() {
    assert!(!should_use_tty(true, false, true));
}

#[test]
fn should_use_tty_is_false_when_stdout_is_not_a_terminal() {
    assert!(!should_use_tty(true, true, false));
}

fn user_mapping_fixture() -> UserMapping {
    UserMapping {
        user: crate::user::CurrentUser {
            uid: 1000,
            gid: 1000,
            username: "ratect".to_string(),
            groupname: "ratect".to_string(),
        },
        home_directory: "/home/ratect".to_string(),
        cache_directories: Vec::new(),
    }
}

fn tar_entry_contents(tar_bytes: &[u8], path: &str) -> String {
    let mut archive = tar::Archive::new(tar_bytes);
    let mut entry = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap())
        .find(|e| e.path().unwrap().to_string_lossy() == path)
        .unwrap_or_else(|| panic!("no {path:?} entry found"));
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut entry, &mut contents).unwrap();
    contents
}

#[test]
fn build_user_mapping_tar_includes_passwd_shadow_and_group() {
    let mapping = user_mapping_fixture();
    let tar_bytes = build_user_mapping_tar(&mapping).unwrap();
    let entries = tar_entry_paths(&tar_bytes);

    assert_eq!(entries, vec!["passwd", "shadow", "group"]);
    assert_eq!(
        tar_entry_contents(&tar_bytes, "passwd"),
        crate::user::generate_passwd_file(&mapping.user, &mapping.home_directory)
    );
    assert_eq!(
        tar_entry_contents(&tar_bytes, "shadow"),
        crate::user::generate_shadow_file(&mapping.user)
    );
    assert_eq!(
        tar_entry_contents(&tar_bytes, "group"),
        crate::user::generate_group_file(&mapping.user)
    );
}

#[test]
fn build_user_mapping_tar_entries_are_root_owned_with_correct_modes() {
    let tar_bytes = build_user_mapping_tar(&user_mapping_fixture()).unwrap();
    let mut archive = tar::Archive::new(tar_bytes.as_slice());
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let header = entry.header();
        assert_eq!(header.uid().unwrap(), 0);
        assert_eq!(header.gid().unwrap(), 0);
        let expected_mode = match entry.path().unwrap().to_string_lossy().as_ref() {
            "shadow" => 0o640,
            _ => 0o644,
        };
        assert_eq!(header.mode().unwrap(), expected_mode);
    }
}

#[test]
fn build_owned_directory_tar_creates_a_directory_entry_owned_by_the_mapped_user() {
    let tar_bytes = build_owned_directory_tar("/home/ratect", 1000, 1000).unwrap();
    let mut archive = tar::Archive::new(tar_bytes.as_slice());
    let mut entries = archive.entries().unwrap().map(|e| e.unwrap());
    let entry = entries.next().unwrap();

    assert_eq!(entry.path().unwrap().to_string_lossy(), "ratect/");
    assert_eq!(entry.header().entry_type(), tar::EntryType::Directory);
    assert_eq!(entry.header().uid().unwrap(), 1000);
    assert_eq!(entry.header().gid().unwrap(), 1000);
    assert_eq!(entry.header().mode().unwrap(), 0o755);
    assert!(entries.next().is_none());
}

#[test]
fn owned_directory_parent_is_the_directory_above_the_leaf() {
    assert_eq!(owned_directory_parent("/home/ratect"), "/home");
}

/// A cache mount is the other caller, and its container path is arbitrary —
/// `/cache` sits directly under the root, and a nested one several levels
/// down. Both have to land in the right parent for the ownership change to
/// apply to the mount point rather than something else.
#[test]
fn owned_directory_parent_handles_a_cache_mount_at_any_depth() {
    assert_eq!(owned_directory_parent("/cache"), "/");
    assert_eq!(
        owned_directory_parent("/home/special-place/subdir/cache"),
        "/home/special-place/subdir"
    );
}

/// Batect rejects a non-absolute cache mount path rather than uploading to
/// a surprising place — `uploadDirectory` would otherwise resolve it
/// relative to whatever `path` the API call names.
#[test]
fn build_owned_directory_tar_rejects_a_relative_path() {
    let err = build_owned_directory_tar("cache", 1000, 1000)
        .expect_err("a relative path should be rejected");
    assert!(
        format!("{err:#}").contains("not an absolute path"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn owned_directory_parent_is_root_for_a_top_level_home_directory() {
    assert_eq!(owned_directory_parent("/ratect"), "/");
}

#[test]
fn ensure_host_volume_directories_exist_creates_a_missing_directory() {
    let dir = unique_temp_dir();
    let host_path = dir.join("missing");
    let volumes = vec![format!("{}:/code", host_path.display())];

    assert!(!host_path.exists());
    ensure_host_volume_directories_exist(Some(&volumes)).unwrap();
    assert!(host_path.is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_leaves_an_existing_directory_alone() {
    let dir = unique_temp_dir();
    let volumes = vec![format!("{}:/code", dir.display())];

    ensure_host_volume_directories_exist(Some(&volumes)).unwrap();
    assert!(dir.is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_does_nothing_when_there_are_no_volumes() {
    ensure_host_volume_directories_exist(None).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_skips_a_named_docker_volume() {
    // A `cache` mount under `CacheType::Volume` resolves to a bare
    // volume name (non-absolute), not a host path — this must not be
    // `mkdir -p`'d relative to the current directory. Deliberately
    // doesn't mutate the process's real current directory to prove
    // this (unsafe under `cargo test`'s default parallel execution —
    // other tests, e.g. in config.rs, read the real current directory
    // too); instead uses a name specific enough that no other test
    // could plausibly create a same-named directory in the real
    // current directory, and cleans it up regardless in case a
    // regression here ever did create it.
    let unlikely_relative_name =
        "ratect-test-ensure-host-volume-directories-exist-skips-a-named-docker-volume";
    let would_be_created_here = std::env::current_dir()
        .unwrap()
        .join(unlikely_relative_name);
    assert!(!would_be_created_here.exists());

    let volumes = vec![format!("{unlikely_relative_name}:/root/.gradle")];
    ensure_host_volume_directories_exist(Some(&volumes)).unwrap();

    let created = would_be_created_here.exists();
    if created {
        fs::remove_dir_all(&would_be_created_here).unwrap();
    }
    assert!(!created);
}

/// Mounting a single existing *file* — `~/.gitconfig`, a known-hosts
/// file, an SSH agent socket — is a legitimate pattern, and Batect
/// supports it (`!Files.exists`): the file is left exactly as it is,
/// for Docker to bind-mount, not `mkdir`ed over.
#[test]
fn ensure_host_volume_directories_exist_leaves_an_existing_file_alone() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    let host_path = dir.join("gitconfig");
    fs::write(&host_path, "[user]\n  name = someone\n").unwrap();
    let volumes = vec![format!("{}:/root/.gitconfig", host_path.display())];

    ensure_host_volume_directories_exist(Some(&volumes)).unwrap();

    assert!(host_path.is_file(), "the file must be left untouched");

    fs::remove_dir_all(&dir).unwrap();
}

/// Docker Desktop's own injected paths (the SSH agent socket lives at
/// `/run/host-services/ssh-auth.sock`) don't exist on a macOS host, so
/// `mkdir -p` on one fails — mounting the SSH agent with
/// `run_as_current_user` on hit exactly this. They must be skipped, not
/// created. See Batect's `isSpecialDockerDesktopPath`.
#[test]
fn ensure_host_volume_directories_exist_skips_special_docker_desktop_paths() {
    let volumes = vec![
        "/run/host-services/ssh-auth.sock:/run/host-services/ssh-auth.sock".to_string(),
        "/run/guest-services/something:/something".to_string(),
    ];

    // The bug was this returning an error; that it's `Ok` is the fix.
    ensure_host_volume_directories_exist(Some(&volumes)).unwrap();
}
