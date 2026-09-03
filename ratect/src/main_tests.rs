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
use clap::CommandFactory;
use std::time::Duration;

#[test]
fn the_cli_definition_is_internally_valid() {
    Cli::command().debug_assert();
}

#[test]
fn run_takes_the_task_name_as_its_own_argument() {
    let cli = Cli::try_parse_from(["ratect", "run", "build"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.task, "build");
            assert!(args.args.is_empty());
        }
        other => panic!("expected a run command, got {other:?}"),
    }
}

/// The deliberate absence of `ratect <task>` sugar: with more verbs
/// coming, "is `doctor` a task or a subcommand?" is a question the
/// interface should never have to answer — see ROADMAP.md.
#[test]
fn a_bare_task_name_is_not_accepted_as_a_shorthand_for_run() {
    assert!(Cli::try_parse_from(["ratect", "build"]).is_err());
}

#[test]
fn arguments_after_a_double_dash_go_to_the_task_command() {
    let cli = Cli::try_parse_from(["ratect", "run", "build", "--", "--verbose", "extra"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.task, "build");
            assert_eq!(args.args, vec!["--verbose", "extra"]);
        }
        other => panic!("expected a run command, got {other:?}"),
    }
}

#[test]
fn completions_takes_a_shell_and_rejects_an_unknown_one() {
    let cli = Cli::try_parse_from(["ratect", "completions", "zsh"]).unwrap();
    assert!(matches!(cli.command, Command::Completions(_)));
    assert!(Cli::try_parse_from(["ratect", "completions", "klingon"]).is_err());
}

#[test]
fn config_validate_is_a_subcommand_of_config() {
    let cli = Cli::try_parse_from(["ratect", "config", "validate"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Config {
            command: ConfigCommand::Validate(_)
        }
    ));
}

#[test]
fn config_convert_rejects_stdout_and_force_together() {
    assert!(matches!(
        Cli::try_parse_from(["ratect", "config", "convert"])
            .unwrap()
            .command,
        Command::Config {
            command: ConfigCommand::Convert(_)
        }
    ));
    // `--stdout` writes nothing, so `--force` (overwrite a file) is
    // meaningless with it.
    assert!(Cli::try_parse_from(["ratect", "config", "convert", "--stdout", "--force"]).is_err());
}

#[test]
fn tasks_list_is_its_own_subcommand_not_a_flag() {
    let cli = Cli::try_parse_from(["ratect", "tasks", "list"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Tasks {
            command: TasksCommand::List(_)
        }
    ));
    // `--list-tasks` is `ratect-compat`'s spelling, and stays there.
    assert!(Cli::try_parse_from(["ratect", "--list-tasks"]).is_err());
}

/// Global options are accepted on either side of the subcommand — `-f`
/// before `run` reads naturally, and after it is what anyone used to
/// the flat CLI will type first.
#[test]
fn global_options_work_before_and_after_the_subcommand() {
    for arguments in [
        ["ratect", "-f", "custom.yml", "run", "build"],
        ["ratect", "run", "build", "-f", "custom.yml"],
    ] {
        let cli = Cli::try_parse_from(arguments).unwrap();
        assert_eq!(cli.global.config_file, PathBuf::from("custom.yml"));
    }
}

#[test]
fn a_repeatable_name_value_option_collects_every_occurrence() {
    let cli = Cli::try_parse_from([
        "ratect",
        "run",
        "build",
        "--config-var",
        "one=1",
        "--config-var",
        "two=2",
    ])
    .unwrap();
    let config_var = match cli.command {
        Command::Run(args) => args.config_vars.config_var,
        other => panic!("expected a run command, got {other:?}"),
    };
    assert_eq!(
        config_var,
        vec![
            ("one".to_string(), "1".to_string()),
            ("two".to_string(), "2".to_string())
        ]
    );
}

#[test]
fn a_name_value_option_without_an_equals_sign_is_rejected() {
    assert!(Cli::try_parse_from(["ratect", "run", "build", "--config-var", "no-equals"]).is_err());
}

#[test]
fn caches_clean_removes_everything_when_no_names_are_given() {
    let cli = Cli::try_parse_from(["ratect", "caches", "clean"]).unwrap();
    match cli.command {
        Command::Caches {
            command: CachesCommand::Clean(args),
        } => assert!(args.names.is_empty()),
        other => panic!("expected a caches clean command, got {other:?}"),
    }
}

#[test]
fn caches_clean_takes_the_names_to_remove_as_positional_arguments() {
    let cli =
        Cli::try_parse_from(["ratect", "caches", "clean", "npm-cache", "gradle-cache"]).unwrap();
    match cli.command {
        Command::Caches {
            command: CachesCommand::Clean(args),
        } => assert_eq!(args.names, vec!["npm-cache", "gradle-cache"]),
        other => panic!("expected a caches clean command, got {other:?}"),
    }
}

/// Which storage to act on has to be askable of both sub-verbs, or
/// `list` and `clean` would disagree about what a cache even is.
#[test]
fn cache_type_applies_to_both_caches_subcommands() {
    for arguments in [
        vec!["ratect", "caches", "list", "--cache-type", "directory"],
        vec!["ratect", "caches", "clean", "--cache-type", "directory"],
    ] {
        let cli = Cli::try_parse_from(&arguments).unwrap();
        let cache_type = match cli.command {
            Command::Caches {
                command: CachesCommand::List(args),
            } => args.cache_type,
            Command::Caches {
                command: CachesCommand::Clean(args),
            } => args.caches.cache_type,
            other => panic!("expected a caches command, got {other:?}"),
        };
        assert_eq!(cache_type, CacheTypeArg::Directory);
    }
}

fn run_args(arguments: &[&str]) -> RunArgs {
    match Cli::try_parse_from(arguments)
        .expect("should parse")
        .command
    {
        Command::Run(args) => args,
        other => panic!("expected a run command, got {other:?}"),
    }
}

fn settings_from(flags: &[&str]) -> TaskEngineSettings {
    let mut arguments = vec!["ratect", "run", "build"];
    arguments.extend_from_slice(flags);
    run_args(&arguments).engine_settings(PathBuf::from("/p"))
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
/// This is the test that catches *cross-wiring*: with several flags set
/// at once, a field reading the wrong one of two same-shaped flags
/// looks identical to one reading the right flag. Setting a single flag
/// and asserting the exact set of changed fields is what tells them
/// apart — an all-at-once test can't.
#[test]
fn each_flag_changes_only_its_own_setting() {
    for (flag, expected) in FLAG_TO_SETTING {
        assert_eq!(
            changed_from_default(&settings_from(flag)),
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
    let settings = run_args(&["ratect", "run", "build"]).engine_settings(PathBuf::from("/p"));

    assert!(
        settings.interrupt.is_some(),
        "every run must carry an interrupt tracker"
    );
}

/// With nothing asked for, the engine must be left exactly as it would
/// be with no settings applied at all — an inverted boolean here would
/// silently change the default behavior of every run.
#[test]
fn no_flags_maps_to_the_engines_own_defaults() {
    let settings = settings_from(&[]);
    assert!(
        changed_from_default(&settings).is_empty(),
        "no flag should mean no setting moved: {:?}",
        changed_from_default(&settings)
    );
    // The two this binary always supplies, unlike the rest.
    assert_eq!(
        settings.cache,
        Some((ratect_core::cache::CacheType::Volume, PathBuf::from("/p")))
    );
    assert_eq!(
        settings.ratect_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

/// Values, not just which field moved — the per-flag test above proves
/// a flag reaches the right setting, this proves what it puts there.
#[test]
fn a_flags_value_reaches_its_setting_intact() {
    let settings = settings_from(&[
        "--use-network",
        "existing-network",
        "--override-image",
        "db=postgres:16",
        "--tag-image",
        "app=extra",
        "--tag-image",
        "app=second",
        "--max-parallelism",
        "3",
        "--cache-type",
        "directory",
    ]);

    assert_eq!(
        settings.existing_network.as_deref(),
        Some("existing-network")
    );
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
    assert_eq!(settings.max_parallelism, Some(3));
    assert_eq!(
        settings.cache,
        Some((
            ratect_core::cache::CacheType::Directory,
            PathBuf::from("/p")
        ))
    );
}

/// `--no-cleanup` is the pair of them together; each half also stands
/// alone, and confusing the two would leave containers behind (or not)
/// in exactly the case the user asked about.
#[test]
fn no_cleanup_is_both_halves_and_each_half_stands_alone() {
    assert_eq!(
        changed_from_default(&settings_from(&["--no-cleanup"])),
        vec!["cleanup_after_success", "cleanup_after_failure"]
    );
}

#[test]
fn resources_has_a_list_and_a_clean_verb() {
    assert!(matches!(
        Cli::try_parse_from(["ratect", "resources", "list"])
            .unwrap()
            .command,
        Command::Resources {
            command: ResourcesCommand::List(_)
        }
    ));
    assert!(matches!(
        Cli::try_parse_from(["ratect", "resources", "clean"])
            .unwrap()
            .command,
        Command::Resources {
            command: ResourcesCommand::Clean(_)
        }
    ));
}

/// Both scoping options apply to both verbs — listing everything and
/// then only being able to clean this project's would be a trap.
#[test]
fn scope_options_apply_to_both_resources_verbs() {
    for verb in ["list", "clean"] {
        let cli = Cli::try_parse_from([
            "ratect",
            "resources",
            verb,
            "--all-projects",
            "--older-than",
            "2h",
        ])
        .unwrap();
        let args = match cli.command {
            Command::Resources {
                command: ResourcesCommand::List(args) | ResourcesCommand::Clean(args),
            } => args,
            other => panic!("expected a resources command, got {other:?}"),
        };
        assert!(args.all_projects);
        assert_eq!(args.older_than, Some(Duration::from_secs(2 * 60 * 60)));
    }
}

#[test]
fn an_age_accepts_seconds_minutes_hours_and_days() {
    assert_eq!(parse_age("90s"), Ok(Duration::from_secs(90)));
    assert_eq!(parse_age("30m"), Ok(Duration::from_secs(1_800)));
    assert_eq!(parse_age("2h"), Ok(Duration::from_secs(7_200)));
    // The unit anyone reaches for when clearing up after last week,
    // and the reason this isn't Batect's own duration format.
    assert_eq!(parse_age("7d"), Ok(Duration::from_secs(604_800)));
}

#[test]
fn an_age_without_a_valid_unit_is_rejected() {
    for value in ["30", "30x", "d", "", "-1h", "1.5h"] {
        assert!(parse_age(value).is_err(), "{value} should be rejected");
    }
}

/// Rounded to one unit: "3 days" is what makes a leftover recognizable
/// as old, and singular/plural is the kind of thing that reads as
/// sloppy in the one place someone is already annoyed.
#[test]
fn an_age_reads_as_a_single_rounded_unit() {
    assert_eq!(format_age(1), "1 second");
    assert_eq!(format_age(59), "59 seconds");
    assert_eq!(format_age(60), "1 minute");
    assert_eq!(format_age(60 * 90), "1 hour");
    assert_eq!(format_age(60 * 60 * 25), "1 day");
    assert_eq!(format_age(60 * 60 * 24 * 3), "3 days");
    // A clock skew between the daemon and here shouldn't print
    // something absurd.
    assert_eq!(format_age(-5), "0 seconds");
}

#[test]
fn doctor_is_its_own_verb_and_reaches_a_daemon() {
    assert!(matches!(
        Cli::try_parse_from(["ratect", "doctor"]).unwrap().command,
        Command::Doctor(_)
    ));
    // It checks the daemon, so it takes the options for reaching one.
    assert!(
        Cli::try_parse_from(["ratect", "doctor", "--docker-host", "tcp://example:2376"]).is_ok()
    );
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

/// Builds a `Config` the way a real invocation does — through
/// `load_project` on an actual file — rather than by parsing YAML
/// here, which would need `noyalib` as a dependency of this binary and
/// duplicate knowledge that belongs to `ratect-core`. It also means
/// `build_directory` paths are resolved exactly as they will be at run
/// time, which one of these checks depends on.
async fn config_with(yaml: &str) -> ratect_core::config::Config {
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

    let project = load_project_native(&path, &HashMap::new())
        .await
        .expect("fixture config should load");
    std::fs::remove_dir_all(&directory).unwrap();
    project.config
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

#[test]
fn includes_has_list_clean_and_refresh() {
    for (arguments, matches) in [
        (
            vec!["ratect", "includes", "list"],
            matches!(
                Cli::try_parse_from(["ratect", "includes", "list"])
                    .unwrap()
                    .command,
                Command::Includes {
                    command: IncludesCommand::List
                }
            ),
        ),
        (
            vec!["ratect", "includes", "refresh"],
            matches!(
                Cli::try_parse_from(["ratect", "includes", "refresh"])
                    .unwrap()
                    .command,
                Command::Includes {
                    command: IncludesCommand::Refresh
                }
            ),
        ),
    ] {
        assert!(matches, "{arguments:?} should parse to its own sub-verb");
    }
}

/// The two ways of saying "more than the default" are mutually
/// exclusive: accepting both would leave it ambiguous which won, in a
/// command that deletes things.
#[test]
fn cleaning_includes_takes_all_or_an_age_but_not_both() {
    assert!(Cli::try_parse_from(["ratect", "includes", "clean", "--all"]).is_ok());
    assert!(Cli::try_parse_from(["ratect", "includes", "clean", "--older-than", "7d"]).is_ok());
    assert!(
        Cli::try_parse_from(["ratect", "includes", "clean", "--all", "--older-than", "7d"])
            .is_err()
    );
}

/// The include cache is machine-wide, so none of the project-scoped
/// options mean anything here — and one that's accepted and ignored
/// reads as a promise.
#[test]
fn includes_takes_no_project_or_docker_options() {
    assert!(Cli::try_parse_from([
        "ratect",
        "includes",
        "list",
        "--docker-host",
        "tcp://example:2376"
    ])
    .is_err());
    assert!(Cli::try_parse_from(["ratect", "includes", "list", "--all-projects"]).is_err());
}

#[test]
fn a_size_reads_in_the_largest_useful_unit() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(999), "999 B");
    assert_eq!(format_size(1024), "1.0 KiB");
    assert_eq!(format_size(1024 * 1024), "1.0 MiB");
    assert_eq!(format_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MiB");
    assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
}

/// `caches` locates a project by directory, never by reading its
/// configuration, so config-variable values would have nothing to act
/// on — and offering them would imply otherwise.
#[test]
fn config_variable_options_belong_to_the_commands_that_read_configuration() {
    assert!(Cli::try_parse_from(["ratect", "run", "build", "--config-var", "a=1"]).is_ok());
    assert!(Cli::try_parse_from(["ratect", "tasks", "list", "--config-var", "a=1"]).is_ok());
    assert!(Cli::try_parse_from(["ratect", "caches", "list", "--config-var", "a=1"]).is_err());
}

/// Caches live in Docker volumes by default, so these do reach a
/// daemon — but they never build anything, so they don't take the flag
/// that's about building.
#[test]
fn caches_takes_the_connection_options_but_not_enable_buildkit() {
    assert!(Cli::try_parse_from([
        "ratect",
        "caches",
        "list",
        "--docker-host",
        "tcp://example:2376"
    ])
    .is_ok());
    assert!(Cli::try_parse_from(["ratect", "caches", "list", "--enable-buildkit"]).is_err());
    assert!(Cli::try_parse_from(["ratect", "run", "build", "--enable-buildkit"]).is_ok());
}

/// `tasks list` never reaches a daemon, so it doesn't take the flags
/// for reaching one — an accepted-but-ignored flag is worse than one
/// that isn't offered.
#[test]
fn docker_options_belong_to_run_not_to_tasks_list() {
    assert!(Cli::try_parse_from([
        "ratect",
        "run",
        "build",
        "--docker-host",
        "tcp://example:2376"
    ])
    .is_ok());
    assert!(Cli::try_parse_from([
        "ratect",
        "tasks",
        "list",
        "--docker-host",
        "tcp://example:2376"
    ])
    .is_err());
}
