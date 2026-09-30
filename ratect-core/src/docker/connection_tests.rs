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

/// A fresh, unique scratch directory — same pattern as `docker_tests.rs`'s
/// own `unique_temp_dir` (unshared: these are two different modules now,
/// each with its own counter, which is fine since neither call site needs
/// the other's). Caller cleans up.
fn unique_temp_dir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let dir = std::env::temp_dir().join(format!(
        "ratect-docker-connection-test-{}-{}-{}",
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

#[test]
fn docker_context_id_matches_the_docker_cli_own_hashing() {
    // Verified against a real `~/.docker/contexts/meta/<id>` entry on
    // this machine: `printf 'orbstack' | shasum -a 256`.
    assert_eq!(
        docker_context_id("orbstack"),
        "2d89b732b01a00a2d1675ed3cee9fd0f965daadf90603c989dd3afd4569c6896"
    );
}

fn write_docker_context_meta(config_directory: &Path, context_name: &str, host: &str) {
    write_docker_context_meta_with_skip(config_directory, context_name, host, false);
}

/// `write_docker_context_meta`, with the endpoint's `SkipTLSVerify` flag
/// set as given — the Docker CLI writes the field either way.
fn write_docker_context_meta_with_skip(
    config_directory: &Path,
    context_name: &str,
    host: &str,
    skip_tls_verify: bool,
) {
    let id = docker_context_id(context_name);
    let meta_dir = config_directory.join("contexts").join("meta").join(&id);
    fs::create_dir_all(&meta_dir).unwrap();
    fs::write(
            meta_dir.join("meta.json"),
            format!(
                r#"{{"Name":"{context_name}","Metadata":{{}},"Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":{skip_tls_verify}}}}}}}"#
            ),
        )
        .unwrap();
}

/// Writes `files` (name, contents) into `context_name`'s TLS directory, the
/// layout `docker context create --docker ca=…,cert=…,key=…` produces.
fn write_docker_context_tls_files(
    config_directory: &Path,
    context_name: &str,
    files: &[(&str, &str)],
) -> PathBuf {
    let tls_directory = docker_context_tls_directory(config_directory, context_name);
    fs::create_dir_all(&tls_directory).unwrap();
    for (name, contents) in files {
        fs::write(tls_directory.join(name), contents).unwrap();
    }
    tls_directory
}

fn tls_file(path: impl Into<PathBuf>, named_by: &str) -> TlsFile {
    TlsFile {
        path: path.into(),
        named_by: named_by.to_string(),
    }
}

#[test]
fn docker_context_endpoint_reads_the_endpoints_docker_host_field() {
    let config_directory = unique_temp_dir();
    write_docker_context_meta(
        &config_directory,
        "orbstack",
        "unix:///Users/kevin/.orbstack/run/docker.sock",
    );

    let endpoint = docker_context_endpoint(&config_directory, "orbstack").unwrap();
    assert_eq!(
        endpoint,
        ContextEndpoint {
            host: "unix:///Users/kevin/.orbstack/run/docker.sock".to_string(),
            tls: None,
        }
    );
}

#[test]
fn docker_context_endpoint_errors_clearly_when_the_context_does_not_exist() {
    let config_directory = unique_temp_dir();
    let err = docker_context_endpoint(&config_directory, "no-such-context").unwrap_err();
    assert!(
        err.to_string()
            .contains("Docker context 'no-such-context' does not exist"),
        "{err}"
    );
}

#[test]
fn docker_context_tls_directory_matches_the_docker_cli_layout() {
    assert_eq!(
        docker_context_tls_directory(Path::new("/cfg"), "orbstack"),
        Path::new("/cfg/contexts/tls")
            .join("2d89b732b01a00a2d1675ed3cee9fd0f965daadf90603c989dd3afd4569c6896")
            .join("docker")
    );
}

#[test]
fn docker_context_endpoint_uses_a_stored_ca_certificate_and_key() {
    let config_directory = unique_temp_dir();
    write_docker_context_meta(&config_directory, "remote", "tcp://1.2.3.4:2376");
    let tls_directory = write_docker_context_tls_files(
        &config_directory,
        "remote",
        &[("ca.pem", "ca"), ("cert.pem", "cert"), ("key.pem", "key")],
    );

    let endpoint = docker_context_endpoint(&config_directory, "remote").unwrap();
    assert_eq!(
        endpoint.tls,
        Some(TlsSettings {
            ca: Some(tls_file(
                tls_directory.join("ca.pem"),
                "Docker context 'remote'"
            )),
            client: Some(ClientCertificate {
                cert: tls_file(tls_directory.join("cert.pem"), "Docker context 'remote'"),
                key: tls_file(tls_directory.join("key.pem"), "Docker context 'remote'"),
            }),
            skip_verify_requested: false,
        })
    );
}

#[test]
fn docker_context_endpoint_with_only_a_stored_ca_uses_tls_without_a_client_certificate() {
    let config_directory = unique_temp_dir();
    write_docker_context_meta(&config_directory, "remote", "tcp://1.2.3.4:2376");
    let tls_directory =
        write_docker_context_tls_files(&config_directory, "remote", &[("ca.pem", "ca")]);

    let endpoint = docker_context_endpoint(&config_directory, "remote").unwrap();
    assert_eq!(
        endpoint.tls,
        Some(TlsSettings {
            ca: Some(tls_file(
                tls_directory.join("ca.pem"),
                "Docker context 'remote'"
            )),
            client: None,
            skip_verify_requested: false,
        })
    );
}

#[test]
fn docker_context_endpoint_with_only_skip_tls_verify_uses_tls_against_the_system_roots() {
    let config_directory = unique_temp_dir();
    write_docker_context_meta_with_skip(&config_directory, "remote", "tcp://1.2.3.4:2376", true);

    let endpoint = docker_context_endpoint(&config_directory, "remote").unwrap();
    assert_eq!(
        endpoint.tls,
        Some(TlsSettings {
            ca: None,
            client: None,
            skip_verify_requested: true,
        })
    );
}

#[test]
fn docker_context_endpoint_errors_when_only_half_a_client_key_pair_is_stored() {
    for (stored, missing) in [("cert.pem", "key.pem"), ("key.pem", "cert.pem")] {
        let config_directory = unique_temp_dir();
        write_docker_context_meta(&config_directory, "remote", "tcp://1.2.3.4:2376");
        write_docker_context_tls_files(&config_directory, "remote", &[(stored, "x")]);

        let err = docker_context_endpoint(&config_directory, "remote").unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("Docker context 'remote'"), "{message}");
        assert!(message.contains(stored), "{message}");
        assert!(message.contains(missing), "{message}");
    }
}

#[test]
fn active_docker_context_reads_current_context_from_config_json() {
    let config_directory = unique_temp_dir();
    fs::write(
        config_directory.join("config.json"),
        r#"{"currentContext":"orbstack"}"#,
    )
    .unwrap();

    assert_eq!(
        active_docker_context(&config_directory),
        Some("orbstack".to_string())
    );
}

#[test]
fn active_docker_context_is_none_when_config_json_is_missing() {
    let config_directory = unique_temp_dir();
    assert_eq!(active_docker_context(&config_directory), None);
}

#[test]
fn active_docker_context_is_none_when_current_context_is_unset_or_empty() {
    let config_directory = unique_temp_dir();
    fs::write(config_directory.join("config.json"), r#"{}"#).unwrap();
    assert_eq!(active_docker_context(&config_directory), None);

    fs::write(
        config_directory.join("config.json"),
        r#"{"currentContext":""}"#,
    )
    .unwrap();
    assert_eq!(active_docker_context(&config_directory), None);
}

#[test]
fn resolve_context_name_prefers_an_explicit_context_over_everything_else() {
    let options = DockerConnectionOptions {
        context: Some("explicit".to_string()),
        ..Default::default()
    };
    assert_eq!(
        resolve_context_name(
            &options,
            Some("tcp://from-env:2375"),
            Some("env-context"),
            Some("active".to_string())
        ),
        Some("explicit".to_string())
    );
}

#[test]
fn resolve_context_name_an_explicit_host_skips_context_resolution_entirely() {
    let options = DockerConnectionOptions {
        host: Some("tcp://1.2.3.4:2375".to_string()),
        ..Default::default()
    };
    let host = resolve_host(&options, None);
    assert_eq!(
        resolve_context_name(
            &options,
            host.as_deref(),
            Some("env-context"),
            Some("active".to_string())
        ),
        None
    );
}

#[test]
fn resolve_context_name_a_host_from_the_environment_skips_context_resolution_too() {
    let options = DockerConnectionOptions::default();
    let host = resolve_host(&options, Some("tcp://from-env:2375"));
    assert_eq!(
        resolve_context_name(
            &options,
            host.as_deref(),
            Some("env-context"),
            Some("active".to_string())
        ),
        None
    );
    assert_eq!(
        resolve_context_name(&options, host.as_deref(), None, Some("active".to_string())),
        None
    );
}

#[test]
fn resolve_context_name_falls_back_to_the_env_var_then_the_active_context() {
    let options = DockerConnectionOptions::default();
    assert_eq!(
        resolve_context_name(
            &options,
            None,
            Some("env-context"),
            Some("active".to_string())
        ),
        Some("env-context".to_string())
    );
    assert_eq!(
        resolve_context_name(&options, None, None, Some("active".to_string())),
        Some("active".to_string())
    );
    assert_eq!(resolve_context_name(&options, None, None, None), None);
}

#[test]
fn resolve_context_name_treats_an_empty_docker_context_as_unset() {
    let options = DockerConnectionOptions::default();
    assert_eq!(
        resolve_context_name(&options, None, Some(""), Some("active".to_string())),
        Some("active".to_string())
    );
}

#[test]
fn resolve_context_name_treats_the_default_context_name_as_no_context() {
    let options = DockerConnectionOptions {
        context: Some("default".to_string()),
        ..Default::default()
    };
    assert_eq!(resolve_context_name(&options, None, None, None), None);
}

#[test]
fn connect_rejects_using_both_docker_context_and_docker_host() {
    let options = DockerConnectionOptions {
        host: Some("tcp://1.2.3.4:2375".to_string()),
        context: Some("some-context".to_string()),
        config_directory: None,
        ..Default::default()
    };
    let err = connect(&options).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Cannot use both --docker-context and --docker-host."
    );
}

#[test]
fn connect_via_an_explicit_context_uses_that_contexts_stored_host() {
    let config_directory = unique_temp_dir();
    // A `tcp://` address (unlike `unix://`) only builds a
    // lazily-connecting client (no handshake, no eager socket-existence
    // check) — nothing talks to the daemon until the first real call, so
    // this succeeds against a context whose host is unreachable.
    write_docker_context_meta(&config_directory, "my-context", "tcp://1.2.3.4:2375");

    let options = DockerConnectionOptions {
        host: None,
        context: Some("my-context".to_string()),
        config_directory: Some(config_directory),
        ..Default::default()
    };
    connect(&options).expect("connecting via a valid context's stored host should succeed");
}

#[test]
fn connect_via_an_explicit_context_errors_clearly_when_it_does_not_exist() {
    let config_directory = unique_temp_dir();
    let options = DockerConnectionOptions {
        host: None,
        context: Some("no-such-context".to_string()),
        config_directory: Some(config_directory),
        ..Default::default()
    };
    let err = connect(&options).unwrap_err();
    assert!(
        err.to_string()
            .contains("Docker context 'no-such-context' does not exist"),
        "{err}"
    );
}

#[test]
fn conflicting_option_with_context_names_whichever_tls_option_was_given() {
    let base = DockerConnectionOptions {
        context: Some("some-context".to_string()),
        ..Default::default()
    };

    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls: true,
            ..base.clone()
        }),
        Some("--docker-tls")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_verify: true,
            ..base.clone()
        }),
        Some("--docker-tls-verify")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            cert_path: Some(PathBuf::from("/tmp/certs")),
            ..base.clone()
        }),
        Some("--docker-cert-path")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_ca_cert: Some(PathBuf::from("/tmp/ca.pem")),
            ..base.clone()
        }),
        Some("--docker-tls-ca-cert")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_cert: Some(PathBuf::from("/tmp/cert.pem")),
            ..base.clone()
        }),
        Some("--docker-tls-cert")
    );
    assert_eq!(
        conflicting_option_with_context(&DockerConnectionOptions {
            tls_key: Some(PathBuf::from("/tmp/key.pem")),
            ..base.clone()
        }),
        Some("--docker-tls-key")
    );
    assert_eq!(conflicting_option_with_context(&base), None);
}

#[test]
fn connect_rejects_docker_tls_flags_combined_with_docker_context() {
    for options in [
        DockerConnectionOptions {
            context: Some("some-context".to_string()),
            tls: true,
            ..Default::default()
        },
        DockerConnectionOptions {
            context: Some("some-context".to_string()),
            tls_verify: true,
            ..Default::default()
        },
    ] {
        let err = connect(&options).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Cannot use both --docker-context and --docker-tls"),
            "{err}"
        );
    }
}

#[test]
fn tls_enabled_is_true_for_either_flag_or_the_real_env_var() {
    let base = DockerConnectionOptions::default();
    assert!(!tls_enabled(&base, None));
    assert!(tls_enabled(
        &DockerConnectionOptions {
            tls: true,
            ..base.clone()
        },
        None
    ));
    assert!(tls_enabled(
        &DockerConnectionOptions {
            tls_verify: true,
            ..base.clone()
        },
        None
    ));
    assert!(tls_enabled(&base, Some("1")));
    assert!(tls_enabled(&base, Some("true")));
    assert!(tls_enabled(&base, Some("TRUE")));
    assert!(!tls_enabled(&base, Some("0")));
    assert!(!tls_enabled(&base, Some("false")));
}

#[test]
fn docker_cert_directory_prefers_the_explicit_option_over_the_env_var_and_default() {
    let explicit = unique_temp_dir();
    let from_env = unique_temp_dir();
    let options = DockerConnectionOptions {
        cert_path: Some(explicit.clone()),
        ..Default::default()
    };
    assert_eq!(
        docker_cert_directory(&options, from_env.to_str()).unwrap(),
        (explicit, "--docker-cert-path".to_string())
    );

    let options = DockerConnectionOptions::default();
    assert_eq!(
        docker_cert_directory(&options, from_env.to_str()).unwrap(),
        (from_env, "DOCKER_CERT_PATH".to_string())
    );
    let home = crate::user::home_directory().unwrap().join(".docker");
    for unset in [None, Some("")] {
        assert_eq!(
            docker_cert_directory(&options, unset).unwrap(),
            (home.clone(), "~/.docker".to_string())
        );
    }
}

#[test]
fn flag_tls_settings_errors_naming_what_named_a_cert_directory_that_is_not_there() {
    let missing = unique_temp_dir().join("typo");
    for named_by in ["--docker-cert-path", "DOCKER_CERT_PATH", "~/.docker"] {
        let err =
            flag_tls_settings(&DockerConnectionOptions::default(), &missing, named_by).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains(named_by), "{message}");
        assert!(message.contains("typo"), "{message}");
    }
}

/// `DOCKER_CERT_PATH` left over from a directory long gone, with every file
/// named by a flag, connected before; nothing in the directory is wanted.
#[test]
fn flag_tls_settings_ignores_a_cert_directory_that_is_not_there_when_every_file_has_a_flag() {
    let files = unique_temp_dir();
    for name in ["ca.pem", "cert.pem", "key.pem"] {
        fs::write(files.join(name), name).unwrap();
    }
    let options = DockerConnectionOptions {
        tls_ca_cert: Some(files.join("ca.pem")),
        tls_cert: Some(files.join("cert.pem")),
        tls_key: Some(files.join("key.pem")),
        ..Default::default()
    };

    let settings = flag_tls_settings(
        &options,
        &unique_temp_dir().join("gone"),
        "DOCKER_CERT_PATH",
    )
    .unwrap();
    assert_eq!(
        settings.client,
        Some(ClientCertificate {
            cert: tls_file(files.join("cert.pem"), "--docker-tls-cert"),
            key: tls_file(files.join("key.pem"), "--docker-tls-key"),
        })
    );
}

#[test]
fn flag_tls_settings_uses_an_explicit_ca_path_as_named_by_its_flag() {
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), "default").unwrap();
    let elsewhere = unique_temp_dir().join("my-ca.pem");
    fs::write(&elsewhere, "explicit").unwrap();
    let options = DockerConnectionOptions {
        tls_ca_cert: Some(elsewhere.clone()),
        ..Default::default()
    };

    let settings = flag_tls_settings(&options, &cert_directory, "--docker-cert-path").unwrap();
    assert_eq!(
        settings,
        TlsSettings {
            ca: Some(tls_file(elsewhere, "--docker-tls-ca-cert")),
            client: None,
            skip_verify_requested: false,
        }
    );
}

#[test]
fn flag_tls_settings_uses_the_cert_directory_ca_when_it_is_there() {
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), "ca").unwrap();

    let settings = flag_tls_settings(
        &DockerConnectionOptions::default(),
        &cert_directory,
        "DOCKER_CERT_PATH",
    )
    .unwrap();
    assert_eq!(
        settings.ca,
        Some(tls_file(cert_directory.join("ca.pem"), "DOCKER_CERT_PATH"))
    );
}

#[test]
fn flag_tls_settings_with_no_files_at_all_has_no_ca_and_no_client_certificate() {
    let cert_directory = unique_temp_dir();

    let settings = flag_tls_settings(
        &DockerConnectionOptions::default(),
        &cert_directory,
        "~/.docker",
    )
    .unwrap();
    assert_eq!(
        settings,
        TlsSettings {
            ca: None,
            client: None,
            skip_verify_requested: false,
        }
    );
}

#[test]
fn flag_tls_settings_presents_the_cert_directory_client_pair_when_both_files_are_there() {
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("cert.pem"), "cert").unwrap();
    fs::write(cert_directory.join("key.pem"), "key").unwrap();

    let settings = flag_tls_settings(
        &DockerConnectionOptions::default(),
        &cert_directory,
        "--docker-cert-path",
    )
    .unwrap();
    assert_eq!(settings.ca, None);
    assert_eq!(
        settings.client,
        Some(ClientCertificate {
            cert: tls_file(cert_directory.join("cert.pem"), "--docker-cert-path"),
            key: tls_file(cert_directory.join("key.pem"), "--docker-cert-path"),
        })
    );
}

#[test]
fn flag_tls_settings_errors_when_only_half_a_client_pair_is_in_the_cert_directory() {
    for (present, missing) in [("cert.pem", "key.pem"), ("key.pem", "cert.pem")] {
        let cert_directory = unique_temp_dir();
        fs::write(cert_directory.join(present), "x").unwrap();

        let err = flag_tls_settings(
            &DockerConnectionOptions::default(),
            &cert_directory,
            "--docker-cert-path",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("--docker-cert-path"), "{message}");
        assert!(message.contains(present), "{message}");
        assert!(message.contains(missing), "{message}");
    }
}

#[test]
fn flag_tls_settings_errors_naming_the_flag_when_its_file_does_not_exist() {
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("cert.pem"), "cert").unwrap();
    fs::write(cert_directory.join("key.pem"), "key").unwrap();
    let missing = cert_directory.join("not-there.pem");

    for (flag, options) in [
        (
            "--docker-tls-ca-cert",
            DockerConnectionOptions {
                tls_ca_cert: Some(missing.clone()),
                ..Default::default()
            },
        ),
        (
            "--docker-tls-cert",
            DockerConnectionOptions {
                tls_cert: Some(missing.clone()),
                ..Default::default()
            },
        ),
        (
            "--docker-tls-key",
            DockerConnectionOptions {
                tls_key: Some(missing.clone()),
                ..Default::default()
            },
        ),
    ] {
        let err = flag_tls_settings(&options, &cert_directory, "~/.docker").unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains(flag), "{message}");
        assert!(message.contains("not-there.pem"), "{message}");
    }
}

#[test]
fn flag_tls_settings_with_one_client_flag_takes_the_other_file_from_the_cert_directory() {
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("key.pem"), "key").unwrap();
    let cert = unique_temp_dir().join("me.pem");
    fs::write(&cert, "cert").unwrap();
    let options = DockerConnectionOptions {
        tls_cert: Some(cert.clone()),
        ..Default::default()
    };

    let settings = flag_tls_settings(&options, &cert_directory, "DOCKER_CERT_PATH").unwrap();
    assert_eq!(
        settings.client,
        Some(ClientCertificate {
            cert: tls_file(cert, "--docker-tls-cert"),
            key: tls_file(cert_directory.join("key.pem"), "DOCKER_CERT_PATH"),
        })
    );
}

#[test]
fn flag_tls_settings_with_one_client_flag_errors_when_the_other_file_is_not_in_the_cert_directory()
{
    let cert_directory = unique_temp_dir();
    let cert = unique_temp_dir().join("me.pem");
    fs::write(&cert, "cert").unwrap();
    let options = DockerConnectionOptions {
        tls_cert: Some(cert),
        ..Default::default()
    };

    let err = flag_tls_settings(&options, &cert_directory, "DOCKER_CERT_PATH").unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("DOCKER_CERT_PATH"), "{message}");
    assert!(message.contains("key.pem"), "{message}");
}

/// A throwaway self-signed root CA, and a leaf certificate/key pair it
/// signs for `localhost`/`127.0.0.1` — 2048-bit RSA (`rcgen`'s
/// default), regenerated fresh every test run via `rcgen` rather than
/// a fixed PEM committed to the repo. A static embedded certificate
/// would eventually expire on its own and fail with a stale,
/// disconnected-looking failure long after the fact, unrelated to
/// whatever change actually triggered it — generating at test time
/// with an explicit `not_before`/`not_after` window sidesteps that
/// entirely, and lets the same helper produce a deliberately
/// *already-expired* leaf certificate on demand (see
/// `connect_over_tls_rejects_an_expired_certificate`).
struct GeneratedTlsMaterials {
    /// PEM text for `ca.pem` — what a real `--docker-cert-path`
    /// directory or a Docker context holds, and the only root `connect`
    /// then trusts.
    ca_pem: String,
    cert_pem: String,
    key_pem: String,
    /// DER forms of the same leaf cert/key, for the in-process TLS
    /// server below (`rustls::ServerConfig` wants DER, not PEM).
    cert_der: rustls::pki_types::CertificateDer<'static>,
    key_der: rustls::pki_types::PrivateKeyDer<'static>,
    /// The CA in DER, for a server that verifies client certificates
    /// against it (`serve_one_tls_connection`'s `client_ca`).
    ca_der: rustls::pki_types::CertificateDer<'static>,
    /// A client certificate/key pair the same CA signs — what a Docker
    /// context's or a `--docker-cert-path` directory's `cert.pem`/`key.pem`
    /// hold (see `write_client_tls_materials`).
    client_cert_pem: String,
    client_key_pem: String,
}

fn generate_test_tls_materials(
    not_before: time::OffsetDateTime,
    not_after: time::OffsetDateTime,
) -> GeneratedTlsMaterials {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "ratect-test-ca");
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, ca_key);

    let mut leaf_params =
        rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    leaf_params.not_before = not_before;
    leaf_params.not_after = not_after;
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    let mut client_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    client_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "ratect-test-client");
    client_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let client_key = rcgen::KeyPair::generate().unwrap();
    let client_cert = client_params.signed_by(&client_key, &issuer).unwrap();

    GeneratedTlsMaterials {
        ca_pem: ca_cert.pem(),
        cert_pem: leaf_cert.pem(),
        key_pem: leaf_key.serialize_pem(),
        cert_der: leaf_cert.der().clone(),
        key_der: rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der()),
        ),
        ca_der: ca_cert.der().clone(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
    }
}

fn write_tls_materials(dir: &Path, materials: &GeneratedTlsMaterials) {
    fs::write(dir.join("ca.pem"), &materials.ca_pem).unwrap();
    fs::write(dir.join("cert.pem"), &materials.cert_pem).unwrap();
    fs::write(dir.join("key.pem"), &materials.key_pem).unwrap();
}

/// `materials`' *client* pair as `dir`'s `cert.pem`/`key.pem` — what a
/// daemon requiring a client certificate accepts. `write_tls_materials`
/// writes the *server's* leaf there instead, which loads as a valid pair
/// but which no test server ever asks for.
fn write_client_tls_materials(dir: &Path, materials: &GeneratedTlsMaterials) {
    fs::write(dir.join("cert.pem"), &materials.client_cert_pem).unwrap();
    fs::write(dir.join("key.pem"), &materials.client_key_pem).unwrap();
}

/// Accepts exactly one TCP connection on `listener` and completes a
/// TLS handshake using `cert`/`key`, then responds to the first HTTP
/// request with a minimal 200 OK — just enough for `Docker::ping` to
/// succeed once the handshake itself does. With `client_ca` set, the
/// handshake also *requires* a client certificate that CA signed, as a
/// `dockerd --tlsverify` daemon does — so a connection that fails to
/// present one fails the handshake, rather than passing because nothing
/// asked. Without it, no client certificate is requested.
///
/// If the handshake itself fails (e.g. the client rejects an expired
/// certificate), `TlsAcceptor::accept` returns `Err` and this simply
/// returns — there's nothing to serve, and that's the expected outcome
/// for that test.
async fn serve_one_tls_connection(
    listener: tokio::net::TcpListener,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    client_ca: Option<rustls::pki_types::CertificateDer<'static>>,
) {
    ensure_crypto_provider_installed();
    let builder = rustls::ServerConfig::builder();
    let builder = match client_ca {
        Some(ca) => {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(ca).unwrap();
            let verifier = rustls::server::WebPkiClientVerifier::builder(roots.into())
                .build()
                .unwrap();
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let config = builder
        .with_single_cert(vec![cert], key)
        .expect("valid cert/key pair");
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));

    let (stream, _) = listener.accept().await.expect("accept");
    if let Ok(mut tls_stream) = acceptor.accept(stream).await {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = [0u8; 1024];
        let _ = tls_stream.read(&mut buf).await;
        let _ = tls_stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: text/plain\r\n\
                      Connection: close\r\n\r\nOK",
            )
            .await;
        let _ = tls_stream.shutdown().await;
    }
}

/// `connect` loads the OS trust store (`rustls-native-certs`, through
/// `system_trust_roots`) whenever no CA is configured — no `ca.pem` and no
/// `--docker-tls-ca-cert`, or a context storing none. On macOS that load
/// intermittently fails ("Could not load native certs"), once thought to
/// be two threads hitting the Security framework at once — so TLS
/// `connect` calls take a shared lock instead of running concurrently.
/// Recovered from poisoning so a failing test reports its own assertion
/// rather than cascading a `PoisonError`.
///
/// **Every TLS-enabled `connect` in these tests takes this lock, not just
/// the ones that load the trust store.** Only a connection with no CA
/// needs it, but it costs nothing, and which tests those are changes as
/// tests do; one left out would race the rest and the wrong test would
/// report the failure. The lock can't help across processes, so it relies
/// on these being the only TLS `connect` callers in one test binary.
/// (`connect` calls that fail *before* the TLS branch — the
/// context-conflict tests — don't need it, and take no lock.)
///
/// See this module's own doc comment for why this lock doesn't fully
/// explain the failure these tests still see from time to time — it's kept
/// because it's still correct given the theory it *was* meant to guard
/// against, not because it's known to be sufficient.
static TLS_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial_tls() -> std::sync::MutexGuard<'static, ()> {
    TLS_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tokio::test]
async fn connect_over_tls_completes_a_real_handshake_against_a_valid_certificate() {
    let now = time::OffsetDateTime::now_utc();
    let materials = generate_test_tls_materials(
        now - time::Duration::days(1),
        now + time::Duration::days(365),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_tls_connection(
        listener,
        materials.cert_der.clone(),
        materials.key_der.clone_key(),
        None,
    ));

    let cert_directory = unique_temp_dir();
    write_tls_materials(&cert_directory, &materials);
    let options = DockerConnectionOptions {
        host: Some(format!("tcp://127.0.0.1:{port}")),
        tls_verify: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    // The lock spans only `connect`, which keeps it off the `.await`s
    // below (which `clippy::await_holding_lock` would flag).
    let docker = {
        let _guard = serial_tls();
        connect(&options)
            .expect("connecting over TLS should build a client")
            .docker
    };
    let result = docker.ping().await;
    server.await.expect("server task should not panic");

    assert!(
        result.is_ok(),
        "expected a successful handshake and ping against a valid certificate, got {result:?}"
    );
}

#[tokio::test]
async fn connect_over_tls_rejects_an_expired_certificate() {
    let now = time::OffsetDateTime::now_utc();
    // Already expired, entirely in the past — not "expires during the
    // test", which would be flaky under any scheduling delay.
    let materials =
        generate_test_tls_materials(now - time::Duration::days(2), now - time::Duration::days(1));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_tls_connection(
        listener,
        materials.cert_der.clone(),
        materials.key_der.clone_key(),
        None,
    ));

    let cert_directory = unique_temp_dir();
    write_tls_materials(&cert_directory, &materials);
    let options = DockerConnectionOptions {
        host: Some(format!("tcp://127.0.0.1:{port}")),
        tls_verify: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    let docker = {
        let _guard = serial_tls();
        connect(&options)
            .expect("connecting over TLS should build a client")
            .docker
    };
    let result = docker.ping().await;
    server.await.expect("server task should not panic");

    assert!(
        result.is_err(),
        "expected an expired certificate to be rejected, got {result:?}"
    );
}

#[test]
fn connect_over_tls_errors_clearly_when_the_ca_file_is_not_valid_pem() {
    let cert_directory = unique_temp_dir();
    // Plain garbage with no `-----BEGIN CERTIFICATE-----` marker at all
    // just yields zero parsed certificates, not an error (an empty
    // trust store isn't a construction-time failure — only a later
    // handshake would ever notice) — this needs to *look* like a PEM
    // block to actually exercise the parse-failure path.
    fs::write(
        cert_directory.join("ca.pem"),
        b"-----BEGIN CERTIFICATE-----\nnot valid base64!!!\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    fs::write(cert_directory.join("cert.pem"), b"").unwrap();
    fs::write(cert_directory.join("key.pem"), b"").unwrap();

    let options = DockerConnectionOptions {
        host: Some("tcp://127.0.0.1:2376".to_string()),
        tls_verify: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    let err = {
        let _guard = serial_tls();
        connect(&options).unwrap_err()
    };
    let message = format!("{err:#}");
    assert!(message.contains("over TLS"), "{message}");
    assert!(message.contains("--docker-cert-path"), "{message}");
    assert!(message.contains("ca.pem"), "{message}");
}

#[test]
fn require_host_for_tls_errors_clearly_when_no_host_resolved() {
    let err = require_host_for_tls(None).unwrap_err();
    assert!(
        err.to_string()
            .contains("--docker-tls/--docker-tls-verify requires --docker-host"),
        "{err}"
    );
}

#[test]
fn require_host_for_tls_passes_through_a_resolved_host() {
    assert_eq!(
        require_host_for_tls(Some("tcp://1.2.3.4:2376".to_string())).unwrap(),
        "tcp://1.2.3.4:2376"
    );
}

#[test]
fn resolve_host_prefers_the_explicit_option_then_the_injected_env_value() {
    let options = DockerConnectionOptions {
        host: Some("tcp://explicit:2375".to_string()),
        ..Default::default()
    };
    assert_eq!(
        resolve_host(&options, Some("tcp://from-env:2375")),
        Some("tcp://explicit:2375".to_string())
    );

    let options = DockerConnectionOptions::default();
    assert_eq!(
        resolve_host(&options, Some("tcp://from-env:2375")),
        Some("tcp://from-env:2375".to_string())
    );
    assert_eq!(resolve_host(&options, None), None);
}

#[test]
fn resolve_host_treats_an_empty_docker_host_as_unset() {
    let options = DockerConnectionOptions::default();
    assert_eq!(resolve_host(&options, Some("")), None);
}

/// A context named `context_name` in a fresh config directory, pointing at
/// `127.0.0.1:port` with `SkipTLSVerify` as given and `files` stored in its
/// TLS directory — plus the options that select it.
fn tls_context_options(
    port: u16,
    context_name: &str,
    skip_tls_verify: bool,
    files: &[(&str, &str)],
) -> DockerConnectionOptions {
    let config_directory = unique_temp_dir();
    write_docker_context_meta_with_skip(
        &config_directory,
        context_name,
        &format!("tcp://127.0.0.1:{port}"),
        skip_tls_verify,
    );
    write_docker_context_tls_files(&config_directory, context_name, files);
    DockerConnectionOptions {
        context: Some(context_name.to_string()),
        config_directory: Some(config_directory),
        ..Default::default()
    }
}

/// Connects as `options` say to a one-shot TLS server presenting
/// `materials`' leaf certificate (requiring a client certificate its CA
/// signed when `require_client_cert`), and returns the `ping` result
/// already passed through `Connection::explain_failure`.
async fn ping_over_tls(
    options: &DockerConnectionOptions,
    listener: tokio::net::TcpListener,
    materials: &GeneratedTlsMaterials,
    require_client_cert: bool,
) -> Result<()> {
    let server = tokio::spawn(serve_one_tls_connection(
        listener,
        materials.cert_der.clone(),
        materials.key_der.clone_key(),
        require_client_cert.then(|| materials.ca_der.clone()),
    ));
    let connection = {
        let _guard = serial_tls();
        connect(options).expect("connecting over TLS should build a client")
    };
    let result = connection
        .docker
        .ping()
        .await
        .map(|_| ())
        .map_err(|err| connection.explain_failure(anyhow::Error::new(err)));
    server.await.expect("server task should not panic");
    result
}

fn valid_materials() -> GeneratedTlsMaterials {
    let now = time::OffsetDateTime::now_utc();
    generate_test_tls_materials(
        now - time::Duration::days(1),
        now + time::Duration::days(365),
    )
}

async fn local_listener() -> (tokio::net::TcpListener, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

#[tokio::test]
async fn connect_via_a_context_presents_its_stored_client_certificate_to_a_daemon_requiring_one() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let options = tls_context_options(
        port,
        "mtls",
        false,
        &[
            ("ca.pem", &materials.ca_pem),
            ("cert.pem", &materials.client_cert_pem),
            ("key.pem", &materials.client_key_pem),
        ],
    );

    let result = ping_over_tls(&options, listener, &materials, true).await;
    assert!(result.is_ok(), "expected a verified mTLS ping: {result:?}");
}

#[tokio::test]
async fn connect_via_a_context_with_only_a_stored_ca_verifies_the_daemon_without_a_client_certificate(
) {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let options = tls_context_options(port, "ca-only", false, &[("ca.pem", &materials.ca_pem)]);

    let result = ping_over_tls(&options, listener, &materials, false).await;
    assert!(result.is_ok(), "expected a verified ping: {result:?}");
}

#[tokio::test]
async fn connect_via_a_context_with_only_a_stored_ca_is_refused_by_a_daemon_requiring_a_client_certificate(
) {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let options = tls_context_options(port, "ca-only", false, &[("ca.pem", &materials.ca_pem)]);

    let result = ping_over_tls(&options, listener, &materials, true).await;
    assert!(result.is_err(), "expected the daemon to refuse: {result:?}");
}

#[tokio::test]
async fn connect_via_a_context_asking_to_skip_verification_still_verifies_and_says_so() {
    let materials = valid_materials();
    // A CA that did not sign the server's certificate.
    let other_ca = valid_materials().ca_pem;
    let (listener, port) = local_listener().await;
    let options = tls_context_options(port, "insecure", true, &[("ca.pem", &other_ca)]);

    let err = ping_over_tls(&options, listener, &materials, false)
        .await
        .unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("Docker context 'insecure'"), "{message}");
    assert!(message.contains("SkipTLSVerify"), "{message}");
    assert!(message.contains("always verifies"), "{message}");
}

#[tokio::test]
async fn connect_via_a_context_with_only_skip_tls_verify_still_verifies() {
    // The test CA is in no system trust store, so verification fails — the
    // success half (a daemon certificate a public CA signed) isn't testable
    // here.
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let options = tls_context_options(port, "insecure", true, &[]);

    let err = ping_over_tls(&options, listener, &materials, false)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("always verifies"), "{err:#}");
}

#[tokio::test]
async fn a_failed_verification_is_not_explained_when_the_context_did_not_ask_to_skip_it() {
    let materials = valid_materials();
    let other_ca = valid_materials().ca_pem;
    let (listener, port) = local_listener().await;
    let options = tls_context_options(port, "strict", false, &[("ca.pem", &other_ca)]);

    let err = ping_over_tls(&options, listener, &materials, false)
        .await
        .unwrap_err();
    assert!(!format!("{err:#}").contains("always verifies"), "{err:#}");
}

#[tokio::test]
async fn a_failure_other_than_verification_is_not_explained_as_one() {
    let materials = valid_materials();
    // Bound then dropped: nothing listens, so the connection is refused
    // before any certificate is seen.
    let (listener, port) = local_listener().await;
    drop(listener);
    let options = tls_context_options(port, "insecure", true, &[("ca.pem", &materials.ca_pem)]);

    let connection = {
        let _guard = serial_tls();
        connect(&options).unwrap()
    };
    let err = connection.explain_failure(anyhow::Error::new(
        connection.docker.ping().await.unwrap_err(),
    ));
    assert!(!format!("{err:#}").contains("always verifies"), "{err:#}");
}

/// Connects through a context storing `files`, expecting it to fail before
/// any network I/O, and returns the error text.
fn context_connect_error(files: &[(&str, &str)]) -> String {
    connect_error(&tls_context_options(1, "broken", false, files))
}

#[test]
fn connect_via_a_context_errors_naming_the_file_when_its_ca_is_not_valid_pem() {
    let message = context_connect_error(&[(
        "ca.pem",
        "-----BEGIN CERTIFICATE-----\nnot valid base64!!!\n-----END CERTIFICATE-----\n",
    )]);
    assert!(message.contains("Docker context 'broken'"), "{message}");
    assert!(message.contains("ca.pem"), "{message}");
}

#[test]
fn connect_via_a_context_errors_naming_the_file_when_its_ca_holds_no_certificate() {
    let message = context_connect_error(&[("ca.pem", "nothing here")]);
    assert!(message.contains("Docker context 'broken'"), "{message}");
    assert!(message.contains("ca.pem"), "{message}");
}

#[test]
fn connect_via_a_context_errors_naming_the_file_when_its_key_is_encrypted() {
    let materials = valid_materials();
    let message = context_connect_error(&[
        ("ca.pem", &materials.ca_pem),
        ("cert.pem", &materials.client_cert_pem),
        (
            "key.pem",
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIBAA==\n-----END ENCRYPTED PRIVATE KEY-----\n",
        ),
    ]);
    assert!(message.contains("Docker context 'broken'"), "{message}");
    assert!(message.contains("key.pem"), "{message}");
    assert!(message.contains("encrypted"), "{message}");
}

#[test]
fn connect_via_a_context_errors_naming_the_files_when_its_key_does_not_match_its_certificate() {
    let materials = valid_materials();
    let other = valid_materials();
    let message = context_connect_error(&[
        ("ca.pem", &materials.ca_pem),
        ("cert.pem", &materials.client_cert_pem),
        ("key.pem", &other.client_key_pem),
    ]);
    assert!(message.contains("Docker context 'broken'"), "{message}");
    assert!(message.contains("key.pem"), "{message}");
    assert!(message.contains("cert.pem"), "{message}");
}

#[test]
fn connect_via_a_context_with_tls_settings_errors_when_its_host_is_not_tcp() {
    let config_directory = unique_temp_dir();
    write_docker_context_meta_with_skip(
        &config_directory,
        "socket",
        "unix:///var/run/docker.sock",
        true,
    );
    let options = DockerConnectionOptions {
        context: Some("socket".to_string()),
        config_directory: Some(config_directory),
        ..Default::default()
    };

    let err = {
        let _guard = serial_tls();
        connect(&options).unwrap_err()
    };
    let message = format!("{err:#}");
    assert!(message.contains("Docker context 'socket'"), "{message}");
    assert!(message.contains("unix:///var/run/docker.sock"), "{message}");
}

/// `explain_failure` as production reaches it: through `DockerClient::new`,
/// whose first round trip (`negotiate_version`) is what fails. The only
/// TLS `connect` caller here without `serial_tls`: `DockerClient::new`
/// connects and awaits in one call, so the lock would be held across an
/// `.await` — and with a stored CA, no system trust store is loaded.
#[tokio::test]
async fn docker_client_new_explains_a_verification_failure_for_a_context_asking_to_skip_it() {
    let materials = valid_materials();
    let other_ca = valid_materials().ca_pem;
    let (listener, port) = local_listener().await;
    let options = tls_context_options(port, "insecure", true, &[("ca.pem", &other_ca)]);
    let server = tokio::spawn(serve_one_tls_connection(
        listener,
        materials.cert_der.clone(),
        materials.key_der.clone_key(),
        None,
    ));

    let result = crate::docker::DockerClient::new(&options).await;
    server.await.expect("server task should not panic");

    let Err(err) = result else {
        panic!("expected the daemon's certificate to be rejected");
    };
    let message = format!("{err:#}");
    assert!(message.contains("Docker context 'insecure'"), "{message}");
    assert!(message.contains("always verifies"), "{message}");
}

/// The options for `--docker-host tcp://127.0.0.1:<port> --docker-tls-verify
/// --docker-cert-path <cert_directory>`.
fn tls_flag_options(port: u16, cert_directory: &Path) -> DockerConnectionOptions {
    DockerConnectionOptions {
        host: Some(format!("tcp://127.0.0.1:{port}")),
        tls_verify: true,
        cert_path: Some(cert_directory.to_path_buf()),
        ..Default::default()
    }
}

/// Connects as `options` say, expecting it to fail before any network I/O,
/// and returns the error text.
fn connect_error(options: &DockerConnectionOptions) -> String {
    let err = {
        let _guard = serial_tls();
        connect(options).expect_err("invalid TLS material should fail")
    };
    format!("{err:#}")
}

#[tokio::test]
async fn connect_over_tls_with_only_a_ca_verifies_the_daemon_without_a_client_certificate() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), &materials.ca_pem).unwrap();

    let options = tls_flag_options(port, &cert_directory);
    let result = ping_over_tls(&options, listener, &materials, false).await;
    assert!(result.is_ok(), "expected a verified ping: {result:?}");
}

#[tokio::test]
async fn connect_over_tls_presents_its_client_certificate_to_a_daemon_requiring_one() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), &materials.ca_pem).unwrap();
    write_client_tls_materials(&cert_directory, &materials);

    let options = tls_flag_options(port, &cert_directory);
    let result = ping_over_tls(&options, listener, &materials, true).await;
    assert!(result.is_ok(), "expected a verified mTLS ping: {result:?}");
}

#[tokio::test]
async fn connect_over_tls_with_only_a_ca_is_refused_by_a_daemon_requiring_a_client_certificate() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), &materials.ca_pem).unwrap();

    let options = tls_flag_options(port, &cert_directory);
    let result = ping_over_tls(&options, listener, &materials, true).await;
    assert!(result.is_err(), "expected the daemon to refuse: {result:?}");
}

/// No file at all used to be an error before any connection was made. It is
/// now a valid configuration — the system trust store, no client
/// certificate — so what's left to get wrong is a forgotten `ca.pem`, and
/// the failure has to say where one was looked for.
///
/// The test CA is in no system trust store, so verification fails — the
/// success half (a daemon certificate a public CA signed) isn't testable
/// here.
#[tokio::test]
async fn connect_over_tls_without_a_ca_verifies_against_the_system_trust_store_and_says_so() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let cert_directory = unique_temp_dir();

    let options = tls_flag_options(port, &cert_directory);
    let err = ping_over_tls(&options, listener, &materials, false)
        .await
        .unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("system trust store"), "{message}");
    assert!(
        message.contains(&cert_directory.join("ca.pem").display().to_string()),
        "{message}"
    );
    assert!(message.contains("--docker-tls-ca-cert"), "{message}");
    assert!(!message.contains("Docker context"), "{message}");
}

/// Only weak evidence that `ca.pem` is the *sole* root: the server's
/// certificate is signed by a throwaway CA that no system trust store holds
/// either, so this would be rejected even if the OS store were still added
/// alongside. What it does prove is that a given CA is used, and that the
/// failure isn't then explained as a missing one.
#[tokio::test]
async fn connect_over_tls_rejects_a_daemon_its_ca_did_not_sign() {
    let materials = valid_materials();
    let other_ca = valid_materials().ca_pem;
    let (listener, port) = local_listener().await;
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), other_ca).unwrap();

    let options = tls_flag_options(port, &cert_directory);
    let err = ping_over_tls(&options, listener, &materials, false)
        .await
        .unwrap_err();
    assert!(
        !format!("{err:#}").contains("system trust store"),
        "{err:#}"
    );
}

/// Used to mean "present no client certificate", silently: the key was only
/// loaded during the handshake, by a resolver that swallowed the error.
#[test]
fn connect_over_tls_errors_naming_the_file_when_its_key_is_not_a_key() {
    let materials = valid_materials();
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), &materials.ca_pem).unwrap();
    fs::write(cert_directory.join("cert.pem"), &materials.client_cert_pem).unwrap();
    fs::write(cert_directory.join("key.pem"), "not a key").unwrap();

    let message = connect_error(&tls_flag_options(1, &cert_directory));
    assert!(message.contains("over TLS"), "{message}");
    assert!(message.contains("--docker-cert-path"), "{message}");
    assert!(message.contains("key.pem"), "{message}");
}

#[test]
fn connect_over_tls_errors_naming_the_files_when_its_key_does_not_match_its_certificate() {
    let materials = valid_materials();
    let other = valid_materials();
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), &materials.ca_pem).unwrap();
    fs::write(cert_directory.join("cert.pem"), &materials.client_cert_pem).unwrap();
    fs::write(cert_directory.join("key.pem"), &other.client_key_pem).unwrap();

    let message = connect_error(&tls_flag_options(1, &cert_directory));
    assert!(message.contains("key.pem"), "{message}");
    assert!(message.contains("cert.pem"), "{message}");
}

#[test]
fn connect_over_tls_errors_naming_the_flag_when_its_key_is_encrypted_or_mismatched() {
    let materials = valid_materials();
    let other = valid_materials();
    let encrypted =
        "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIBAA==\n-----END ENCRYPTED PRIVATE KEY-----\n";

    for (key, expected) in [
        (encrypted, "encrypted"),
        (other.client_key_pem.as_str(), "can't be used with"),
    ] {
        let cert_directory = unique_temp_dir();
        write_client_tls_materials(&cert_directory, &materials);
        let key_path = unique_temp_dir().join("elsewhere.pem");
        fs::write(&key_path, key).unwrap();
        let options = DockerConnectionOptions {
            tls_key: Some(key_path),
            ..tls_flag_options(1, &cert_directory)
        };

        let message = connect_error(&options);
        assert!(message.contains("--docker-tls-key"), "{message}");
        assert!(message.contains("elsewhere.pem"), "{message}");
        assert!(message.contains(expected), "{message}");
    }
}

/// `bollard`'s `connect_with_ssl` built a client for either without
/// complaint, which then failed on its first request. Refused before any
/// file is read: the cert directory named here doesn't exist, and isn't
/// what the error is about.
#[test]
fn connect_over_tls_errors_naming_the_host_when_it_is_not_tcp() {
    for host in ["unix:///var/run/docker.sock", "http://127.0.0.1:2376"] {
        let options = DockerConnectionOptions {
            host: Some(host.to_string()),
            ..tls_flag_options(1, &unique_temp_dir().join("never-read"))
        };

        let message = connect_error(&options);
        assert!(message.contains(host), "{message}");
        assert!(message.contains("over TLS"), "{message}");
        assert!(!message.contains("Docker context"), "{message}");
        assert!(!message.contains("never-read"), "{message}");
    }
}

/// What `connect_with_ssl` accepted, so what `--docker-host` with
/// `--docker-tls` always has.
#[tokio::test]
async fn connect_over_tls_accepts_a_host_with_no_scheme() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), &materials.ca_pem).unwrap();

    let options = DockerConnectionOptions {
        host: Some(format!("127.0.0.1:{port}")),
        ..tls_flag_options(port, &cert_directory)
    };
    let result = ping_over_tls(&options, listener, &materials, false).await;
    assert!(result.is_ok(), "expected a verified ping: {result:?}");
}

#[test]
fn connect_over_tls_errors_naming_the_cert_directory_when_it_is_not_there() {
    let options = tls_flag_options(1, &unique_temp_dir().join("typo"));

    let message = connect_error(&options);
    assert!(message.contains("over TLS"), "{message}");
    assert!(message.contains("--docker-cert-path"), "{message}");
    assert!(message.contains("typo"), "{message}");
}

/// An `https://` host is a TLS connection whether or not a flag says so —
/// through Ratect's own route, with the certificate directory's files, not
/// `bollard`'s own default paths (which would look in `~/.docker` and fail
/// here for want of a `ca.pem`).
#[tokio::test]
async fn connect_to_an_https_host_uses_the_tls_route_without_a_tls_flag() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let cert_directory = unique_temp_dir();
    fs::write(cert_directory.join("ca.pem"), &materials.ca_pem).unwrap();
    let options = DockerConnectionOptions {
        host: Some(format!("https://127.0.0.1:{port}")),
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    let result = ping_over_tls(&options, listener, &materials, false).await;
    assert!(result.is_ok(), "expected a verified ping: {result:?}");
}

/// A context storing no TLS material but naming an `https://` host also
/// connects over TLS, against the system trust store (which the test CA is
/// not in — so verification fails, not the connection setup).
#[tokio::test]
async fn connect_via_a_context_with_an_https_host_uses_the_tls_route() {
    let materials = valid_materials();
    let (listener, port) = local_listener().await;
    let config_directory = unique_temp_dir();
    write_docker_context_meta(
        &config_directory,
        "https",
        &format!("https://127.0.0.1:{port}"),
    );
    let options = DockerConnectionOptions {
        context: Some("https".to_string()),
        config_directory: Some(config_directory),
        ..Default::default()
    };

    let err = ping_over_tls(&options, listener, &materials, false)
        .await
        .unwrap_err();
    assert!(holds_certificate_error(err.as_ref()), "{err:#}");
}
