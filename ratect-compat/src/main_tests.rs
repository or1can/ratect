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
/// `ratect-compat` takes the task name after every option, as Batect does:
/// nothing after it is read as one (ratect#435).
const TASK_ARGUMENTS: &[&str] = &["build"];

fn settings_from(arguments: &[&str]) -> TaskEngineSettings {
    args(arguments).engine_settings(
        PathBuf::from("/p"),
        Dialect::BatectCompatible,
        HashMap::new(),
        CacheType::Volume,
    )
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
/// the per-flag test below. `cache`/`ratect_version` are excluded: both are
/// always supplied, so they always differ. That they *are* always supplied
/// is asserted separately below, since nothing here would notice one going
/// missing.
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

/// With nothing asked for, the engine must behave exactly as it would
/// with no settings applied — an inverted boolean would silently change
/// the default behavior of every run.
#[test]
fn no_flags_maps_to_the_engines_own_defaults() {
    let settings = args(&["ratect-compat", "build"]).engine_settings(
        PathBuf::from("/p"),
        Dialect::BatectCompatible,
        HashMap::new(),
        CacheType::Volume,
    );
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
    let parsed = args(&[
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
    ]);
    let settings = parsed.engine_settings(
        PathBuf::from("/projects/demo"),
        Dialect::BatectCompatible,
        HashMap::new(),
        parsed.cache_type(None).unwrap(),
    );

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
    let success = args(&["ratect-compat", "--no-cleanup-after-success", "build"]).engine_settings(
        PathBuf::from("/p"),
        Dialect::BatectCompatible,
        HashMap::new(),
        CacheType::Volume,
    );
    assert!(!success.cleanup_after_success);
    assert!(success.cleanup_after_failure);

    let failure = args(&["ratect-compat", "--no-cleanup-after-failure", "build"]).engine_settings(
        PathBuf::from("/p"),
        Dialect::BatectCompatible,
        HashMap::new(),
        CacheType::Volume,
    );
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

/// The `clap` error `Args::parse_checked` gives `arguments`, rendered.
fn argument_error(arguments: &[&str]) -> String {
    match Args::parse_checked(arguments) {
        Ok(args) => panic!("{arguments:?} should be rejected, parsed as {args:?}"),
        Err(error) => error.to_string(),
    }
}

/// An empty container or value in `--override-image`/`--tag-image` is
/// refused while the arguments are parsed, as Batect's `(.+)=(.+)` refuses
/// it, in both the `--opt k=v` and `--opt=k=v` forms (ratect#438).
#[test]
fn rejects_an_empty_side_in_override_image_and_tag_image() {
    for (arguments, reason) in [
        (
            &["--override-image", "c="][..],
            "the image after '=' is empty",
        ),
        (
            &["--override-image", "=a"],
            "the container name before '=' is empty",
        ),
        (
            &["--override-image", "="],
            "the container name before '=' is empty",
        ),
        (&["--override-image=c="], "the image after '=' is empty"),
        (&["--tag-image", "c="], "the tag after '=' is empty"),
        (
            &["--tag-image", "=a"],
            "the container name before '=' is empty",
        ),
        (
            &["--tag-image", "="],
            "the container name before '=' is empty",
        ),
        (&["--tag-image=c="], "the tag after '=' is empty"),
    ] {
        let line = [&[BINARY], arguments, &["--list-tasks"]].concat();
        let error = argument_error(&line);
        let option = arguments[0].split('=').next().unwrap();
        assert!(
            error.contains(&format!("for '{option} <")) && error.contains(reason),
            "{arguments:?}: {error}"
        );
    }
}

/// A key given twice to `--override-image` or `--config-var` is refused,
/// whether or not the values agree, where it used to be last-wins
/// (ratect#438). Batect's `SingleValueMapOption` refuses it too.
#[test]
fn rejects_a_key_repeated_in_override_image_or_config_var() {
    for (arguments, message) in [
        (
            &["--override-image", "c=a", "--override-image", "c=b"][..],
            "the container 'c' was given to --override-image more than once",
        ),
        (
            &["--override-image", "c=a", "--override-image=c=a"],
            "the container 'c' was given to --override-image more than once",
        ),
        (
            &["--config-var", "x=1", "--config-var", "x=2"],
            "the variable 'x' was given to --config-var more than once",
        ),
    ] {
        let line = [&[BINARY], arguments, &["build"]].concat();
        let error = argument_error(&line);
        assert!(error.contains(message), "{arguments:?}: {error}");
    }
}

/// What stays accepted: an empty `--config-var` value, a split at the
/// first `=` (both recorded divergences), a container tagged twice, and
/// distinct keys — including one also set by a `--config-vars-file`, which
/// the command line still overrides rather than repeats.
#[test]
fn parse_checked_still_accepts_what_batect_does_and_the_recorded_divergences() {
    let parsed = Args::parse_checked([
        BINARY,
        "--config-var",
        "x=",
        "--config-var",
        "y=a=b",
        "--config-vars-file",
        "vars.yml",
        "--override-image",
        "c=a=b",
        "--override-image=d=e",
        "--tag-image",
        "c=t1",
        "--tag-image",
        "c=t2",
        "build",
    ])
    .unwrap();
    assert_eq!(
        parsed.config_var,
        vec![
            ("x".to_string(), String::new()),
            ("y".to_string(), "a=b".to_string()),
        ]
    );
    let settings = parsed.engine_settings(
        PathBuf::from("/p"),
        Dialect::BatectCompatible,
        HashMap::new(),
        CacheType::Volume,
    );
    assert_eq!(
        settings.image_overrides,
        HashMap::from([
            ("c".to_string(), "a=b".to_string()),
            ("d".to_string(), "e".to_string()),
        ])
    );
    assert_eq!(
        settings.image_tags["c"],
        HashSet::from(["t1".to_string(), "t2".to_string()])
    );
}

/// `parse_checked`'s parse of a line it accepts.
fn parsed(arguments: &[&str]) -> Args {
    Args::parse_checked(arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error}"))
}

/// Options end at the task name, as in Batect: anything after it but
/// `-- ARGS…` is refused, naming the task and the first extra argument,
/// rather than read as an option (ratect#435).
#[test]
fn an_argument_after_the_task_name_is_refused_unless_it_follows_double_dash() {
    for (arguments, task, extra) in [
        (
            &["test-task", "--list-tasks"][..],
            "test-task",
            "--list-tasks",
        ),
        (&["build", "--disable-ports"], "build", "--disable-ports"),
        (&["test-task", "extra"], "test-task", "extra"),
        (&["--no-color", "t", "extra", "--", "x"], "t", "extra"),
        (&["t", "-f", "other.yml"], "t", "-f"),
    ] {
        let line = [&[BINARY], arguments].concat();
        let error = argument_error(&line);
        assert!(
            error.contains(&format!("'{task}'"))
                && error.contains(&format!("'{extra}'"))
                && error.contains("before the task name")
                && error.contains("--"),
            "{arguments:?}: {error}"
        );
    }
}

/// No task name, and no flag that replaces one, is refused while the
/// arguments are parsed, as Batect's `parseTaskName` refuses it — `--`
/// with task arguments and no task included (ratect#434).
#[test]
fn a_run_with_no_task_name_is_refused_while_parsing() {
    for arguments in [
        &[][..],
        &["-f", "x.yml"],
        &["--", "foo"],
        &["--no-color", "--"],
    ] {
        let error = argument_error(&[&[BINARY], arguments].concat());
        assert!(
            error.contains("no task name") && error.contains("--list-tasks"),
            "{arguments:?}: {error}"
        );
    }
}

#[test]
fn arguments_after_double_dash_reach_the_task_whatever_they_look_like() {
    let args = parsed(&[
        BINARY,
        "-f",
        "x.yml",
        "test-task",
        "--",
        "a",
        "-b",
        "--c",
        "--",
    ]);
    assert_eq!(args.task_name.as_deref(), Some("test-task"));
    assert_eq!(args.additional_args, ["a", "-b", "--c", "--"]);
    assert_eq!(args.config_file, PathBuf::from("x.yml"));
}

/// A flag that replaces the task ignores everything after the options, as
/// Batect's `parseTaskName` returns early for one (ratect#435).
#[test]
fn a_flag_that_replaces_the_task_ignores_everything_after_the_options() {
    for arguments in [
        &["--list-tasks", "a", "b"][..],
        &["--list-tasks", "a", "--", "x"],
        &["--clean-cache", "foo", "test-task", "extra"],
        &["--clean", "test-task", "x"],
        &["--upgrade", "--", "x"],
    ] {
        let args = parsed(&[&[BINARY], arguments].concat());
        assert!(!args.runs_a_task(), "{arguments:?}");
        assert_eq!(args.task_name, None, "{arguments:?}");
        assert!(args.additional_args.is_empty(), "{arguments:?}");
    }
}

/// A value given as its own argument that starts with `-` is refused, as
/// `clap` refuses it, but the error points at the `=` form rather than at
/// `--`, which passes arguments to the task (ratect#435).
#[test]
fn a_hyphen_leading_value_is_refused_with_a_tip_naming_the_equals_form() {
    for (arguments, tip) in [
        (
            &["--use-network", "-x", "test-task"][..],
            "--use-network=-x",
        ),
        (
            &["--clean-cache", "--list-tasks"],
            "--clean-cache=--list-tasks",
        ),
        (&["-f", "-x", "test-task"], "-f=-x"),
    ] {
        let error = argument_error(&[&[BINARY], arguments].concat());
        assert!(
            error.contains(tip) && !error.contains("'-- "),
            "{arguments:?}: {error}"
        );
    }
}

/// `--help` and `--version` still act as soon as they are reached, before
/// a value refused while splitting at the task name is reported — as before
/// that check existed (decisions/0016).
#[test]
fn help_and_version_win_over_a_refused_value() {
    for arguments in [
        &["--version", "--use-network", "-x"][..],
        &["--help", "-f", "-x"],
    ] {
        let error = Args::parse_checked([&[BINARY], arguments].concat())
            .expect_err("help and version are clap errors");
        assert!(!error.use_stderr(), "{arguments:?}: {error}");
    }
}

/// The `=` forms reach their option, a value containing `=` included, and
/// the recorded short-option leniency stays (ratect#435).
#[test]
fn the_equals_forms_and_combined_short_options_still_parse() {
    let args = parsed(&[BINARY, "--use-network=-x", "-f=a=b.yml", "--list-tasks"]);
    assert_eq!(args.use_network.as_deref(), Some("-x"));
    assert_eq!(args.config_file, PathBuf::from("a=b.yml"));

    let args = parsed(&[
        BINARY,
        "--output=quiet",
        "--log-file=l",
        "--max-parallelism=2",
        "t",
    ]);
    assert_eq!(args.output, Some(OutputStyleArg::Quiet));
    assert_eq!(args.log_file, Some(PathBuf::from("l")));
    assert_eq!(args.max_parallelism, Some(2));
    assert_eq!(args.task_name.as_deref(), Some("t"));

    let args = parsed(&[BINARY, "-Tf", "x.yml"]);
    assert!(args.list_tasks);
    assert_eq!(args.config_file, PathBuf::from("x.yml"));
    let args = parsed(&[BINARY, "-fx.yml", "-oquiet", "t"]);
    assert_eq!(args.config_file, PathBuf::from("x.yml"));
    assert_eq!(args.output, Some(OutputStyleArg::Quiet));
    assert_eq!(args.task_name.as_deref(), Some("t"));
}

#[test]
fn rejects_a_tag_image_value_docker_would_refuse_while_parsing_arguments() {
    let error = Args::try_parse_from(["ratect", "--tag-image", "app=_nonsense", "--list-tasks"])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("'_nonsense' is not a valid image tag: invalid reference format"),
        "{error}"
    );
}

#[test]
fn accepts_a_tag_image_value_naming_a_registry_repository_and_tag() {
    let args = args(&["ratect", "--tag-image", "app=myorg/app:1.0", "build"]);
    assert_eq!(
        args.tag_image,
        vec![("app".to_string(), "myorg/app:1.0".to_string())]
    );
}

#[test]
fn override_image_values_stay_unchecked() {
    let args = args(&["ratect", "--override-image", "app=_nonsense", "build"]);
    assert_eq!(
        args.override_image,
        vec![("app".to_string(), "_nonsense".to_string())]
    );
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

/// Two containers' overrides each reach the engine under their own
/// container (ratect#418) — the `Vec` above, collected into the map.
#[test]
fn repeated_override_image_flags_each_reach_the_engine_under_their_own_container() {
    let settings = settings_from(&[
        BINARY,
        "--override-image",
        "build-env=alpine:3.18",
        "--override-image",
        "test-env=ubuntu:22.04",
        "build",
    ]);
    assert_eq!(
        settings.image_overrides,
        HashMap::from([
            ("build-env".to_string(), "alpine:3.18".to_string()),
            ("test-env".to_string(), "ubuntu:22.04".to_string()),
        ])
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
    assert_eq!(args.cache_type(None).unwrap(), CacheType::Volume);
}

#[test]
fn parses_cache_type_flag() {
    let args = Args::try_parse_from(["ratect", "--cache-type", "directory", "build"]).unwrap();
    assert_eq!(args.cache_type(None).unwrap(), CacheType::Directory);

    let args = Args::try_parse_from(["ratect", "--cache-type", "volume", "build"]).unwrap();
    assert_eq!(args.cache_type(None).unwrap(), CacheType::Volume);
}

/// #441: `BATECT_CACHE_TYPE` is `--cache-type`'s default, which the flag
/// overrides.
#[test]
fn batect_cache_type_is_the_cache_type_default() {
    let no_flag = args(&["ratect", "build"]);
    assert_eq!(
        no_flag.cache_type(Some("directory")).unwrap(),
        CacheType::Directory
    );
    assert_eq!(
        no_flag.cache_type(Some("volume")).unwrap(),
        CacheType::Volume
    );
    let flag = args(&["ratect", "--cache-type", "volume", "build"]);
    assert_eq!(
        flag.cache_type(Some("directory")).unwrap(),
        CacheType::Volume
    );
}

/// #441: a `BATECT_CACHE_TYPE` that is no cache type — matched exactly, as
/// Batect does, so `Directory` and an empty value aren't either — is an
/// invalid option value naming it, unless `--cache-type` makes it moot.
#[test]
fn an_invalid_batect_cache_type_is_refused_unless_the_flag_is_given() {
    let no_flag = args(&["ratect", "build"]);
    for value in ["bogus", "Directory", ""] {
        let error = no_flag.cache_type(Some(value)).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(&format!("BATECT_CACHE_TYPE is '{value}'")),
            "{message}"
        );
        assert!(
            message.contains("volume") && message.contains("directory"),
            "{message}"
        );
    }
    let flag = args(&["ratect", "--cache-type", "directory", "build"]);
    assert_eq!(
        flag.cache_type(Some("bogus")).unwrap(),
        CacheType::Directory
    );
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
    run(
        args,
        ColorMode::Off,
        &Console::new(Box::new(std::io::sink()), false),
        unreachable_sweep,
    )
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
    run(
        args,
        ColorMode::Off,
        &Console::new(Box::new(std::io::sink()), false),
        unreachable_sweep,
    )
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
        resolve_config_vars_file(Some(explicit.clone()), &dir).unwrap(),
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
    assert_eq!(resolve_config_vars_file(None, &dir).unwrap(), Some(default));
}

#[test]
fn config_vars_file_default_is_absent_when_batect_local_yml_is_missing() {
    // An absent default file is "no overrides", not an error.
    let dir = unique_temp_dir();
    assert_eq!(resolve_config_vars_file(None, &dir).unwrap(), None);
}

/// #450: a `batect.local.yml` that exists but isn't a file is an invalid
/// default, refused naming it and saying it is `--config-vars-file`'s
/// default — unless an explicit `--config-vars-file` makes it moot.
#[test]
fn a_batect_local_yml_that_is_not_a_file_is_an_invalid_default() {
    let dir = unique_temp_dir();
    std::fs::create_dir(dir.join("batect.local.yml")).unwrap();

    let error = resolve_config_vars_file(None, &dir).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&format!("{:?}", dir.join("batect.local.yml"))),
        "{message}"
    );
    assert!(message.contains("--config-vars-file"), "{message}");
    assert!(message.contains("not a file"), "{message}");
    assert!(error.is::<InvalidOptionValue>());

    let explicit = PathBuf::from("chosen.yml");
    assert_eq!(
        resolve_config_vars_file(Some(explicit.clone()), &dir).unwrap(),
        Some(explicit)
    );
}

/// #450: one whose metadata can't be read says so, rather than calling it
/// not a file.
#[test]
fn a_batect_local_yml_that_cannot_be_read_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let dir = unique_temp_dir();
    let sealed = dir.join("sealed");
    std::fs::create_dir(&sealed).unwrap();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = resolve_config_vars_file(None, &sealed);
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();

    let message = result.unwrap_err().to_string();
    assert!(message.contains("can't be read: "), "{message}");
    assert!(!message.contains("not a file"), "{message}");
}

/// A line is a run unless a flag `run` handles instead of a task appears,
/// whether or not a task is named — so a broken config under `--list-tasks`
/// still exits `1`, and under a task name, or none, `125` (decisions/0016).
#[test]
fn every_line_but_a_flag_that_replaces_a_task_is_a_run() {
    assert!(args(&["ratect", "build"]).runs_a_task());
    assert!(args(&["ratect", "--log-file", "x", "build"]).runs_a_task());
    assert!(args(&["ratect"]).runs_a_task());

    assert!(!args(&["ratect", "--list-tasks", "build"]).runs_a_task());
    assert!(!args(&["ratect", "--clean", "build"]).runs_a_task());
    assert!(!args(&["ratect", "--clean-cache", "cargo", "build"]).runs_a_task());
    assert!(!args(&["ratect", "--upgrade", "build"]).runs_a_task());
}

fn raw(line: &[&str]) -> Vec<String> {
    line.iter().map(|word| word.to_string()).collect()
}

/// The raw-argument reading, for a line `clap` rejects: a flag that replaces
/// a task counts only among the options — not after the task name or `--`,
/// not as another option's value — and an unknown option takes no value
/// (decisions/0016; ratect#435).
#[test]
fn a_line_that_does_not_parse_is_a_run_unless_a_flag_replaces_the_task() {
    assert!(is_a_run(&raw(&["--bogus", "build"])));
    assert!(is_a_run(&raw(&["--bogus"])));
    assert!(is_a_run(&raw(&["--bogus", "--help"])));
    assert!(is_a_run(&raw(&["build", "--bogus", "--", "--clean"])));
    assert!(is_a_run(&raw(&["-f", "--clean", "--bogus"])));
    assert!(is_a_run(&raw(&["--config-file=--clean", "--bogus"])));
    // Options end at the task name (ratect#435).
    assert!(is_a_run(&raw(&["build", "--list-tasks"])));
    assert!(is_a_run(&raw(&["build", "-T", "--clean"])));

    assert!(!is_a_run(&raw(&["--bogus", "--clean"])));
    assert!(!is_a_run(&raw(&["--bogus", "-T"])));
    assert!(!is_a_run(&raw(&["--bogus", "--list-tasks"])));
    assert!(!is_a_run(&raw(&["--bogus", "--clean-cache=cargo"])));
    assert!(!is_a_run(&raw(&["--bogus", "--upgrade"])));
    assert!(!is_a_run(&raw(&["--no-color", "-T", "--bogus"])));
}

/// On a line that parses, the raw reading agrees with the parsed one, so a
/// line's path never depends on whether `clap` accepted it — including a
/// line with a flag that replaces the task after its task name, where
/// `split_at_task` and `command_line_words` must agree the options ended.
#[test]
fn the_raw_reading_agrees_with_the_parsed_one() {
    for line in [
        &["build"][..],
        &["build", "--", "--clean"],
        &["-f", "x.yml", "--list-tasks"],
        &["-T"],
        &["--clean"],
        &["--clean-cache", "cargo"],
        &["--clean-cache=cargo", "build"],
        &["--upgrade"],
        &["-o", "quiet", "build"],
        &["--list-tasks", "build", "--clean"],
        &["-Tf", "x.yml", "a", "b"],
        &["-oquiet", "--clean-cache=a,b", "x"],
        &["build", "--", "-T"],
    ] {
        let mut parsed = vec!["ratect"];
        parsed.extend_from_slice(line);
        assert_eq!(
            is_a_run(&raw(line)),
            Args::parse_checked(&parsed).unwrap().runs_a_task(),
            "{line:?}"
        );
    }
}

/// A `sweep` for a `run` that must return before making one.
fn unreachable_sweep() -> Sweep {
    panic!("this invocation should never start the Git include cache sweep")
}

/// What a fake sweep does once `run` has started it.
#[derive(Clone, Copy)]
enum FakeSweep {
    Succeeds,
    Fails,
    NeverFinishes,
}

/// What `run` did, given a fake sweep.
struct Swept {
    result: Result<()>,
    /// How many sweeps `run` made.
    made: usize,
    /// Whether the sweep it made was then run, not only made.
    ran: bool,
}

/// Runs `run` on `arguments` with a fake sweep that reports both its
/// making and its running, so no test here touches the real
/// `~/.ratect/incl`. Bounded: a `run` that waited on a sweep that never
/// finishes fails the test rather than hanging it.
async fn run_with_fake_sweep(arguments: &[&str], fake: FakeSweep) -> Swept {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let made = Arc::new(AtomicUsize::new(0));
    let (running, mut ran) = tokio::sync::oneshot::channel::<()>();
    let sweep = {
        let made = Arc::clone(&made);
        move || -> Sweep {
            made.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let _ = running.send(());
                match fake {
                    FakeSweep::Succeeds => Ok(()),
                    FakeSweep::Fails => Err(anyhow::anyhow!("the fake sweep failed")),
                    FakeSweep::NeverFinishes => std::future::pending().await,
                }
            })
        }
    };
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run(
            args(arguments),
            ColorMode::Off,
            &Console::new(Box::new(std::io::sink()), false),
            sweep,
        ),
    )
    .await
    .expect("run should not wait for the sweep");
    let made = made.load(Ordering::SeqCst);
    // Only a sweep `run` made can have been spawned; once it has, give the
    // detached task its turn on this test's runtime.
    let ran = made > 0
        && tokio::time::timeout(Duration::from_secs(5), &mut ran)
            .await
            .is_ok_and(|sent| sent.is_ok());
    Swept { result, made, ran }
}

/// A project with one task, `build`, in a directory of its own, so `run`
/// gets past `load_project`. Returns the config file's path.
fn project_with_one_task() -> String {
    let dir = unique_temp_dir();
    let config = dir.join("batect.yml");
    std::fs::write(
        &config,
        "project_name: sweep-wiring\n\
         containers:\n  box:\n    image: alpine:3.20\n\
         tasks:\n  build:\n    run:\n      container: box\n",
    )
    .unwrap();
    config.to_str().unwrap().to_owned()
}

/// A `--docker-host` no daemon listens on, so a task run fails at the
/// connection — the first thing it does that needs Docker — without one.
/// A run gets that far only after arming `Interrupt`, whose signal handlers
/// then stay installed for the rest of this test binary.
fn unreachable_docker_host(config: &str) -> String {
    let dir = Path::new(config).parent().unwrap();
    format!("unix://{}", dir.join("no-such-docker.sock").display())
}

/// Batect's `RunTaskCommand` starts its background tasks after loading
/// the config and before checking Docker connectivity, so a run that fails
/// at the connection has already started the sweep — once.
#[tokio::test]
async fn naming_a_task_starts_the_sweep_once_after_load_and_before_docker() {
    let config = project_with_one_task();
    let host = unreachable_docker_host(&config);
    let swept = run_with_fake_sweep(
        &[
            "ratect",
            "-f",
            &config,
            "-o",
            "quiet",
            "--docker-host",
            &host,
            "build",
        ],
        FakeSweep::Succeeds,
    )
    .await;

    let error = swept.result.expect_err("no daemon listens on the socket");
    assert!(
        format!("{error:?}").contains("Failed to connect to Docker"),
        "run should fail at the Docker connection: {error:?}"
    );
    assert_eq!(swept.made, 1, "a task run starts the sweep exactly once");
    assert!(
        swept.ran,
        "the sweep a task run makes is spawned, not dropped"
    );
}

/// Batect starts its background tasks only from `RunTaskCommand`, so
/// nothing `run` handles instead of running a task starts the sweep.
#[tokio::test]
async fn only_a_task_run_starts_the_sweep() {
    let config = project_with_one_task();
    for arguments in [
        &["ratect", "-f", &config, "--list-tasks", "build"][..],
        &[
            "ratect",
            "-f",
            &config,
            "--clean",
            "--cache-type",
            "directory",
            "build",
        ],
        &[
            "ratect",
            "-f",
            &config,
            "--clean-cache",
            "x",
            "--cache-type",
            "directory",
            "build",
        ],
        &["ratect", "-f", &config, "--upgrade", "build"],
    ] {
        let swept = run_with_fake_sweep(arguments, FakeSweep::Succeeds).await;
        swept
            .result
            .unwrap_or_else(|e| panic!("{arguments:?} should succeed: {e:?}"));
        assert_eq!(swept.made, 0, "{arguments:?} should not start the sweep");
    }
}

/// The sweep starts only once the config has loaded.
#[tokio::test]
async fn a_config_that_fails_to_load_does_not_start_the_sweep() {
    let swept = run_with_fake_sweep(
        &["ratect", "-f", "/no/such/batect.yml", "build"],
        FakeSweep::Succeeds,
    )
    .await;

    swept.result.expect_err("the config file does not exist");
    assert_eq!(swept.made, 0);
}

/// Like Batect's daemon thread, the sweep is never waited for: one that
/// never finishes, or fails, leaves `run`'s own result as it would be.
#[tokio::test]
async fn run_neither_waits_for_the_sweep_nor_takes_its_result() {
    let config = project_with_one_task();
    let host = unreachable_docker_host(&config);
    let arguments = [
        "ratect",
        "-f",
        &config,
        "-o",
        "quiet",
        "--docker-host",
        &host,
        "build",
    ];
    let error = |fake| async move {
        let swept = run_with_fake_sweep(&arguments, fake).await;
        assert_eq!(swept.made, 1);
        assert!(swept.ran);
        format!(
            "{:?}",
            swept.result.expect_err("no daemon listens on the socket")
        )
    };

    let expected = error(FakeSweep::Succeeds).await;
    assert_eq!(error(FakeSweep::NeverFinishes).await, expected);
    assert_eq!(error(FakeSweep::Fails).await, expected);
}

/// `--list-tasks` takes precedence over `--clean`, `--clean-cache` and
/// `--upgrade`, whichever order they come in, as in Batect's
/// `CommandFactory`: it lists, so it needs the config, and cleans nothing
/// (ratect#442). Asserted on the cache directory itself, not only `run`'s
/// result.
#[tokio::test]
async fn list_tasks_takes_precedence_over_clean_and_upgrade() {
    let config = project_with_one_task();
    let cache = Path::new(&config).with_file_name(".batect/caches/foo");
    std::fs::create_dir_all(&cache).unwrap();
    let directory = ["--cache-type", "directory"];
    for flags in [
        &["-T", "--clean"][..],
        &["-T", "--clean-cache", "foo"],
        &["--clean", "-T"],
        &["--clean-cache", "foo", "--list-tasks"],
        &["-T", "--upgrade"],
    ] {
        let line = [&["ratect", "-f", &config][..], &directory, flags].concat();
        run_with_fake_sweep(&line, FakeSweep::Succeeds)
            .await
            .result
            .unwrap_or_else(|e| panic!("{flags:?} should list the tasks: {e:?}"));
        assert!(cache.is_dir(), "{flags:?} removed the cache");
    }

    for flags in [&["-T", "--upgrade"][..], &["-T", "--clean"]] {
        let line = [
            &["ratect", "-f", "/no/such/batect.yml"][..],
            &directory,
            flags,
        ]
        .concat();
        run_with_fake_sweep(&line, FakeSweep::Succeeds)
            .await
            .result
            .expect_err(&format!("{flags:?} should load the config to list"));
    }
}
