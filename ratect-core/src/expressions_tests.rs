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

fn host_env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name: &str| vars.get(name).cloned()
}

#[test]
fn passes_through_literal_text_unchanged() {
    let result = interpolate("hello world", host_env(&[]), &HashMap::new()).unwrap();
    assert_eq!(result, "hello world");
}

#[test]
fn expands_bare_host_var() {
    let result = interpolate("$FOO", host_env(&[("FOO", "bar")]), &HashMap::new()).unwrap();
    assert_eq!(result, "bar");
}

#[test]
fn expands_braced_host_var() {
    let result = interpolate("${FOO}", host_env(&[("FOO", "bar")]), &HashMap::new()).unwrap();
    assert_eq!(result, "bar");
}

#[test]
fn expands_host_var_within_surrounding_literal_text() {
    let result = interpolate(
        "prefix-$FOO-suffix",
        host_env(&[("FOO", "bar")]),
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(result, "prefix-bar-suffix");
}

#[test]
fn uses_default_when_host_var_unset() {
    let result = interpolate("${FOO:-fallback}", host_env(&[]), &HashMap::new()).unwrap();
    assert_eq!(result, "fallback");
}

#[test]
fn prefers_set_host_var_over_default() {
    let result = interpolate(
        "${FOO:-fallback}",
        host_env(&[("FOO", "actual")]),
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(result, "actual");
}

#[test]
fn errors_when_host_var_unset_and_no_default() {
    let err = interpolate("$FOO", host_env(&[]), &HashMap::new()).unwrap_err();
    assert!(err.to_string().contains("FOO"));
    assert!(err.to_string().contains("is not set"));
}

#[test]
fn expands_bare_config_var() {
    let mut config_vars = HashMap::new();
    config_vars.insert("name".to_string(), Some("value".to_string()));
    let result = interpolate("<name", host_env(&[]), &config_vars).unwrap();
    assert_eq!(result, "value");
}

#[test]
fn expands_braced_config_var() {
    let mut config_vars = HashMap::new();
    config_vars.insert("name".to_string(), Some("value".to_string()));
    let result = interpolate("<{name}", host_env(&[]), &config_vars).unwrap();
    assert_eq!(result, "value");
}

#[test]
fn errors_on_undeclared_config_var() {
    let err = interpolate("<missing", host_env(&[]), &HashMap::new()).unwrap_err();
    assert!(err.to_string().contains("missing"));
    assert!(err.to_string().contains("not declared"));
}

#[test]
fn errors_on_declared_config_var_with_no_value() {
    let mut config_vars = HashMap::new();
    config_vars.insert("name".to_string(), None);
    let err = interpolate("<name", host_env(&[]), &config_vars).unwrap_err();
    assert!(err.to_string().contains("no value"));
}

#[test]
fn leaves_dollar_sign_not_followed_by_identifier_as_literal() {
    let result = interpolate("$ $$ $5", host_env(&[]), &HashMap::new()).unwrap();
    assert_eq!(result, "$ $$ $5");
}

#[test]
fn leaves_unterminated_braced_expression_as_literal() {
    let result = interpolate("${FOO", host_env(&[]), &HashMap::new()).unwrap();
    assert_eq!(result, "${FOO");
}

#[test]
fn mixes_host_and_config_var_expressions_in_one_string() {
    let mut config_vars = HashMap::new();
    config_vars.insert("env_name".to_string(), Some("staging".to_string()));
    let result = interpolate(
        "$SERVICE-<env_name>-${REGION:-eu}",
        host_env(&[("SERVICE", "api")]),
        &config_vars,
    )
    .unwrap();
    // `<env_name` expands (consuming just the identifier), leaving the
    // trailing '>' from the input as literal text.
    assert_eq!(result, "api-staging>-eu");
}

#[test]
fn expands_bare_config_var_with_dotted_name() {
    let mut config_vars = HashMap::new();
    config_vars.insert(
        "batect.project_directory".to_string(),
        Some("/abs/project".to_string()),
    );
    let result = interpolate("<batect.project_directory", host_env(&[]), &config_vars).unwrap();
    assert_eq!(result, "/abs/project");
}

#[test]
fn expands_braced_config_var_with_dotted_name() {
    let mut config_vars = HashMap::new();
    config_vars.insert(
        "batect.project_directory".to_string(),
        Some("/abs/project".to_string()),
    );
    let result = interpolate(
        "<{batect.project_directory}/scripts",
        host_env(&[]),
        &config_vars,
    )
    .unwrap();
    assert_eq!(result, "/abs/project/scripts");
}

#[test]
fn dot_is_not_a_valid_host_var_identifier_character() {
    // Unlike config variables, host env var names never contain '.', so
    // `$batect.project_directory` should expand just `$batect` (which
    // errors here, since it's unset) rather than treating the dot as
    // part of the identifier.
    let err = interpolate("$batect.project_directory", host_env(&[]), &HashMap::new()).unwrap_err();
    assert!(err.to_string().contains("batect"));
    assert!(!err.to_string().contains("batect.project_directory"));
}

/// `contains_expression` exists to gate a `batect.yml`, so a false positive
/// rejects a config that is valid under both tools. It must therefore agree
/// with `interpolate` about what counts as an expression rather than merely
/// scanning for the sigils.
///
/// Asserted as an equivalence against `interpolate` itself, with every
/// variable resolvable, so the two cannot drift: if interpolation changes the
/// string, detection must say so, and if it leaves it alone, detection must
/// not.
#[test]
fn detection_agrees_with_interpolation_about_what_is_an_expression() {
    let config_vars: HashMap<String, Option<String>> =
        HashMap::from([("tag".to_string(), Some("1.2.3".to_string()))]);
    let env = host_env(&[("TAG", "1.2.3"), ("REGISTRY", "example.com")]);

    for input in [
        // Expressions, in every form the syntax has.
        "$TAG",
        "${TAG}",
        "${MISSING:-latest}",
        "repo/img:$TAG",
        "$REGISTRY/repo/img:${TAG}",
        "<tag",
        "<{tag}",
        // Literals — including the near-misses that make a naive
        // "contains a sigil" check wrong.
        "alpine:3.18",
        "repo/img:1.2.3",
        "$",
        "$ ",
        "$1nvalid",
        "${unterminated",
        "<",
        "<{unterminated",
        "price: $100",
    ] {
        match interpolate(input, &env, &config_vars) {
            // Substituted something, or deliberately left it alone.
            Ok(interpolated) => assert_eq!(
                contains_expression(input),
                interpolated != input,
                "detection and interpolation disagree on {input:?} (interpolated to \
                 {interpolated:?})"
            ),
            // Well-formed but unresolvable is still an expression — see
            // `an_unresolvable_variable_still_counts_as_an_expression`.
            Err(_) => assert!(
                contains_expression(input),
                "interpolation treated {input:?} as an expression; detection did not"
            ),
        }
    }
}

/// `<` binds tighter than it looks: `a<b` is not the literal it appears to
/// be, because `<b` is a config-variable reference. Worth pinning, since it
/// is the case where a human eyeballing an image name would guess wrong —
/// and the reason detection reuses the parser instead of matching sigils.
#[test]
fn a_bare_angle_bracket_before_a_name_is_a_config_variable() {
    assert!(contains_expression("a<b"));
    let err = interpolate("a<b", host_env(&[]), &HashMap::new()).unwrap_err();
    assert!(err.to_string().contains("'b'"), "unexpected error: {err}");
}

/// A well-formed token whose variable cannot be resolved is still an
/// expression — `interpolate` errors on it rather than passing it through, so
/// treating it as literal would let it reach a `batect.yml` ungated.
#[test]
fn an_unresolvable_variable_still_counts_as_an_expression() {
    assert!(contains_expression("repo/img:$NOT_SET_ANYWHERE"));
    assert!(interpolate("repo/img:$NOT_SET_ANYWHERE", host_env(&[]), &HashMap::new()).is_err());
}
