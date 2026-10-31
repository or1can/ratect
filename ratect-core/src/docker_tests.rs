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
use crate::ui::test_support::FailingWriter;
use std::time::Duration;

/// The entry `proxy::ProxyEnvironment::host_gateway` produces for a run that
/// rewrote a proxy URL — spelt out here rather than called for, so these
/// tests pin the string Docker actually receives.
const HOST_GATEWAY: crate::proxy::HostGateway = crate::proxy::HostGateway {
    name: "host.docker.internal",
    address: "host-gateway",
};

/// A fresh, unique scratch directory — same pattern as
/// `config_tests.rs`'s `unique_temp_dir`. Caller cleans up.
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
/// only variant `drain_interleaved_output`'s tests need — and reports
/// its result stream's reader gone once `reader_gone_after` lines have been
/// posted, if set.
#[derive(Default)]
struct RecordingEventSink {
    lines: std::sync::Mutex<Vec<String>>,
    reader_gone_after: Option<usize>,
}

impl EventSink for RecordingEventSink {
    fn post(&self, event: TaskEvent) {
        if let TaskEvent::ContainerOutput { line, .. } = event {
            self.lines.lock().unwrap().push(line);
        }
    }

    fn result_reader_gone(&self) -> bool {
        self.reader_gone_after
            .is_some_and(|after| self.lines.lock().unwrap().len() >= after)
    }
}

/// A reader that has gone is what `StdoutClosed` means, and nothing else
/// is: any other failed write is Ratect failing to deliver the output, not
/// the user deciding they have read enough (ratect#670).
#[test]
fn a_container_output_write_to_a_gone_reader_is_stdout_closed() {
    let error =
        write_container_output(&mut FailingWriter(std::io::ErrorKind::BrokenPipe), b"one\n")
            .unwrap_err();
    assert!(error.is::<StdoutClosed>(), "{error:#}");

    let error = write_container_output(&mut FailingWriter(std::io::ErrorKind::Other), b"one\n")
        .unwrap_err();
    assert!(!error.is::<StdoutClosed>(), "{error:#}");
}

#[test]
fn a_container_output_write_reaches_the_writer_whole() {
    let mut written = Vec::new();
    write_container_output(&mut written, b"one\ntwo").unwrap();
    assert_eq!(written, b"one\ntwo");
}

/// Under `-o all` the container's lines reach stdout through the logger's
/// `Console`, which ignores a failed write — so the stream asks the sink
/// after each chunk, and stops the run the moment its reader has gone
/// rather than following the container to its end (ratect#670).
#[tokio::test]
async fn drain_interleaved_output_stops_once_the_result_reader_has_gone() {
    let chunks = vec![
        Ok(bollard::container::LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"one\n"),
        }),
        Ok(bollard::container::LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"two\n"),
        }),
    ];
    let sink = std::sync::Arc::new(RecordingEventSink {
        reader_gone_after: Some(1),
        ..Default::default()
    });
    let dyn_sink: std::sync::Arc<dyn EventSink> = sink.clone();

    let error = drain_interleaved_output(futures::stream::iter(chunks), &dyn_sink, "app")
        .await
        .unwrap_err();

    assert!(error.is::<StdoutClosed>(), "{error:#}");
    assert_eq!(*sink.lines.lock().unwrap(), vec!["one"]);
}

#[tokio::test]
async fn drain_interleaved_output_flushes_a_buffered_partial_line_before_a_stream_error() {
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

    let result = drain_interleaved_output(stream, &dyn_sink, "app").await;

    assert!(result.is_err(), "the stream error should still propagate");
    assert_eq!(
        *sink.lines.lock().unwrap(),
        vec!["first line", "unterminated final line"],
        "the unterminated final line must still be flushed despite the stream error"
    );
}

/// Under `-o all` the container's stderr is a `ContainerOutput` line like
/// its stdout, in the order the two arrived — never set aside for the
/// process's own stderr, as Batect's single stdout sink for both streams
/// does (ratect#349, NRC-1).
#[tokio::test]
async fn drain_interleaved_output_posts_stderr_and_stdout_in_arrival_order() {
    let chunks = vec![
        Ok(bollard::container::LogOutput::StdErr {
            message: bytes::Bytes::from_static(b"err 1\n"),
        }),
        Ok(bollard::container::LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"out 1\n"),
        }),
        Ok(bollard::container::LogOutput::StdErr {
            message: bytes::Bytes::from_static(b"err 2\n"),
        }),
    ];
    let sink = std::sync::Arc::new(RecordingEventSink::default());
    let dyn_sink: std::sync::Arc<dyn EventSink> = sink.clone();

    drain_interleaved_output(futures::stream::iter(chunks), &dyn_sink, "app")
        .await
        .unwrap();

    assert_eq!(*sink.lines.lock().unwrap(), vec!["err 1", "out 1", "err 2"]);
}

/// A line is posted as soon as its newline arrives, not when the stream
/// ends: polled once after each send, the sink already holds every
/// completed line, one event apiece (ratect#349, U28).
#[tokio::test]
async fn drain_interleaved_output_posts_each_line_as_it_arrives() {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let sink = std::sync::Arc::new(RecordingEventSink::default());
    let dyn_sink: std::sync::Arc<dyn EventSink> = sink.clone();
    let mut drain = std::pin::pin!(drain_interleaved_output(rx, &dyn_sink, "app"));

    tx.unbounded_send(Ok(bollard::container::LogOutput::StdOut {
        message: bytes::Bytes::from_static(b"Line 1\n"),
    }))
    .unwrap();
    assert!(futures::poll!(drain.as_mut()).is_pending());
    assert_eq!(*sink.lines.lock().unwrap(), vec!["Line 1"]);

    tx.unbounded_send(Ok(bollard::container::LogOutput::StdOut {
        message: bytes::Bytes::from_static(b"Line 2\n"),
    }))
    .unwrap();
    assert!(futures::poll!(drain.as_mut()).is_pending());
    assert_eq!(*sink.lines.lock().unwrap(), vec!["Line 1", "Line 2"]);

    drop(tx);
    drain.await.unwrap();
}

/// An interrupt drops the run future, and with it this one: what was
/// posted before the drop stays posted, and nothing sent afterwards is
/// (ratect#349, U33).
#[tokio::test]
async fn drain_interleaved_output_dropped_mid_stream_keeps_only_earlier_lines() {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let sink = std::sync::Arc::new(RecordingEventSink::default());
    let dyn_sink: std::sync::Arc<dyn EventSink> = sink.clone();
    let mut drain = Box::pin(drain_interleaved_output(rx, &dyn_sink, "app"));

    tx.unbounded_send(Ok(bollard::container::LogOutput::StdOut {
        message: bytes::Bytes::from_static(b"Line 1\n"),
    }))
    .unwrap();
    assert!(futures::poll!(drain.as_mut()).is_pending());
    drop(drain);
    // The receiver went with the dropped future, so this send has nowhere
    // to go — which is the point.
    let _ = tx.unbounded_send(Ok(bollard::container::LogOutput::StdOut {
        message: bytes::Bytes::from_static(b"Line 2\n"),
    }));

    assert_eq!(*sink.lines.lock().unwrap(), vec!["Line 1"]);
}

#[tokio::test]
async fn await_log_follower_waits_for_the_spawned_task_to_finish() {
    let followers = LogFollowers::default();
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished_in_task = std::sync::Arc::clone(&finished);
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        finished_in_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    followers.insert("container-1".to_string(), handle);

    followers.await_and_remove("container-1").await;

    assert!(
        finished.load(std::sync::atomic::Ordering::SeqCst),
        "await_and_remove should not return before the spawned task finishes — this is \
             the exact race the fix closes: ContainerRemoved posting before a follower's final \
             flush"
    );
    assert!(
        followers.handles.lock().unwrap().is_empty(),
        "the entry should be removed once awaited, so a later call for the same id is a no-op"
    );
}

#[tokio::test]
async fn await_log_follower_is_a_no_op_for_a_container_with_no_follower() {
    let followers = LogFollowers::default();
    // Every non-interleaved run (and the task's own container, always)
    // never inserts an entry at all — must return immediately, not hang.
    followers.await_and_remove("no-such-container").await;
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

/// #391: a duration past `i64::MAX` nanoseconds (about 292 years) is
/// clamped to it, as Batect's Docker client does, rather than wrapping to
/// a negative count Docker would refuse.
#[test]
fn build_health_config_clamps_a_duration_too_long_for_docker() {
    for too_long in [
        Duration::from_nanos(i64::MAX as u64 + 1),
        Duration::from_nanos(u64::MAX),
    ] {
        let options = HealthCheckOptions {
            command: None,
            interval: Some(too_long),
            retries: None,
            start_period: Some(too_long),
            timeout: Some(too_long),
        };

        let config = build_health_config(Some(&options)).unwrap();
        assert_eq!(config.interval, Some(i64::MAX));
        assert_eq!(config.timeout, Some(i64::MAX));
        assert_eq!(config.start_period, Some(i64::MAX));
    }
}

/// #391: the clamp starts past `i64::MAX` nanoseconds, not at it.
#[test]
fn build_health_config_passes_a_duration_docker_can_hold_through() {
    for duration in [
        Duration::from_nanos(i64::MAX as u64),
        Duration::from_millis(1),
    ] {
        let options = HealthCheckOptions {
            command: None,
            interval: Some(duration),
            retries: None,
            start_period: Some(duration),
            timeout: Some(duration),
        };

        let expected = Some(duration.as_nanos() as i64);
        let config = build_health_config(Some(&options)).unwrap();
        assert_eq!(config.interval, expected);
        assert_eq!(config.timeout, expected);
        assert_eq!(config.start_period, expected);
    }
}

#[test]
fn build_extra_hosts_formats_and_sorts_name_ip_pairs() {
    let mut hosts = HashMap::new();
    hosts.insert("zeta-service".to_string(), "10.0.0.2".to_string());
    hosts.insert("alpha-service".to_string(), "10.0.0.1".to_string());

    assert_eq!(
        build_extra_hosts(Some(&hosts), None),
        Some(vec![
            "alpha-service:10.0.0.1".to_string(),
            "zeta-service:10.0.0.2".to_string(),
        ])
    );
}

#[test]
fn build_extra_hosts_is_none_when_there_is_nothing_to_add() {
    assert_eq!(build_extra_hosts(None, None), None);
}

/// A run with a rewritten proxy URL but no `additional_hosts` at all still
/// needs the entry — the `None` short-circuit is about having nothing to
/// add, not about the config being silent.
#[test]
fn build_extra_hosts_adds_the_proxy_host_gateway_on_its_own() {
    assert_eq!(
        build_extra_hosts(None, Some(HOST_GATEWAY)),
        Some(vec!["host.docker.internal:host-gateway".to_string()])
    );
}

#[test]
fn build_extra_hosts_merges_the_proxy_host_gateway_with_config_entries() {
    let mut hosts = HashMap::new();
    hosts.insert("alpha-service".to_string(), "10.0.0.1".to_string());

    assert_eq!(
        build_extra_hosts(Some(&hosts), Some(HOST_GATEWAY)),
        Some(vec![
            "alpha-service:10.0.0.1".to_string(),
            "host.docker.internal:host-gateway".to_string(),
        ])
    );
}

/// The injection supplies a name nothing else provides, so where the project
/// has already said what that name means, it means that — otherwise enabling
/// a proxy would silently repoint a host the config deliberately pinned.
#[test]
fn a_config_entry_wins_over_the_injected_proxy_host_gateway() {
    let mut hosts = HashMap::new();
    hosts.insert("host.docker.internal".to_string(), "10.0.0.9".to_string());

    assert_eq!(
        build_extra_hosts(Some(&hosts), Some(HOST_GATEWAY)),
        Some(vec!["host.docker.internal:10.0.0.9".to_string()])
    );
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

#[test]
fn build_ulimits_maps_name_soft_and_hard() {
    let ulimits = vec![
        ("nofile".to_string(), 1024, 2048),
        ("core".to_string(), -1, -1),
    ];

    assert_eq!(
        build_ulimits(Some(&ulimits)),
        Some(vec![
            ResourcesUlimits {
                name: Some("nofile".to_string()),
                soft: Some(1024),
                hard: Some(2048),
            },
            ResourcesUlimits {
                name: Some("core".to_string()),
                soft: Some(-1),
                hard: Some(-1),
            },
        ])
    );
}

#[test]
fn build_ulimits_is_none_when_ulimits_is_absent() {
    assert_eq!(build_ulimits(None), None);
}

/// `network_mode` (ratect#106) is spelled as Docker's own `--network` would
/// spell it — for the task's network too, its name, so the container is
/// created on that network and never on the daemon's default `bridge`
/// (ratect#303).
#[test]
fn build_network_mode_spells_each_mode_as_docker_does() {
    assert_eq!(
        build_network_mode(&ContainerNetwork::Task("ratect-run".to_string())),
        "ratect-run"
    );
    assert_eq!(build_network_mode(&ContainerNetwork::Host), "host");
    assert_eq!(build_network_mode(&ContainerNetwork::None), "none");
    assert_eq!(
        build_network_mode(&ContainerNetwork::Container("abc123".to_string())),
        "container:abc123"
    );
}

/// The one endpoint a container on the task's network is created with, as
/// docker-client creates it: keyed by that network, its aliases the
/// container's own name then each `additional_hostnames` entry, in order
/// (ratect#303).
fn task_endpoint(network: &str, aliases: &[&str]) -> bollard::models::NetworkingConfig {
    bollard::models::NetworkingConfig {
        endpoints_config: Some(HashMap::from([(
            network.to_string(),
            EndpointSettings {
                aliases: Some(aliases.iter().map(|alias| alias.to_string()).collect()),
                ..Default::default()
            },
        )])),
    }
}

#[test]
fn build_networking_config_gives_the_task_network_its_aliases() {
    let hostnames = vec!["db-alias".to_string(), "another".to_string()];
    assert_eq!(
        build_networking_config(
            &ContainerNetwork::Task("ratect-run".to_string()),
            "database",
            Some(&hostnames),
        ),
        Some(task_endpoint(
            "ratect-run",
            &["database", "db-alias", "another"]
        ))
    );
}

#[test]
fn build_networking_config_aliases_only_the_name_without_additional_hostnames() {
    assert_eq!(
        build_networking_config(
            &ContainerNetwork::Task("ratect-run".to_string()),
            "database",
            None,
        ),
        Some(task_endpoint("ratect-run", &["database"]))
    );
}

/// Off the task's network there is no endpoint to configure: docker-client
/// sends no networking config at all, and the mode alone says where it goes.
#[test]
fn build_networking_config_is_none_off_the_task_network() {
    let hostnames = vec!["db-alias".to_string()];
    for network in [
        ContainerNetwork::Host,
        ContainerNetwork::None,
        ContainerNetwork::Container("abc123".to_string()),
    ] {
        assert_eq!(
            build_networking_config(&network, "database", Some(&hostnames)),
            None,
            "{network:?}"
        );
    }
}

/// Docker refuses a hostname alongside `container:` — the namespace owner's
/// is the one in use — so only that mode goes without.
#[test]
fn build_hostname_is_omitted_only_when_sharing_a_namespace() {
    for network in [
        ContainerNetwork::Task("ratect-run".to_string()),
        ContainerNetwork::Host,
        ContainerNetwork::None,
    ] {
        assert_eq!(
            build_hostname("app", &network),
            Some("app".to_string()),
            "{network:?}"
        );
    }
    assert_eq!(
        build_hostname("app", &ContainerNetwork::Container("abc123".to_string())),
        None
    );
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

/// Batect holds an agent's paths as a set, so a socket listed twice is one
/// socket, not two agents under one id (ratect#390).
#[test]
fn an_ssh_agent_listing_the_same_socket_twice_forwards_that_socket() {
    let mut scratch = ScratchPaths::new();
    let socket = scratch.socket("agent.sock");

    assert_eq!(
        classify_ssh_agent_paths("default", &[socket.clone(), socket.clone()]).unwrap(),
        SshAgentSource::Socket(socket.clone())
    );
}

#[test]
fn an_ssh_agent_listing_the_same_key_file_twice_serves_it_once() {
    let scratch = ScratchPaths::new();
    let key = scratch.file("id_ed25519");
    let other = scratch.file("id_rsa");

    assert_eq!(
        classify_ssh_agent_paths("default", &[key.clone(), other.clone(), key.clone()]).unwrap(),
        SshAgentSource::Keys(vec![key.clone(), other.clone()])
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

fn host_port(port: &str) -> PortBinding {
    PortBinding {
        host_ip: None,
        host_port: Some(port.to_string()),
    }
}

/// ratect#291: one container port published on two host ports binds both,
/// as docker-client does, rather than the second replacing the first.
#[test]
fn build_port_config_binds_every_host_port_for_one_container_port() {
    let ports = vec![
        (19000, 80, "tcp".to_string()),
        (19001, 80, "tcp".to_string()),
    ];
    let (exposed, bindings) = build_port_config(Some(&ports)).unwrap();

    assert_eq!(
        bindings["80/tcp"],
        Some(vec![host_port("19000"), host_port("19001")])
    );
    assert_eq!(exposed, vec!["80/tcp".to_string()]);
}

#[test]
fn build_port_config_binds_an_identical_mapping_once() {
    let ports = vec![(9000, 80, "tcp".to_string()), (9000, 80, "tcp".to_string())];
    let (exposed, bindings) = build_port_config(Some(&ports)).unwrap();

    assert_eq!(bindings["80/tcp"], Some(vec![host_port("9000")]));
    assert_eq!(exposed, vec!["80/tcp".to_string()]);
}

#[test]
fn build_port_config_keeps_one_container_port_over_two_protocols_apart() {
    let ports = vec![(9000, 80, "tcp".to_string()), (9001, 80, "udp".to_string())];
    let (exposed, bindings) = build_port_config(Some(&ports)).unwrap();

    assert_eq!(exposed, vec!["80/tcp".to_string(), "80/udp".to_string()]);
    assert_eq!(bindings["80/tcp"], Some(vec![host_port("9000")]));
    assert_eq!(bindings["80/udp"], Some(vec![host_port("9001")]));
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

/// #399: with no `command`, additional arguments become the whole `Cmd`, so
/// the image's own default `CMD` is the one replaced — reported only when
/// there is one.
#[test]
fn replaced_image_cmd_is_the_images_default_cmd_when_args_stand_in_for_it() {
    let args = vec!["--debug".to_string()];
    let image_cmd = vec!["--port".to_string(), "80".to_string()];

    assert_eq!(
        replaced_image_cmd(None, &args, Some(&image_cmd)),
        Some(image_cmd.as_slice())
    );
    // No arguments: the image's `CMD` runs.
    assert_eq!(replaced_image_cmd(None, &[], Some(&image_cmd)), None);
    // A `command` replaces the `CMD` itself, arguments or not.
    assert_eq!(
        replaced_image_cmd(Some("server"), &args, Some(&image_cmd)),
        None
    );
    // Nothing to replace.
    assert_eq!(replaced_image_cmd(None, &args, Some(&[])), None);
    assert_eq!(replaced_image_cmd(None, &args, None), None);
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

/// docker-client resets every context entry to root (`resetUIDAndGID`); the
/// host's ownership would otherwise reach a BuildKit `COPY`. Fails whenever
/// the host user isn't root, which is every CI run.
#[test]
fn build_context_tar_entries_are_root_owned_and_keep_their_mode() {
    let dir = unique_temp_dir();
    fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
    fs::create_dir_all(dir.join("subdir")).unwrap();
    fs::write(dir.join("subdir/run.sh"), "#!/bin/sh").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.join("subdir/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
    let mut archive = tar::Archive::new(tar_bytes.as_slice());
    let mut seen = Vec::new();
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let header = entry.header();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        assert_eq!(header.uid().unwrap(), 0, "{path}");
        assert_eq!(header.gid().unwrap(), 0, "{path}");
        for name in [header.username().unwrap(), header.groupname().unwrap()] {
            assert!(
                matches!(name, None | Some("") | Some("root")),
                "{path}: {name:?}"
            );
        }
        #[cfg(unix)]
        if path == "subdir/run.sh" {
            assert_eq!(header.mode().unwrap() & 0o777, 0o755);
        }
        seen.push(path);
    }
    seen.sort();
    assert_eq!(seen, vec!["Dockerfile", "subdir/run.sh"]);

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

fn secrets_only() -> BuildKitOptions {
    BuildKitOptions {
        secrets: HashMap::from([(
            "token".to_string(),
            BuildSecretSource::Environment("TOKEN".to_string()),
        )]),
        ssh_agents: HashMap::new(),
    }
}

fn ssh_agents_only() -> BuildKitOptions {
    BuildKitOptions {
        secrets: HashMap::new(),
        ssh_agents: HashMap::from([("default".to_string(), SshAgentSource::HostAgent)]),
    }
}

/// Batect's rules U70 and U71 (ratect#521): a daemon that advertises no
/// builder gets the classic one, which can serve neither `build_secrets`
/// nor `build_ssh`, so the build is refused.
#[test]
fn a_daemon_advertising_no_builder_refuses_buildkit_only_build_fields() {
    let builder = select_builder_version(None, None).unwrap();
    for options in [secrets_only(), ssh_agents_only()] {
        let refusal =
            refuse_buildkit_options_on_the_classic_builder(&builder, Some(&options), "demo-app")
                .unwrap_err()
                .to_string();
        assert!(
            refusal.contains("requires BuildKit") && refusal.contains("'demo-app'"),
            "{refusal}"
        );
    }
}

/// Batect's rule U72: `DOCKER_BUILDKIT=0` forces the classic builder even
/// on a daemon advertising BuildKit, and `build_ssh` is refused there too.
#[test]
fn a_forced_classic_builder_refuses_build_ssh() {
    let builder = select_builder_version(Some("0"), Some("2")).unwrap();
    assert!(refuse_buildkit_options_on_the_classic_builder(
        &builder,
        Some(&ssh_agents_only()),
        "t"
    )
    .is_err());
}

#[test]
fn a_build_is_refused_only_for_buildkit_fields_on_the_classic_builder() {
    let classic = select_builder_version(None, None).unwrap();
    let buildkit = select_builder_version(None, Some("2")).unwrap();
    assert!(refuse_buildkit_options_on_the_classic_builder(&classic, None, "t").is_ok());
    assert!(
        refuse_buildkit_options_on_the_classic_builder(&buildkit, Some(&secrets_only()), "t")
            .is_ok()
    );
}

#[test]
fn builder_selection_treats_an_empty_header_as_classic() {
    use bollard::query_parameters::BuilderVersion;
    assert_eq!(
        select_builder_version(None, Some("")).unwrap(),
        BuilderVersion::BuilderV1
    );
}

/// docker-client's `BuilderVersion.fromAPI` throws on anything but `""`,
/// `"1"` or `"2"`; guessing the classic builder would hide a daemon this
/// code doesn't understand.
#[test]
fn builder_selection_rejects_an_unknown_daemon_advertised_builder() {
    let error = select_builder_version(None, Some("3"))
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "The Docker daemon's Builder-Version ping header is '3', which is not a builder \
         Ratect knows — set the DOCKER_BUILDKIT environment variable to '1' to use BuildKit or \
         '0' to use the classic builder."
    );
}

#[test]
fn an_explicit_builder_choice_ignores_an_unknown_daemon_advertised_builder() {
    use bollard::query_parameters::BuilderVersion;
    assert_eq!(
        select_builder_version(Some("1"), Some("3")).unwrap(),
        BuilderVersion::BuilderBuildKit
    );
    assert_eq!(
        select_builder_version(Some("0"), Some("3")).unwrap(),
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
    for value in ["banana", "Banana", "BANANA"] {
        let err = select_builder_version(Some(value), Some("2")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains(&format!("'{value}'")), "{message}");
        assert!(message.contains("yes"), "{message}");
        assert!(
            err.is::<crate::exit_code::InvalidOptionValue>(),
            "{message}"
        );
    }
}

/// #447: Batect's whole vocabulary, in any case, chooses the builder
/// whatever the daemon advertises.
#[test]
fn docker_buildkit_accepts_batects_vocabulary_in_any_case() {
    use bollard::query_parameters::BuilderVersion;
    for value in ["1", "yes", "YES", "true", "TRUE", "True", "Yes"] {
        for advertised in [None, Some("1"), Some("2")] {
            assert_eq!(
                select_builder_version(Some(value), advertised).unwrap(),
                BuilderVersion::BuilderBuildKit,
                "{value} {advertised:?}"
            );
        }
    }
    for value in ["0", "no", "NO", "false", "FALSE", "False", "No"] {
        for advertised in [None, Some("1"), Some("2")] {
            assert_eq!(
                select_builder_version(Some(value), advertised).unwrap(),
                BuilderVersion::BuilderV1,
                "{value} {advertised:?}"
            );
        }
    }
}

/// #447: an empty `DOCKER_BUILDKIT` is no choice at all — the daemon's
/// advertised builder is used, as with none.
#[test]
fn an_empty_docker_buildkit_defers_to_the_daemon() {
    use bollard::query_parameters::BuilderVersion;
    assert_eq!(
        select_builder_version(Some(""), Some("2")).unwrap(),
        BuilderVersion::BuilderBuildKit
    );
    assert_eq!(
        select_builder_version(Some(""), None).unwrap(),
        BuilderVersion::BuilderV1
    );
}

/// `--enable-buildkit` wins over any value, an invalid or empty one too.
#[test]
fn enable_buildkit_overrides_an_invalid_or_empty_docker_buildkit() {
    use bollard::query_parameters::BuilderVersion;
    for value in ["banana", ""] {
        let env_value = docker_buildkit_env_value(true, Some(value));
        assert_eq!(env_value.as_deref(), Some("1"));
        assert_eq!(
            select_builder_version(env_value.as_deref(), None).unwrap(),
            BuilderVersion::BuilderBuildKit
        );
    }
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

/// A tagless reference pulls only `latest`, as the docker CLI and Batect's
/// docker-client do — the API alone would pull every tag (ratect#536).
#[test]
fn pull_request_reference_defaults_a_tagless_reference_to_latest() {
    assert_eq!(pull_request_reference("alpine"), ("alpine", Some("latest")));
    assert_eq!(
        pull_request_reference("team/tool"),
        ("team/tool", Some("latest"))
    );
    assert_eq!(
        pull_request_reference("localhost:5000/team/tool"),
        ("localhost:5000/team/tool", Some("latest"))
    );
}

#[test]
fn pull_request_reference_keeps_a_given_tag() {
    assert_eq!(
        pull_request_reference("alpine:3.20"),
        ("alpine", Some("3.20"))
    );
    assert_eq!(
        pull_request_reference("localhost:5000/team/tool:v2"),
        ("localhost:5000/team/tool", Some("v2"))
    );
}

#[test]
fn pull_request_reference_sends_a_digest_reference_unchanged() {
    let digest = format!("sha256:{}", "a".repeat(64));
    for reference in [format!("alpine@{digest}"), format!("alpine:3.20@{digest}")] {
        assert_eq!(
            pull_request_reference(&reference),
            (reference.as_str(), None)
        );
    }
}

/// The pulled image is found by the digest the pull reported, not by a tag
/// that may have moved since (ratect#301) — any tag or earlier digest in
/// the configured reference gives way to it.
#[test]
fn digest_pinned_reference_names_the_repository_at_the_pulled_digest() {
    let digest = format!("sha256:{}", "b".repeat(64));
    let older = format!("sha256:{}", "a".repeat(64));
    for (reference, pinned) in [
        ("alpine".to_string(), format!("alpine@{digest}")),
        ("alpine:3.20".to_string(), format!("alpine@{digest}")),
        (
            "localhost:5000/team/tool:v2".to_string(),
            format!("localhost:5000/team/tool@{digest}"),
        ),
        (format!("alpine@{older}"), format!("alpine@{digest}")),
        (format!("alpine:3.20@{older}"), format!("alpine@{digest}")),
    ] {
        assert_eq!(digest_pinned_reference(&reference, &digest), pinned);
    }
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
fn check_api_version_floor_accepts_a_version_above_the_floor() {
    check_api_version_floor(ClientVersion {
        major_version: 1,
        minor_version: 53,
    })
    .expect("a daemon well above the floor should pass");
}

#[test]
fn check_api_version_floor_accepts_the_floor_exactly() {
    check_api_version_floor(MINIMUM_API_VERSION).expect("the floor itself should pass");
}

#[test]
fn check_api_version_floor_rejects_a_version_below_the_floor() {
    let err = check_api_version_floor(ClientVersion {
        major_version: 1,
        minor_version: 40,
    })
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Ratect requires Docker 20.10 or newer, but the daemon only supports API version 1.40 \
         (Ratect requires at least 1.41) — upgrade Docker to use Ratect."
    );
}

#[test]
fn check_daemon_os_accepts_linux_in_any_case_and_a_missing_or_empty_one() {
    for os in [Some("linux"), Some("Linux"), Some(""), None] {
        check_daemon_os(os).unwrap_or_else(|err| panic!("{os:?}: {err:#}"));
    }
}

#[test]
fn check_daemon_os_refuses_windows_in_any_case_naming_what_was_reported() {
    for os in ["windows", "Windows"] {
        let err = check_daemon_os(Some(os)).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "The Docker daemon is running Windows containers (it reports its operating \
                 system as '{os}'), but Ratect needs one running Linux containers — switch \
                 Docker to Linux containers to use Ratect."
            )
        );
    }
}

#[tokio::test]
async fn docker_client_new_refuses_a_daemon_running_windows_containers() {
    let options = stub_daemon(StubDaemon {
        version_body: serde_json::json!({ "ApiVersion": "1.41", "Version": "27.3.1", "Os": "windows" }),
        ..Default::default()
    })
    .await;
    let Err(err) = DockerClient::new(&options, |_| {}).await else {
        panic!("a Windows-containers daemon should be refused");
    };
    assert!(
        err.to_string()
            .starts_with("The Docker daemon is running Windows containers"),
        "{err:#}"
    );
}

/// A daemon that stops answering between `DockerClient::new`'s two
/// `/version` requests is still reported as unreachable, naming the host
/// tried, not as a failure to read its operating system.
#[tokio::test]
async fn docker_client_new_reports_a_daemon_gone_before_its_second_request_as_unreachable() {
    let options = stub_daemon(StubDaemon {
        version_body: stub_version_body(Some("27.3.1")),
        connections: 1,
        ..Default::default()
    })
    .await;
    let Err(err) = DockerClient::new(&options, |_| {}).await else {
        panic!("the second request should find nothing listening");
    };
    let host = options.host.unwrap();
    assert!(
        err.to_string()
            .starts_with(&format!("Couldn't reach the Docker daemon at '{host}'")),
        "{err:#}"
    );
    assert!(
        !format!("{err:#}").contains("Failed to read the Docker daemon's operating system"),
        "{err:#}"
    );
}

/// Batect's `DockerClientConfigurationFactorySpec`: the Docker config
/// directory connecting chose is the one the client reads `config.json`
/// from, for a build's proxies and the registries it authenticates to.
#[tokio::test]
async fn docker_client_new_reads_config_json_from_the_chosen_config_directory() {
    let config_directory = unique_temp_dir();
    let options = DockerConnectionOptions {
        config_directory: Some(config_directory.clone()),
        ..stub_daemon(StubDaemon {
            version_body: stub_version_body(Some("27.3.1")),
            ..Default::default()
        })
        .await
    };

    let client = DockerClient::new(&options, |_| {}).await.unwrap();

    assert_eq!(
        client.docker_config_path,
        config_directory.join("config.json")
    );
    std::fs::remove_dir_all(&config_directory).unwrap();
}

/// A [`StubDaemon`]'s `/version` body, reporting `version` as the daemon's
/// own version (none when `None`) and an API version at Ratect's floor.
fn stub_version_body(version: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({ "ApiVersion": "1.41" });
    if let Some(version) = version {
        body["Version"] = version.into();
    }
    body
}

/// The options connecting to `daemon`, answering only the
/// daemon-information requests it answers itself: anything else is a 404.
async fn stub_daemon(daemon: StubDaemon) -> DockerConnectionOptions {
    daemon.serve(|_, _| (404, String::new())).await.0
}

/// A `DockerClient` connected to a [`StubDaemon`] serving `version_body`
/// and `builder_version`.
async fn client_for_stub_daemon(
    version_body: serde_json::Value,
    builder_version: Option<&'static str>,
) -> DockerClient {
    let options = stub_daemon(StubDaemon {
        version_body,
        builder_version,
        ..Default::default()
    })
    .await;
    DockerClient::new(&options, |_| {})
        .await
        .expect("the stub daemon should be connectable")
}

/// Batect's `DockerClientDaemonInformationSpec`, "be able to get daemon
/// information": the version is the daemon's own string, `+suffix` build
/// tag and all, never parsed or reformatted.
#[tokio::test]
async fn server_version_passes_a_build_tagged_version_through_unchanged() {
    let client = client_for_stub_daemon(stub_version_body(Some("27.3.1+dfsg-1")), None).await;
    assert_eq!(client.server_version().await.unwrap(), "27.3.1+dfsg-1");
}

/// Batect's `DockerClientDaemonInformationSpec`, "be able to get daemon
/// information": the legacy `master-dockerproject-YYYY-MM-DD` form, which
/// isn't a version number at all, passes through the same way.
#[tokio::test]
async fn server_version_passes_a_legacy_dockerproject_version_through_unchanged() {
    let client = client_for_stub_daemon(
        stub_version_body(Some("master-dockerproject-2016-01-01")),
        None,
    )
    .await;
    assert_eq!(
        client.server_version().await.unwrap(),
        "master-dockerproject-2016-01-01"
    );
}

/// Ratect's own fallback, with no Batect rule to mirror: Batect's client
/// library reports whatever the daemon sent, and has no `doctor` to word it.
#[tokio::test]
async fn server_version_is_unknown_when_the_daemon_reports_none() {
    let client = client_for_stub_daemon(stub_version_body(None), None).await;
    assert_eq!(client.server_version().await.unwrap(), "unknown version");
}

/// Batect's `DockerClientDaemonInformationSpec`, "be able to ping the
/// daemon": the ping's `Builder-Version` header is what picks the builder
/// when neither `DOCKER_BUILDKIT` nor `--enable-buildkit` does.
#[tokio::test]
async fn builder_version_is_buildkit_when_the_daemon_ping_advertises_it() {
    let client = client_for_stub_daemon(stub_version_body(Some("27.3.1")), Some("2")).await;
    assert_eq!(
        client.builder_version_given(None).await.unwrap(),
        bollard::query_parameters::BuilderVersion::BuilderBuildKit
    );
}

/// Batect's `DockerClientDaemonInformationSpec`, "be able to ping the
/// daemon", for a ping advertising the classic builder or none at all.
#[tokio::test]
async fn builder_version_is_classic_when_the_daemon_ping_advertises_it_or_nothing() {
    for advertised in [Some("1"), None] {
        let client = client_for_stub_daemon(stub_version_body(Some("27.3.1")), advertised).await;
        assert_eq!(
            client.builder_version_given(None).await.unwrap(),
            bollard::query_parameters::BuilderVersion::BuilderV1,
            "advertised: {advertised:?}"
        );
    }
}

/// One request a [`StubDaemon`] received: its method, its path with the
/// `/vX.Y` API-version prefix stripped (so `/containers/abc/stop`, not
/// `/v1.41/containers/abc/stop`), its raw query string, empty when it
/// had none, and its body, read in full and empty when it had none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedRequest {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

impl RecordedRequest {
    /// The query's `key=value` pairs, in the order sent.
    fn query_pairs(&self) -> Vec<(String, String)> {
        self.query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => (key.to_string(), value.to_string()),
                None => (pair.to_string(), String::new()),
            })
            .collect()
    }

    /// The value of the query's `key`, if it was sent.
    fn query_value(&self, key: &str) -> Option<String> {
        self.query_pairs()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value)
    }
}

/// A scripted, recording stand-in Docker daemon on a local TCP port, for
/// testing `DockerClient` at the HTTP level: a real `DockerClient`, and
/// real bollard, talk to it in plain HTTP, so a test sees exactly the
/// requests the client sends.
///
/// What it emulates:
/// - the two daemon-information endpoints, answered by the daemon itself so
///   `DockerClient::new` connects: `/version` with `version_body`, and
///   `/_ping` with `builder_version` as its `Builder-Version` header when
///   there is one;
/// - every other request, answered by the test's route from its method and
///   path: an HTTP status and a JSON body. An error goes back the way
///   Docker sends one — a status and `{"message": "..."}` — which bollard
///   surfaces as `DockerResponseServerError`. Every route here answers
///   anything it doesn't script with a 404, which is still recorded, so a
///   test can assert nothing else was asked for;
/// - recording: each request's method, version-stripped path, query and
///   body, in arrival order, `/version` and `/_ping` included. Bodies are
///   read in full, sent with `Content-Length` or chunked. A request it
///   can't read — malformed, or a bug in this harness — is recorded with
///   the method `UNREADABLE` and the reason as its path, so a test that
///   asserts what was sent fails naming it, rather than the client seeing
///   only a dropped connection;
///
/// What it doesn't: state (a route sees one request at a time and keeps
/// its own counters if it needs them), keep-alive (one request per
/// connection, then `Connection: close`), a held-open stream (each response
/// is written whole, so an event or progress stream ends where its body
/// does), or a protocol upgrade (an attach's `101` is answered, then the
/// connection is closed — a container that printed nothing).
struct StubDaemon {
    version_body: serde_json::Value,
    builder_version: Option<&'static str>,
    /// How many connections it accepts, at least one, before it stops
    /// listening, so the next one is refused. It stops before answering the
    /// last, and holds
    /// the port lock for writing from then until the test's runtime ends, so
    /// nothing else can be listening there when the next one is made
    /// (ratect#728, see `test_support::stop_listening`).
    connections: usize,
}

impl Default for StubDaemon {
    /// A Linux daemon at Ratect's API floor, advertising no builder,
    /// answering any number of requests.
    fn default() -> Self {
        Self {
            version_body: serde_json::json!({ "ApiVersion": "1.41", "Os": "linux" }),
            builder_version: None,
            connections: usize::MAX,
        }
    }
}

/// What a [`StubDaemon`] hands each request that isn't `/version` or
/// `/_ping`: its method and version-stripped path in, a status and a JSON
/// body out.
type StubRoute = dyn Fn(&str, &str) -> (u16, String) + Send + Sync;

/// A [`StubDaemon`]'s shared request log.
type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<RecordedRequest>>>;

impl StubDaemon {
    /// Starts listening, answering with `route`, until the test's runtime
    /// ends or it has taken its `connections`; returns the options
    /// connecting to it and its request log.
    async fn serve(
        self,
        route: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
    ) -> (DockerConnectionOptions, RequestLog) {
        let (listener, port) = super::test_support::bind_ephemeral_port().await;
        let route: std::sync::Arc<StubRoute> = std::sync::Arc::new(route);
        let log = RequestLog::default();
        let recorded = log.clone();
        let daemon = std::sync::Arc::new(self);
        tokio::spawn(async move {
            for _ in 1..daemon.connections {
                let (stream, _) = listener.accept().await.unwrap();
                let (daemon, route, recorded) = (daemon.clone(), route.clone(), recorded.clone());
                tokio::spawn(async move { daemon.answer(stream, &*route, &recorded).await });
            }
            let (stream, _) = listener.accept().await.unwrap();
            let _ports = super::test_support::stop_listening(listener).await;
            daemon.answer(stream, &*route, &recorded).await;
            // Keeps `_ports` held until the runtime drops this task.
            std::future::pending::<()>().await;
        });
        (
            DockerConnectionOptions {
                host: Some(format!("tcp://127.0.0.1:{port}")),
                ..Default::default()
            },
            log,
        )
    }

    /// Reads, records and answers the one request `stream` carries.
    async fn answer(
        &self,
        mut stream: tokio::net::TcpStream,
        route: &StubRoute,
        recorded: &std::sync::Mutex<Vec<RecordedRequest>>,
    ) {
        use futures::FutureExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let read = std::panic::AssertUnwindSafe(read_stub_request(&mut stream))
            .catch_unwind()
            .await;
        let request = match read {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(panic) => {
                let reason = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                recorded.lock().unwrap().push(RecordedRequest {
                    method: "UNREADABLE".to_string(),
                    path: reason,
                    query: String::new(),
                    body: Vec::new(),
                });
                return;
            }
        };
        recorded.lock().unwrap().push(request.clone());
        let (status, headers, body) = match request.path.as_str() {
            "/version" => (
                200,
                "Content-Type: application/json\r\n".to_string(),
                self.version_body.to_string(),
            ),
            "/_ping" => (
                200,
                self.builder_version
                    .map(|version| format!("Builder-Version: {version}\r\n"))
                    .unwrap_or_default(),
                "OK".to_string(),
            ),
            path => {
                let (status, body) = route(&request.method, path);
                (
                    status,
                    "Content-Type: application/json\r\n".to_string(),
                    body,
                )
            }
        };
        let body = if request.method == "HEAD" { "" } else { &body };
        let response = format!(
            "HTTP/1.1 {status} Stub\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n\
             {body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        // Read whatever the client still sends — stdin, down an attach —
        // until it hangs up: closing a socket with unread input resets the
        // connection, and the client would see that reset instead of the
        // response it has already been sent.
        let mut buf = [0u8; 4096];
        while matches!(stream.read(&mut buf).await, Ok(read) if read > 0) {}
    }
}

/// Reads one HTTP request from `stream` — its body in full, whether sent
/// with `Content-Length` or chunked — or `None` if the connection closed
/// before its headers ended.
async fn read_stub_request(stream: &mut tokio::net::TcpStream) -> Option<RecordedRequest> {
    let mut data = Vec::new();
    let headers_end = loop {
        if let Some(end) = find_bytes(&data, b"\r\n\r\n") {
            break end + 4;
        }
        if !read_more(stream, &mut data).await {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&data[..headers_end]).into_owned();
    let header = |wanted: &str| {
        head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(wanted)
                .then(|| value.trim().to_string())
        })
    };
    let body = if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
        read_chunked_body(stream, &mut data, headers_end).await
    } else {
        let content_length = header("content-length").map_or(0, |value| {
            value.parse::<usize>().expect("a numeric Content-Length")
        });
        fill_to(stream, &mut data, headers_end + content_length).await;
        data[headers_end..headers_end + content_length].to_vec()
    };
    let mut request_line = head.split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let target = request_line.next().unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = match path.strip_prefix("/v") {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => {
            rest.find('/').map_or(path, |slash| &rest[slash..])
        }
        _ => path,
    };
    Some(RecordedRequest {
        method,
        path: path.to_string(),
        query: query.to_string(),
        body,
    })
}

/// A chunked body starting at `data[start..]`, decoded, reading from
/// `stream` as needed through its terminating zero-length chunk and the
/// (ignored) trailers after it.
async fn read_chunked_body(
    stream: &mut tokio::net::TcpStream,
    data: &mut Vec<u8>,
    start: usize,
) -> Vec<u8> {
    let mut body = Vec::new();
    let mut position = start;
    loop {
        let line_end = read_line_end(stream, data, position).await;
        let size_line = std::str::from_utf8(&data[position..line_end]).unwrap();
        let size_field = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_field, 16).expect("a hexadecimal chunk size");
        position = line_end + 2;
        if size == 0 {
            break;
        }
        fill_to(stream, data, position + size + 2).await;
        body.extend_from_slice(&data[position..position + size]);
        position += size + 2;
    }
    // Trailers, if any, up to the blank line that ends the request.
    loop {
        let line_end = read_line_end(stream, data, position).await;
        if line_end == position {
            return body;
        }
        position = line_end + 2;
    }
}

/// The index of the first `\r\n` at or after `from`, reading from `stream`
/// until there is one.
async fn read_line_end(
    stream: &mut tokio::net::TcpStream,
    data: &mut Vec<u8>,
    from: usize,
) -> usize {
    loop {
        if let Some(end) = find_bytes(&data[from..], b"\r\n") {
            return from + end;
        }
        assert!(
            read_more(stream, data).await,
            "the request body ended mid-chunk"
        );
    }
}

/// Reads from `stream` until `data` holds at least `length` bytes.
async fn fill_to(stream: &mut tokio::net::TcpStream, data: &mut Vec<u8>, length: usize) {
    while data.len() < length {
        assert!(
            read_more(stream, data).await,
            "the request body ended before its declared length"
        );
    }
}

/// Appends what `stream` has next to `data`; `false` once it has closed.
async fn read_more(stream: &mut tokio::net::TcpStream, data: &mut Vec<u8>) -> bool {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    let read = stream.read(&mut buf).await.unwrap();
    data.extend_from_slice(&buf[..read]);
    read > 0
}

/// Where `needle` first occurs in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The harness's own contract, which no `DockerClient` call exercises:
/// bollard sends its bodies with a `Content-Length`, so a chunked one is
/// only ever sent here, by hand.
#[tokio::test]
async fn the_stub_daemon_records_a_chunked_request_body_in_full() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (options, log) = recording_daemon(|_, _| (204, String::new())).await;
    let address = options.host.unwrap().replace("tcp://", "");
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            b"POST /v1.41/containers/abc/archive?path=%2Fetc HTTP/1.1\r\n\
              Host: stub\r\nTransfer-Encoding: chunked\r\n\r\n\
              5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\nX-Trailer: yes\r\n\r\n",
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    assert_eq!(
        log.lock().unwrap().clone(),
        vec![RecordedRequest {
            method: "POST".to_string(),
            path: "/containers/abc/archive".to_string(),
            query: "path=%2Fetc".to_string(),
            body: b"hello, world".to_vec(),
        }]
    );
}

/// The harness's own contract: a request it can't read is recorded as
/// `UNREADABLE`, naming why, instead of vanishing with the connection.
#[tokio::test]
async fn the_stub_daemon_records_a_request_it_cannot_read_naming_why() {
    use tokio::io::AsyncWriteExt;
    let (options, log) = recording_daemon(|_, _| (204, String::new())).await;
    let address = options.host.unwrap().replace("tcp://", "");
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            b"POST /v1.41/containers/abc/archive HTTP/1.1\r\n\
              Host: stub\r\nTransfer-Encoding: chunked\r\n\r\nnot-hex\r\n",
        )
        .await
        .unwrap();
    stream.shutdown().await.unwrap();

    let recorded = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(request) = log.lock().unwrap().first().cloned() {
                return request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the unreadable request should be recorded");

    assert_eq!(recorded.method, "UNREADABLE");
    assert!(
        recorded.path.contains("a hexadecimal chunk size"),
        "{recorded:?}"
    );
}

/// A [`StubDaemon`] at its defaults, answering with `route`.
async fn recording_daemon(
    route: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
) -> (DockerConnectionOptions, RequestLog) {
    StubDaemon::default().serve(route).await
}

/// A `DockerClient` connected to a [`recording_daemon`] answering with
/// `route`, and that daemon's request log with `DockerClient::new`'s own
/// requests already cleared — so the log holds only what the test did.
async fn client_for_recording_daemon(
    route: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
) -> (
    DockerClient,
    std::sync::Arc<std::sync::Mutex<Vec<RecordedRequest>>>,
) {
    let (options, log) = recording_daemon(route).await;
    let client = DockerClient::new(&options, |_| {})
        .await
        .expect("the recording daemon should be connectable");
    log.lock().unwrap().clear();
    (client, log)
}

/// A [`recording_daemon`] route for container cleanup: the stop of a
/// container answers with `stop_status` (`stop_status_for` picking it per
/// id), its forced delete with 204, and anything else with a 404.
fn cleanup_route(
    stop_status_for: impl Fn(&str) -> u16 + Send + Sync + 'static,
) -> impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static {
    move |method, path| {
        let id = path
            .strip_prefix("/containers/")
            .map(|rest| rest.split('/').next().unwrap_or_default());
        match (method, id) {
            ("POST", Some(id)) if path.ends_with("/stop") => {
                let status = stop_status_for(id);
                let body = match status {
                    204 | 304 => String::new(),
                    _ => r#"{"message":"stub daemon refused the stop"}"#.to_string(),
                };
                (status, body)
            }
            ("DELETE", Some(_)) => (204, String::new()),
            _ => (404, r#"{"message":"not found"}"#.to_string()),
        }
    }
}

/// The `(method, path)` of every request in `log`, in the order received.
fn requests_sent(log: &std::sync::Mutex<Vec<RecordedRequest>>) -> Vec<(String, String)> {
    method_and_path(&log.lock().unwrap())
}

/// The `(method, path)` of each of `requests`, in order.
fn method_and_path(requests: &[RecordedRequest]) -> Vec<(String, String)> {
    requests
        .iter()
        .map(|request| (request.method.clone(), request.path.clone()))
        .collect()
}

fn stop_of(id: &str) -> (String, String) {
    ("POST".to_string(), format!("/containers/{id}/stop"))
}

fn delete_of(id: &str) -> (String, String) {
    ("DELETE".to_string(), format!("/containers/{id}"))
}

/// `create_container`'s one request for a `db` dependency on `network`,
/// declaring `db.local` as an additional hostname: the `name` it was
/// created under, and its body.
async fn create_request(network: ContainerNetwork) -> (Option<String>, serde_json::Value) {
    let (client, log) = client_for_recording_daemon(|method, path| match (method, path) {
        ("POST", "/containers/create") => (201, r#"{"Id":"new-id","Warnings":[]}"#.to_string()),
        _ => (404, r#"{"message":"not found"}"#.to_string()),
    })
    .await;
    let container = crate::config::Container {
        additional_hostnames: Some(vec!["db.local".to_string()]),
        ..Default::default()
    };
    let run_labels = crate::labels::RunLabels::new("demo", "task", "run-id", None);
    let spec = crate::container_spec::derive_spec(crate::container_spec::ContainerSpecInputs {
        name: "db",
        docker_name: "demo-db-abc123",
        container_config: &container,
        overlay: crate::container_spec::Overlay::Customise(None),
        image: "postgres:16",
        network,
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: crate::labels::ContainerRole::Dependency,
        run_labels: &run_labels,
    });

    assert_eq!(client.create_container(&spec).await.unwrap(), "new-id");

    let requests = log.lock().unwrap().clone();
    let [create] = &requests[..] else {
        panic!("one request, the create: {requests:?}");
    };
    (
        create.query_value("name"),
        serde_json::from_slice(&create.body).unwrap(),
    )
}

/// What `create_container` asks the daemon for: Docker's own name for the
/// container, and — on the task's network — that network as its mode with
/// its aliases on the creation endpoint, so it is created there and never
/// also on the default `bridge` (ratect#303). The builders each have their
/// own tests; this proves the request carries what they build.
#[tokio::test]
async fn create_container_names_the_container_and_creates_it_on_the_task_network() {
    let (name, body) =
        create_request(ContainerNetwork::Task("demo-network-abc123".to_string())).await;

    assert_eq!(name.as_deref(), Some("demo-db-abc123"));
    assert_eq!(body["HostConfig"]["NetworkMode"], "demo-network-abc123");
    assert_eq!(
        body["NetworkingConfig"]["EndpointsConfig"],
        serde_json::json!({"demo-network-abc123": {"Aliases": ["db", "db.local"]}}),
    );
}

/// Off the task's network there is no endpoint to configure, so the request
/// carries none — aliases there would attach the container to the task
/// network as well, or be refused.
#[tokio::test]
async fn create_container_off_the_task_network_configures_no_endpoint() {
    for (network, mode) in [
        (ContainerNetwork::Host, "host"),
        (ContainerNetwork::None, "none"),
        (
            ContainerNetwork::Container("peer-id".to_string()),
            "container:peer-id",
        ),
    ] {
        let (name, body) = create_request(network).await;

        assert_eq!(name.as_deref(), Some("demo-db-abc123"), "{mode}");
        assert_eq!(body["HostConfig"]["NetworkMode"], mode);
        assert!(body["NetworkingConfig"].is_null(), "{mode}: {body}");
    }
}

/// Batect's RMC-1/RMC-3 (ratect#310): a started container is stopped, and
/// only then removed — one stop, one delete, same id, nothing before the
/// stop.
#[tokio::test]
async fn stop_and_remove_container_stops_before_it_removes() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 204)).await;

    client
        .stop_and_remove_container("abc", None, None, None)
        .await
        .unwrap();

    assert_eq!(requests_sent(&log), vec![stop_of("abc"), delete_of("abc")]);
}

/// Batect's EST-4/CSP-9 (ratect#310): a stop the daemon fails means no
/// removal — what `docs/differences-from-batect.md`'s "A container whose
/// stop fails isn't removed" says.
#[tokio::test]
async fn stop_and_remove_container_does_not_remove_after_a_failed_stop() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 500)).await;

    let err = client
        .stop_and_remove_container("abc", None, None, None)
        .await
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("Failed to stop container 'abc'"),
        "{err:#}"
    );
    assert_eq!(requests_sent(&log), vec![stop_of("abc")]);
}

/// `stopped` is signaled once the stop succeeds — a 304 for a container
/// already stopped included, as Docker treats it — and never when it fails.
/// Whether the container was ever started is the engine's to know.
#[tokio::test]
async fn stop_and_remove_container_signals_stopped_only_after_a_successful_stop() {
    let (client, _) = client_for_recording_daemon(cleanup_route(|_| 304)).await;
    let (stopped, on_stopped) = tokio::sync::oneshot::channel();
    client
        .stop_and_remove_container("abc", None, None, Some(stopped))
        .await
        .unwrap();
    assert!(on_stopped.await.is_ok());

    let (client, _) = client_for_recording_daemon(cleanup_route(|_| 204)).await;
    let (stopped, on_stopped) = tokio::sync::oneshot::channel();
    client
        .stop_and_remove_container("abc", None, None, Some(stopped))
        .await
        .unwrap();
    assert!(on_stopped.await.is_ok());

    let (client, _) = client_for_recording_daemon(cleanup_route(|_| 500)).await;
    let (stopped, on_stopped) = tokio::sync::oneshot::channel();
    client
        .stop_and_remove_container("abc", None, None, Some(stopped))
        .await
        .unwrap_err();
    assert!(on_stopped.await.is_err());
}

/// Batect's U31 (ratect#310), now a recorded divergence (ratect#609): a
/// container already gone answers its stop with a 404, which counts as
/// stopped — `stopped` is signaled and the forced removal still follows,
/// which tolerates the same 404 — so the call succeeds. Nothing is left
/// behind, so it isn't a cleanup failure (decisions/0016).
#[tokio::test]
async fn stop_and_remove_container_treats_a_missing_container_as_stopped_and_removed() {
    let (client, log) = client_for_recording_daemon(|method, path| match (method, path) {
        ("POST", "/containers/abc/stop") | ("DELETE", "/containers/abc") => {
            (404, r#"{"message":"No such container: abc"}"#.to_string())
        }
        _ => (404, r#"{"message":"not found"}"#.to_string()),
    })
    .await;
    let (stopped, on_stopped) = tokio::sync::oneshot::channel();

    client
        .stop_and_remove_container("abc", None, None, Some(stopped))
        .await
        .unwrap();

    assert!(on_stopped.await.is_ok());
    assert_eq!(requests_sent(&log), vec![stop_of("abc"), delete_of("abc")]);
}

/// Batect's U32 and NRM-5/ECS-3 (ratect#310): a container created but
/// never started answers its stop with a 304, which isn't a failure — it
/// is still removed.
#[tokio::test]
async fn stop_and_remove_container_removes_a_never_started_container() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 304)).await;

    client
        .stop_and_remove_container("abc", None, None, None)
        .await
        .unwrap();

    assert_eq!(requests_sent(&log), vec![stop_of("abc"), delete_of("abc")]);
}

/// Batect's NRM-1 (ratect#310): the removal is forced and takes the
/// container's anonymous volumes with it.
#[tokio::test]
async fn stop_and_remove_container_removes_forcibly_with_volumes() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 204)).await;

    client
        .stop_and_remove_container("abc", None, None, None)
        .await
        .unwrap();

    let log = log.lock().unwrap();
    let delete = log
        .iter()
        .find(|request| request.method == "DELETE")
        .expect("a delete should have been sent");
    assert_eq!(delete.query_value("force").as_deref(), Some("true"));
    assert_eq!(delete.query_value("v").as_deref(), Some("true"));
}

/// Batect's NST-1 (ratect#310): with neither `stop_signal` nor
/// `stop_grace_period` set, the stop carries neither `signal` nor `t`, so
/// the daemon's own default applies.
#[tokio::test]
async fn stop_and_remove_container_sends_no_stop_options_by_default() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 204)).await;

    client
        .stop_and_remove_container("abc", None, None, None)
        .await
        .unwrap();

    let log = log.lock().unwrap();
    let stop = log
        .iter()
        .find(|request| request.method == "POST")
        .expect("a stop should have been sent");
    assert_eq!(stop.query_value("signal"), None, "{stop:?}");
    assert_eq!(stop.query_value("t"), None, "{stop:?}");
}

/// ratect#310: `stop_signal` passes through as the stop's `signal`, and
/// `stop_grace_period` becomes `t` rounded *up* to whole seconds.
#[tokio::test]
async fn stop_and_remove_container_sends_the_signal_and_a_rounded_up_grace_period() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 204)).await;

    client
        .stop_and_remove_container(
            "abc",
            Some("SIGINT"),
            Some(Duration::from_millis(500)),
            None,
        )
        .await
        .unwrap();

    let log = log.lock().unwrap();
    let stop = log
        .iter()
        .find(|request| request.method == "POST")
        .expect("a stop should have been sent");
    assert_eq!(stop.query_value("signal").as_deref(), Some("SIGINT"));
    assert_eq!(stop.query_value("t").as_deref(), Some("1"));
}

/// Batect's RMC-2 (ratect#310): a container's removal waits on its own stop
/// only — one container's failed stop neither removes it nor holds back
/// another's stop and removal.
#[tokio::test]
async fn stop_and_remove_container_gates_each_removal_on_its_own_stop() {
    let (client, log) =
        client_for_recording_daemon(cleanup_route(|id| if id == "first" { 500 } else { 204 }))
            .await;

    assert!(client
        .stop_and_remove_container("first", None, None, None)
        .await
        .is_err());
    client
        .stop_and_remove_container("second", None, None, None)
        .await
        .unwrap();

    assert_eq!(
        requests_sent(&log),
        vec![stop_of("first"), stop_of("second"), delete_of("second")]
    );
}

/// A [`RegistryCredentialResolver`] returning a fixed, pre-arranged answer
/// for every registry — no subprocess, no filesystem, no daemon — so
/// `resolve_pull_credential`'s own decision (what credential to pass,
/// whether to warn) is testable in isolation.
struct FakeCredentialResolver {
    result: std::sync::Mutex<Option<anyhow::Result<Option<bollard::auth::DockerCredentials>>>>,
}

impl FakeCredentialResolver {
    fn returning(result: anyhow::Result<Option<bollard::auth::DockerCredentials>>) -> Self {
        Self {
            result: std::sync::Mutex::new(Some(result)),
        }
    }
}

#[async_trait::async_trait]
impl crate::registry_auth::RegistryCredentialResolver for FakeCredentialResolver {
    async fn resolve(
        &self,
        _registry: &str,
    ) -> anyhow::Result<Option<bollard::auth::DockerCredentials>> {
        self.result
            .lock()
            .unwrap()
            .take()
            .expect("resolve() called more than once on this fake")
    }

    async fn list(&self, store: &str) -> anyhow::Result<Vec<String>> {
        panic!("list() called for credential store '{store}', which a pull never lists")
    }
}

#[tokio::test]
async fn resolve_pull_credential_passes_through_a_successful_resolution() {
    let credential = bollard::auth::DockerCredentials {
        username: Some("alice".to_string()),
        password: Some("hunter2".to_string()),
        ..Default::default()
    };
    let resolver = FakeCredentialResolver::returning(Ok(Some(credential.clone())));

    let (credentials, warning) =
        resolve_pull_credential(&resolver, "myregistry.example.com/foo").await;

    assert_eq!(credentials, Some(credential));
    assert!(
        warning.is_none(),
        "a successful resolution should never warn"
    );
}

#[tokio::test]
async fn resolve_pull_credential_is_silent_when_nothing_is_configured() {
    let resolver = FakeCredentialResolver::returning(Ok(None));

    let (credentials, warning) = resolve_pull_credential(&resolver, "nginx").await;

    assert_eq!(credentials, None);
    assert!(
        warning.is_none(),
        "no configured registries should mean no behaviour change and no warning"
    );
}

#[tokio::test]
async fn resolve_pull_credential_warns_and_proceeds_without_credentials_on_failure() {
    let resolver = FakeCredentialResolver::returning(Err(anyhow::anyhow!("helper exploded")));

    let (credentials, warning) =
        resolve_pull_credential(&resolver, "myregistry.example.com/foo").await;

    assert_eq!(
        credentials, None,
        "a resolution failure must not abort the pull — it proceeds with no credentials"
    );
    let warning = warning.expect("a resolution failure should produce exactly one warning");
    assert!(
        warning.contains("myregistry.example.com"),
        "the warning should name the registry that failed: {warning}"
    );
    assert!(
        warning.contains("helper exploded"),
        "the warning should include the underlying reason: {warning}"
    );
    assert_eq!(
        warning,
        "Could not resolve credentials for registry 'myregistry.example.com' (helper \
         exploded); proceeding without them.",
        "one sentence, the reason in parentheses"
    );
}

/// A [`RegistryCredentialResolver`] with a different, pre-arranged answer
/// per registry — `FakeCredentialResolver`'s single fixed answer isn't
/// enough for testing wiring that resolves more than one registry per call
/// (build's credential resolution). Each registry is expected to be
/// resolved at most once, matching production usage.
struct FakeMultiCredentialResolver {
    answers:
        std::sync::Mutex<HashMap<String, anyhow::Result<Option<bollard::auth::DockerCredentials>>>>,
}

impl FakeMultiCredentialResolver {
    fn new(
        answers: HashMap<String, anyhow::Result<Option<bollard::auth::DockerCredentials>>>,
    ) -> Self {
        Self {
            answers: std::sync::Mutex::new(answers),
        }
    }
}

#[async_trait::async_trait]
impl crate::registry_auth::RegistryCredentialResolver for FakeMultiCredentialResolver {
    async fn resolve(
        &self,
        registry: &str,
    ) -> anyhow::Result<Option<bollard::auth::DockerCredentials>> {
        self.answers
            .lock()
            .unwrap()
            .remove(registry)
            .unwrap_or_else(|| {
                panic!("resolve() called for unexpected or already-resolved registry '{registry}'")
            })
    }

    async fn list(&self, store: &str) -> anyhow::Result<Vec<String>> {
        panic!("list() called for credential store '{store}' on a fake that has none")
    }
}

#[tokio::test]
async fn resolve_build_credentials_resolves_every_registry() {
    let alice = bollard::auth::DockerCredentials {
        username: Some("alice".to_string()),
        password: Some("hunter2".to_string()),
        ..Default::default()
    };
    let bob = bollard::auth::DockerCredentials {
        username: Some("bob".to_string()),
        password: Some("swordfish".to_string()),
        ..Default::default()
    };
    let resolver = FakeMultiCredentialResolver::new(HashMap::from([
        (
            "registry-a.example.com".to_string(),
            Ok(Some(alice.clone())),
        ),
        ("registry-b.example.com".to_string(), Ok(Some(bob.clone()))),
    ]));
    let registries = vec![
        "registry-a.example.com".to_string(),
        "registry-b.example.com".to_string(),
    ];

    let (credentials, warnings) = resolve_build_credentials(&resolver, &registries).await;

    assert_eq!(
        credentials,
        HashMap::from([
            ("registry-a.example.com".to_string(), alice),
            ("registry-b.example.com".to_string(), bob),
        ])
    );
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn resolve_build_credentials_is_empty_with_no_registries_configured() {
    let resolver = FakeMultiCredentialResolver::new(HashMap::new());

    let (credentials, warnings) = resolve_build_credentials(&resolver, &[]).await;

    assert!(credentials.is_empty());
    assert!(
        warnings.is_empty(),
        "no configured registries should mean no behaviour change and no warning"
    );
}

#[tokio::test]
async fn resolve_build_credentials_skips_a_failed_registry_without_aborting_the_others() {
    let bob = bollard::auth::DockerCredentials {
        username: Some("bob".to_string()),
        password: Some("swordfish".to_string()),
        ..Default::default()
    };
    let resolver = FakeMultiCredentialResolver::new(HashMap::from([
        (
            "broken.example.com".to_string(),
            Err(anyhow::anyhow!("helper exploded")),
        ),
        ("registry-b.example.com".to_string(), Ok(Some(bob.clone()))),
        ("registry-c.example.com".to_string(), Ok(None)),
    ]));
    let registries = vec![
        "broken.example.com".to_string(),
        "registry-b.example.com".to_string(),
        "registry-c.example.com".to_string(),
    ];

    let (credentials, warnings) = resolve_build_credentials(&resolver, &registries).await;

    assert_eq!(
        credentials,
        HashMap::from([("registry-b.example.com".to_string(), bob)]),
        "the other registries must still resolve despite one failing"
    );
    assert_eq!(
        warnings.len(),
        1,
        "exactly one warning, for the one registry that failed: {warnings:?}"
    );
    assert!(
        warnings[0].contains("broken.example.com"),
        "the warning should name the registry that failed: {}",
        warnings[0]
    );
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

/// Piped stdin without a TTY still has to close the container's stdin when
/// it ends (ratect#220): Docker only does that for a `stdin_once` container,
/// so `stdin_once` follows stdin being attached at all, not the TTY — as
/// `docker run -i` and Batect's own docker-client both set it.
#[test]
fn stdin_flags_close_the_containers_stdin_once_piped_input_ends() {
    assert_eq!(
        stdin_flags(true, false),
        StdinFlags {
            tty: None,
            open_stdin: Some(true),
            attach_stdin: Some(true),
            stdin_once: Some(true),
        }
    );
}

#[test]
fn stdin_flags_allocate_a_tty_only_when_one_is_used() {
    assert_eq!(
        stdin_flags(true, true),
        StdinFlags {
            tty: Some(true),
            open_stdin: Some(true),
            attach_stdin: Some(true),
            stdin_once: Some(true),
        }
    );
}

/// A container that isn't a task's own (a dependency) gets no stdin at all,
/// leaving every flag at Docker's own default.
#[test]
fn stdin_flags_are_all_unset_when_not_interactive() {
    assert_eq!(
        stdin_flags(false, false),
        StdinFlags {
            tty: None,
            open_stdin: None,
            attach_stdin: None,
            stdin_once: None,
        }
    );
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

/// A trailing `/` names the same directory, as Batect's
/// `splitToPathSegments` trims it: a home or cache path's leaf is uploaded
/// unchanged, owned as any other.
#[test]
fn build_owned_directory_tar_ignores_a_trailing_slash() {
    for (path, leaf) in [
        ("/home/some-user/", "some-user/"),
        ("/caches/first-cache/", "first-cache/"),
    ] {
        let tar_bytes = build_owned_directory_tar(path, 123, 456).unwrap();
        let mut archive = tar::Archive::new(tar_bytes.as_slice());
        let mut entries = archive.entries().unwrap().map(|e| e.unwrap());
        let entry = entries.next().unwrap();

        assert_eq!(entry.path().unwrap().to_string_lossy(), leaf);
        assert_eq!(entry.header().entry_type(), tar::EntryType::Directory);
        assert_eq!(entry.header().uid().unwrap(), 123);
        assert_eq!(entry.header().gid().unwrap(), 456);
        assert_eq!(entry.header().mode().unwrap(), 0o755);
        assert!(entries.next().is_none());
    }
}

/// The parent a trailing-slash path's leaf is uploaded into, as Batect's
/// `splitToPathSegments` gives it.
#[test]
fn owned_directory_parent_ignores_a_trailing_slash() {
    assert_eq!(owned_directory_parent("/home/some-user/"), "/home");
    assert_eq!(owned_directory_parent("/caches/first-cache/"), "/caches");
}

/// A root host user is passed through as `0:0`, as in Batect's
/// `uploadDirectory`/`determineUserAndGroup`: its directories are owned by
/// `0:0`, still mode `0755`, and the container runs as `0:0`.
#[tokio::test]
async fn a_root_host_user_owns_its_directories_and_runs_the_container_as_root() {
    let tar_bytes = build_owned_directory_tar("/home/root", 0, 0).unwrap();
    let mut archive = tar::Archive::new(tar_bytes.as_slice());
    let entry = archive.entries().unwrap().next().unwrap().unwrap();
    assert_eq!(entry.header().uid().unwrap(), 0);
    assert_eq!(entry.header().gid().unwrap(), 0);
    assert_eq!(entry.header().mode().unwrap(), 0o755);

    let (client, _) = client_for_recording_daemon(|_, _| (404, String::new())).await;
    let mut spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Task,
    );
    let mut mapping = user_mapping_fixture();
    mapping.user.uid = 0;
    mapping.user.gid = 0;
    spec.shared.user_mapping = Some(mapping);
    let config = client.container_config(&spec).unwrap();
    assert_eq!(config.user.as_deref(), Some("0:0"));
}

/// A `local` (or directory-type `cache`) mount of `source` at `container`.
fn host_bind(source: &Path, container: &str) -> BindMount {
    BindMount {
        source: BindSource::Host(source.display().to_string()),
        container: container.to_string(),
        options: None,
    }
}

/// A named-volume `cache` mount at `container`.
fn volume_bind(name: &str, container: &str) -> BindMount {
    BindMount {
        source: BindSource::Volume(name.to_string()),
        container: container.to_string(),
        options: None,
    }
}

#[test]
fn ensure_host_volume_directories_exist_creates_a_missing_directory() {
    let dir = unique_temp_dir();
    let host_path = dir.join("missing");

    assert!(!host_path.exists());
    ensure_host_volume_directories_exist(&[host_bind(&host_path, "/code")], &[]).unwrap();
    assert!(host_path.is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_leaves_an_existing_directory_alone() {
    let dir = unique_temp_dir();

    ensure_host_volume_directories_exist(&[host_bind(&dir, "/code")], &[]).unwrap();
    assert!(dir.is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_does_nothing_when_there_are_no_volumes() {
    ensure_host_volume_directories_exist(&[], &[]).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_skips_a_named_docker_volume() {
    // A `cache` mount under `CacheType::Volume` resolves to a bare
    // volume name, not a host path — this must not be `mkdir -p`'d
    // relative to the current directory. Deliberately doesn't mutate the
    // process's real current directory to prove this (unsafe under `cargo
    // test`'s default parallel execution — other tests, e.g. in
    // config.rs, read the real current directory too); instead uses a
    // name specific enough that no other test could plausibly create a
    // same-named directory in the real current directory, and cleans it
    // up regardless in case a regression here ever did create it.
    let unlikely_relative_name =
        "ratect-test-ensure-host-volume-directories-exist-skips-a-named-docker-volume";
    let would_be_created_here = std::env::current_dir()
        .unwrap()
        .join(unlikely_relative_name);
    assert!(!would_be_created_here.exists());

    ensure_host_volume_directories_exist(
        &[volume_bind(unlikely_relative_name, "/root/.gradle")],
        &[],
    )
    .unwrap();

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

    ensure_host_volume_directories_exist(&[host_bind(&host_path, "/root/.gitconfig")], &[])
        .unwrap();

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
    let binds = [
        host_bind(
            Path::new("/run/host-services/ssh-auth.sock"),
            "/run/host-services/ssh-auth.sock",
        ),
        host_bind(Path::new("/run/guest-services/something"), "/something"),
    ];

    // The bug was this returning an error; that it's `Ok` is the fix.
    ensure_host_volume_directories_exist(&binds, &[]).unwrap();
}

/// A host path can contain `:` (an object-form `local`, or `.` under a
/// project directory like `my:proj`); the whole path is what's created,
/// never the text before its first `:` (ratect#452).
#[test]
fn ensure_host_volume_directories_exist_creates_a_host_path_containing_a_colon_whole() {
    let dir = unique_temp_dir();
    let host_path = dir.join("a:b");

    ensure_host_volume_directories_exist(&[host_bind(&host_path, "/code")], &[]).unwrap();

    assert!(host_path.is_dir());
    assert!(
        !dir.join("a").exists(),
        "the prefix before ':' must not be created"
    );

    fs::remove_dir_all(&dir).unwrap();
}

/// A host mount inside another host mount's container path has its mount
/// point created inside the outer host path, as the current user — not left
/// for the daemon to create as root in the user's project (ratect#321).
#[test]
fn ensure_host_volume_directories_exist_creates_a_nested_host_mount_point() {
    let dir = unique_temp_dir();
    let (outer, inner) = (dir.join("outer"), dir.join("inner"));

    ensure_host_volume_directories_exist(
        &[host_bind(&outer, "/code"), host_bind(&inner, "/code/path")],
        &[],
    )
    .unwrap();

    assert!(outer.is_dir());
    assert!(inner.is_dir());
    assert!(outer.join("path").is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_creates_a_nested_volume_mount_point() {
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            volume_bind("some-volume", "/code/path"),
        ],
        &[],
    )
    .unwrap();

    assert!(local.join("path").is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_creates_a_nested_tmpfs_mount_point() {
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[host_bind(&local, "/code")],
        &[("/code/path".to_string(), String::new())],
    )
    .unwrap();

    assert!(local.join("path").is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_creates_every_intermediate_directory_of_a_deep_mount_point()
{
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            volume_bind("some-volume", "/code/path/to/volume"),
        ],
        &[("/code/path/to/tmpfs".to_string(), String::new())],
    )
    .unwrap();

    assert!(local.join("path/to/volume").is_dir());
    assert!(local.join("path/to/tmpfs").is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

/// The mount point goes in the *closest* enclosing mount — the longest
/// container-path prefix — so a volume under two nested host mounts lands
/// in the inner one's host path, not the outer one's.
#[test]
fn ensure_host_volume_directories_exist_creates_a_mount_point_in_the_closest_host_mount() {
    let dir = unique_temp_dir();
    let (outer, inner) = (dir.join("outer"), dir.join("inner"));

    ensure_host_volume_directories_exist(
        &[
            host_bind(&outer, "/code"),
            host_bind(&inner, "/code/path"),
            volume_bind("some-volume", "/code/path/to/volume"),
        ],
        &[],
    )
    .unwrap();

    assert!(outer.join("path").is_dir());
    assert!(inner.join("to/volume").is_dir());
    assert!(!outer.join("path/to").exists());

    fs::remove_dir_all(&dir).unwrap();
}

/// Only a host mount gets anything created in it: a volume inside a
/// volume inside a host mount creates the outer volume's mount point and
/// nothing for the inner one, which is the outer volume's to hold.
#[test]
fn ensure_host_volume_directories_exist_creates_nothing_for_a_mount_whose_closest_parent_is_a_volume(
) {
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            volume_bind("volume-1", "/code/volume-1"),
            volume_bind("volume-2", "/code/volume-1/volume-2"),
        ],
        &[],
    )
    .unwrap();

    assert!(local.join("volume-1").is_dir());
    assert!(!local.join("volume-1/volume-2").exists());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_uses_a_host_mount_nested_in_a_volume_as_the_closest_parent()
{
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[
            volume_bind("volume-1", "/code"),
            host_bind(&local, "/code/path"),
            volume_bind("volume-2", "/code/path/volume-2"),
        ],
        &[],
    )
    .unwrap();

    assert_eq!(
        fs::read_dir(&local)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        vec![std::ffi::OsString::from("volume-2")]
    );

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ensure_host_volume_directories_exist_creates_nothing_inside_a_host_mount_with_no_nested_mounts()
{
    let dir = unique_temp_dir();
    let (inside_volume, beside_volume) = (dir.join("inside-volume"), dir.join("beside-volume"));

    ensure_host_volume_directories_exist(
        &[
            volume_bind("volume-1", "/volume"),
            host_bind(&inside_volume, "/volume/code"),
            host_bind(&beside_volume, "/code"),
        ],
        &[("/tmp".to_string(), String::new())],
    )
    .unwrap();

    assert_eq!(fs::read_dir(&inside_volume).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&beside_volume).unwrap().count(), 0);

    fs::remove_dir_all(&dir).unwrap();
}

/// Container paths are compared segment by segment, trailing `/` aside, so
/// `/code/` encloses `/code/path` and `/codebase` isn't inside `/code`.
#[test]
fn ensure_host_volume_directories_exist_compares_container_paths_by_segment() {
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code/"),
            volume_bind("volume-1", "/code/path/"),
            volume_bind("volume-2", "/codebase"),
        ],
        &[],
    )
    .unwrap();

    assert_eq!(
        fs::read_dir(&local)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        vec![std::ffi::OsString::from("path")]
    );

    fs::remove_dir_all(&dir).unwrap();
}

/// A file or socket bind-mounted inside a host mount — `./config.json` over
/// `/code/config.json` — gets an empty regular file at its mount point, with
/// the directories above it: Linux binds a non-directory onto a file, so a
/// directory there (Batect's) would fail the container at start.
#[test]
fn ensure_host_volume_directories_exist_creates_a_file_at_a_nested_file_or_socket_mount_point() {
    let mut scratch = ScratchPaths::new();
    let local = scratch.directory.join("local");
    let file = scratch.file("config.json");
    let socket_path = scratch.socket("agent.sock");

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            host_bind(&file, "/code/nested/config.json"),
            host_bind(&socket_path, "/code/agent.sock"),
        ],
        &[],
    )
    .unwrap();

    for mount_point in [local.join("nested/config.json"), local.join("agent.sock")] {
        let metadata = fs::symlink_metadata(&mount_point).unwrap();
        assert!(metadata.is_file(), "{mount_point:?} is not a regular file");
        assert_eq!(metadata.len(), 0);
    }

    fs::remove_dir_all(&scratch.directory).unwrap();
}

/// A Docker Desktop special path's type can't be known on the host — it
/// doesn't exist there — so nothing is created for one nested in a host
/// mount; the daemon provides it.
#[test]
fn ensure_host_volume_directories_exist_creates_nothing_for_a_nested_special_docker_desktop_path() {
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            host_bind(
                Path::new("/run/host-services/ssh-auth.sock"),
                "/code/ssh-auth.sock",
            ),
        ],
        &[],
    )
    .unwrap();

    assert_eq!(fs::read_dir(&local).unwrap().count(), 0);

    fs::remove_dir_all(&dir).unwrap();
}

/// An existing mount point is never touched, whatever its type and whatever
/// the mount's source — a type mismatch is left for Docker to report.
#[test]
fn ensure_host_volume_directories_exist_leaves_an_existing_mount_point_of_either_type_alone() {
    let dir = unique_temp_dir();
    let local = dir.join("local");
    fs::create_dir_all(local.join("a-directory")).unwrap();
    fs::write(local.join("a-directory/kept"), "kept").unwrap();
    fs::write(local.join("a-file"), "kept").unwrap();
    let file = dir.join("config.json");
    fs::write(&file, "{}").unwrap();

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            host_bind(&file, "/code/a-directory"),
            volume_bind("some-volume", "/code/a-file"),
        ],
        &[],
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(local.join("a-directory/kept")).unwrap(),
        "kept"
    );
    assert_eq!(fs::read_to_string(local.join("a-file")).unwrap(), "kept");

    fs::remove_dir_all(&dir).unwrap();
}

/// Three host mounts nested in one another: each mount point goes in the
/// one directly enclosing it, never further out.
#[test]
fn ensure_host_volume_directories_exist_uses_the_closest_of_three_nested_host_mounts() {
    let dir = unique_temp_dir();
    let (first, second, third) = (dir.join("first"), dir.join("second"), dir.join("third"));

    ensure_host_volume_directories_exist(
        &[
            host_bind(&first, "/code"),
            host_bind(&second, "/code/a"),
            host_bind(&third, "/code/a/b"),
            volume_bind("some-volume", "/code/a/b/c"),
        ],
        &[],
    )
    .unwrap();

    assert!(first.join("a").is_dir());
    assert!(second.join("b").is_dir());
    assert!(third.join("c").is_dir());
    assert!(!first.join("a/b").exists());
    assert!(!second.join("b/c").exists());

    fs::remove_dir_all(&dir).unwrap();
}

/// Container paths are compared as Docker cleans them, so a `..` can't
/// carry a mount point out of the host mount it appears to be under.
#[test]
fn ensure_host_volume_directories_exist_compares_container_paths_as_cleaned() {
    let dir = unique_temp_dir();
    let local = dir.join("local");

    ensure_host_volume_directories_exist(
        &[host_bind(&local, "/code")],
        &[
            ("/code/../scratch".to_string(), String::new()),
            ("/code/./a/../b".to_string(), String::new()),
        ],
    )
    .unwrap();

    assert!(!dir.join("scratch").exists());
    assert_eq!(
        fs::read_dir(&local)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        vec![std::ffi::OsString::from("b")]
    );

    fs::remove_dir_all(&dir).unwrap();
}

/// A dangling symlink at a mount point is something already there, left for
/// Docker like any other existing entry, not a missing path to create.
#[test]
fn ensure_host_volume_directories_exist_leaves_a_dangling_symlink_mount_point_alone() {
    let dir = unique_temp_dir();
    let local = dir.join("local");
    fs::create_dir_all(&local).unwrap();
    std::os::unix::fs::symlink(dir.join("nowhere"), local.join("tmp")).unwrap();

    ensure_host_volume_directories_exist(
        &[host_bind(&local, "/code")],
        &[("/code/tmp".to_string(), String::new())],
    )
    .unwrap();

    assert!(fs::symlink_metadata(local.join("tmp"))
        .unwrap()
        .file_type()
        .is_symlink());

    fs::remove_dir_all(&dir).unwrap();
}

/// A symlink leading out of a host mount's source, or nowhere, isn't
/// followed to a mount point: it's left for the daemon, which resolves it
/// inside the container, rather than created wherever the link points on the
/// host — possibly outside the project (ratect#703).
#[test]
fn ensure_host_volume_directories_exist_follows_no_symlink_out_of_the_host_mount() {
    let dir = unique_temp_dir();
    let (local, outside) = (dir.join("local"), dir.join("outside"));
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, local.join("build")).unwrap();
    std::os::unix::fs::symlink(dir.join("nowhere"), local.join("dangling")).unwrap();
    let file = dir.join("config.json");
    fs::write(&file, "{}").unwrap();

    ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            host_bind(&file, "/code/build/config.json"),
            volume_bind("some-volume", "/code/dangling/volume"),
        ],
        &[("/code/build/deep/scratch".to_string(), String::new())],
    )
    .unwrap();

    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert!(!dir.join("nowhere").exists());

    fs::remove_dir_all(&dir).unwrap();
}

/// A relative symlink that stays inside the host mount's source leads the
/// daemon to the same place it leads the host, so it is followed: the mount
/// point is created where the link leads, as the host user, rather than left
/// for the daemon to create as root.
#[test]
fn ensure_host_volume_directories_exist_follows_a_symlink_that_stays_inside_the_host_mount() {
    let dir = unique_temp_dir();
    let local = dir.join("local");
    fs::create_dir_all(local.join("target")).unwrap();
    std::os::unix::fs::symlink("target", local.join("build")).unwrap();

    ensure_host_volume_directories_exist(
        &[host_bind(&local, "/code")],
        &[("/code/build/scratch".to_string(), String::new())],
    )
    .unwrap();

    assert!(local.join("target/scratch").is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

/// What the host user can't see into is the daemon's to resolve, as root: it
/// fails no run here.
#[test]
fn ensure_host_volume_directories_exist_leaves_a_mount_point_it_cant_see_to_the_daemon() {
    use std::os::unix::fs::PermissionsExt;
    let dir = unique_temp_dir();
    let local = dir.join("local");
    let sealed = local.join("sealed");
    fs::create_dir_all(sealed.join("inside")).unwrap();
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
    // Root sees through any mode, so there is nothing to test as root.
    let sees_through = fs::read_dir(&sealed).is_ok();

    let result = ensure_host_volume_directories_exist(
        &[host_bind(&local, "/code")],
        &[("/code/sealed/inside/scratch".to_string(), String::new())],
    );

    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).unwrap();
    if !sees_through {
        result.unwrap();
        assert!(!sealed.join("inside/scratch").exists());
    }

    fs::remove_dir_all(&dir).unwrap();
}

/// A directory the host user can search but not write — a root-owned one a
/// run before ratect#321 left behind — is the daemon's to create in, as
/// root: it fails no run here.
#[test]
fn ensure_host_volume_directories_exist_leaves_a_mount_point_it_cant_create_to_the_daemon() {
    use std::os::unix::fs::PermissionsExt;
    let dir = unique_temp_dir();
    let local = dir.join("local");
    let read_only = local.join("build");
    fs::create_dir_all(&read_only).unwrap();
    fs::write(dir.join("config.json"), "{}").unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555)).unwrap();
    // Root writes through any mode, so there is nothing to test as root.
    let writes_through = fs::create_dir(read_only.join("probe")).is_ok();

    let result = ensure_host_volume_directories_exist(
        &[
            host_bind(&local, "/code"),
            host_bind(&dir.join("config.json"), "/code/build/config.json"),
        ],
        &[("/code/build/deep/scratch".to_string(), String::new())],
    );

    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o755)).unwrap();
    if !writes_through {
        result.unwrap();
        assert_eq!(fs::read_dir(&read_only).unwrap().count(), 0);
    }

    fs::remove_dir_all(&dir).unwrap();
}

/// An existing non-directory part-way to a mount point has nothing to create
/// inside, so the mount point is left for Docker to report rather than
/// failing the run here.
#[test]
fn ensure_host_volume_directories_exist_creates_nothing_past_an_existing_file() {
    let dir = unique_temp_dir();
    let local = dir.join("local");
    fs::create_dir_all(&local).unwrap();
    fs::write(local.join("tmp"), "a file").unwrap();

    ensure_host_volume_directories_exist(
        &[host_bind(&local, "/code")],
        &[("/code/tmp/scratch".to_string(), String::new())],
    )
    .unwrap();

    assert_eq!(fs::read_to_string(local.join("tmp")).unwrap(), "a file");

    fs::remove_dir_all(&dir).unwrap();
}

/// A host mount that isn't a directory — a file, a socket, or a Docker
/// Desktop special path that doesn't exist on the host — has nowhere to put
/// a mount point, so nothing is created for one nested under it.
#[test]
fn ensure_host_volume_directories_exist_creates_nothing_under_a_host_mount_that_isnt_a_directory() {
    let dir = unique_temp_dir();
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("config.json");
    fs::write(&file, "{}").unwrap();

    ensure_host_volume_directories_exist(
        &[
            host_bind(&file, "/config"),
            host_bind(Path::new("/run/host-services/ssh-auth.sock"), "/ssh"),
        ],
        &[
            ("/config/x".to_string(), String::new()),
            ("/ssh/x".to_string(), String::new()),
        ],
    )
    .unwrap();

    assert!(file.is_file());

    fs::remove_dir_all(&dir).unwrap();
}

/// A `cache` under `--cache-type=directory`, in a project directory whose
/// path contains `:`, is still only created at its full path — the route
/// to the stray `:`-prefix ratect#452's triage found besides `local`.
#[test]
fn a_directory_cache_in_a_project_path_containing_a_colon_creates_no_stray_prefix() {
    let dir = unique_temp_dir();
    let project = dir.join("my:proj");
    let options = crate::cache::CacheOptions {
        cache_type: crate::cache::CacheType::Directory,
        project_directory: project.clone(),
    };
    let mount = crate::config::CacheVolumeMount {
        name: "gradle-cache".to_string(),
        container: "/root/.gradle".to_string(),
        options: None,
        scope: Default::default(),
    };

    let bind =
        crate::cache::resolve_cache_mount(&options, || Ok("key".to_string()), &mount).unwrap();
    ensure_host_volume_directories_exist(&[bind], &[]).unwrap();

    assert!(crate::cache::cache_directory(&project)
        .join("gradle-cache")
        .is_dir());
    assert!(!dir.join("my").exists());

    fs::remove_dir_all(&dir).unwrap();
}

/// Component-wise, as Java's `Path.startsWith` is in Batect's own
/// `isSpecialDockerDesktopPath`.
#[test]
fn only_paths_under_docker_desktops_own_directories_are_special() {
    assert!(is_special_docker_desktop_path(Path::new(
        "/run/host-services/ssh-auth.sock"
    )));
    assert!(is_special_docker_desktop_path(Path::new(
        "/run/guest-services/something"
    )));
    assert!(!is_special_docker_desktop_path(Path::new(
        "/run/host-services-not/something"
    )));
    assert!(!is_special_docker_desktop_path(Path::new(
        "/run/guest-services-not/something"
    )));
}

/// Pre-creation is for a container running as the host user only:
/// otherwise a missing host path is left for Docker, as Batect's early
/// return leaves it.
#[tokio::test]
async fn container_config_creates_host_directories_only_for_a_container_running_as_the_host_user() {
    let dir = unique_temp_dir();
    let missing = dir.join("missing");
    let (client, _) = client_for_recording_daemon(|_, _| (404, String::new())).await;
    let mut spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Task,
    );
    spec.shared.volumes = Some(vec![host_bind(&missing, "/code")]);

    client.container_config(&spec).unwrap();
    assert!(!missing.exists());

    spec.shared.runs_as_host_user = true;
    client.container_config(&spec).unwrap();
    assert!(missing.is_dir());

    fs::remove_dir_all(&dir).unwrap();
}

fn build_spec_with(
    build_args: Option<HashMap<String, String>>,
    config_file_proxies: bool,
) -> crate::container_spec::BuildSpec {
    crate::container_spec::BuildSpec {
        build_directory: std::path::PathBuf::from("/project"),
        dockerfile: "Dockerfile".to_string(),
        build_args,
        target: None,
        buildkit: None,
        tag: "tag".to_string(),
        force_pull: false,
        proxy_host_gateway: None,
        config_file_proxies,
    }
}

const PROXIES_JSON: &str = r#"{"proxies": {"default": {
    "httpProxy": "http://config", "noProxy": "config.example"
}}}"#;

/// The `config.json` layer fills in beneath the build's own args: a
/// host-environment proxy (already merged into `build_args`) and the
/// container's explicit `build_args` both win over it (ratect#295).
#[test]
fn host_environment_proxies_and_build_args_beat_config_file_proxies() {
    let host_proxy = crate::proxy::proxy_environment_variables(
        |name| (name == "http_proxy").then(|| "http://host.example".to_string()),
        &std::collections::BTreeSet::new(),
    );
    let container = crate::config::Container {
        build_args: Some(HashMap::from([(
            "NO_PROXY".to_string(),
            "explicit.example".to_string(),
        )])),
        ..Default::default()
    };
    let spec = crate::container_spec::derive_build_spec(
        &container,
        "/project",
        "Dockerfile".to_string(),
        "tag",
        Some(&host_proxy),
        true,
    )
    .unwrap();

    let build_args = build_args_with_config_file_proxies(
        &spec,
        Some(PROXIES_JSON),
        "unix:///var/run/docker.sock",
    )
    .unwrap();

    assert_eq!(build_args["http_proxy"], "http://host.example");
    assert_eq!(build_args["HTTP_PROXY"], "http://host.example");
    assert_eq!(build_args["NO_PROXY"], "explicit.example");
    assert_eq!(build_args["no_proxy"], "config.example");
}

/// `--no-proxy-vars` sends a build's explicit `build_args` and nothing from
/// `config.json`, even with a `default` entry there.
#[test]
fn config_file_proxies_are_skipped_when_turned_off() {
    let explicit = HashMap::from([("VERSION".to_string(), "1".to_string())]);
    let spec = build_spec_with(Some(explicit.clone()), false);
    assert_eq!(
        build_args_with_config_file_proxies(
            &spec,
            Some(PROXIES_JSON),
            "unix:///var/run/docker.sock"
        ),
        Some(explicit)
    );
    let spec = build_spec_with(None, false);
    assert_eq!(
        build_args_with_config_file_proxies(
            &spec,
            Some(PROXIES_JSON),
            "unix:///var/run/docker.sock"
        ),
        None
    );
}

/// A build with no args of its own gets the `config.json` ones; with no
/// file, it sends none, exactly as before.
#[test]
fn config_file_proxies_alone_make_a_builds_args() {
    let spec = build_spec_with(None, true);
    let build_args = build_args_with_config_file_proxies(
        &spec,
        Some(PROXIES_JSON),
        "unix:///var/run/docker.sock",
    )
    .unwrap();
    assert_eq!(build_args["http_proxy"], "http://config");
    assert_eq!(
        build_args_with_config_file_proxies(&spec, None, "unix:///var/run/docker.sock"),
        None
    );
}

/// The `list` half of a fake resolver: what the store answers, and whether
/// it was asked.
struct FakeStoreResolver {
    resolver: FakeMultiCredentialResolver,
    listed: std::sync::Mutex<Option<anyhow::Result<Vec<String>>>>,
    asked: std::sync::atomic::AtomicBool,
}

impl FakeStoreResolver {
    fn new(
        answers: HashMap<String, anyhow::Result<Option<bollard::auth::DockerCredentials>>>,
        listed: anyhow::Result<Vec<String>>,
    ) -> Self {
        Self {
            resolver: FakeMultiCredentialResolver::new(answers),
            listed: std::sync::Mutex::new(Some(listed)),
            asked: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl crate::registry_auth::RegistryCredentialResolver for FakeStoreResolver {
    async fn resolve(
        &self,
        registry: &str,
    ) -> anyhow::Result<Option<bollard::auth::DockerCredentials>> {
        self.resolver.resolve(registry).await
    }

    async fn list(&self, store: &str) -> anyhow::Result<Vec<String>> {
        assert_eq!(store, "desktop");
        self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
        self.listed
            .lock()
            .unwrap()
            .take()
            .expect("list() called more than once")
    }
}

fn credential(username: &str) -> bollard::auth::DockerCredentials {
    bollard::auth::DockerCredentials {
        username: Some(username.to_string()),
        password: Some("secret".to_string()),
        ..Default::default()
    }
}

/// ratect#306: a registry only the default `credsStore` holds is resolved
/// and sent like one `config.json` names.
#[tokio::test]
async fn a_registry_only_the_credential_store_holds_is_sent() {
    let resolver = FakeStoreResolver::new(
        HashMap::from([(
            "store-only.example.com".to_string(),
            Ok(Some(credential("alice"))),
        )]),
        Ok(vec!["store-only.example.com".to_string()]),
    );

    let (registries, warning) = build_registries(&resolver, r#"{"credsStore": "desktop"}"#).await;
    let (credentials, warnings) = resolve_build_credentials(&resolver, &registries).await;

    assert_eq!(warning, None);
    assert!(warnings.is_empty());
    assert_eq!(
        credentials,
        HashMap::from([("store-only.example.com".to_string(), credential("alice"))])
    );
}

/// What `auths`, `credHelpers` and the store hold together, once each, in a
/// stable order.
#[tokio::test]
async fn build_registries_unions_config_and_store_registries_once_each() {
    let resolver = FakeStoreResolver::new(
        HashMap::new(),
        Ok(vec![
            "z-store.example.com".to_string(),
            "auths.example.com".to_string(),
        ]),
    );
    let config = r#"{
        "credsStore": "desktop",
        "auths": {"auths.example.com": {}},
        "credHelpers": {"helper.example.com": "ecr-login"}
    }"#;

    let (registries, warning) = build_registries(&resolver, config).await;

    assert_eq!(warning, None);
    assert_eq!(
        registries,
        vec![
            "auths.example.com".to_string(),
            "helper.example.com".to_string(),
            "z-store.example.com".to_string(),
        ]
    );
}

/// A store that can't be listed costs only what it would have added: the
/// registries `config.json` names still go, and one warning names the
/// store.
#[tokio::test]
async fn a_credential_store_that_cant_be_listed_warns_once_and_keeps_the_rest() {
    let resolver = FakeStoreResolver::new(
        HashMap::new(),
        Err(anyhow::anyhow!(
            "`docker-credential-desktop` failed: locked"
        )),
    );
    let config = r#"{"credsStore": "desktop", "auths": {"auths.example.com": {}}}"#;

    let (registries, warning) = build_registries(&resolver, config).await;

    assert_eq!(registries, vec!["auths.example.com".to_string()]);
    assert_eq!(
        warning.as_deref(),
        Some(
            "Could not list the registries credential store 'desktop' holds \
             (`docker-credential-desktop` failed: locked); building with only those \
             config.json names."
        )
    );
}

/// Without a `credsStore` nothing is listed, and the registries are exactly
/// `configured_registries`'.
#[tokio::test]
async fn without_a_credential_store_nothing_is_listed() {
    let resolver = FakeStoreResolver::new(HashMap::new(), Ok(vec!["never".to_string()]));
    let config = r#"{"auths": {"auths.example.com": {}}}"#;

    let (registries, warning) = build_registries(&resolver, config).await;

    assert!(!resolver.asked.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        registries,
        crate::registry_auth::configured_registries(config)
    );
    assert_eq!(warning, None);
}

/// A non-interactive spec named `app`, in `role`, derived from `container`.
fn spec_from(
    container: &crate::config::Container,
    role: crate::labels::ContainerRole,
) -> ContainerSpec {
    use crate::container_spec::{derive_spec, ContainerSpecInputs, Overlay};
    let run_labels = crate::labels::RunLabels::new("demo", "task", "run-id", None);
    derive_spec(ContainerSpecInputs {
        name: "app",
        docker_name: "demo-app-abc123",
        container_config: container,
        overlay: Overlay::Customise(None),
        image: "alpine:3.20",
        network: ContainerNetwork::Task("demo-network-abc123".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role,
        run_labels: &run_labels,
    })
}

/// The create request's `HostConfig.init` for a dependency derived from
/// `container`, as `container_config` builds it for `create_container`.
async fn init_sent_for(container: &crate::config::Container) -> Option<bool> {
    let (client, _) = client_for_recording_daemon(|_, _| (404, String::new())).await;
    let spec = spec_from(container, crate::labels::ContainerRole::Dependency);
    let config = client.container_config(&spec).unwrap();
    config.host_config.unwrap().init
}

/// Every create request carries an explicit `Init`, as docker-client's
/// always-non-nil `&useInitProcess` does: an omitted `enable_init_process`
/// is sent as `false`, so a daemon configured with `"init": true` can't
/// put an init process in front of the command (ratect#293).
#[tokio::test]
async fn container_config_always_sends_init() {
    let mut container = crate::config::Container::default();
    assert_eq!(init_sent_for(&container).await, Some(false));

    container.enable_init_process = Some(false);
    assert_eq!(init_sent_for(&container).await, Some(false));

    container.enable_init_process = Some(true);
    assert_eq!(init_sent_for(&container).await, Some(true));
}

/// A [`recording_daemon`] route for running one container: its attach
/// upgrades and ends at once, as a container that printed nothing would;
/// its inspect (bollard's, to pick the attach stream's framing) reports no
/// TTY; its start succeeds; its wait reports exit code 0.
fn run_route(method: &str, path: &str) -> (u16, String) {
    match (method, path) {
        ("POST", "/containers/app-id/attach") => (101, String::new()),
        ("GET", "/containers/app-id/json") => (200, r#"{"Config":{"Tty":false}}"#.to_string()),
        ("POST", "/containers/app-id/start") => (204, String::new()),
        ("POST", "/containers/app-id/wait") => (200, r#"{"StatusCode":0}"#.to_string()),
        _ => (404, r#"{"message":"not found"}"#.to_string()),
    }
}

/// Asserts `log` holds an attach to `app-id` before its start, and no
/// `logs` request at all.
fn assert_attached_before_start(log: &std::sync::Mutex<Vec<RecordedRequest>>, context: &str) {
    let paths: Vec<_> = requests_sent(log)
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    let position = |wanted: &str| paths.iter().position(|path| path == wanted);
    let attach = position("/containers/app-id/attach");
    let start = position("/containers/app-id/start");
    assert!(
        attach.is_some() && start.is_some() && attach < start,
        "{context}: no attach before the start: {paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.ends_with("/logs")),
        "{context}: read through logs: {paths:?}"
    );
}

/// A sink whose output style is `all`, so dependencies' output is followed.
struct InterleavedSink;

impl EventSink for InterleavedSink {
    fn post(&self, _: TaskEvent) {}

    fn container_io_streaming(&self) -> ContainerIoStreaming {
        ContainerIoStreaming::Interleaved
    }
}

/// The task's own container's output is read from an attach made before
/// it starts, never through the `logs` API, which can't read a container
/// whose log driver Docker can't read back (ratect#517).
#[tokio::test]
async fn a_task_container_is_attached_before_it_starts_and_never_read_through_logs() {
    for interleaved in [false, true] {
        let (client, log) = client_for_recording_daemon(run_route).await;
        let client = if interleaved {
            client.with_event_sink(std::sync::Arc::new(InterleavedSink))
        } else {
            client
        };
        let spec = spec_from(
            &crate::config::Container::default(),
            crate::labels::ContainerRole::Task,
        );

        client.run_container("app-id", &spec, None).await.unwrap();

        assert_attached_before_start(&log, &format!("interleaved: {interleaved}"));
    }
}

/// Under `-o all`, a dependency's output follower attaches before the
/// container starts, not through the `logs` API afterwards (ratect#517).
#[tokio::test]
async fn a_followed_dependency_is_attached_before_it_starts() {
    let (client, log) = client_for_recording_daemon(run_route).await;
    let client = client.with_event_sink(std::sync::Arc::new(InterleavedSink));
    let spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Dependency,
    );

    client
        .start_background_container("app-id", &spec)
        .await
        .unwrap();

    assert_attached_before_start(&log, "dependency");
}

/// The stdin-forwarding path reads its output from the same attach that
/// carries its stdin, made before the container starts — not from a
/// stdin-only attach plus the `logs` API (ratect#517).
#[tokio::test]
async fn a_stdin_forwarding_task_container_attaches_stdin_and_output_before_it_starts() {
    let (client, log) = client_for_recording_daemon(run_route).await;
    let reader: &'static stdin::StdinReader = Box::leak(Box::new(stdin::StdinReader::spawn(
        std::io::Cursor::new(b"echo hi\n".to_vec()),
        false,
    )));

    let exit_code = client
        .run_container_forwarding_stdin("app", "app-id", reader, None)
        .await
        .unwrap();

    assert_eq!(exit_code, 0);
    assert_attached_before_start(&log, "stdin forwarding");
    let attach_query = log
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.path == "/containers/app-id/attach")
        .map(|request| request.query.clone())
        .unwrap();
    for stream in ["stdin", "stdout", "stderr"] {
        assert!(
            attach_query.contains(&format!("{stream}=true")),
            "{stream} not attached: {attach_query}"
        );
    }
}

/// A [`recording_daemon`] route answering `method` on `path` with `status`
/// and `body`, and anything else with a 404.
fn one_route(
    method: &'static str,
    path: &'static str,
    status: u16,
    body: &'static str,
) -> impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static {
    move |m, p| {
        if (m, p) == (method, path) {
            (status, body.to_string())
        } else {
            (404, r#"{"message":"not found"}"#.to_string())
        }
    }
}

/// Batect's `DockerClientContainerManagementSpec`, run with no working
/// directory, user, shm size or entrypoint set, and
/// `DockerContainerCreationSpecFactorySpec`'s "no entrypoint": the create
/// request carries none of the four, so the image's own working directory,
/// user and `ENTRYPOINT`, and the daemon's 64 MiB `/dev/shm`, apply.
#[tokio::test]
async fn container_config_leaves_unset_defaults_to_the_image_and_daemon() {
    let (client, _) = client_for_recording_daemon(|_, _| (404, String::new())).await;
    let spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Task,
    );

    let config = client.container_config(&spec).unwrap();

    assert_eq!(config.entrypoint, None);
    assert_eq!(config.working_dir, None);
    assert_eq!(config.user, None);
    assert_eq!(config.host_config.unwrap().shm_size, None);
}

/// Batect's `DockerClientContainerManagementSpec`, creating a container
/// from an image the daemon doesn't have: the create fails naming the
/// container as configured, with the daemon's reason in the chain, and
/// nothing to clean up.
#[tokio::test]
async fn create_container_reports_a_missing_image_naming_the_container() {
    let (client, _) = client_for_recording_daemon(one_route(
        "POST",
        "/containers/create",
        404,
        r#"{"message":"No such image: alpine:3.20"}"#,
    ))
    .await;
    let spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Task,
    );

    let err = client.create_container(&spec).await.unwrap_err();

    assert_eq!(err.created, None);
    assert_eq!(err.to_string(), "Failed to create container 'app'");
    let chain = format!("{:#}", err.error);
    assert!(chain.contains("No such image: alpine:3.20"), "{chain}");
}

/// `create_container` for `app` under `run_as_current_user`, against a
/// daemon that creates it as `new-id` and answers its uploads with `upload`,
/// given each upload's position (0 for `/etc`'s, then one per directory).
async fn create_with_user_mapping(
    upload: impl Fn(usize) -> (u16, String) + Send + Sync + 'static,
) -> CreateContainerError {
    let uploads = std::sync::atomic::AtomicUsize::new(0);
    let (client, _) = client_for_recording_daemon(move |method, path| match (method, path) {
        ("POST", "/containers/create") => (201, r#"{"Id":"new-id","Warnings":[]}"#.to_string()),
        ("PUT", "/containers/new-id/archive") => {
            upload(uploads.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
        }
        _ => (404, r#"{"message":"not found"}"#.to_string()),
    })
    .await;
    let mut spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Task,
    );
    spec.shared.user_mapping = Some(user_mapping_fixture());

    client.create_container(&spec).await.unwrap_err()
}

/// Batect's `DockerClientContainerManagementSpec`, uploading a file to a
/// directory the container doesn't have: `run_as_current_user`'s `/etc`
/// upload fails naming the container and the field, with the daemon's
/// reason in the chain — and the container, which exists by then, is handed
/// back for cleanup.
#[tokio::test]
async fn a_user_mapping_upload_the_daemon_refuses_fails_the_create_naming_the_container() {
    let err = create_with_user_mapping(|_| {
        (
            404,
            r#"{"message":"Could not find the file /etc in container new-id"}"#.to_string(),
        )
    })
    .await;

    assert_eq!(err.created.as_deref(), Some("new-id"));
    assert_eq!(
        err.to_string(),
        "Failed to apply run_as_current_user to container 'app'"
    );
    let chain = format!("{:#}", err.error);
    assert!(
        chain.contains("Failed to upload user mapping files to container 'new-id'"),
        "{chain}"
    );
    assert!(
        chain.contains("Could not find the file /etc in container new-id"),
        "{chain}"
    );
}

/// The same rule for a directory upload: the home directory's ownership
/// archive, refused after `/etc`'s succeeded, fails the create the same way.
#[tokio::test]
async fn a_directory_ownership_upload_the_daemon_refuses_fails_the_create_naming_the_container() {
    let err = create_with_user_mapping(|position| match position {
        0 => (200, String::new()),
        _ => (
            404,
            r#"{"message":"Could not find the file /home in container new-id"}"#.to_string(),
        ),
    })
    .await;

    assert_eq!(err.created.as_deref(), Some("new-id"));
    assert_eq!(
        err.to_string(),
        "Failed to apply run_as_current_user to container 'app'"
    );
    let chain = format!("{:#}", err.error);
    assert!(
        chain.contains("Failed to take ownership of '/home/ratect' in container 'new-id'"),
        "{chain}"
    );
    assert!(
        chain.contains("Could not find the file /home in container new-id"),
        "{chain}"
    );
}

/// `create_container`'s requests for a dependency `app` under
/// `run_as_current_user` — home directory `/home/ratect`, cache mounts at
/// `/caches/cargo` and `/workspace/target` — against a daemon answering its
/// create with `create_status` (as `new-id` when that's a 201) and its
/// uploads with `upload`, given each upload's position (0 for `/etc`'s,
/// then one per directory): what the call returned, and every request it
/// sent, in order.
async fn create_requests_under_user_mapping(
    create_status: u16,
    upload: impl Fn(usize) -> (u16, String) + Send + Sync + 'static,
) -> (
    std::result::Result<String, CreateContainerError>,
    Vec<RecordedRequest>,
) {
    let uploads = std::sync::atomic::AtomicUsize::new(0);
    let (client, log) = client_for_recording_daemon(move |method, path| match (method, path) {
        ("POST", "/containers/create") if create_status == 201 => {
            (201, r#"{"Id":"new-id","Warnings":[]}"#.to_string())
        }
        ("POST", "/containers/create") => (
            create_status,
            r#"{"message":"stub daemon refused the create"}"#.to_string(),
        ),
        ("PUT", "/containers/new-id/archive") => {
            upload(uploads.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
        }
        _ => (404, r#"{"message":"not found"}"#.to_string()),
    })
    .await;
    let mut spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Dependency,
    );
    spec.shared.user_mapping = Some(UserMapping {
        cache_directories: vec!["/caches/cargo".to_string(), "/workspace/target".to_string()],
        ..user_mapping_fixture()
    });

    let result = client.create_container(&spec).await;

    let requests = log.lock().unwrap().clone();
    (result, requests)
}

/// An upload's `path` query parameter, decoded: the container directory
/// its archive is extracted into.
fn upload_path(request: &RecordedRequest) -> Option<String> {
    url::form_urlencoded::parse(request.query.as_bytes())
        .find(|(key, _)| key == "path")
        .map(|(_, value)| value.into_owned())
}

/// Each request in `requests` as `(method, path, upload path, tar entry
/// names)` — the last two only for an archive upload.
fn create_sequence(
    requests: &[RecordedRequest],
) -> Vec<(String, String, Option<String>, Vec<String>)> {
    requests
        .iter()
        .map(|request| {
            let is_upload = request.path.ends_with("/archive");
            (
                request.method.clone(),
                request.path.clone(),
                upload_path(request).filter(|_| is_upload),
                if is_upload {
                    tar_entry_paths(&request.body)
                } else {
                    Vec::new()
                },
            )
        })
        .collect()
}

/// Batect's NCC-4 and NCC-5 (ratect#340): under `run_as_current_user`, a
/// created container gets the user-mapping archive — `passwd`, `shadow` and
/// `group`, whose contents have their own tests — extracted into `/etc`,
/// then one ownership archive for the home directory and one per cache
/// mount, each extracted into its parent; and that is everything
/// `create_container` sends. Docker's create comes first, since every
/// upload needs its id; nothing is started; and there is no network connect
/// to order, since the container joins the task's network in the create
/// request itself (ratect#303 — pinned by
/// `create_container_names_the_container_and_creates_it_on_the_task_network`).
#[tokio::test]
async fn a_user_mapped_create_uploads_etc_then_each_owned_directory_and_starts_nothing() {
    let (result, requests) =
        create_requests_under_user_mapping(201, |_| (200, String::new())).await;

    assert_eq!(result.unwrap(), "new-id");
    let upload = |path: &str, entries: &[&str]| {
        (
            "PUT".to_string(),
            "/containers/new-id/archive".to_string(),
            Some(path.to_string()),
            entries.iter().map(|entry| entry.to_string()).collect(),
        )
    };
    assert_eq!(
        create_sequence(&requests),
        vec![
            (
                "POST".to_string(),
                "/containers/create".to_string(),
                None,
                Vec::new()
            ),
            upload("/etc", &["passwd", "shadow", "group"]),
            upload("/home", &["ratect/"]),
            upload("/caches", &["cargo/"]),
            upload("/workspace", &["target/"]),
        ]
    );
}

/// Batect's NCC-9, the Docker half (ratect#340): an `/etc` upload the
/// daemon refuses ends the create there — no ownership upload, no start —
/// and hands back the id Docker's create returned, since that container
/// exists and is the caller's to remove.
#[tokio::test]
async fn a_refused_etc_upload_ends_the_create_carrying_the_created_id() {
    let (result, requests) = create_requests_under_user_mapping(201, |_| {
        (
            500,
            r#"{"message":"stub daemon refused the upload"}"#.to_string(),
        )
    })
    .await;

    assert_eq!(result.unwrap_err().created.as_deref(), Some("new-id"));
    let sent: Vec<_> = create_sequence(&requests)
        .into_iter()
        .map(|(method, path, upload_path, _)| (method, path, upload_path))
        .collect();
    assert_eq!(
        sent,
        vec![
            ("POST".to_string(), "/containers/create".to_string(), None),
            (
                "PUT".to_string(),
                "/containers/new-id/archive".to_string(),
                Some("/etc".to_string())
            ),
        ]
    );
}

/// Batect's RRC-3/RRC-7, the Docker half (ratect#340): a create the daemon
/// refuses leaves nothing created — `created` is `None` — and nothing
/// follows it: no upload, though the container is user-mapped, and no
/// start.
#[tokio::test]
async fn a_refused_create_carries_no_id_and_sends_nothing_after_it() {
    let (result, requests) =
        create_requests_under_user_mapping(500, |_| (200, String::new())).await;

    assert_eq!(result.unwrap_err().created, None);
    assert_eq!(
        method_and_path(&requests),
        vec![("POST".to_string(), "/containers/create".to_string())]
    );
}

/// A sink whose output style streams container I/O as `streaming` says.
struct StreamingSink(ContainerIoStreaming);

impl EventSink for StreamingSink {
    fn post(&self, _: TaskEvent) {}

    fn container_io_streaming(&self) -> ContainerIoStreaming {
        self.0
    }
}

/// Batect's TCIO-25 to TCIO-33 (ratect#340), Docker's half: a dependency's
/// spec is never interactive, so it is created with no TTY and no stdin —
/// `Tty`, `OpenStdin`, `AttachStdin` and `StdinOnce` all unset — whatever
/// the output style. The engine's half, that a dependency's spec is never
/// made interactive, is `a_prerequisites_dependency_is_never_interactive`'s.
#[tokio::test]
async fn a_dependency_is_created_with_no_tty_or_stdin_under_any_output_style() {
    for streaming in [
        ContainerIoStreaming::TaskContainerOnly,
        ContainerIoStreaming::Interleaved,
    ] {
        let (client, _) = client_for_recording_daemon(|_, _| (404, String::new())).await;
        let client = client.with_event_sink(std::sync::Arc::new(StreamingSink(streaming)));
        let spec = spec_from(
            &crate::config::Container::default(),
            crate::labels::ContainerRole::Dependency,
        );

        let config = client.container_config(&spec).unwrap();

        assert_eq!(
            (
                config.tty,
                config.open_stdin,
                config.attach_stdin,
                config.stdin_once
            ),
            (None, None, None, None),
            "{streaming:?}"
        );
    }
}

/// Batect's `DockerClientContainerManagementSpec`, starting a container
/// that doesn't exist: a dependency's start fails naming it as configured,
/// with the daemon's reason in the chain.
#[tokio::test]
async fn start_background_container_reports_a_missing_container_naming_it() {
    let (client, _) = client_for_recording_daemon(one_route(
        "POST",
        "/containers/app-id/start",
        404,
        r#"{"message":"No such container: app-id"}"#,
    ))
    .await;
    let spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Dependency,
    );

    let err = client
        .start_background_container("app-id", &spec)
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "Failed to start container 'app'");
    assert!(
        format!("{err:#}").contains("No such container: app-id"),
        "{err:#}"
    );
}

/// The same rule for the task's own container, started after its output is
/// attached.
#[tokio::test]
async fn run_container_reports_a_missing_container_naming_it() {
    let (client, _) = client_for_recording_daemon(|method, path| match (method, path) {
        ("POST", "/containers/app-id/start") => (
            404,
            r#"{"message":"No such container: app-id"}"#.to_string(),
        ),
        _ => run_route(method, path),
    })
    .await;
    let spec = spec_from(
        &crate::config::Container::default(),
        crate::labels::ContainerRole::Task,
    );

    let err = client
        .run_container("app-id", &spec, None)
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "Failed to start container 'app'");
    assert!(
        format!("{err:#}").contains("No such container: app-id"),
        "{err:#}"
    );
}

/// A [`recording_daemon`] route for pulling `alpine:3.20`: the pull answers
/// with `stream` as its body, newline-delimited JSON as the daemon streams
/// it, and the image's inspect finds it.
fn pull_route(stream: String) -> impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static {
    move |method, path| match (method, path) {
        ("POST", "/images/create") => (200, stream.clone()),
        ("GET", "/images/alpine:3.20/json") => (200, r#"{"Id":"sha256:abc"}"#.to_string()),
        _ => (404, r#"{"message":"not found"}"#.to_string()),
    }
}

/// Whether `log` holds an inspect of the pulled image.
fn inspected_after_pulling(log: &std::sync::Mutex<Vec<RecordedRequest>>) -> bool {
    requests_sent(log)
        .iter()
        .any(|(method, path)| method == "GET" && path.starts_with("/images/"))
}

/// Batect's `DockerClientImagePullSpec`: a pull stream that ends (EOF)
/// after its progress lines is a successful pull.
#[tokio::test]
async fn pull_image_succeeds_when_the_stream_ends_without_an_error() {
    let (client, log) = client_for_recording_daemon(pull_route(
        "{\"status\":\"Pulling from library/alpine\"}\n{\"status\":\"Download complete\"}\n"
            .to_string(),
    ))
    .await;

    assert_eq!(
        client.pull_image("alpine:3.20").await.unwrap(),
        "sha256:abc"
    );
    assert!(inspected_after_pulling(&log));
}

/// Batect's `DockerClientImagePullSpec`, an image that doesn't exist or is
/// for another platform: the pull fails at the stream's error, naming the
/// image, with the daemon's message in the chain — and goes no further.
/// The headline is Ratect's (see `docs/differences-from-batect.md`'s
/// "Failure messages are Ratect's own").
#[tokio::test]
async fn pull_image_fails_at_the_daemons_stream_error_with_its_message() {
    for message in [
        "manifest for alpine:3.20 not found: manifest unknown: manifest unknown",
        "pull access denied for alpine, repository does not exist or may require 'docker login'",
        "no matching manifest for linux/arm64/v8 in the manifest list entries",
        "image operating system \"windows\" cannot be used on this platform",
    ] {
        let stream = format!(
            "{{\"status\":\"Pulling from library/alpine\"}}\n{}\n{{\"status\":\"never read\"}}\n",
            serde_json::json!({ "errorDetail": { "message": message }, "error": message })
        );
        let (client, log) = client_for_recording_daemon(pull_route(stream)).await;

        let err = client.pull_image("alpine:3.20").await.unwrap_err();

        assert_eq!(err.to_string(), "Failed to pull image alpine:3.20");
        assert!(format!("{err:#}").contains(message), "{err:#}");
        assert!(!inspected_after_pulling(&log), "{message}");
    }
}

/// Batect's `DockerClientImagePullSpec`: a stream that isn't JSON fails the
/// pull too, rather than being skipped.
#[tokio::test]
async fn pull_image_fails_on_a_malformed_stream() {
    let (client, log) =
        client_for_recording_daemon(pull_route("this is not json\n".to_string())).await;

    let err = client.pull_image("alpine:3.20").await.unwrap_err();

    assert_eq!(err.to_string(), "Failed to pull image alpine:3.20");
    assert!(!inspected_after_pulling(&log));
}

/// Batect's `DockerClientImagePullSpec`, getting an image that isn't there:
/// only the daemon's not-found means "not present locally"; any other
/// inspect failure is an error naming the image.
#[tokio::test]
async fn local_image_id_is_none_only_for_a_daemon_not_found() {
    let inspect = "/images/alpine:3.20/json";
    let (client, _) =
        client_for_recording_daemon(one_route("GET", inspect, 200, r#"{"Id":"sha256:abc"}"#)).await;
    assert_eq!(
        client
            .local_image_id("alpine:3.20")
            .await
            .unwrap()
            .as_deref(),
        Some("sha256:abc")
    );

    let (client, log) = client_for_recording_daemon(one_route(
        "GET",
        inspect,
        404,
        r#"{"message":"No such image: alpine:3.20"}"#,
    ))
    .await;
    assert_eq!(client.local_image_id("alpine:3.20").await.unwrap(), None);
    assert_eq!(
        requests_sent(&log),
        vec![("GET".to_string(), inspect.to_string())]
    );

    let (client, _) = client_for_recording_daemon(one_route(
        "GET",
        inspect,
        500,
        r#"{"message":"stub daemon failed the inspect"}"#,
    ))
    .await;
    let err = client.local_image_id("alpine:3.20").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Failed to check whether image 'alpine:3.20' exists locally"
    );
    assert!(
        format!("{err:#}").contains("stub daemon failed the inspect"),
        "{err:#}"
    );
}

/// Batect's `DockerClientNetworkManagementSpec`, getting a network by name
/// or ID: the string given is what's inspected, so either works; a
/// not-found — what a lookup after deleting one gets — is "doesn't exist",
/// not an error; any other failure is an error naming the network.
#[tokio::test]
async fn network_exists_is_false_only_for_a_daemon_not_found() {
    let id = "4f3c1b2a9d8e";
    let (client, log) =
        client_for_recording_daemon(one_route("GET", "/networks/4f3c1b2a9d8e", 200, "{}")).await;
    assert!(client.network_exists(id).await.unwrap());
    assert_eq!(
        requests_sent(&log),
        vec![("GET".to_string(), format!("/networks/{id}"))]
    );

    let (client, log) = client_for_recording_daemon(one_route(
        "GET",
        "/networks/gone",
        404,
        r#"{"message":"network gone not found"}"#,
    ))
    .await;
    assert!(!client.network_exists("gone").await.unwrap());
    assert_eq!(
        requests_sent(&log),
        vec![("GET".to_string(), "/networks/gone".to_string())]
    );

    let (client, _) = client_for_recording_daemon(one_route(
        "GET",
        "/networks/broken",
        500,
        r#"{"message":"stub daemon failed the inspect"}"#,
    ))
    .await;
    let err = client.network_exists("broken").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Failed to check whether network 'broken' exists"
    );
    assert!(
        format!("{err:#}").contains("stub daemon failed the inspect"),
        "{err:#}"
    );
}

/// Batect's `DockerClientNetworkManagementSpec` fails deleting a network
/// that doesn't exist; Ratect doesn't (ratect#609): a network already gone
/// is the outcome asked for, so it isn't a cleanup failure
/// (decisions/0016).
#[tokio::test]
async fn remove_network_succeeds_for_a_missing_network() {
    let (client, _) = client_for_recording_daemon(one_route(
        "DELETE",
        "/networks/gone",
        404,
        r#"{"message":"network gone not found"}"#,
    ))
    .await;

    client.remove_network("gone").await.unwrap();
}

/// Any other refusal still fails, naming the network, with the daemon's
/// reason in the chain — a network with a container still attached, say.
#[tokio::test]
async fn remove_network_fails_when_the_daemon_refuses() {
    let (client, _) = client_for_recording_daemon(one_route(
        "DELETE",
        "/networks/busy",
        403,
        r#"{"message":"error while removing network: network busy has active endpoints"}"#,
    ))
    .await;

    let err = client.remove_network("busy").await.unwrap_err();

    assert_eq!(err.to_string(), "Failed to remove network 'busy'");
    assert!(
        format!("{err:#}").contains("has active endpoints"),
        "{err:#}"
    );
}

/// Batect's `DockerClientVolumeManagementSpec`, deleting a volume that
/// doesn't exist: a failure naming the volume, with the daemon's reason in
/// the chain.
#[tokio::test]
async fn remove_volume_fails_for_a_missing_volume() {
    let (client, _) = client_for_recording_daemon(one_route(
        "DELETE",
        "/volumes/gone",
        404,
        r#"{"message":"get gone: no such volume"}"#,
    ))
    .await;

    let err = client.remove_volume("gone").await.unwrap_err();

    assert_eq!(err.to_string(), "Failed to remove volume 'gone'");
    assert!(
        format!("{err:#}").contains("get gone: no such volume"),
        "{err:#}"
    );
}

/// `unhealthy_message`'s fixed verdict sentence, which every unhealthy
/// message starts with.
const UNHEALTHY_VERDICT: &str = "The configured health check did not indicate that the container \
                                 was healthy within the timeout period.";

/// A health-check run that exited `exit_code` having printed `output`.
fn health_check_run(exit_code: i64, output: &str) -> bollard::models::HealthcheckResult {
    bollard::models::HealthcheckResult {
        exit_code: Some(exit_code),
        output: Some(output.to_string()),
        ..Default::default()
    }
}

/// Batect's U35 (ratect#348): with no health-check run to explain it, the
/// message is the verdict alone — the fallback ratect#328 recorded as a
/// divergence.
#[test]
fn unhealthy_message_is_the_verdict_alone_without_a_health_check_run() {
    assert_eq!(unhealthy_message(None), UNHEALTHY_VERDICT);
}

/// Batect's U35: a last run that exited 0 means the container turned
/// healthy just too late, whatever it printed — nothing included.
#[test]
fn unhealthy_message_explains_a_last_run_that_exited_0_as_just_too_late() {
    for output in ["ok", ""] {
        assert_eq!(
            unhealthy_message(Some(health_check_run(0, output))),
            format!(
                "{UNHEALTHY_VERDICT} The most recent health check exited with code 0, which \
                 usually indicates that the container became healthy just after the timeout \
                 period expired."
            ),
            "output: {output:?}"
        );
    }
}

/// Batect's U35: a failing last run that printed nothing says so.
#[test]
fn unhealthy_message_says_a_failing_last_run_produced_no_output() {
    assert_eq!(
        unhealthy_message(Some(health_check_run(1, ""))),
        format!(
            "{UNHEALTHY_VERDICT} The last health check exited with code 1 but did not produce \
             any output."
        )
    );
}

/// Batect's U35: a failing last run's output follows its exit code,
/// trimmed of surrounding whitespace.
#[test]
fn unhealthy_message_gives_a_failing_last_runs_output_trimmed() {
    assert_eq!(
        unhealthy_message(Some(health_check_run(7, "\n  connection refused\n\n"))),
        format!(
            "{UNHEALTHY_VERDICT} The last health check exited with code 7 and output:\n\
             connection refused"
        )
    );
}

/// An inspect of a container whose config declares a health check.
const INSPECT_WITH_HEALTH_CHECK: &str =
    r#"{"Config":{"Healthcheck":{"Test":["CMD-SHELL","exit 1"]}}}"#;

/// One Docker event with `action`, as a line of the `/events` stream.
fn event_line(action: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "Type": "container",
            "Action": action,
            "Actor": {"ID": "db-id", "Attributes": {}},
            "time": 1,
        })
    )
}

/// `wait_for_container_healthy("db-id")` against a [`StubDaemon`] answering
/// the container's inspects with `inspects` in turn (the last repeated)
/// and `/events` with `events`: what it returned, and every request it
/// sent.
async fn wait_healthy_against(
    inspects: Vec<(u16, String)>,
    events: (u16, String),
) -> (Result<()>, Vec<RecordedRequest>) {
    let inspected = std::sync::atomic::AtomicUsize::new(0);
    let (client, log) = client_for_recording_daemon(move |method, path| match (method, path) {
        ("GET", "/containers/db-id/json") => {
            let turn = inspected.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            inspects[turn.min(inspects.len() - 1)].clone()
        }
        ("GET", "/events") => events.clone(),
        _ => (404, r#"{"message":"not found"}"#.to_string()),
    })
    .await;

    let result = client.wait_for_container_healthy("db-id").await;

    let requests = log.lock().unwrap().clone();
    (result, requests)
}

/// [`wait_healthy_against`] for a container with a health check, whose
/// `/events` stream answers `events` with a 200.
async fn wait_healthy_through_events(events: String) -> (Result<()>, Vec<RecordedRequest>) {
    wait_healthy_against(
        vec![(200, INSPECT_WITH_HEALTH_CHECK.to_string())],
        (200, events),
    )
    .await
}

/// Whether `requests` include a request for Docker's event stream.
fn requested_events(requests: &[RecordedRequest]) -> bool {
    requests.iter().any(|request| request.path == "/events")
}

/// Batect's U34 (ratect#348): an inspect the daemon refuses fails the wait
/// naming the container, the daemon's own words beneath it, before any
/// event is asked for.
#[tokio::test]
async fn wait_for_container_healthy_fails_on_a_refused_inspect_before_asking_for_events() {
    let (result, requests) = wait_healthy_against(
        vec![(404, r#"{"message":"No such container: db-id"}"#.to_string())],
        (200, event_line("health_status: healthy")),
    )
    .await;

    let err = result.unwrap_err();
    assert_eq!(err.to_string(), "Failed to inspect container 'db-id'");
    assert!(
        format!("{err:#}").contains("No such container: db-id"),
        "{err:#}"
    );
    assert!(!requested_events(&requests), "{requests:?}");
}

/// Batect's U34, for `container_is_running`, which inspects the same way.
#[tokio::test]
async fn container_is_running_fails_on_a_refused_inspect_naming_the_container() {
    let (client, _) = client_for_recording_daemon(one_route(
        "GET",
        "/containers/db-id/json",
        404,
        r#"{"message":"No such container: db-id"}"#,
    ))
    .await;

    let err = client.container_is_running("db-id").await.unwrap_err();

    assert_eq!(err.to_string(), "Failed to inspect container 'db-id'");
    assert!(
        format!("{err:#}").contains("No such container: db-id"),
        "{err:#}"
    );
}

/// Batect's U35 shortcut (ratect#348): a container with no health check —
/// no config, no `Healthcheck`, or an empty `Test` — is healthy at once,
/// without asking for a single event.
#[tokio::test]
async fn a_container_without_a_health_check_is_healthy_without_asking_for_events() {
    for inspect in [
        r#"{}"#,
        r#"{"Config":{}}"#,
        r#"{"Config":{"Healthcheck":{}}}"#,
        r#"{"Config":{"Healthcheck":{"Test":[]}}}"#,
    ] {
        let (result, requests) = wait_healthy_against(
            vec![(200, inspect.to_string())],
            (200, event_line("health_status: unhealthy")),
        )
        .await;

        assert!(result.is_ok(), "{inspect}: {result:?}");
        assert!(!requested_events(&requests), "{inspect}: {requests:?}");
    }
}

/// Batect's U50 and U51 (ratect#348): the event stream is asked for this
/// container's `die` and `health_status` events only — the daemon ANDs
/// the two filters — replayed from the beginning of time (`since=0`) with
/// no end (`until`), so a verdict Docker reached before the stream opened
/// still arrives.
#[tokio::test]
async fn the_health_wait_asks_for_this_containers_verdicts_since_the_beginning_of_time() {
    let (result, requests) =
        wait_healthy_through_events(event_line("health_status: healthy")).await;

    result.unwrap();
    let events: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/events")
        .collect();
    let [events] = events[..] else {
        panic!("one events request: {requests:?}");
    };
    let query: HashMap<String, String> = url::form_urlencoded::parse(events.query.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(
        query.get("since").map(String::as_str),
        Some("0"),
        "{query:?}"
    );
    assert!(!query.contains_key("until"), "{query:?}");
    let filters: HashMap<String, Vec<String>> =
        serde_json::from_str(query.get("filters").expect("a filters parameter")).unwrap();
    assert_eq!(
        filters,
        HashMap::from([
            ("container".to_string(), vec!["db-id".to_string()]),
            (
                "event".to_string(),
                vec!["die".to_string(), "health_status".to_string()]
            ),
        ])
    );
}

/// Batect's U51 and U49 (ratect#348): a verdict waiting in the stream the
/// moment it opens — as a replayed one is — decides the wait, and only the
/// first event counts: a contrary verdict after it is never read.
#[tokio::test]
async fn only_the_first_event_in_the_stream_decides_the_health_wait() {
    let (result, _) = wait_healthy_through_events(format!(
        "{}{}",
        event_line("health_status: healthy"),
        event_line("health_status: unhealthy")
    ))
    .await;
    assert!(result.is_ok(), "healthy, then unhealthy: {result:?}");

    let (result, _) = wait_healthy_through_events(format!(
        "{}{}",
        event_line("health_status: unhealthy"),
        event_line("health_status: healthy")
    ))
    .await;
    let err = result.unwrap_err();
    assert!(
        err.to_string().starts_with(UNHEALTHY_VERDICT),
        "unhealthy, then healthy: {err:#}"
    );
}

/// Batect's U49: a container that exits before a verdict fails the wait
/// saying so.
#[tokio::test]
async fn a_container_that_dies_before_a_verdict_fails_the_health_wait() {
    let (result, _) = wait_healthy_through_events(event_line("die")).await;

    assert_eq!(
        result.unwrap_err().to_string(),
        "The container exited before becoming healthy."
    );
}

/// Batect's U49: any other event fails the wait, naming it.
#[tokio::test]
async fn an_unexpected_event_fails_the_health_wait_naming_it() {
    let (result, _) = wait_healthy_through_events(event_line("health_status: starting")).await;

    assert_eq!(
        result.unwrap_err().to_string(),
        "Unexpected event 'health_status: starting' received while waiting for the container to \
         become healthy"
    );
}

/// Batect's U52 (ratect#348): a stream that fails fails the wait saying
/// what it was doing, the stream's own error beneath it.
#[tokio::test]
async fn a_failed_event_stream_fails_the_health_wait_with_its_cause() {
    let (result, _) = wait_healthy_through_events("this is not json\n".to_string()).await;

    let err = result.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Failed to stream Docker events while waiting for the container to become healthy"
    );
    assert!(err.chain().count() > 1, "no cause beneath it: {err:#}");
}

/// Batect's U52: a stream that ends before any event fails the wait saying
/// so, rather than waiting for ever or passing.
#[tokio::test]
async fn an_event_stream_that_ends_without_a_verdict_fails_the_health_wait() {
    let (result, _) = wait_healthy_through_events(String::new()).await;

    assert_eq!(
        result.unwrap_err().to_string(),
        "Docker's event stream ended without reporting a health status"
    );
}

/// Batect's U35, end to end through the daemon (ratect#348): an unhealthy
/// verdict re-inspects the container, and the error explains it from the
/// last health-check run in its health log.
#[tokio::test]
async fn an_unhealthy_verdict_is_explained_from_the_reinspected_health_log() {
    let reinspect = serde_json::json!({
        "Config": {"Healthcheck": {"Test": ["CMD-SHELL", "exit 1"]}},
        "State": {"Health": {"Status": "unhealthy", "Log": [
            {"ExitCode": 0, "Output": "an earlier run"},
            {"ExitCode": 1, "Output": ""},
        ]}},
    });
    let (result, requests) = wait_healthy_against(
        vec![
            (200, INSPECT_WITH_HEALTH_CHECK.to_string()),
            (200, reinspect.to_string()),
        ],
        (200, event_line("health_status: unhealthy")),
    )
    .await;

    assert_eq!(
        result.unwrap_err().to_string(),
        unhealthy_message(Some(health_check_run(1, "")))
    );
    assert_eq!(
        method_and_path(&requests),
        vec![
            ("GET".to_string(), "/containers/db-id/json".to_string()),
            ("GET".to_string(), "/events".to_string()),
            ("GET".to_string(), "/containers/db-id/json".to_string()),
        ]
    );
}

/// Batect's U35 fallback (ratect#328's recorded divergence): when the
/// re-inspect fails, the unhealthy error is the verdict alone — not an
/// inspect error masking it.
#[tokio::test]
async fn an_unhealthy_verdict_whose_reinspect_fails_is_the_verdict_alone() {
    let (result, _) = wait_healthy_against(
        vec![
            (200, INSPECT_WITH_HEALTH_CHECK.to_string()),
            (404, r#"{"message":"No such container: db-id"}"#.to_string()),
        ],
        (200, event_line("health_status: unhealthy")),
    )
    .await;

    let err = result.unwrap_err();
    assert_eq!(format!("{err:#}"), UNHEALTHY_VERDICT);
}

/// Every event posted, in order.
#[derive(Default)]
struct AllEventsSink(std::sync::Mutex<Vec<TaskEvent>>);

impl EventSink for AllEventsSink {
    fn post(&self, event: TaskEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// A terminal left in raw mode has no echo and no line editing, so a user
/// with `RUST_LOG` unset is worse off without being told how to fix it:
/// a printed warning, not a log line (decisions/0017, ratect#614).
#[test]
fn a_terminal_left_in_raw_mode_is_a_warning_naming_the_fix() {
    let _lock = crate::ui::raw_mode::test_lock();
    let sink = std::sync::Arc::new(AllEventsSink::default());
    drop(
        RawModeGuard::enable_with(
            sink.clone(),
            || Ok(()),
            || Err(std::io::Error::other("no tty")),
        )
        .unwrap(),
    );
    let events = sink.0.lock().unwrap().clone();
    let [TaskEvent::Warning { message }] = events.as_slice() else {
        panic!("expected exactly one warning, got {events:?}");
    };
    let message = message.as_str();
    assert!(
        message.contains("raw mode") && message.contains("reset") && message.contains("stty sane"),
        "{message}"
    );
    assert!(!message.contains('\n'), "{message:?}");
}

#[test]
fn a_restored_terminal_posts_nothing() {
    let _lock = crate::ui::raw_mode::test_lock();
    let sink = std::sync::Arc::new(AllEventsSink::default());
    drop(RawModeGuard::enable_with(sink.clone(), || Ok(()), || Ok(())).unwrap());
    assert_eq!(*sink.0.lock().unwrap(), Vec::new());
}

/// Whether raw mode was recorded as on when each event was posted.
#[derive(Default)]
struct RawModeAtPostSink(std::sync::Mutex<Vec<bool>>);

impl EventSink for RawModeAtPostSink {
    fn post(&self, _: TaskEvent) {
        self.0
            .lock()
            .unwrap()
            .push(crate::ui::raw_mode::is_active());
    }
}

/// #613: lines printed during the session end `\r\n` exactly while the
/// guard holds the terminal raw.
#[test]
fn the_guard_records_raw_mode_from_enable_until_drop() {
    let _lock = crate::ui::raw_mode::test_lock();
    let guard = RawModeGuard::enable_with(
        std::sync::Arc::new(AllEventsSink::default()),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert!(crate::ui::raw_mode::is_active());
    drop(guard);
    assert!(!crate::ui::raw_mode::is_active());
}

/// A terminal that failed to restore is still raw, so the warning saying
/// so is printed translated; the record is cleared after it regardless.
#[test]
fn a_failed_restore_warns_while_still_recorded_raw_then_clears() {
    let _lock = crate::ui::raw_mode::test_lock();
    let sink = std::sync::Arc::new(RawModeAtPostSink::default());
    drop(
        RawModeGuard::enable_with(
            sink.clone(),
            || Ok(()),
            || Err(std::io::Error::other("no tty")),
        )
        .unwrap(),
    );
    assert_eq!(*sink.0.lock().unwrap(), vec![true]);
    assert!(!crate::ui::raw_mode::is_active());
}

#[test]
fn a_failed_enable_leaves_raw_mode_unrecorded() {
    let _lock = crate::ui::raw_mode::test_lock();
    let result = RawModeGuard::enable_with(
        std::sync::Arc::new(AllEventsSink::default()),
        || Err(std::io::Error::other("no tty")),
        || Ok(()),
    );
    assert!(result.is_err());
    assert!(!crate::ui::raw_mode::is_active());
}

/// #770: a pull, and the local lookup before it, quote the image escaped,
/// so a line break in it can't add a line to the error.
#[tokio::test]
async fn a_pull_failure_quotes_the_image_with_its_line_break_escaped() {
    let image = "alpine\nError: forged";
    let (client, _) =
        client_for_recording_daemon(|_, _| (500, r#"{"message":"daemon refused"}"#.to_string()))
            .await;

    let pulled = client.pull_image(image).await.unwrap_err();
    let looked_up = client.local_image_id(image).await.unwrap_err();

    assert_eq!(
        pulled.to_string(),
        "Failed to pull image alpine\\nError: forged"
    );
    assert_eq!(
        looked_up.to_string(),
        "Failed to check whether image 'alpine\\nError: forged' exists locally"
    );
}

/// #770: a mount point Ratect takes ownership of is quoted escaped.
#[test]
fn an_owned_directory_refusal_quotes_the_path_with_its_line_break_escaped() {
    let err = build_owned_directory_tar("cache\nError: forged", 1000, 1000).unwrap_err();

    assert_eq!(
        err.to_string(),
        "'cache\\nError: forged' is not an absolute path"
    );
}

/// #770: a `build_ssh` refusal quotes the agent's id escaped.
#[test]
fn a_build_ssh_refusal_quotes_the_id_with_its_line_break_escaped() {
    let dir = std::env::temp_dir().join(format!("ratect-770-ssh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("agent.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let key = dir.join("key");
    std::fs::write(&key, "").unwrap();

    let err = classify_ssh_agent_paths("id\nError: forged", &[socket, key]).unwrap_err();
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        err.to_string()
            .starts_with("The 'build_ssh' agent 'id\\nError: forged' lists both"),
        "{err}"
    );
}
