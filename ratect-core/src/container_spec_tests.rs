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
use crate::labels::{ContainerRole, RunLabels};
use std::collections::HashMap;

fn sample_container() -> Container {
    Container {
        extends: None,
        build_args: None,
        image: Some("alpine:3.18".to_string()),
        image_pull_policy: None,
        build_directory: None,
        dockerfile: None,
        build_target: None,
        build_secrets: None,
        build_ssh: None,
        volumes: None,
        dependencies: None,
        environment: Some(HashMap::from([("FOO".to_string(), "bar".to_string())])),
        run_as_current_user: None,
        additional_hostnames: Some(vec!["alias".to_string()]),
        additional_hosts: None,
        ports: None,
        working_directory: Some("/app".to_string()),
        command: Some("echo hi".to_string()),
        entrypoint: Some("/bin/sh".to_string()),
        labels: Some(HashMap::from([(
            "team".to_string(),
            "platform".to_string(),
        )])),
        capabilities_to_add: None,
        capabilities_to_drop: None,
        privileged: None,
        shm_size: None,
        devices: None,
        enable_init_process: None,
        log_driver: None,
        log_options: None,
        health_check: None,
        setup_commands: None,
        run_to_completion: None,
        external_health_check: None,
        reports_readiness_for: None,
        stop_signal: None,
        stop_grace_period: None,
        ulimits: None,
        dns: None,
        dns_search: None,
        dns_options: None,
        network_mode: None,
    }
}

fn empty_run() -> TaskRun {
    TaskRun {
        container: "app".to_string(),
        command: None,
        environment: None,
        ports: None,
        working_directory: None,
        entrypoint: None,
    }
}

/// The property this module's own doc comment promises: a task-derived and
/// a dependency-derived spec for the *same* container must agree on
/// `shared` when neither overlay contributes anything — proving the two
/// call sites in `engine.rs` (the task's own container, and
/// `ensure_container_ready`'s dependency start) really do share one
/// assembly path now, rather than two that happen to agree today. Before
/// this candidate, there was no single function whose output both paths
/// could be compared against — each built its own `NetworkOptions`/
/// `ContainerOptions` by hand, so this property could only ever be
/// eyeballed from the two hand-written blocks, not asserted.
#[test]
fn shared_is_identical_between_an_empty_run_overlay_and_no_customise_overlay() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", Some("0.6.0"));
    let run = empty_run();

    let task_spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&run),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: true,
        additional_args: &["extra".to_string()],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    let dependency_spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Customise(None),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Dependency,
        run_labels: &run_labels,
    });

    assert_eq!(task_spec.shared, dependency_spec.shared);

    // The fields deliberately kept *outside* `shared` had better actually
    // vary, or the split above would be proving nothing.
    assert_eq!(task_spec.role, ContainerRole::Task);
    assert_eq!(dependency_spec.role, ContainerRole::Dependency);
    assert!(task_spec.interactive);
    assert!(!dependency_spec.interactive);
    assert_eq!(task_spec.additional_args, vec!["extra".to_string()]);
    assert!(dependency_spec.additional_args.is_empty());
    // `role` is baked into `labels`' own values, so the two must differ
    // here too even though `shared` agrees on everything else.
    assert_ne!(task_spec.labels, dependency_spec.labels);
}

/// A non-empty `Run` overlay overrides the container's own `command`/
/// `working_directory`/`entrypoint`/`environment`/`ports` — exercising
/// every arm of `derive_spec`'s exhaustive `Overlay::Run` destructure.
#[test]
fn a_run_overlay_overrides_the_containers_own_fields() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", None);
    let run = TaskRun {
        container: "app".to_string(),
        command: Some("echo overridden".to_string()),
        environment: Some(HashMap::from([(
            "FOO".to_string(),
            "overridden".to_string(),
        )])),
        ports: None,
        working_directory: Some("/overridden".to_string()),
        entrypoint: Some("/overridden-entrypoint".to_string()),
    };

    let spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&run),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    assert_eq!(spec.shared.command, Some("echo overridden".to_string()));
    assert_eq!(
        spec.shared.environment,
        Some(HashMap::from([(
            "FOO".to_string(),
            "overridden".to_string()
        )]))
    );
    assert_eq!(
        spec.shared.options.working_directory,
        Some("/overridden".to_string())
    );
    assert_eq!(
        spec.shared.options.entrypoint,
        Some("/overridden-entrypoint".to_string())
    );
}

/// A `Customise` overlay overrides `working_directory`/`environment`/
/// `ports`, but has no `command`/`entrypoint` field to override with at
/// all (matching Batect's own `TaskContainerCustomisation`) — so those two
/// always fall through to the container's own value regardless.
#[test]
fn a_customise_overlay_has_no_command_or_entrypoint_override() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", None);
    let customisation = TaskContainerCustomisation {
        environment: Some(HashMap::from([(
            "FOO".to_string(),
            "customised".to_string(),
        )])),
        ports: None,
        working_directory: Some("/customised".to_string()),
    };

    let spec = derive_spec(ContainerSpecInputs {
        name: "database",
        container_config: &container_config,
        overlay: Overlay::Customise(Some(&customisation)),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Dependency,
        run_labels: &run_labels,
    });

    // Overridden.
    assert_eq!(
        spec.shared.environment,
        Some(HashMap::from([(
            "FOO".to_string(),
            "customised".to_string()
        )]))
    );
    assert_eq!(
        spec.shared.options.working_directory,
        Some("/customised".to_string())
    );
    // Not overridden — the container's own values pass through untouched.
    assert_eq!(spec.shared.command, container_config.command);
    assert_eq!(spec.shared.options.entrypoint, container_config.entrypoint);
}

/// A build's own `dockerfile`/`build_target` pass straight through, and a
/// missing `dockerfile` defaults to `"Dockerfile"` — matching the default
/// case documented on `ContainerRuntime::build_image` before this candidate.
#[test]
fn derive_build_spec_maps_dockerfile_target_and_defaults() {
    let mut container_config = sample_container();
    container_config.dockerfile = Some("Dockerfile.build".to_string());
    container_config.build_target = Some("builder".to_string());

    let spec = derive_build_spec(&container_config, "/project/docker", "demo-app", None).unwrap();

    assert_eq!(spec.build_directory, PathBuf::from("/project/docker"));
    assert_eq!(spec.dockerfile, "Dockerfile.build");
    assert_eq!(spec.target, Some("builder".to_string()));
    assert_eq!(spec.tag, "demo-app");

    let mut no_dockerfile = sample_container();
    no_dockerfile.dockerfile = None;
    let spec = derive_build_spec(&no_dockerfile, "/project/docker", "demo-app", None).unwrap();
    assert_eq!(spec.dockerfile, "Dockerfile");
}

/// Batect's second use of `image_pull_policy`: only `Always` force-pulls a
/// build's own base image; the default (`IfNotPresent`, including an unset
/// policy) does not.
#[test]
fn derive_build_spec_force_pulls_only_under_the_always_policy() {
    let mut container_config = sample_container();
    container_config.image_pull_policy = Some(crate::config::ImagePullPolicy::Always);
    let spec = derive_build_spec(&container_config, "/project", "tag", None).unwrap();
    assert!(spec.force_pull);

    let mut container_config = sample_container();
    container_config.image_pull_policy = Some(crate::config::ImagePullPolicy::IfNotPresent);
    let spec = derive_build_spec(&container_config, "/project", "tag", None).unwrap();
    assert!(!spec.force_pull);

    let mut container_config = sample_container();
    container_config.image_pull_policy = None;
    let spec = derive_build_spec(&container_config, "/project", "tag", None).unwrap();
    assert!(!spec.force_pull);
}

/// `buildkit` is `None` when neither `build_secrets` nor `build_ssh` is
/// set, and `Some` — carrying the classified secret sources — when either
/// is.
#[test]
fn derive_build_spec_includes_buildkit_only_when_secrets_or_ssh_are_set() {
    let container_config = sample_container();
    let spec = derive_build_spec(&container_config, "/project", "tag", None).unwrap();
    assert_eq!(spec.buildkit, None);

    let mut with_secrets = sample_container();
    with_secrets.build_secrets = Some(HashMap::from([(
        "token".to_string(),
        crate::config::BuildSecret::Environment("TOKEN".to_string()),
    )]));
    let spec = derive_build_spec(&with_secrets, "/project", "tag", None).unwrap();
    let buildkit = spec.buildkit.expect("build_secrets requires BuildKit");
    assert_eq!(
        buildkit.secrets.get("token"),
        Some(&crate::docker::BuildSecretSource::Environment(
            "TOKEN".to_string()
        ))
    );
    assert!(buildkit.ssh_agents.is_empty());
}

/// A container whose only `build_secrets` entry, `cert`, reads `path`.
fn container_with_secret_file(path: &std::path::Path) -> Container {
    let mut container_config = sample_container();
    container_config.build_secrets = Some(HashMap::from([(
        "cert".to_string(),
        crate::config::BuildSecret::Path(path.display().to_string()),
    )]));
    container_config
}

/// #315: a file secret is checked before the build, as Batect does, rather
/// than left to BuildKit — which reads it only if a `RUN` step mounts it, so
/// a missing one that nothing mounts would build and run without complaint.
#[test]
fn derive_build_spec_refuses_a_secret_file_that_does_not_exist() {
    let missing = std::env::temp_dir().join(format!("ratect-315-{}", uuid::Uuid::new_v4()));
    let err = derive_build_spec(
        &container_with_secret_file(&missing),
        "/project",
        "tag",
        None,
    )
    .expect_err("a missing secret file should fail before the build");
    assert_eq!(
        format!("{err:#}"),
        format!(
            "The 'build_secrets' entry 'cert' reads '{}', which does not exist",
            missing.display()
        )
    );
}

/// #315: anything that isn't a regular file — here a directory — is refused
/// too. (Batect's own message for this case says "is not a directory", the
/// opposite of what it checks; this one says what it means.)
#[test]
fn derive_build_spec_refuses_a_secret_path_that_is_not_a_file() {
    let directory = std::env::temp_dir();
    let err = derive_build_spec(
        &container_with_secret_file(&directory),
        "/project",
        "tag",
        None,
    )
    .expect_err("a directory secret should fail before the build");
    assert_eq!(
        format!("{err:#}"),
        format!(
            "The 'build_secrets' entry 'cert' reads '{}', which is not a file",
            directory.display()
        )
    );
}

/// #315: of several bad entries, the same one is reported every run — the
/// first by id — rather than whichever the map happens to yield first.
#[test]
fn derive_build_spec_reports_the_first_bad_secret_by_id() {
    let missing = std::env::temp_dir().join(format!("ratect-315-{}", uuid::Uuid::new_v4()));
    let mut container_config = sample_container();
    container_config.build_secrets = Some(
        ('a'..='z')
            .map(|id| {
                (
                    id.to_string(),
                    crate::config::BuildSecret::Path(missing.display().to_string()),
                )
            })
            .collect(),
    );
    let err = derive_build_spec(&container_config, "/project", "tag", None).unwrap_err();
    assert!(
        format!("{err:#}").starts_with("The 'build_secrets' entry 'a' "),
        "{err:#}"
    );
}

/// #315: an existing regular file passes through unchanged.
#[test]
fn derive_build_spec_accepts_a_secret_file_that_exists() {
    let file = std::env::temp_dir().join(format!("ratect-315-{}", uuid::Uuid::new_v4()));
    std::fs::write(&file, "secret").unwrap();
    let result = derive_build_spec(&container_with_secret_file(&file), "/project", "tag", None);
    std::fs::remove_file(&file).unwrap();
    let buildkit = result
        .unwrap()
        .buildkit
        .expect("build_secrets requires BuildKit");
    assert_eq!(
        buildkit.secrets.get("cert"),
        Some(&crate::docker::BuildSecretSource::File(file))
    );
}

/// `build_args` merges proxy-derived variables with the container's own
/// (matching today's call, `term_var`/`overlay_env` both absent), and
/// `proxy_host_gateway` carries the same rewritten-URL entry `NetworkOptions`
/// does — both derived from the same `proxy` argument.
#[test]
fn derive_build_spec_merges_proxy_vars_and_carries_the_host_gateway() {
    let mut container_config = sample_container();
    container_config.build_args = Some(HashMap::from([(
        "VERSION".to_string(),
        "1.2.3".to_string(),
    )]));

    let proxy = crate::proxy::proxy_environment_variables(
        |name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()),
        &std::collections::BTreeSet::new(),
    );

    let spec = derive_build_spec(&container_config, "/project", "tag", Some(&proxy)).unwrap();

    let build_args = spec
        .build_args
        .expect("proxy and config build_args are both set");
    assert_eq!(build_args.get("VERSION"), Some(&"1.2.3".to_string()));
    assert_eq!(
        build_args.get("http_proxy"),
        Some(&"http://host.docker.internal:3333/".to_string())
    );
    assert_eq!(
        spec.proxy_host_gateway,
        Some(crate::proxy::HostGateway {
            name: "host.docker.internal",
            address: "host-gateway",
        })
    );
}

/// A container's `ulimits` reach `ContainerOptions` as plain
/// `(name, soft, hard)` triples, config types left behind at this boundary
/// exactly as `devices` are (ratect#95).
#[test]
fn derive_spec_carries_ulimits_as_plain_triples() {
    let mut container_config = sample_container();
    container_config.ulimits = Some(vec![
        crate::config::Ulimit {
            name: crate::config::UlimitResource::Nofile,
            soft: 1024,
            hard: 2048,
        },
        crate::config::Ulimit {
            name: crate::config::UlimitResource::Core,
            soft: -1,
            hard: -1,
        },
    ]);
    let run_labels = RunLabels::new("demo", "task", "run-id", None);

    let spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&empty_run()),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    assert_eq!(
        spec.shared.options.ulimits,
        Some(vec![
            ("nofile".to_string(), 1024, 2048),
            ("core".to_string(), -1, -1),
        ])
    );
}

/// A container with no `ulimits` leaves the field `None`, so container
/// creation is byte-for-byte what it was before ratect#95.
#[test]
fn derive_spec_leaves_ulimits_unset_when_the_container_declares_none() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", None);

    let spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&empty_run()),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    assert_eq!(spec.shared.options.ulimits, None);
}

/// A container's `dns`/`dns_search`/`dns_options` (ratect#105) reach
/// `ContainerOptions` unchanged — and the task's `run` overlay has no say in
/// them.
#[test]
fn derive_spec_carries_the_containers_own_dns_settings() {
    let mut container_config = sample_container();
    container_config.dns = Some(vec![
        "1.1.1.1".to_string(),
        "2606:4700:4700::1111".to_string(),
    ]);
    container_config.dns_search = Some(vec!["corp.example.com".to_string()]);
    container_config.dns_options = Some(vec!["ndots:2".to_string()]);
    let run_labels = RunLabels::new("demo", "task", "run-id", None);

    let spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&empty_run()),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    let options = &spec.shared.options;
    assert_eq!(
        options.dns,
        Some(vec![
            "1.1.1.1".to_string(),
            "2606:4700:4700::1111".to_string()
        ])
    );
    assert_eq!(
        options.dns_search,
        Some(vec!["corp.example.com".to_string()])
    );
    assert_eq!(options.dns_options, Some(vec!["ndots:2".to_string()]));
}

/// A container that sets none of the three leaves all three `None`, so
/// container creation is byte-for-byte what it was before ratect#105.
#[test]
fn derive_spec_leaves_dns_unset_when_the_container_declares_none() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", None);

    let spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&empty_run()),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    let options = &spec.shared.options;
    assert_eq!(options.dns, None);
    assert_eq!(options.dns_search, None);
    assert_eq!(options.dns_options, None);
}

/// `resolve_network` (ratect#106): a container with no `network_mode` joins
/// the task's network by name; `host`/`none` pass straight through; and
/// `container:<name>` becomes that dependency's running container id, since
/// Ratect's containers are anonymous and Docker can't address them by name.
#[test]
fn resolve_network_maps_each_mode_onto_what_docker_is_given() {
    let dependency_ids = HashMap::from([("peer".to_string(), "peer-id".to_string())]);

    assert_eq!(
        resolve_network(None, "ratect-run-id", &dependency_ids).unwrap(),
        ContainerNetwork::Task("ratect-run-id".to_string())
    );
    assert_eq!(
        resolve_network(Some(&NetworkMode::Host), "ratect-run-id", &dependency_ids).unwrap(),
        ContainerNetwork::Host
    );
    assert_eq!(
        resolve_network(Some(&NetworkMode::None), "ratect-run-id", &dependency_ids).unwrap(),
        ContainerNetwork::None
    );
    assert_eq!(
        resolve_network(
            Some(&NetworkMode::Container("peer".to_string())),
            "ratect-run-id",
            &dependency_ids
        )
        .unwrap(),
        ContainerNetwork::Container("peer-id".to_string())
    );
}

/// The loader guarantees a `container:` target is a direct dependency, so
/// its id is always there — but a missing one is an error naming the
/// target, not a panic.
#[test]
fn resolve_network_errors_when_the_container_target_has_no_running_id() {
    let err = resolve_network(
        Some(&NetworkMode::Container("peer".to_string())),
        "ratect-run-id",
        &HashMap::new(),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("'peer'"), "{err:#}");
}

fn spec_on_network(network: ContainerNetwork) -> ContainerSpec {
    let mut container_config = sample_container();
    container_config.additional_hostnames = None;
    let proxy = crate::proxy::proxy_environment_variables(
        |name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()),
        &std::collections::BTreeSet::new(),
    );
    let run_labels = RunLabels::new("demo", "task", "run-id", None);
    derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&empty_run()),
        image: "alpine:3.18",
        network,
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: Some(&proxy),
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    })
}

/// A container sharing another's namespace uses that container's
/// `/etc/hosts`, and Docker refuses extra hosts alongside `container:` — so
/// the proxy's host-gateway entry is dropped there, and only there.
#[test]
fn derive_spec_drops_the_proxy_host_gateway_only_when_sharing_a_namespace() {
    let gateway = Some(crate::proxy::HostGateway {
        name: "host.docker.internal",
        address: "host-gateway",
    });
    for network in [
        ContainerNetwork::Task("ratect-run-id".to_string()),
        ContainerNetwork::Host,
        ContainerNetwork::None,
    ] {
        assert_eq!(
            spec_on_network(network.clone())
                .shared
                .network_options
                .proxy_host_gateway,
            gateway,
            "{network:?}"
        );
    }
    let spec = spec_on_network(ContainerNetwork::Container("peer-id".to_string()));
    assert_eq!(spec.shared.network_options.proxy_host_gateway, None);
    assert_eq!(
        spec.shared.network,
        ContainerNetwork::Container("peer-id".to_string())
    );
}

/// An unquoted `environment` value in a `batect.yml` reaches the container
/// as written, not as the number it looks like (#378): `3.10` is a Python
/// version, and Batect passes it on as `3.10`, never `3.1`.
#[test]
fn derive_spec_passes_an_unquoted_yaml_environment_value_on_as_written() {
    let config: crate::config::Config = crate::yaml::from_str(
        "project_name: demo\n\
         containers:\n\
         \x20 app:\n\
         \x20   image: alpine:3.18\n\
         \x20   environment:\n\
         \x20     PYTHON_VERSION: 3.10\n\
         tasks: {}\n",
    )
    .unwrap();
    let run_labels = RunLabels::new("demo", "task", "run-id", None);

    let spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &config.containers["app"],
        overlay: Overlay::Run(&empty_run()),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    assert_eq!(spec.shared.environment.unwrap()["PYTHON_VERSION"], "3.10");
}

/// An unquoted `build_args` value in a `batect.yml` reaches the build as
/// written (#376): Batect passes `3.10` on as `3.10`, where Ratect used to
/// refuse the file — and reformatting it would have built Python 3.1.
#[test]
fn derive_build_spec_passes_an_unquoted_yaml_build_arg_on_as_written() {
    let config: crate::config::Config = crate::yaml::from_str(
        "project_name: demo\n\
         containers:\n\
         \x20 app:\n\
         \x20   build_directory: docker\n\
         \x20   build_args:\n\
         \x20     PYTHON_VERSION: 3.10\n\
         tasks: {}\n",
    )
    .unwrap();

    let spec =
        derive_build_spec(&config.containers["app"], "/project/docker", "tag", None).unwrap();

    assert_eq!(spec.build_args.unwrap()["PYTHON_VERSION"], "3.10");
}

/// #417 (Batect's TSF-15): a `customise` entry that sets nothing leaves its
/// container exactly as no entry would — the equivalence [`Overlay`]'s own
/// doc comment promises. The container sets every field a customisation
/// could override, so a fallback that went wrong would show.
#[test]
fn an_empty_customisation_derives_the_same_spec_as_none() {
    let mut container_config = sample_container();
    container_config.ports = Some(vec![crate::config::PortMapping {
        local: crate::config::PortRange {
            from: 8080,
            to: 8080,
        },
        container: crate::config::PortRange { from: 80, to: 80 },
        protocol: "tcp".to_string(),
    }]);
    let run_labels = RunLabels::new("demo", "task", "run-id", Some("0.6.0"));
    let empty = TaskContainerCustomisation {
        environment: None,
        ports: None,
        working_directory: None,
    };
    let spec = |overlay| {
        derive_spec(ContainerSpecInputs {
            name: "db",
            container_config: &container_config,
            overlay,
            image: "alpine:3.18",
            network: ContainerNetwork::Task("ratect-run-id".to_string()),
            interactive: false,
            additional_args: &[],
            user_mapping: None,
            volumes: None,
            term_var: None,
            proxy: None,
            publish_ports: true,
            role: ContainerRole::Dependency,
            run_labels: &run_labels,
        })
    };

    assert_eq!(
        spec(Overlay::Customise(Some(&empty))),
        spec(Overlay::Customise(None))
    );
}

fn port_mappings(mappings: &[&str]) -> Option<Vec<crate::config::PortMapping>> {
    Some(
        mappings
            .iter()
            .map(|mapping| serde_json::from_value(serde_json::json!(mapping)).unwrap())
            .collect(),
    )
}

/// The `(local, container, protocol)` triples a task's container publishes,
/// given its own `ports` and the task's `run.ports`.
fn published_ports(container_ports: &[&str], run_ports: &[&str]) -> Vec<(u16, u16, String)> {
    let mut container_config = sample_container();
    container_config.ports = port_mappings(container_ports);
    let run = TaskRun {
        ports: port_mappings(run_ports),
        ..empty_run()
    };
    let run_labels = RunLabels::new("demo", "task", "run-id", Some("0.6.0"));
    derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&run),
        image: "alpine:3.18",
        network: ContainerNetwork::Task("ratect-run-id".to_string()),
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    })
    .shared
    .network_options
    .ports
    .unwrap_or_default()
}

fn tcp(local: u16, container: u16) -> (u16, u16, String) {
    (local, container, "tcp".to_string())
}

/// Batect unions a container's ports with the task's as a set, so the same
/// mapping in both is published once (ratect#390).
#[test]
fn a_port_in_both_the_container_and_the_run_is_published_once() {
    assert_eq!(
        published_ports(&["8080:80"], &["8080:80"]),
        vec![tcp(8080, 80)]
    );
}

/// Compared after a range is expanded, so a single mapping that a range
/// also covers collapses into it.
#[test]
fn a_port_a_range_also_covers_is_published_once() {
    assert_eq!(
        published_ports(&["8080:80", "8080-8081:80-81"], &[]),
        vec![tcp(8080, 80), tcp(8081, 81)]
    );
}

#[test]
fn distinct_ports_keep_the_order_they_were_listed_in() {
    assert_eq!(
        published_ports(&["9000:90", "8000:80"], &["7000:70"]),
        vec![tcp(9000, 90), tcp(8000, 80), tcp(7000, 70)]
    );
}

/// ratect#291: one container port on two host ports is two mappings, not a
/// duplicate — both reach `docker.rs`, whichever side each came from.
#[test]
fn one_container_port_on_two_host_ports_from_the_container_and_the_run_keeps_both() {
    assert_eq!(
        published_ports(&["9000:80"], &["9001:80"]),
        vec![tcp(9000, 80), tcp(9001, 80)]
    );
}
