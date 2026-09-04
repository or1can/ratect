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
use crate::docker::{ContainerRuntime, LabelledResource};
use std::collections::HashMap;

/// Builds a `Config` the way a real invocation does — through
/// `load_project_native` on an actual file — rather than by parsing YAML
/// here, which would duplicate knowledge that belongs to `config.rs`. It
/// also means `build_directory` paths are resolved exactly as they will be
/// at run time, which one of these checks depends on.
async fn config_with(yaml: &str) -> Config {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "ratect-doctor-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        count
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("batect.yml");
    std::fs::write(&path, yaml).unwrap();

    let project = crate::config::load_project_native(&path, &HashMap::new())
        .await
        .expect("fixture config should load");
    std::fs::remove_dir_all(&directory).unwrap();
    project.config
}

/// Docker treats a missing tag as `latest`, so both are the same
/// reproducibility hazard — and a registry port is a colon that isn't
/// a tag, which is the case that makes this worth a function.
#[test]
fn a_floating_image_tag_is_latest_or_no_tag_at_all() {
    assert!(floating_image_tag("alpine"));
    assert!(floating_image_tag("alpine:latest"));
    assert!(floating_image_tag("registry.example.com/team/app"));
    assert!(floating_image_tag("registry.example.com:5000/team/app"));

    assert!(!floating_image_tag("alpine:3.18.2"));
    assert!(!floating_image_tag(
        "registry.example.com:5000/team/app:1.2.3"
    ));
    assert!(!floating_image_tag(
        "alpine@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    ));
}

#[tokio::test]
async fn doctor_warns_about_floating_tags_and_unguarded_dependencies() {
    let config = config_with(
        r#"
project_name: demo
containers:
  database:
    image: postgres
  cache:
    image: redis:7-alpine
  app:
    image: alpine:3.18.2
    dependencies:
      - database
      - cache
tasks:
  test:
    run:
      container: app
      command: echo hi
"#,
    )
    .await;

    let findings = config_findings(&config);
    let messages: Vec<String> = findings
        .iter()
        .map(|finding| finding.render().trim().to_string())
        .collect();

    assert!(
        messages
            .iter()
            .any(|m| m.contains("'database'") && m.contains("floating image tag")),
        "an untagged image is a floating tag: {messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("'cache'") && m.contains("floating")),
        "a pinned tag shouldn't be warned about: {messages:?}"
    );
    // Both dependencies lack a health check; the task's own container
    // isn't a dependency and so isn't gating anything.
    assert!(messages
        .iter()
        .any(|m| m.contains("'cache'") && m.contains("health_check")));
    assert!(messages
        .iter()
        .any(|m| m.contains("'database'") && m.contains("health_check")));
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("'app'") && m.contains("health_check")),
        "the task's own container gates nothing: {messages:?}"
    );
    assert!(
        findings.iter().all(|f| !matches!(f, Finding::Problem(_))),
        "none of this stops a run: {messages:?}"
    );
}

/// The marker Batect's authors put near the top of both wrapper forms
/// — the thing that tells a still-runs-Batect script from one already
/// repointed at Ratect.
#[test]
fn a_batect_wrapper_is_recognized_by_its_own_notice_line() {
    // The real Unix and Windows headers, trimmed to the marker line.
    assert!(is_batect_wrapper(
        "#!/usr/bin/env bash\n# This file is part of Batect.\n# Do not modify...\n"
    ));
    assert!(is_batect_wrapper(
        "@echo off\nrem This file is part of Batect.\nrem Do not modify...\n"
    ));

    // Anything that no longer runs Batect must not be flagged, however
    // it got that way: a hand-written shim that execs ratect-compat, or
    // a symlink to the ratect-compat binary (read as binary bytes).
    // Flagging one would mean flagging a finished migration.
    assert!(!is_batect_wrapper("#!/bin/sh\nexec ratect-compat \"$@\"\n"));
    assert!(!is_batect_wrapper("\u{7f}ELF\u{2}\u{1}\u{1}\u{0}"));
    assert!(!is_batect_wrapper(""));
}

/// The filesystem half: a leftover wrapper in the project directory is
/// a warning (it still works, and that's the trap), never a problem.
#[test]
fn a_leftover_wrapper_in_the_project_directory_is_warned_about() {
    let directory = std::env::temp_dir().join(format!(
        "ratect-wrapper-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("batect"),
        "#!/usr/bin/env bash\n# This file is part of Batect.\n",
    )
    .unwrap();
    // A same-named file that isn't the wrapper mustn't be flagged.
    std::fs::write(directory.join("batect.cmd"), "echo not really batect\n").unwrap();

    let findings = wrapper_script_findings(&directory);
    std::fs::remove_dir_all(&directory).unwrap();

    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(matches!(
        &findings[0],
        Finding::Warning(message) if message.contains("'batect'") && message.contains("still runs Batect")
    ));
}

#[test]
fn a_project_with_no_wrapper_scripts_is_not_warned() {
    let directory = std::env::temp_dir().join(format!(
        "ratect-nowrapper-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    assert!(wrapper_script_findings(&directory).is_empty());
    std::fs::remove_dir_all(&directory).unwrap();
}

/// A build directory that isn't there fails the run, so it's a problem
/// rather than a warning — and `doctor` exits non-zero on those, which
/// is what makes it usable as a CI step.
#[tokio::test]
async fn a_missing_build_directory_is_a_problem() {
    let config = config_with(
        r#"
project_name: demo
containers:
  app:
    build_directory: /nonexistent/build/context
tasks:
  test:
    run:
      container: app
      command: echo hi
"#,
    )
    .await;

    let findings = config_findings(&config);
    assert!(
            findings.iter().any(|finding| matches!(
                finding,
                Finding::Problem(message) if message.contains("build_directory") && message.contains("doesn't exist")
            )),
            "{findings:?}"
        );
}

/// A container named only by a *task*'s `dependencies` gates that task
/// just as much as a container-level one.
#[tokio::test]
async fn a_task_level_dependency_counts_as_a_dependency() {
    let config = config_with(
        r#"
project_name: demo
containers:
  queue:
    image: redis:7-alpine
  app:
    image: alpine:3.18.2
tasks:
  test:
    run:
      container: app
      command: echo hi
    dependencies:
      - queue
"#,
    )
    .await;

    assert_eq!(dependency_names(&config), vec!["queue"]);
}

/// Implements only what `leftover_finding` reaches through
/// `resources::find` — a fake scoped to this module, not shared with
/// `resources_tests.rs`'s: each test module's fake answers only what that
/// module calls, and both implement the same compiler-checked trait, so
/// neither can silently drift from `ContainerRuntime`'s real shape.
#[derive(Default)]
struct FakeRuntime {
    containers: Vec<LabelledResource>,
    networks: Vec<LabelledResource>,
}

#[async_trait::async_trait]
impl ContainerRuntime for FakeRuntime {
    async fn list_containers(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> anyhow::Result<Vec<LabelledResource>> {
        Ok(self.containers.clone())
    }

    async fn list_networks(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> anyhow::Result<Vec<LabelledResource>> {
        Ok(self.networks.clone())
    }

    async fn stop_and_remove_container(&self, _container_id: &str) -> anyhow::Result<()> {
        unimplemented!("doctor never removes a container")
    }

    async fn remove_network(&self, _name: &str) -> anyhow::Result<()> {
        unimplemented!("doctor never removes a network")
    }

    async fn pull_image(&self, _image: &str) -> anyhow::Result<()> {
        unimplemented!("doctor never pulls an image")
    }

    async fn image_exists_locally(&self, _image: &str) -> anyhow::Result<bool> {
        unimplemented!("doctor never inspects an image")
    }

    async fn build_image(
        &self,
        _build_directory: &std::path::Path,
        _dockerfile: &str,
        _build_args: Option<&HashMap<String, String>>,
        _target: Option<&str>,
        _buildkit: Option<&crate::docker::BuildKitOptions>,
        _tag: &str,
        _force_pull: bool,
        _proxy_host_gateway: Option<crate::proxy::HostGateway>,
    ) -> anyhow::Result<String> {
        unimplemented!("doctor never builds an image")
    }

    async fn tag_image(&self, _image_id: &str, _tags: &[String]) -> anyhow::Result<()> {
        unimplemented!("doctor never tags an image")
    }

    async fn create_network(
        &self,
        _name: &str,
        _labels: &HashMap<String, String>,
    ) -> anyhow::Result<()> {
        unimplemented!("doctor never creates a network")
    }

    async fn network_exists(&self, _name: &str) -> anyhow::Result<bool> {
        unimplemented!("doctor never checks for a network")
    }

    async fn start_background_container(
        &self,
        _spec: &crate::container_spec::ContainerSpec,
    ) -> anyhow::Result<String> {
        unimplemented!("doctor never starts a container")
    }

    async fn wait_for_container_healthy(&self, _container_id: &str) -> anyhow::Result<()> {
        unimplemented!("doctor never waits on a container")
    }

    async fn exec_in_container(
        &self,
        _container_id: &str,
        _command: &str,
        _working_directory: Option<&str>,
        _environment: Option<&HashMap<String, String>>,
        _user_mapping: Option<&crate::docker::UserMapping>,
    ) -> anyhow::Result<crate::docker::ExecResult> {
        unimplemented!("doctor never execs in a container")
    }

    async fn run_container(
        &self,
        _spec: &crate::container_spec::ContainerSpec,
        _created: Option<tokio::sync::oneshot::Sender<String>>,
        _started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> anyhow::Result<()> {
        unimplemented!("doctor never runs a container")
    }

    async fn list_volumes(&self) -> anyhow::Result<Vec<String>> {
        unimplemented!("doctor never lists volumes")
    }

    async fn remove_volume(&self, _name: &str) -> anyhow::Result<()> {
        unimplemented!("doctor never removes a volume")
    }
}

fn resource(id: &str, project: &str) -> LabelledResource {
    LabelledResource {
        id: id.to_string(),
        name: id.to_string(),
        labels: HashMap::from([(crate::labels::PROJECT.to_string(), project.to_string())]),
        created: Some(0),
        state: Some("running".to_string()),
    }
}

/// No connection at all — the connection finding already said so, so this
/// contributes nothing rather than repeating it.
#[tokio::test]
async fn no_docker_connection_means_no_leftover_finding() {
    let docker: Option<&FakeRuntime> = None;
    assert_eq!(leftover_finding(docker, "demo", 1_000).await, None);
}

/// `doctor` reports the leftover count through the exact same selection
/// `resources list` uses — the two must never be able to disagree about
/// what counts as a leftover. Cross-project exclusion itself is
/// `resources_tests.rs`'s to prove; this only proves the two callers agree
/// on whatever `resources::find` returns.
#[tokio::test]
async fn a_leftover_count_matches_what_resources_find_returns() {
    let docker = FakeRuntime {
        containers: vec![resource("c1", "demo"), resource("c2", "demo")],
        networks: vec![resource("n1", "demo")],
    };

    let expected = crate::resources::find(&docker, Some("demo"), None, 1_000)
        .await
        .unwrap();
    assert_eq!(expected.len(), 3);

    let finding = leftover_finding(Some(&docker), "demo", 1_000)
        .await
        .unwrap();
    assert!(matches!(
        &finding,
        Finding::Warning(message) if message.contains("3 resource(s)")
    ));
}

#[tokio::test]
async fn no_leftovers_is_a_fine_finding() {
    let docker = FakeRuntime::default();
    let finding = leftover_finding(Some(&docker), "demo", 1_000)
        .await
        .unwrap();
    assert_eq!(
        finding,
        Finding::Fine("no leftovers from previous runs".to_string())
    );
}
