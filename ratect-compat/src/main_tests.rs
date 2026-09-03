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

#[test]
fn defaults_to_batect_yml_with_no_task() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert_eq!(args.config_file, PathBuf::from("batect.yml"));
    assert!(!args.list_tasks);
    assert_eq!(args.task_name, None);
    assert!(args.additional_args.is_empty());
}

fn args(arguments: &[&str]) -> Args {
    Args::try_parse_from(arguments).expect("should parse")
}

const BINARY: &str = "ratect-compat";
/// `ratect-compat` takes the task name as a trailing positional, after
/// its flags.
const TASK_ARGUMENTS: &[&str] = &["build"];

fn settings_from(arguments: &[&str]) -> TaskEngineSettings {
    args(arguments).engine_settings(PathBuf::from("/p"))
}

/// One flag (with any value it needs) against the single setting it is
/// supposed to move. `--no-cleanup` is deliberately absent: it moves
/// two, and has its own test.
const FLAG_TO_SETTING: &[(&[&str], &str)] = &[
    (&["--use-network", "existing-network"], "existing_network"),
    (&["--disable-ports"], "publish_ports"),
    (
        &["--no-proxy-vars"],
        "propagate_proxy_environment_variables",
    ),
    (&["--skip-prerequisites"], "run_prerequisites"),
    (&["--override-image", "db=postgres:16"], "image_overrides"),
    (&["--tag-image", "app=extra"], "image_tags"),
    (&["--no-cleanup-after-success"], "cleanup_after_success"),
    (&["--no-cleanup-after-failure"], "cleanup_after_failure"),
    (&["--max-parallelism", "3"], "max_parallelism"),
];

/// Which settings differ from the engine's own defaults — the basis of
/// the per-flag test below. `cache`/`ratect_version`/`interrupt` are
/// excluded: all three are always supplied, so they always differ. That
/// they *are* always supplied is asserted separately below, since nothing
/// here would notice one going missing.
fn changed_from_default(settings: &TaskEngineSettings) -> Vec<&'static str> {
    let defaults = TaskEngineSettings::default();
    let mut changed = Vec::new();
    if settings.existing_network != defaults.existing_network {
        changed.push("existing_network");
    }
    if settings.publish_ports != defaults.publish_ports {
        changed.push("publish_ports");
    }
    if settings.propagate_proxy_environment_variables
        != defaults.propagate_proxy_environment_variables
    {
        changed.push("propagate_proxy_environment_variables");
    }
    if settings.run_prerequisites != defaults.run_prerequisites {
        changed.push("run_prerequisites");
    }
    if settings.image_overrides != defaults.image_overrides {
        changed.push("image_overrides");
    }
    if settings.image_tags != defaults.image_tags {
        changed.push("image_tags");
    }
    if settings.cleanup_after_success != defaults.cleanup_after_success {
        changed.push("cleanup_after_success");
    }
    if settings.cleanup_after_failure != defaults.cleanup_after_failure {
        changed.push("cleanup_after_failure");
    }
    if settings.max_parallelism != defaults.max_parallelism {
        changed.push("max_parallelism");
    }
    changed
}

/// Each flag on its own must move its own setting and nothing else.
///
/// This is the test that catches *cross-wiring*, which the all-flags-at-
/// once test above cannot: with `--disable-ports` and `--no-proxy-vars`
/// both set, a field reading the wrong one of the two looks identical
/// to a field reading the right one. Setting one flag at a time and
/// asserting the exact set of changed fields is what tells them apart.
#[test]
fn each_flag_changes_only_its_own_setting() {
    for (flag, expected) in FLAG_TO_SETTING {
        let mut arguments = vec![BINARY];
        arguments.extend_from_slice(flag);
        arguments.extend_from_slice(TASK_ARGUMENTS);
        let settings = settings_from(&arguments);
        assert_eq!(
            changed_from_default(&settings),
            vec![*expected],
            "{flag:?} should change exactly `{expected}`"
        );
    }
}

/// An interrupt tracker must always reach the engine, or a signalled run
/// stops cleaning up after itself — a regression the flag-mapping tests above
/// deliberately can't see, since they only compare against the defaults
/// and `interrupt` is excluded from that comparison. Until this existed,
/// only the `#[ignore]`d Docker test covered the wiring at all.
#[test]
fn an_interrupt_tracker_is_always_supplied_to_the_engine() {
    let settings = args(&["ratect-compat", "build"]).engine_settings(PathBuf::from("/p"));

    assert!(
        settings.interrupt.is_some(),
        "every run must carry an interrupt tracker"
    );
}

/// With nothing asked for, the engine must behave exactly as it would
/// with no settings applied — an inverted boolean would silently change
/// the default behavior of every run.
#[test]
fn no_flags_maps_to_the_engines_own_defaults() {
    let settings = args(&["ratect-compat", "build"]).engine_settings(PathBuf::from("/p"));
    let defaults = TaskEngineSettings::default();

    assert_eq!(settings.existing_network, defaults.existing_network);
    assert_eq!(settings.publish_ports, defaults.publish_ports);
    assert_eq!(
        settings.propagate_proxy_environment_variables,
        defaults.propagate_proxy_environment_variables
    );
    assert_eq!(settings.run_prerequisites, defaults.run_prerequisites);
    assert_eq!(settings.image_overrides, defaults.image_overrides);
    assert_eq!(settings.image_tags, defaults.image_tags);
    assert_eq!(
        settings.cleanup_after_success,
        defaults.cleanup_after_success
    );
    assert_eq!(
        settings.cleanup_after_failure,
        defaults.cleanup_after_failure
    );
    assert_eq!(settings.max_parallelism, defaults.max_parallelism);
    assert_eq!(
        settings.cache,
        Some((ratect_core::cache::CacheType::Volume, PathBuf::from("/p")))
    );
    assert_eq!(
        settings.ratect_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

/// The regression guard for the whole flag surface: every field is set
/// to something the default would never produce, so a field wired to
/// the wrong flag, or a negation dropped, fails here. (A field missing
/// from the literal is a compile error instead — see
/// [`Args::engine_settings`].) It also catches a flag that's declared
/// but never actually read, which nothing else would.
#[test]
fn every_flag_reaches_its_own_engine_setting() {
    let settings = args(&[
        "ratect-compat",
        "--use-network",
        "existing-network",
        "--disable-ports",
        "--no-proxy-vars",
        "--skip-prerequisites",
        "--override-image",
        "db=postgres:16",
        "--tag-image",
        "app=extra",
        "--tag-image",
        "app=second",
        "--no-cleanup",
        "--max-parallelism",
        "3",
        "--cache-type",
        "directory",
        "build",
    ])
    .engine_settings(PathBuf::from("/projects/demo"));

    assert_eq!(
        settings.existing_network.as_deref(),
        Some("existing-network")
    );
    assert!(!settings.publish_ports);
    assert!(!settings.propagate_proxy_environment_variables);
    assert!(!settings.run_prerequisites);
    assert_eq!(
        settings.image_overrides,
        HashMap::from([("db".to_string(), "postgres:16".to_string())])
    );
    assert_eq!(
        settings.image_tags,
        HashMap::from([(
            "app".to_string(),
            HashSet::from(["extra".to_string(), "second".to_string()])
        )]),
        "a container named more than once collects every tag"
    );
    assert!(!settings.cleanup_after_success);
    assert!(!settings.cleanup_after_failure);
    assert_eq!(settings.max_parallelism, Some(3));
    assert_eq!(
        settings.cache,
        Some((
            ratect_core::cache::CacheType::Directory,
            PathBuf::from("/projects/demo")
        ))
    );
    assert_eq!(
        settings.ratect_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

/// `--no-cleanup` is both halves together; each also stands alone, and
/// confusing them would leave containers behind (or not) in exactly the
/// case the user asked about.
#[test]
fn each_no_cleanup_flag_affects_only_its_own_half() {
    let success = args(&["ratect-compat", "--no-cleanup-after-success", "build"])
        .engine_settings(PathBuf::from("/p"));
    assert!(!success.cleanup_after_success);
    assert!(success.cleanup_after_failure);

    let failure = args(&["ratect-compat", "--no-cleanup-after-failure", "build"])
        .engine_settings(PathBuf::from("/p"));
    assert!(failure.cleanup_after_success);
    assert!(!failure.cleanup_after_failure);
}

#[test]
fn parses_list_tasks_flag() {
    let args = Args::try_parse_from(["ratect", "--list-tasks"]).unwrap();
    assert!(args.list_tasks);

    let args = Args::try_parse_from(["ratect", "-T"]).unwrap();
    assert!(args.list_tasks);
}

#[test]
fn parses_custom_config_file() {
    let args = Args::try_parse_from(["ratect", "-f", "custom.yml", "build"]).unwrap();
    assert_eq!(args.config_file, PathBuf::from("custom.yml"));
    assert_eq!(args.task_name.as_deref(), Some("build"));
}

#[test]
fn parses_task_name_and_trailing_args() {
    let args = Args::try_parse_from(["ratect", "build", "--", "--flag", "value"]).unwrap();
    assert_eq!(args.task_name.as_deref(), Some("build"));
    assert_eq!(
        args.additional_args,
        vec!["--flag".to_string(), "value".to_string()]
    );
}

#[test]
fn parses_repeated_config_var_flags() {
    let args = Args::try_parse_from([
        "ratect",
        "--config-var",
        "ENV=prod",
        "--config-var",
        "REGION=eu",
        "build",
    ])
    .unwrap();
    assert_eq!(
        args.config_var,
        vec![
            ("ENV".to_string(), "prod".to_string()),
            ("REGION".to_string(), "eu".to_string()),
        ]
    );
}

#[test]
fn rejects_config_var_without_equals_sign() {
    let result = Args::try_parse_from(["ratect", "--config-var", "NOEQUALS", "build"]);
    assert!(result.is_err());
}

#[test]
fn parses_config_vars_file() {
    let args = Args::try_parse_from(["ratect", "--config-vars-file", "vars.yml", "build"]).unwrap();
    assert_eq!(args.config_vars_file, Some(PathBuf::from("vars.yml")));
}

#[test]
fn defaults_config_var_flags_to_empty() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(args.config_var.is_empty());
    assert_eq!(args.config_vars_file, None);
}

#[test]
fn parses_use_network_flag() {
    let args = Args::try_parse_from(["ratect", "--use-network", "my-network", "build"]).unwrap();
    assert_eq!(args.use_network, Some("my-network".to_string()));
}

#[test]
fn defaults_use_network_to_none() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert_eq!(args.use_network, None);
}

#[test]
fn parses_disable_ports_flag() {
    let args = Args::try_parse_from(["ratect", "--disable-ports", "build"]).unwrap();
    assert!(args.disable_ports);
}

#[test]
fn defaults_disable_ports_to_false() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.disable_ports);
}

#[test]
fn parses_no_proxy_vars_flag() {
    let args = Args::try_parse_from(["ratect", "--no-proxy-vars", "build"]).unwrap();
    assert!(args.no_proxy_vars);
}

#[test]
fn defaults_no_proxy_vars_to_false() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.no_proxy_vars);
}

#[test]
fn parses_skip_prerequisites_flag() {
    let args = Args::try_parse_from(["ratect", "--skip-prerequisites", "build"]).unwrap();
    assert!(args.skip_prerequisites);
}

#[test]
fn defaults_skip_prerequisites_to_false() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.skip_prerequisites);
}

#[test]
fn parses_repeated_override_image_flags() {
    let args = Args::try_parse_from([
        "ratect",
        "--override-image",
        "build-env=alpine:3.18",
        "--override-image",
        "test-env=ubuntu:22.04",
        "build",
    ])
    .unwrap();
    assert_eq!(
        args.override_image,
        vec![
            ("build-env".to_string(), "alpine:3.18".to_string()),
            ("test-env".to_string(), "ubuntu:22.04".to_string()),
        ]
    );
}

#[test]
fn defaults_override_image_to_empty() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(args.override_image.is_empty());
}

#[test]
fn rejects_override_image_without_equals_sign() {
    let result = Args::try_parse_from(["ratect", "--override-image", "NOEQUALS", "build"]);
    assert!(result.is_err());
}

#[test]
fn parses_repeated_tag_image_flags() {
    let args = Args::try_parse_from([
        "ratect",
        "--tag-image",
        "build-env=my.registry/app:v1",
        "--tag-image",
        "build-env=my.registry/app:latest",
        "build",
    ])
    .unwrap();
    assert_eq!(
        args.tag_image,
        vec![
            ("build-env".to_string(), "my.registry/app:v1".to_string()),
            (
                "build-env".to_string(),
                "my.registry/app:latest".to_string()
            ),
        ]
    );
}

#[test]
fn defaults_tag_image_to_empty() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(args.tag_image.is_empty());
}

#[test]
fn rejects_tag_image_without_equals_sign() {
    let result = Args::try_parse_from(["ratect", "--tag-image", "NOEQUALS", "build"]);
    assert!(result.is_err());
}

#[test]
fn parses_no_cleanup_flags() {
    let args = Args::try_parse_from(["ratect", "--no-cleanup", "build"]).unwrap();
    assert!(args.no_cleanup);
    assert!(!args.no_cleanup_after_failure);
    assert!(!args.no_cleanup_after_success);

    let args = Args::try_parse_from(["ratect", "--no-cleanup-after-failure", "build"]).unwrap();
    assert!(args.no_cleanup_after_failure);

    let args = Args::try_parse_from(["ratect", "--no-cleanup-after-success", "build"]).unwrap();
    assert!(args.no_cleanup_after_success);
}

#[test]
fn defaults_no_cleanup_flags_to_false() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.no_cleanup);
    assert!(!args.no_cleanup_after_failure);
    assert!(!args.no_cleanup_after_success);
}

#[test]
fn parses_enable_buildkit_flag() {
    let args = Args::try_parse_from(["ratect", "--enable-buildkit", "build"]).unwrap();
    assert!(args.enable_buildkit);
}

#[test]
fn defaults_enable_buildkit_to_false() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.enable_buildkit);
}

#[test]
fn parses_docker_connection_flags() {
    let args = Args::try_parse_from([
        "ratect",
        "--docker-host",
        "tcp://1.2.3.4:2375",
        "--docker-config",
        "/tmp/docker-config",
        "build",
    ])
    .unwrap();
    assert_eq!(args.docker_host, Some("tcp://1.2.3.4:2375".to_string()));
    assert_eq!(args.docker_context, None);
    assert_eq!(
        args.docker_config,
        Some(PathBuf::from("/tmp/docker-config"))
    );

    let args = Args::try_parse_from(["ratect", "--docker-context", "my-context", "build"]).unwrap();
    assert_eq!(args.docker_context, Some("my-context".to_string()));
}

#[test]
fn defaults_docker_connection_flags_to_none() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert_eq!(args.docker_host, None);
    assert_eq!(args.docker_context, None);
    assert_eq!(args.docker_config, None);
}

#[test]
fn parses_docker_tls_flags() {
    let args = Args::try_parse_from([
        "ratect",
        "--docker-tls-verify",
        "--docker-cert-path",
        "/tmp/certs",
        "--docker-tls-ca-cert",
        "/tmp/ca.pem",
        "--docker-tls-cert",
        "/tmp/cert.pem",
        "--docker-tls-key",
        "/tmp/key.pem",
        "build",
    ])
    .unwrap();
    assert!(!args.docker_tls);
    assert!(args.docker_tls_verify);
    assert_eq!(args.docker_cert_path, Some(PathBuf::from("/tmp/certs")));
    assert_eq!(args.docker_tls_ca_cert, Some(PathBuf::from("/tmp/ca.pem")));
    assert_eq!(args.docker_tls_cert, Some(PathBuf::from("/tmp/cert.pem")));
    assert_eq!(args.docker_tls_key, Some(PathBuf::from("/tmp/key.pem")));

    let args = Args::try_parse_from(["ratect", "--docker-tls", "build"]).unwrap();
    assert!(args.docker_tls);
}

#[test]
fn defaults_docker_tls_flags_to_false_or_none() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.docker_tls);
    assert!(!args.docker_tls_verify);
    assert_eq!(args.docker_cert_path, None);
    assert_eq!(args.docker_tls_ca_cert, None);
    assert_eq!(args.docker_tls_cert, None);
    assert_eq!(args.docker_tls_key, None);
}

#[test]
fn parses_max_parallelism_flag() {
    let args = Args::try_parse_from(["ratect", "--max-parallelism", "4", "build"]).unwrap();
    assert_eq!(args.max_parallelism, Some(4));
}

#[test]
fn defaults_max_parallelism_to_none() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert_eq!(args.max_parallelism, None);
}

#[test]
fn rejects_a_zero_max_parallelism() {
    let result = Args::try_parse_from(["ratect", "--max-parallelism", "0", "build"]);
    assert!(result.is_err());
}

#[test]
fn defaults_cache_type_to_volume() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert_eq!(args.cache_type, CacheTypeArg::Volume);
}

#[test]
fn parses_cache_type_flag() {
    let args = Args::try_parse_from(["ratect", "--cache-type", "directory", "build"]).unwrap();
    assert_eq!(args.cache_type, CacheTypeArg::Directory);

    let args = Args::try_parse_from(["ratect", "--cache-type", "volume", "build"]).unwrap();
    assert_eq!(args.cache_type, CacheTypeArg::Volume);
}

#[test]
fn rejects_an_unknown_cache_type_naming_the_valid_ones() {
    let error = Args::try_parse_from(["ratect", "--cache-type", "host", "build"])
        .unwrap_err()
        .to_string();
    for name in ["volume", "directory"] {
        assert!(error.contains(name), "error should list '{name}': {error}");
    }
}

#[test]
fn parses_clean_flag() {
    let args = Args::try_parse_from(["ratect", "--clean"]).unwrap();
    assert!(args.clean);
}

#[test]
fn defaults_clean_to_false() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.clean);
}

#[test]
fn parses_repeated_clean_cache_flags() {
    let args = Args::try_parse_from([
        "ratect",
        "--clean-cache",
        "gradle-cache",
        "--clean-cache",
        "npm-cache",
    ])
    .unwrap();
    assert_eq!(
        args.clean_cache,
        vec!["gradle-cache".to_string(), "npm-cache".to_string()]
    );
}

#[test]
fn defaults_clean_cache_to_empty() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(args.clean_cache.is_empty());
}

/// `--clean`/`--clean-cache`'s per-item progress line, pinned exactly as
/// Batect's own `CleanupCachesCommand` prints it — nothing else in this
/// suite runs the Docker-backed half of `clean_caches`, so this wording is
/// otherwise proven by nothing `cargo test` executes.
#[test]
fn a_cache_removal_is_reported_in_batects_own_wording() {
    assert_eq!(
        deleting_line(
            ratect_core::cache::CacheType::Volume,
            "batect-cache-abc123-gradle-cache"
        ),
        "Deleting volume 'batect-cache-abc123-gradle-cache'..."
    );
    assert_eq!(
        deleting_line(
            ratect_core::cache::CacheType::Directory,
            "/project/.batect/caches/gradle-cache"
        ),
        "Deleting '/project/.batect/caches/gradle-cache'..."
    );
}

#[test]
fn a_cache_removal_summary_uses_the_singular_for_exactly_one() {
    assert_eq!(
        done_line(ratect_core::cache::CacheType::Volume, 0),
        "Done! Deleted 0 volumes."
    );
    assert_eq!(
        done_line(ratect_core::cache::CacheType::Volume, 1),
        "Done! Deleted 1 volume."
    );
    assert_eq!(
        done_line(ratect_core::cache::CacheType::Volume, 2),
        "Done! Deleted 2 volumes."
    );
    assert_eq!(
        done_line(ratect_core::cache::CacheType::Directory, 1),
        "Done! Deleted 1 directory."
    );
    assert_eq!(
        done_line(ratect_core::cache::CacheType::Directory, 2),
        "Done! Deleted 2 directories."
    );
}

#[tokio::test]
async fn clean_cache_type_directory_short_circuits_before_touching_the_config_file() {
    // `--cache-type directory` makes no Docker connection at all, so
    // this can run as a normal unit test — unlike a `--cache-type
    // volume` clean, which would need a real daemon. A nonexistent
    // config file would normally fail `run` immediately (see the
    // "Configuration file ... not found" check); `--clean` must return
    // `Ok` before ever reaching that check, proving it's a genuine
    // short-circuit, the same way `--upgrade` is (see
    // `upgrade_flag_short_circuits_before_touching_the_config_file`).
    let args = Args::try_parse_from([
        "ratect",
        "--clean",
        "--cache-type",
        "directory",
        "-f",
        "/no/such/batect.yml",
    ])
    .unwrap();
    run(args)
        .await
        .expect("--clean should return Ok without touching the config file");
}

#[test]
fn parses_log_file_flag() {
    let args = Args::try_parse_from(["ratect", "--log-file", "/tmp/ratect.log", "build"]).unwrap();
    assert_eq!(args.log_file, Some(PathBuf::from("/tmp/ratect.log")));
}

#[test]
fn defaults_log_file_to_none() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert_eq!(args.log_file, None);
}

#[test]
fn parses_batect_wrapper_flags_without_erroring() {
    // These have no effect in Ratect (see each field's own doc comment)
    // but must still parse cleanly — a Batect invocation carrying them
    // shouldn't hard-fail just because Ratect doesn't have a
    // self-updating wrapper script to apply them to.
    let args = Args::try_parse_from([
        "ratect",
        "--upgrade",
        "--no-update-notification",
        "--no-wrapper-cache-cleanup",
        "build",
    ])
    .unwrap();
    assert!(args.upgrade);
    assert!(args.no_update_notification);
    assert!(args.no_wrapper_cache_cleanup);
}

#[test]
fn defaults_batect_wrapper_flags_to_false() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.upgrade);
    assert!(!args.no_update_notification);
    assert!(!args.no_wrapper_cache_cleanup);
}

#[tokio::test]
async fn upgrade_flag_short_circuits_before_touching_the_config_file() {
    // A nonexistent config file would normally fail `run` immediately
    // (see the "Configuration file ... not found" check) — `--upgrade`
    // must return `Ok` before ever reaching that check, proving it's a
    // genuine short-circuit rather than a flag that happens to be
    // harmless most of the time.
    let args = Args::try_parse_from(["ratect", "--upgrade", "-f", "/no/such/batect.yml"]).unwrap();
    run(args)
        .await
        .expect("--upgrade should return Ok without touching the config file");
}

#[test]
fn parses_output_style_long_and_short_forms() {
    let args = Args::try_parse_from(["ratect", "--output", "quiet", "build"]).unwrap();
    assert_eq!(args.output, Some(OutputStyleArg::Quiet));
    let args = Args::try_parse_from(["ratect", "-o", "simple", "build"]).unwrap();
    assert_eq!(args.output, Some(OutputStyleArg::Simple));
    let args = Args::try_parse_from(["ratect", "-o", "fancy", "build"]).unwrap();
    assert_eq!(args.output, Some(OutputStyleArg::Fancy));
    let args = Args::try_parse_from(["ratect", "-o", "all", "build"]).unwrap();
    assert_eq!(args.output, Some(OutputStyleArg::All));
}

#[test]
fn defaults_output_style_to_unset_meaning_auto_select() {
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert_eq!(args.output, None);
}

#[test]
fn rejects_an_unknown_output_style_naming_the_valid_ones() {
    let error = Args::try_parse_from(["ratect", "-o", "verbose", "build"])
        .unwrap_err()
        .to_string();
    for name in ["fancy", "simple", "quiet", "all"] {
        assert!(error.contains(name), "error should list '{name}': {error}");
    }
}

#[test]
fn parses_no_color_flag_and_defaults_it_off() {
    let args = Args::try_parse_from(["ratect", "--no-color", "build"]).unwrap();
    assert!(args.no_color);
    let args = Args::try_parse_from(["ratect"]).unwrap();
    assert!(!args.no_color);
}

#[test]
fn fancy_with_no_color_parses_cleanly() {
    // Deliberately *not* a parse error, unlike Batect (whose console
    // couples color and cursor movement — Ratect's doesn't, so
    // colorless fancy is supportable). See docs/differences-from-batect.md.
    let args = Args::try_parse_from(["ratect", "-o", "fancy", "--no-color", "build"]).unwrap();
    assert_eq!(args.output, Some(OutputStyleArg::Fancy));
    assert!(args.no_color);
}

/// Unique empty directory under the system temp dir — the project's own
/// convention (see `ratect-core`'s cache tests) rather than a
/// `tempfile` dev-dependency just for this.
fn unique_temp_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-cvf-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn explicit_config_vars_file_wins_over_the_default() {
    let dir = unique_temp_dir();
    std::fs::write(dir.join("batect.local.yml"), "IGNORED: true").unwrap();
    let explicit = PathBuf::from("chosen.yml");
    assert_eq!(
        resolve_config_vars_file(Some(explicit.clone()), &dir),
        Some(explicit),
        "an explicit --config-vars-file must win even when batect.local.yml exists",
    );
}

#[test]
fn config_vars_file_defaults_to_batect_local_yml_when_present() {
    // Batect's default: batect.local.yml in the current directory, used
    // only when it exists.
    let dir = unique_temp_dir();
    let default = dir.join("batect.local.yml");
    std::fs::write(&default, "FROM_FILE: value").unwrap();
    assert_eq!(resolve_config_vars_file(None, &dir), Some(default));
}

#[test]
fn config_vars_file_default_is_absent_when_batect_local_yml_is_missing() {
    // An absent default file is "no overrides", not an error.
    let dir = unique_temp_dir();
    assert_eq!(resolve_config_vars_file(None, &dir), None);
}
