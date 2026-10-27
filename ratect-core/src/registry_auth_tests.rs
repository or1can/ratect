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
fn a_bare_single_segment_name_has_no_domain() {
    assert_eq!(registry_hostname("nginx"), "docker.io");
}

#[test]
fn a_bare_single_segment_name_has_no_domain_even_if_it_would_otherwise_look_like_one() {
    // No `/` at all means no domain, regardless of the name's own spelling
    // — `localhost`/`MyRegistry`/a dotted name alone are all just image
    // names here, not registries.
    assert_eq!(registry_hostname("localhost"), "docker.io");
    assert_eq!(registry_hostname("MyRegistry"), "docker.io");
    assert_eq!(registry_hostname("example.com"), "docker.io");
}

#[test]
fn a_two_segment_name_with_no_domain_like_first_segment_has_no_domain() {
    assert_eq!(registry_hostname("myuser/myimage"), "docker.io");
}

#[test]
fn a_dotted_first_segment_is_the_domain() {
    assert_eq!(
        registry_hostname("myregistry.example.com/foo/bar"),
        "myregistry.example.com"
    );
}

#[test]
fn localhost_is_always_a_domain() {
    assert_eq!(registry_hostname("localhost/foo"), "localhost");
}

#[test]
fn localhost_with_a_port_is_the_domain_including_the_port() {
    assert_eq!(registry_hostname("localhost:5000/foo"), "localhost:5000");
}

#[test]
fn index_docker_io_canonicalizes_to_docker_io() {
    assert_eq!(
        registry_hostname("index.docker.io/library/nginx"),
        "docker.io"
    );
}

#[test]
fn any_uppercase_character_in_the_first_segment_makes_it_a_domain() {
    assert_eq!(registry_hostname("MyRegistry/foo/bar"), "MyRegistry");
}

#[test]
fn configured_registries_unions_auths_and_cred_helpers_keys() {
    let config = r#"{
        "auths": {
            "myregistry.example.com": { "auth": "dGVzdDp0ZXN0" }
        },
        "credHelpers": {
            "123456789.dkr.ecr.us-east-1.amazonaws.com": "ecr-login"
        }
    }"#;
    assert_eq!(
        configured_registries(config),
        vec![
            "123456789.dkr.ecr.us-east-1.amazonaws.com".to_string(),
            "myregistry.example.com".to_string(),
        ]
    );
}

#[test]
fn configured_registries_deduplicates_a_registry_in_both_maps() {
    let config = r#"{
        "auths": { "myregistry.example.com": { "auth": "dGVzdDp0ZXN0" } },
        "credHelpers": { "myregistry.example.com": "some-helper" }
    }"#;
    assert_eq!(
        configured_registries(config),
        vec!["myregistry.example.com".to_string()]
    );
}

#[test]
fn configured_registries_is_empty_for_a_config_with_neither_field() {
    assert_eq!(configured_registries("{}"), Vec::<String>::new());
}

#[test]
fn configured_registries_is_empty_for_malformed_json() {
    assert_eq!(
        configured_registries("not json at all"),
        Vec::<String>::new()
    );
}

#[test]
fn username_password_maps_to_the_matching_bollard_fields() {
    let credential = to_bollard_credentials(docker_credential::DockerCredential::UsernamePassword(
        "alice".to_string(),
        "hunter2".to_string(),
    ));
    assert_eq!(
        credential,
        bollard::auth::DockerCredentials {
            username: Some("alice".to_string()),
            password: Some("hunter2".to_string()),
            ..Default::default()
        }
    );
}

#[test]
fn identity_token_maps_to_the_identitytoken_field() {
    let credential = to_bollard_credentials(docker_credential::DockerCredential::IdentityToken(
        "some-token".to_string(),
    ));
    assert_eq!(
        credential,
        bollard::auth::DockerCredentials {
            identitytoken: Some("some-token".to_string()),
            ..Default::default()
        }
    );
}

#[test]
fn recognizes_the_real_credential_helpers_not_found_sentinel() {
    assert!(is_credentials_not_found(
        "credentials not found in native keychain"
    ));
    // Real output isn't necessarily trimmed to exactly the sentinel.
    assert!(is_credentials_not_found(
        "error getting credentials - credentials not found in native keychain\n"
    ));
}

#[test]
fn does_not_treat_an_unrelated_helper_failure_as_not_found() {
    assert!(!is_credentials_not_found(""));
    assert!(!is_credentials_not_found(
        "exec: \"docker-credential-nonexistent\": executable file not found in $PATH"
    ));
}

/// A helper's failure is one line: the helper, then what it printed —
/// stderr, where a helper explains itself, else stdout — with its line
/// breaks and blank lines collapsed. The library's own message is a
/// multi-line `stdout:`/`stderr:` block, which on stderr in every style
/// (decisions/0017) left `proceeding without them` stranded on a line of
/// its own.
#[test]
fn a_helper_failure_is_summarised_on_one_line() {
    assert_eq!(
        helper_failure_summary("docker-credential-x", "", "helper exploded\n"),
        "`docker-credential-x` failed: helper exploded"
    );
    assert_eq!(
        helper_failure_summary("docker-credential-x", "  first\n\n second  \n", ""),
        "`docker-credential-x` failed: first second"
    );
    assert_eq!(
        helper_failure_summary("docker-credential-x", "on stdout", "on stderr"),
        "`docker-credential-x` failed: on stderr"
    );
    assert_eq!(
        helper_failure_summary("docker-credential-x", "\n", " \n"),
        "`docker-credential-x` failed with no output"
    );
}

/// A fresh, unique scratch directory — same pattern as `config_tests.rs`'s
/// `unique_temp_dir`. Caller cleans up.
fn unique_temp_dir() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let dir = std::env::temp_dir().join(format!(
        "ratect-registry-auth-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        count
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn a_missing_config_file_resolves_as_nothing_configured() {
    let config_directory = unique_temp_dir();
    let resolver = DockerCredentialHelperResolver::new(config_directory.clone());

    // Cleanup runs before either fallible step below is unwrapped, so a
    // regression that makes `resolve()` return `Err` can't leak the temp
    // directory alongside the test failure it should also produce.
    let result = resolver.resolve("myregistry.example.com").await;
    std::fs::remove_dir_all(config_directory).ok();

    assert_eq!(
        result.unwrap(),
        None,
        "a fresh machine with no config.json at all should resolve like an empty one"
    );
}

#[tokio::test]
async fn an_empty_config_file_resolves_as_nothing_configured() {
    let config_directory = unique_temp_dir();
    std::fs::write(config_directory.join("config.json"), "{}").unwrap();
    let resolver = DockerCredentialHelperResolver::new(config_directory.clone());

    let result = resolver.resolve("myregistry.example.com").await;
    std::fs::remove_dir_all(config_directory).ok();

    assert_eq!(result.unwrap(), None);
}

/// ratect#306: a build also sends what the default `credsStore` holds, so
/// which store is configured has to be read too.
#[test]
fn configured_creds_store_names_the_default_store() {
    assert_eq!(
        configured_creds_store(r#"{"credsStore": "desktop"}"#).as_deref(),
        Some("desktop")
    );
    for config in ["{}", r#"{"credsStore": ""}"#, "not json", ""] {
        assert_eq!(configured_creds_store(config), None, "{config:?}");
    }
}

/// A unique scratch directory, removed by the caller.
fn scratch_directory() -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!("ratect-306-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// A stand-in credential helper at an absolute path, so no test has to
/// change `PATH`: a shell script that answers `list` with `body` on stdout
/// and exits `status`.
#[cfg(unix)]
fn stub_helper(directory: &std::path::Path, body: &str, status: i32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let helper = directory.join("docker-credential-stub");
    std::fs::write(
        &helper,
        format!("#!/bin/sh\n[ \"$1\" = list ] || exit 9\nprintf '%s' '{body}'\nexit {status}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    helper
}

#[cfg(unix)]
#[tokio::test]
async fn listing_a_store_gives_the_server_urls_its_helper_reports() {
    let directory = scratch_directory();
    let helper = stub_helper(
        &directory,
        r#"{"https://index.docker.io/v1/": "alice", "registry.example.com": "bob"}"#,
        0,
    );
    let listed = list_store_with(helper.as_os_str()).await;
    std::fs::remove_dir_all(&directory).unwrap();
    assert_eq!(
        listed.unwrap(),
        vec![
            "https://index.docker.io/v1/".to_string(),
            "registry.example.com".to_string()
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn listing_a_store_fails_for_a_helper_that_fails_or_answers_nonsense() {
    let directory = scratch_directory();
    let failing = stub_helper(&directory, "the keychain is locked", 1);
    let failed = list_store_with(failing.as_os_str()).await;
    let nonsense = stub_helper(&directory, "not json", 0);
    let unparsed = list_store_with(nonsense.as_os_str()).await;
    let missing = list_store_with(directory.join("no-such-helper").as_os_str()).await;
    std::fs::remove_dir_all(&directory).unwrap();

    let failed = format!("{:#}", failed.unwrap_err());
    assert!(failed.contains("the keychain is locked"), "{failed}");
    assert!(unparsed.is_err());
    assert!(missing.is_err());
}
