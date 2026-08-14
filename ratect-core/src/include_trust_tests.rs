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

fn id(remote: &str) -> BundleId {
    BundleId {
        remote: remote.to_string(),
        git_ref: "main".to_string(),
    }
}

fn asks(host_paths: bool, nested_git: Option<bool>) -> Grants {
    Grants {
        host_paths,
        nested_git,
    }
}

/// The rule table both walkers now share. Previously asserted against
/// completion's own copy of it, by a test named as though it compared the two
/// implementations — it never touched the loader's.
#[test]
fn an_owned_file_confers_exactly_what_it_asks_for() {
    let nothing = Bundle::granted(None, id("a"), asks(false, None));
    assert_eq!(nothing.trust, Trust::NONE);

    let host = Bundle::granted(None, id("a"), asks(true, None));
    assert!(host.trust.host_paths && !host.trust.nested_git);

    let nested = Bundle::granted(None, id("a"), asks(false, Some(true)));
    assert!(!nested.trust.host_paths && nested.trust.nested_git);

    let both = Bundle::granted(None, id("a"), asks(true, Some(true)));
    assert!(both.trust.host_paths && both.trust.nested_git);
}

/// Writing the field as `false` is the same as not writing it — the flag
/// grants, it never revokes.
#[test]
fn an_explicit_false_confers_nothing() {
    let explicit = Bundle::granted(None, id("a"), asks(false, Some(false)));
    assert_eq!(explicit.trust, Trust::NONE);
}

/// The property that makes a grant one level deep rather than a subtree: a
/// bundle can neither grant itself anything nor pass on what it was granted,
/// however loudly its own entries ask.
#[test]
fn a_bundle_cannot_grant_anything_however_much_it_asks() {
    let granted = Bundle::granted(None, id("outer"), asks(true, Some(true)));
    assert!(granted.trust.host_paths && granted.trust.nested_git);

    let passed_on = Bundle::granted(Some(&granted), id("inner"), asks(true, Some(true)));
    assert_eq!(
        passed_on.trust,
        Trust::NONE,
        "a grant stops at the bundle it admits"
    );

    let third = Bundle::granted(Some(&passed_on), id("deeper"), asks(true, Some(true)));
    assert_eq!(third.trust, Trust::NONE);
}

/// `allow_nested_git_includes` is native-only, so nothing restricts a
/// `batect.yml` — and both the gate and the clone-detail redaction fall away
/// together, because both read this one answer.
#[test]
fn a_batect_yml_is_never_restricted() {
    let bundle = Bundle::granted(None, id("outer"), asks(false, None));
    assert!(restricting(Some(&bundle), ConfigFormat::Compat).is_none());
    assert!(restricting(Some(&bundle), ConfigFormat::Native).is_some());
}

/// An owned file declares its own includes, so there is no bundle to ask.
#[test]
fn an_owned_file_is_never_restricted() {
    assert!(restricting(None, ConfigFormat::Native).is_none());
    assert!(restricting(None, ConfigFormat::Compat).is_none());
}

#[test]
fn a_restricted_bundle_may_declare_a_git_include_only_when_granted() {
    let ungranted = Bundle::granted(None, id("outer"), asks(false, None));
    let error = check_may_declare_git(Some(&ungranted), "inner").unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("The bundle 'outer' at 'main' declares a Git include of its own"),
        "names the bundle responsible, not just the remote it wanted: {message}"
    );
    assert!(
        message.contains("Set 'allow_nested_git_includes' to true"),
        "gives the remedy: {message}"
    );
    assert!(
        !message.contains(": true") && !message.contains("= true"),
        "names the field without spelling either syntax, since a native \
         project's include entry can sit in a local .yml: {message}"
    );

    let granted = Bundle::granted(None, id("outer"), asks(false, Some(true)));
    assert!(check_may_declare_git(Some(&granted), "inner").is_ok());
    assert!(check_may_declare_git(None, "inner").is_ok());
}

/// The same decision as the test above, through the entry point completion
/// uses — the one walker whose own wiring stays untested, because reaching it
/// needs a populated `~/.ratect/incl` and `cached_working_copy` hardcodes the
/// real home.
#[test]
fn completion_and_the_loader_read_one_answer() {
    let ungranted = Bundle::granted(None, id("outer"), asks(false, None));
    let granted = Bundle::granted(None, id("outer"), asks(false, Some(true)));

    for restricted in [Some(&ungranted), Some(&granted), None] {
        assert_eq!(
            refusing_nested_git(restricted).is_some(),
            check_may_declare_git(restricted, "inner").is_err(),
            "completion declines exactly when the loader refuses"
        );
    }
}

#[test]
fn the_native_only_field_is_refused_in_a_compat_project() {
    let error =
        check_dialect(asks(false, Some(true)), None, "inner", ConfigFormat::Compat).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("The Git include of 'inner' sets 'allow_nested_git_includes'"));
    assert!(message.contains("not supported in Batect-compatible configuration"));
}

/// The field can appear inside a bundle, which a `ratect-compat` user cannot
/// edit — an error about "the Git include of X" would describe a file they
/// have never seen.
#[test]
fn the_compat_refusal_names_the_bundle_that_wrote_the_field() {
    let declaring = Bundle::granted(None, id("outer"), asks(false, None));
    let error = check_dialect(
        asks(false, Some(true)),
        Some(&declaring),
        "inner",
        ConfigFormat::Compat,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains(
        "The bundle 'outer' at 'main' sets 'allow_nested_git_includes' on its Git include \
         of 'inner'"
    ));
}

/// Rejected whichever value was written: the field is unsupported, not the
/// permission.
#[test]
fn a_compat_project_refuses_the_field_set_to_false_too() {
    assert!(check_dialect(
        asks(false, Some(false)),
        None,
        "inner",
        ConfigFormat::Compat
    )
    .is_err());
    assert!(check_dialect(asks(false, None), None, "inner", ConfigFormat::Compat).is_ok());
    assert!(check_dialect(asks(false, Some(true)), None, "inner", ConfigFormat::Native).is_ok());
}

#[test]
fn an_unrestricted_clone_failure_keeps_gits_own_error() {
    let error = hide_clone_detail(
        anyhow::anyhow!("fatal: could not read from remote"),
        "r",
        "v1",
        None,
    );
    let message = format!("{error:#}");
    assert!(message.contains("Failed to resolve Git include 'r' at 'v1'"));
    assert!(
        message.contains("fatal: could not read from remote"),
        "the owner named this remote themselves, so the detail is theirs: {message}"
    );
}

#[test]
fn a_restricted_clone_failure_withholds_gits_error() {
    let declaring = Bundle::granted(None, id("outer"), asks(false, Some(true)));
    let error = hide_clone_detail(
        anyhow::anyhow!("fatal: could not read from remote"),
        "internal.example",
        "v1",
        Some(&declaring),
    );
    let message = format!("{error:#}");
    assert!(
        !message.contains("fatal: could not read from remote"),
        "a bundle able to name hosts and read the log can walk a network: {message}"
    );
    assert!(message.contains("declared by the bundle 'outer' at 'main'"));
    assert!(message.contains("RUST_LOG=debug"));
}

#[test]
fn a_grant_the_winning_route_did_not_carry_is_refused() {
    let file = PathBuf::from("/clone/bundle.toml");
    let mut effective = EffectiveGrants::default();
    effective.record(file.clone(), Trust::NONE);

    for (wanted, field) in [
        (
            Trust {
                host_paths: true,
                nested_git: false,
            },
            "allow_host_paths",
        ),
        (
            Trust {
                host_paths: false,
                nested_git: true,
            },
            "allow_nested_git_includes",
        ),
    ] {
        let error = effective.check(&file, wanted, "shared").unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains(&format!("'{field}' was set on the include of 'shared'")),
            "names the field that did nothing and the repository it was on: {message}"
        );
        assert!(
            message.contains("Declare this include in your root configuration file"),
            "gives the remedy: {message}"
        );
    }
}

/// The ordinary case, and the reason this only ever reports a permission being
/// *lost*: a bundle reaching a repository the owner also vouched for asks for
/// less than the winning route, and the stricter ask is already satisfied.
#[test]
fn a_route_asking_for_less_than_the_winner_still_loads() {
    let file = PathBuf::from("/clone/bundle.toml");
    let mut effective = EffectiveGrants::default();
    effective.record(
        file.clone(),
        Trust {
            host_paths: true,
            nested_git: true,
        },
    );

    assert!(effective.check(&file, Trust::NONE, "shared").is_ok());
    assert!(effective
        .check(
            &file,
            Trust {
                host_paths: true,
                nested_git: false,
            },
            "shared"
        )
        .is_ok());
}

/// A file nothing has recorded yet carries nothing, so an entry asking for
/// nothing is fine and one asking for a grant is not — the same table, with
/// the absent entry standing in for [`Trust::NONE`].
#[test]
fn an_unrecorded_file_is_treated_as_granted_nothing() {
    let effective = EffectiveGrants::default();
    let file = PathBuf::from("/clone/bundle.toml");
    assert!(effective.check(&file, Trust::NONE, "shared").is_ok());
    assert!(effective
        .check(
            &file,
            Trust {
                host_paths: true,
                nested_git: false,
            },
            "shared"
        )
        .is_err());
}
