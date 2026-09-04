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
    let id = docker_context_id(context_name);
    let meta_dir = config_directory.join("contexts").join("meta").join(&id);
    fs::create_dir_all(&meta_dir).unwrap();
    fs::write(
            meta_dir.join("meta.json"),
            format!(
                r#"{{"Name":"{context_name}","Metadata":{{}},"Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":false}}}}}}"#
            ),
        )
        .unwrap();
}

#[test]
fn docker_context_host_reads_the_endpoints_docker_host_field() {
    let config_directory = unique_temp_dir();
    write_docker_context_meta(
        &config_directory,
        "orbstack",
        "unix:///Users/kevin/.orbstack/run/docker.sock",
    );

    let host = docker_context_host(&config_directory, "orbstack").unwrap();
    assert_eq!(host, "unix:///Users/kevin/.orbstack/run/docker.sock");
}

#[test]
fn docker_context_host_errors_clearly_when_the_context_does_not_exist() {
    let config_directory = unique_temp_dir();
    let err = docker_context_host(&config_directory, "no-such-context").unwrap_err();
    assert!(
        err.to_string()
            .contains("Docker context 'no-such-context' does not exist"),
        "{err}"
    );
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
        host: None,
        context: Some("explicit".to_string()),
        config_directory: None,
        ..Default::default()
    };
    assert_eq!(
        resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
        Some("explicit".to_string())
    );
}

#[test]
fn resolve_context_name_an_explicit_host_skips_context_resolution_entirely() {
    let options = DockerConnectionOptions {
        host: Some("tcp://1.2.3.4:2375".to_string()),
        context: None,
        config_directory: None,
        ..Default::default()
    };
    assert_eq!(
        resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
        None
    );
}

#[test]
fn resolve_context_name_falls_back_to_the_env_var_then_the_active_context() {
    let options = DockerConnectionOptions::default();
    assert_eq!(
        resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
        Some("env-context".to_string())
    );
    assert_eq!(
        resolve_context_name(&options, None, Some("active".to_string())),
        Some("active".to_string())
    );
    assert_eq!(resolve_context_name(&options, None, None), None);
}

#[test]
fn resolve_context_name_treats_the_default_context_name_as_no_context() {
    let options = DockerConnectionOptions {
        host: None,
        context: Some("default".to_string()),
        config_directory: None,
        ..Default::default()
    };
    assert_eq!(resolve_context_name(&options, None, None), None);
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
    // check) — see `await_log_follower_waits_for_the_spawned_task_to_finish`'s
    // own comment for the same property.
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
    let options = DockerConnectionOptions {
        cert_path: Some(PathBuf::from("/tmp/explicit-certs")),
        ..Default::default()
    };
    assert_eq!(
        docker_cert_directory(&options).unwrap(),
        PathBuf::from("/tmp/explicit-certs")
    );
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
    /// directory holds, and what `connect`'s `Docker::connect_with_ssl`
    /// call reads.
    ca_pem: String,
    cert_pem: String,
    key_pem: String,
    /// DER forms of the same leaf cert/key, for the in-process TLS
    /// server below (`rustls::ServerConfig` wants DER, not PEM).
    cert_der: rustls::pki_types::CertificateDer<'static>,
    key_der: rustls::pki_types::PrivateKeyDer<'static>,
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

    GeneratedTlsMaterials {
        ca_pem: ca_cert.pem(),
        cert_pem: leaf_cert.pem(),
        key_pem: leaf_key.serialize_pem(),
        cert_der: leaf_cert.der().clone(),
        key_der: rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der()),
        ),
    }
}

fn write_tls_materials(dir: &Path, materials: &GeneratedTlsMaterials) {
    fs::write(dir.join("ca.pem"), &materials.ca_pem).unwrap();
    fs::write(dir.join("cert.pem"), &materials.cert_pem).unwrap();
    fs::write(dir.join("key.pem"), &materials.key_pem).unwrap();
}

/// Accepts exactly one TCP connection on `listener` and completes a
/// TLS handshake using `cert`/`key`, then responds to the first HTTP
/// request with a minimal 200 OK — just enough for `Docker::ping` to
/// succeed once the handshake itself does. No client-cert auth is
/// requested: this harness only exercises the *client's* verification
/// of the *server's* certificate (what `--docker-tls-verify` actually
/// controls), not Ratect's own client-certificate presentation.
///
/// If the handshake itself fails (e.g. the client rejects an expired
/// certificate), `TlsAcceptor::accept` returns `Err` and this simply
/// returns — there's nothing to serve, and that's the expected outcome
/// for that test.
async fn serve_one_tls_connection(
    listener: tokio::net::TcpListener,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
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

/// Every `connect_over_tls_*` test reaches `connect`'s
/// `Docker::connect_with_ssl`, which loads the OS trust store
/// (`rustls-native-certs`) on top of our throwaway test CA. On macOS that
/// native-cert load intermittently fails ("Could not load native certs")
/// when two threads hit the Security framework at once — so they take a
/// shared lock around `connect` instead of running concurrently.
/// Recovered from poisoning so a failing test reports its own assertion
/// rather than cascading a `PoisonError`.
///
/// **Every TLS-enabled `connect` in these tests needs this lock, not just
/// the ones that complete a handshake.** The racy step is the native-cert
/// load, which happens inside `connect_with_ssl` for a bad certificate
/// path just as much as a good one — so the two error-path tests below
/// take it too, even though neither ever opens a socket. Leaving them out
/// is what made the *handshake* test fail intermittently under a full
/// `--workspace` run: an unlocked error-path test would race it and the
/// wrong test would report the failure.
///
/// The lock can't help across processes, so it relies on these being the
/// only TLS `connect` callers in one test binary. A new one belongs here
/// too. (`connect` calls that fail *before* the TLS branch — the
/// context-conflict tests — don't need it, and take no lock.)
///
/// See this module's own doc comment for why this lock doesn't fully
/// explain the failure this test still sees from time to time — it's kept
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
    ));

    let cert_directory = unique_temp_dir();
    write_tls_materials(&cert_directory, &materials);
    let options = DockerConnectionOptions {
        host: Some(format!("tcp://127.0.0.1:{port}")),
        tls_verify: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    // The lock spans only `connect` — the synchronous native-cert load is
    // the racy part, and scoping it here keeps it off the `.await`s below
    // (which `clippy::await_holding_lock` would flag).
    let docker = {
        let _guard = serial_tls();
        connect(&options).expect("connecting over TLS should build a client")
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
        connect(&options).expect("connecting over TLS should build a client")
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
    assert!(err.to_string().contains("over TLS"), "{err}");
}

#[test]
fn connect_over_tls_errors_clearly_when_a_certificate_file_is_missing() {
    let cert_directory = unique_temp_dir();
    // No certificate files written.

    let options = DockerConnectionOptions {
        host: Some("tcp://127.0.0.1:2376".to_string()),
        tls: true,
        cert_path: Some(cert_directory),
        ..Default::default()
    };

    let err = {
        let _guard = serial_tls();
        connect(&options).unwrap_err()
    };
    assert!(err.to_string().contains("over TLS"), "{err}");
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
