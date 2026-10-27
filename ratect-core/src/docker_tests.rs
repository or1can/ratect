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
    let options = stub_daemon(
        serde_json::json!({ "ApiVersion": "1.41", "Version": "27.3.1", "Os": "windows" }),
        None,
    )
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
    let options = stub_daemon_for(1, stub_version_body(Some("27.3.1")), None).await;
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

/// A stand-in Docker daemon on a local TCP port, answering the two
/// daemon-information requests `DockerClient` makes: `/version` with
/// `version_body`, and `/_ping` with `builder_version` as its
/// `Builder-Version` header when there is one. Any other path gets a 404.
/// It answers one request per connection, any number of them, until the
/// test's runtime ends; the returned options connect to it in plain HTTP.
async fn stub_daemon(
    version_body: serde_json::Value,
    builder_version: Option<&'static str>,
) -> DockerConnectionOptions {
    stub_daemon_for(usize::MAX, version_body, builder_version).await
}

/// [`stub_daemon`], but answering only its first `connections` requests:
/// after that it stops listening, so the next connection is refused.
async fn stub_daemon_for(
    connections: usize,
    version_body: serde_json::Value,
    builder_version: Option<&'static str>,
) -> DockerConnectionOptions {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let version_body = version_body.to_string();
    tokio::spawn(async move {
        for _ in 0..connections {
            let (mut stream, _) = listener.accept().await.unwrap();
            let version_body = version_body.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buf).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..read]);
                }
                let request = String::from_utf8_lossy(&request);
                let mut request_line = request.split_whitespace();
                let method = request_line.next().unwrap_or_default();
                let path = request_line.next().unwrap_or_default();
                let (status, headers, body) = if path.ends_with("/version") {
                    (
                        "200 OK",
                        "Content-Type: application/json\r\n".to_string(),
                        version_body,
                    )
                } else if path.ends_with("/_ping") {
                    let header = builder_version
                        .map(|version| format!("Builder-Version: {version}\r\n"))
                        .unwrap_or_default();
                    ("200 OK", header, "OK".to_string())
                } else {
                    ("404 Not Found", String::new(), String::new())
                };
                let body = if method == "HEAD" { "" } else { &body };
                let response = format!(
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n\
                     {body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    DockerConnectionOptions {
        host: Some(format!("tcp://127.0.0.1:{port}")),
        ..Default::default()
    }
}

/// Batect's `DockerClientConfigurationFactorySpec`: the Docker config
/// directory connecting chose is the one the client reads `config.json`
/// from, for a build's proxies and the registries it authenticates to.
#[tokio::test]
async fn docker_client_new_reads_config_json_from_the_chosen_config_directory() {
    let config_directory = unique_temp_dir();
    let options = DockerConnectionOptions {
        config_directory: Some(config_directory.clone()),
        ..stub_daemon(stub_version_body(Some("27.3.1")), None).await
    };

    let client = DockerClient::new(&options, |_| {}).await.unwrap();

    assert_eq!(
        client.docker_config_path,
        config_directory.join("config.json")
    );
    std::fs::remove_dir_all(&config_directory).unwrap();
}

/// `stub_daemon`'s `/version` body, reporting `version` as the daemon's
/// own version (none when `None`) and an API version at Ratect's floor.
fn stub_version_body(version: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({ "ApiVersion": "1.41" });
    if let Some(version) = version {
        body["Version"] = version.into();
    }
    body
}

/// A `DockerClient` connected to a `stub_daemon` serving `version_body`
/// and `builder_version`.
async fn client_for_stub_daemon(
    version_body: serde_json::Value,
    builder_version: Option<&'static str>,
) -> DockerClient {
    let options = stub_daemon(version_body, builder_version).await;
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

/// One request a [`recording_daemon`] received: its method, its path with
/// the `/vX.Y` API-version prefix stripped (so `/containers/abc/stop`, not
/// `/v1.41/containers/abc/stop`), its raw query string, empty when it
/// had none, and its body, empty when it had none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedRequest {
    method: String,
    path: String,
    query: String,
    body: String,
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

/// A stand-in Docker daemon that records every request and answers each
/// with whatever `route` returns for its method and (version-stripped)
/// path: an HTTP status and a JSON body. It answers `DockerClient::new`'s
/// own `/version` requests itself, at Ratect's API floor on Linux, so the
/// route only has to cover what the test is about; those are recorded too.
///
/// Returns the options connecting to it and the shared request log. Built
/// for any `DockerClient` method whose request sequence is the contract
/// under test, not only `stop_and_remove_container`'s.
async fn recording_daemon(
    route: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
) -> (
    DockerConnectionOptions,
    std::sync::Arc<std::sync::Mutex<Vec<RecordedRequest>>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let route = std::sync::Arc::new(route);
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let route = route.clone();
            let recorded = recorded.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                let headers_end = loop {
                    if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                        break end + 4;
                    }
                    let read = stream.read(&mut buf).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..read]);
                };
                // A body is read to its `Content-Length`, which is how
                // bollard sends one; a chunked body would be recorded
                // truncated, so it fails here rather than at whatever
                // asserts on it.
                let headers = String::from_utf8_lossy(&request[..headers_end]).into_owned();
                let header = |wanted: &str| {
                    headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case(wanted)
                            .then(|| value.trim().to_string())
                    })
                };
                assert!(
                    header("transfer-encoding").is_none(),
                    "the recording daemon can't read a chunked body:\n{headers}"
                );
                let content_length = header("content-length").map_or(0, |value| {
                    value.parse::<usize>().expect("a numeric Content-Length")
                });
                while request.len() < headers_end + content_length {
                    let read = stream.read(&mut buf).await.unwrap();
                    assert!(read > 0, "the request body ended before its Content-Length");
                    request.extend_from_slice(&buf[..read]);
                }
                let body = String::from_utf8_lossy(&request[headers_end..]).into_owned();
                let request = String::from_utf8_lossy(&request[..headers_end]);
                let mut request_line = request.split_whitespace();
                let method = request_line.next().unwrap_or_default().to_string();
                let target = request_line.next().unwrap_or_default();
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let path = match path.strip_prefix("/v") {
                    Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => {
                        rest.find('/').map_or(path, |slash| &rest[slash..])
                    }
                    _ => path,
                }
                .to_string();
                recorded.lock().unwrap().push(RecordedRequest {
                    method: method.clone(),
                    path: path.clone(),
                    query: query.to_string(),
                    body,
                });
                let (status, body) = if path == "/version" {
                    (200, r#"{"ApiVersion":"1.41","Os":"linux"}"#.to_string())
                } else {
                    route(&method, &path)
                };
                let response = format!(
                    "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (
        DockerConnectionOptions {
            host: Some(format!("tcp://127.0.0.1:{port}")),
            ..Default::default()
        },
        log,
    )
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
                    404 => format!(r#"{{"message":"No such container: {id}"}}"#),
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
    log.lock()
        .unwrap()
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
        serde_json::from_str(&create.body).unwrap(),
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
        .stop_and_remove_container("abc", None, None)
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
        .stop_and_remove_container("abc", None, None)
        .await
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("Failed to stop container 'abc'"),
        "{err:#}"
    );
    assert_eq!(requests_sent(&log), vec![stop_of("abc")]);
}

/// Batect's U31 (ratect#310): stopping a container that's already gone is
/// an error naming both the container and the daemon's own reason, and no
/// removal follows.
#[tokio::test]
async fn stop_and_remove_container_reports_a_missing_container_without_removing() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 404)).await;

    let err = client
        .stop_and_remove_container("abc", None, None)
        .await
        .unwrap_err();

    let chain = format!("{err:#}");
    assert!(chain.contains("Failed to stop container 'abc'"), "{chain}");
    assert!(chain.contains("No such container: abc"), "{chain}");
    assert_eq!(requests_sent(&log), vec![stop_of("abc")]);
}

/// Batect's U32 and NRM-5/ECS-3 (ratect#310): a container created but
/// never started answers its stop with a 304, which isn't a failure — it
/// is still removed.
#[tokio::test]
async fn stop_and_remove_container_removes_a_never_started_container() {
    let (client, log) = client_for_recording_daemon(cleanup_route(|_| 304)).await;

    client
        .stop_and_remove_container("abc", None, None)
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
        .stop_and_remove_container("abc", None, None)
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
        .stop_and_remove_container("abc", None, None)
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
        .stop_and_remove_container("abc", Some("SIGINT"), Some(Duration::from_millis(500)))
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
        .stop_and_remove_container("first", None, None)
        .await
        .is_err());
    client
        .stop_and_remove_container("second", None, None)
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
