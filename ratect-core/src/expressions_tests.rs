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

/// #385: a `$` with no name after it is no literal but a mistake, as in
/// Batect — `$5` is a name (#383), the other two are not.
#[test]
fn a_dollar_sign_not_followed_by_a_name_is_an_error() {
    let result = interpolate("$5", host_env(&[("5", "five")]), &HashMap::new()).unwrap();
    assert_eq!(result, "five");
    for input in ["$ ", "$$"] {
        let err = interpolate(input, host_env(&[]), &HashMap::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid environment variable reference: '$' at column 1 does not contain a \
             variable name",
            "{input:?}"
        );
    }
}

/// #385: an unterminated `${` is a mistake, not literal text, as in Batect.
#[test]
fn an_unterminated_braced_expression_is_an_error() {
    let err = interpolate("${FOO", host_env(&[]), &HashMap::new()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid environment variable reference: '{' at column 2 must be followed by a \
         closing '}'"
    );
}

/// #383: names follow Batect's rules. Unbraced, a name is the longest run of
/// letters, digits and `_` — a leading digit and a non-ASCII letter included,
/// and `.` and `-` ending it for either sigil (#384). Braced, it is
/// everything up to the `}` — or, for `$`, the `:` of a default.
#[test]
fn names_follow_batects_rules() {
    let env = host_env(&[
        ("5", "five"),
        ("1abc", "one"),
        ("é", "e-acute"),
        ("a b", "spaced"),
        ("FOO.BAR", "dotted"),
        ("A", "x"),
    ]);
    let config_vars = HashMap::from([
        ("my-var".to_string(), Some("dashed".to_string())),
        ("SOME_VAR:".to_string(), Some("coloned".to_string())),
        ("foo".to_string(), Some("x".to_string())),
    ]);
    for (input, expected) in [
        ("$5", "five"),
        ("$1abc", "one"),
        ("$é", "e-acute"),
        ("${a b}", "spaced"),
        ("${FOO.BAR}", "dotted"),
        ("$A-B", "x-B"),
        ("<{my-var}", "dashed"),
        ("<{SOME_VAR:}", "coloned"),
        ("<foo.txt", "x.txt"),
        ("<foo-bar", "x-bar"),
    ] {
        assert_eq!(
            interpolate(input, &env, &config_vars).unwrap(),
            expected,
            "interpolating {input:?}"
        );
    }
}

/// #384: an unbraced config variable's name ends at a `.`, even when a
/// longer dotted name is declared too — only braces reach that one.
#[test]
fn an_unbraced_config_variable_name_ends_at_a_dot() {
    let config_vars = HashMap::from([
        ("foo".to_string(), Some("A".to_string())),
        ("foo.bar".to_string(), Some("B".to_string())),
        ("dir".to_string(), Some("d".to_string())),
    ]);
    for (input, expected) in [
        ("<foo.bar", "A.bar"),
        ("<{foo.bar}", "B"),
        ("see <dir.", "see d."),
    ] {
        assert_eq!(
            interpolate(input, host_env(&[]), &config_vars).unwrap(),
            expected,
            "interpolating {input:?}"
        );
    }
}

/// #385: every malformed reference Batect refuses is refused, by
/// `check_syntax` as well as `interpolate`, saying what is wrong and at
/// which column of the value — Batect's own wording, from its
/// `ExpressionSpec`, plus the cases its refuters added.
#[test]
fn a_malformed_reference_is_an_error_naming_its_column() {
    for (input, expected) in [
        ("$", "invalid environment variable reference: '$' at column 1 must be followed by a variable name"),
        ("a $", "invalid environment variable reference: '$' at column 3 must be followed by a variable name"),
        ("${", "invalid environment variable reference: '{' at column 2 must be followed by a closing '}'"),
        ("${}", "invalid environment variable reference: '${}' at column 1 does not contain a variable name"),
        ("${some", "invalid environment variable reference: '{' at column 2 must be followed by a closing '}'"),
        ("${some:", "invalid environment variable reference: ':' at column 7 must be immediately followed by '-'"),
        ("${some:}", "invalid environment variable reference: ':' at column 7 must be immediately followed by '-'"),
        ("${X:default}", "invalid environment variable reference: ':' at column 4 must be immediately followed by '-'"),
        ("${a:x:-y}", "invalid environment variable reference: ':' at column 4 must be immediately followed by '-'"),
        (r"${some:-b\", r"invalid environment variable reference: '\' at column 10 must be immediately followed by a character to escape"),
        ("${:-}", "invalid environment variable reference: '${:-}' at column 1 does not contain a variable name"),
        ("${:-default}", "invalid environment variable reference: '${:-default}' at column 1 does not contain a variable name"),
        ("<", "invalid config variable reference: '<' at column 1 must be followed by a variable name"),
        ("a < b", "invalid config variable reference: '<' at column 3 does not contain a variable name"),
        ("<{", "invalid config variable reference: '{' at column 2 must be followed by a closing '}'"),
        ("<{some", "invalid config variable reference: '{' at column 2 must be followed by a closing '}'"),
        ("<{}", "invalid config variable reference: '<{}' at column 1 does not contain a variable name"),
    ] {
        let err = check_syntax(input).unwrap_err();
        assert_eq!(err.to_string(), expected, "checking {input:?}");
        let err = interpolate(input, host_env(&[]), &HashMap::new()).unwrap_err();
        assert_eq!(err.to_string(), expected, "interpolating {input:?}");
    }
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

/// #384: the built-in is reachable braced only, as in Batect — unbraced, the
/// name ends at the `.`, so it reads a variable named `batect`.
#[test]
fn an_unbraced_built_in_reads_a_variable_named_batect() {
    let mut config_vars = HashMap::new();
    config_vars.insert(
        "batect.project_directory".to_string(),
        Some("/abs/project".to_string()),
    );
    let err = interpolate("<batect.project_directory", host_env(&[]), &config_vars).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Config variable 'batect' is not declared in 'config_variables'"
    );
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
/// Asserted as an equivalence against `interpolate` itself, so the two cannot
/// drift: a value is an expression exactly when what it interpolates to
/// depends on its variables' values. Not "interpolation changed it" — an
/// escape (#382) changes the output without being a reference, so `a\b` and
/// `\$TAG` are literals that interpolate to something else.
#[test]
fn detection_agrees_with_interpolation_about_what_is_an_expression() {
    let interpolate_with = |input: &str, value: &str| {
        let config_vars = HashMap::from([("tag".to_string(), Some(value.to_string()))]);
        interpolate(input, |_: &str| Some(value.to_string()), &config_vars)
    };

    for input in [
        // Expressions, in every form the syntax has.
        "$TAG",
        "${TAG}",
        "${MISSING:-latest}",
        "repo/img:$TAG",
        "$REGISTRY/repo/img:${TAG}",
        "<tag",
        "<{tag}",
        "<undeclared",
        // Names under Batect's rules (#383), which an ASCII identifier
        // would miss.
        "$1nvalid",
        "price: $100",
        "${a b}",
        "<{my-var}",
        // An escaped backslash, then a real reference.
        r"\\$TAG",
        // Literals — including the near-misses that make a naive
        // "contains a sigil" check wrong.
        "alpine:3.18",
        "repo/img:1.2.3",
        // Escapes: literal, though interpolation drops the backslash.
        r"\$TAG",
        r"\<tag",
        r"a\b",
    ] {
        match (interpolate_with(input, "1"), interpolate_with(input, "2")) {
            (Ok(first), Ok(second)) => assert_eq!(
                contains_expression(input),
                first != second,
                "detection and interpolation disagree on {input:?} (interpolated to \
                 {first:?} and {second:?})"
            ),
            // Well-formed but unresolvable is still an expression — see
            // `an_unresolvable_variable_still_counts_as_an_expression`.
            _ => assert!(
                contains_expression(input),
                "interpolation treated {input:?} as an expression; detection did not"
            ),
        }
    }
}

/// A malformed value is no expression (#385), whatever references come
/// before its mistake: it never loads as one, so nothing reads it as one — and `image` in a `batect.yml`, which is never parsed,
/// is used as written rather than refused as an expression.
#[test]
fn a_malformed_value_is_not_an_expression() {
    for input in [
        "$",
        "$ ",
        "img$",
        "${unterminated",
        "<",
        "<{unterminated",
        // A reference before the mistake doesn't make it one.
        "$HOME/a$",
        "<x/$",
    ] {
        assert!(check_syntax(input).is_err(), "{input:?}");
        assert!(!contains_expression(input), "{input:?}");
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

/// #382: `\` escapes the next character in literal text, as in Batect — the
/// backslash is dropped and that character is taken literally, so an escaped
/// `$` or `<` starts no reference whether or not its variable resolves.
#[test]
fn a_backslash_escapes_the_next_character_in_literal_text() {
    let config_vars = HashMap::from([("name".to_string(), Some("value".to_string()))]);
    let env = host_env(&[("X", "set")]);
    for (input, expected) in [
        (r"\$X", "$X"),
        (r"\$UNSET", "$UNSET"),
        (r"\<name", "<name"),
        (r"\<undeclared", "<undeclared"),
        (r"a\b", "ab"),
        (r"\\", r"\"),
        (r"\\$X", r"\set"),
    ] {
        assert_eq!(
            interpolate(input, &env, &config_vars).unwrap(),
            expected,
            "interpolating {input:?}"
        );
    }
}

/// #382: a `\` with nothing after it escapes nothing, so Batect refuses the
/// value rather than guessing; so does this, whatever its variables hold —
/// so `check_syntax` refuses it too. The caller quotes the value, as it
/// names the field.
#[test]
fn a_trailing_backslash_is_an_error() {
    for input in [r"a\", r"\", r"$X\"] {
        assert!(check_syntax(input).is_err(), "{input:?}");
        let err = interpolate(input, host_env(&[("X", "set")]), &HashMap::new()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("must be immediately followed by a character to escape"),
            "{message}"
        );
    }
}

/// #382: inside a `${NAME:-default}`, `\` escapes the next character into the
/// default, so an escaped `}` does not close the reference.
#[test]
fn a_backslash_escapes_the_next_character_in_a_default() {
    for (input, env, expected) in [
        (r"${U:-a\}b}", host_env(&[]), "a}b"),
        (r"${U:-x\y}", host_env(&[]), "xy"),
        (r"${U:-a\\}b", host_env(&[]), r"a\b"),
        (r"${U:-a\}b}", host_env(&[("U", "set")]), "set"),
    ] {
        assert_eq!(
            interpolate(input, env, &HashMap::new()).unwrap(),
            expected,
            "interpolating {input:?}"
        );
    }
}

/// #382: escapes apply in literal text and defaults, never in a name, as in
/// Batect. A braced name runs to its `}` whatever it holds (#383), so
/// `${A\B}` names `A\B` — not `AB`, though `AB` is set.
#[test]
fn a_backslash_in_a_variable_name_is_not_an_escape() {
    let env = host_env(&[("AB", "wrong"), (r"A\B", "right")]);
    assert_eq!(
        interpolate(r"${A\B}", env, &HashMap::new()).unwrap(),
        "right"
    );
}

/// #382: an escaped sigil is no reference, so it is no expression either —
/// but an escaped backslash leaves the sigil after it a real one.
#[test]
fn an_escaped_sigil_is_not_an_expression() {
    assert!(!contains_expression(r"\$X"));
    assert!(!contains_expression(r"\<x"));
    assert!(!contains_expression(r"a\b"));
    assert!(contains_expression(r"\\$X"));
}

/// `check_syntax` judges the value as written, so a reference it cannot
/// resolve passes — that is evaluation's to judge.
#[test]
fn check_syntax_accepts_an_unresolvable_reference() {
    check_syntax("$UNSET<undeclared${U:-a\\}b}").unwrap();
}

/// A literal read as an expression (`config convert`, #385/#628) keeps
/// every `\` as a backslash and every reference as one — `$`/`<` reading as
/// they would if the field were an expression — unless that leaves no valid
/// expression, when it is kept wholly literal. Either way, whether the
/// result holds a reference is what `contains_expression` says of it.
#[test]
fn a_literal_as_an_expression_keeps_its_backslashes_and_references() {
    let env = host_env(&[("X", "set")]);
    let config_vars = HashMap::from([("x".to_string(), Some("cv".to_string()))]);
    for (literal, expected, has_reference) in [
        ("plain", "plain", false),
        (r"foo\bar", r"foo\bar", false),
        (r"\\", r"\\", false),
        (r"trailing\", r"trailing\", false),
        ("$X", "set", true),
        (r"$X\y", r"set\y", true),
        (r"a\$X", r"a\set", true),
        (r"\<x/$X", r"\cv/set", true),
        // No valid expression either way: kept as written.
        ("img$", "img$", false),
        (r"img\$", r"img\$", false),
        ("a < b", "a < b", false),
        ("$X$", "$X$", false),
        ("${X:default}", "${X:default}", false),
    ] {
        let converted = literal_as_expression(literal);
        assert_eq!(
            interpolate(&converted, &env, &config_vars).unwrap(),
            expected,
            "{literal:?} as {converted:?}"
        );
        assert_eq!(
            contains_expression(&converted),
            has_reference,
            "{literal:?} as {converted:?}"
        );
    }
}
