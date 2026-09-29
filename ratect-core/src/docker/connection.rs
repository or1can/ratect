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

//! Resolving *which* Docker daemon to talk to and *how* — Docker CLI context
//! lookup (including a context's own stored TLS settings), TLS/cert-directory
//! resolution, and the actual `bollard::Docker` connect call ([`connect`],
//! reached only through [`DockerClient::new`](super::DockerClient::new)). Split out of `docker.rs`
//! in 0.6.0: this has no reference to [`super::ContainerRuntime`] at all — it
//! is a self-contained concept sharing a module with container lifecycle
//! purely by history. [`DockerConnectionOptions`] is re-exported from
//! `docker.rs` (`pub use`), so every existing `ratect_core::docker::
//! DockerConnectionOptions` path is unaffected; everything else here is
//! private, reached only through [`connect`].
//!
//! **The recurring `Could not load native certs` failure**, for whoever
//! reaches for `connect_over_tls_completes_a_real_handshake_against_a_valid_certificate`'s
//! `serial_tls` lock next: don't. That lock was added on the theory that
//! concurrent access to macOS's Security framework was the cause (fixed as a
//! concurrency flake once, widened once), and the theory is refuted — the
//! test has failed alone, three consecutive times, with the sandbox
//! disabled, so nothing else could have been contending for anything. A
//! standalone probe using the same `rustls-native-certs` version returned
//! every certificate with zero errors on the same machine, minutes after the
//! test failed in-process. The trigger is still unidentified; treat that as
//! the open question, not "is this flaky".
//!
//! **A real, independent defect was found investigating it**: `bollard`'s
//! `connect_with_ssl` (which [`connect`] calls for `--docker-tls`/`-verify`)
//! used to treat *any* error from the OS trust-store loader as fatal,
//! discarding every certificate that loaded successfully *and* the explicit
//! `ssl_ca` this module hands it on the very next line. So one unreadable
//! entry anywhere in the OS trust store — nothing to do with Docker,
//! nothing the user did wrong — could make a TLS connection impossible even
//! with a correct `--docker-tls-ca-cert`, on a machine where Docker's own
//! CLI connects fine. User-reachable, not just a test artifact — see
//! `CHANGELOG.md` for the fix, patched into the `bollard` fork this crate's
//! `[patch.crates-io]` pins; offered upstream as
//! [fussybeaver/bollard#796](https://github.com/fussybeaver/bollard/pull/796).
//! This module's own
//! connection failures (`connect`'s `with_context` calls) were never
//! affected — the defect was entirely inside `connect_with_ssl`'s
//! cert-loading, one layer below anything here. A TLS context with no
//! stored CA loads the OS trust store here instead
//! (`system_trust_roots`), tolerating a partial failure the same way.

use anyhow::{Context, Result};
use bollard::Docker;
use std::fs;
use std::path::{Path, PathBuf};

/// CLI-facing Docker daemon connection selection (`--docker-host`,
/// `--docker-context`, `--docker-config`, `--docker-tls`/`-verify`,
/// `--docker-cert-path`, `--docker-tls-ca-cert`/`-cert`/`-key`) — `None`/
/// `false` for anything not explicitly given on the command line, so
/// `DockerClient::new` falls back to the real
/// `DOCKER_HOST`/`DOCKER_CONTEXT`/`DOCKER_CONFIG`/`DOCKER_CERT_PATH`/
/// `DOCKER_TLS_VERIFY` environment variables and the Docker CLI's own
/// active-context resolution, matching Batect's own precedence
/// (`CommandLineOptionsParser.resolveDockerContext`) except that an empty
/// `DOCKER_HOST`/`DOCKER_CONTEXT` counts as unset, as in the Docker CLI.
///
/// One deliberate divergence from Batect, documented in
/// [Differences from Batect](../../../docs/differences-from-batect.md): there's
/// no way to skip TLS verification here — nor through a Docker context, whose
/// `SkipTLSVerify` is not honoured either (see `ContextTls`). Batect's own
/// `--docker-tls` (without `-verify`) sets Go's
/// `tls.Config.InsecureSkipVerify`, which disables *all* server certificate
/// verification — chain of trust, expiry, and hostname matching, not just
/// hostname matching — while still doing the TLS handshake and any configured
/// client-certificate auth.
/// `tls` and `tls_verify` are both accepted here (for command-line
/// compatibility) but behave identically: connecting always fully
/// verifies the daemon's certificate. This matches `rustls` itself (the
/// library this is built on) rather than fighting it: `rustls` has no
/// boolean toggle for this either — disabling verification means
/// implementing its own `ServerCertVerifier` trait from scratch, a
/// deliberate hurdle against careless misuse, not a config flag. See
/// [Connecting to Docker](../../../docs/connecting-to-docker.md#tls-with-a-private-certificate-authority)
/// for the supported (verified) alternative. Re-exported from `docker.rs`
/// (`pub use`) so this module's existence is an implementation detail, not a
/// path change.
#[derive(Debug, Default, Clone)]
pub struct DockerConnectionOptions {
    pub host: Option<String>,
    pub context: Option<String>,
    pub config_directory: Option<PathBuf>,
    pub tls: bool,
    pub tls_verify: bool,
    pub cert_path: Option<PathBuf>,
    pub tls_ca_cert: Option<PathBuf>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

/// The Docker CLI's own context store identifier for a context name —
/// lowercase hex `sha256(name)`. Matches the Docker CLI's own
/// `contextdir.go` naming exactly (verified against a real
/// `~/.docker/contexts/meta/<id>/meta.json` entry on this machine).
fn docker_context_id(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(name.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The subset of a context's `meta.json` this needs — the daemon host to
/// connect to, and whether the context asks to skip TLS verification. Field
/// names/casing match the Docker CLI's own format exactly
/// (`Endpoints.docker.Host`/`SkipTLSVerify`; `docker` itself is lowercase,
/// unlike its sibling fields).
#[derive(serde::Deserialize)]
struct DockerContextMetadata {
    #[serde(rename = "Endpoints")]
    endpoints: DockerContextEndpoints,
}

#[derive(serde::Deserialize)]
struct DockerContextEndpoints {
    docker: DockerContextDockerEndpoint,
}

#[derive(serde::Deserialize)]
struct DockerContextDockerEndpoint {
    #[serde(rename = "Host")]
    host: String,
    #[serde(rename = "SkipTLSVerify", default)]
    skip_tls_verify: bool,
}

/// What connecting through a context needs: its daemon host, and its TLS
/// settings — `None` for a plain connection.
#[derive(Debug, PartialEq)]
struct ContextEndpoint {
    host: String,
    tls: Option<ContextTls>,
}

/// A context's TLS settings, as the Docker CLI reads them
/// (`cli/context/docker/load.go`'s `Endpoint.tlsConfig`): `ca` is the sole
/// trust root when stored, else the system trust store is used; `client` is
/// presented when stored, else none is. `skip_verify_requested` is the
/// context's `SkipTLSVerify`, which Ratect does not honour — it is kept only
/// to explain a verification failure (see [`Connection::explain_failure`]).
#[derive(Debug, PartialEq)]
struct ContextTls {
    ca: Option<PathBuf>,
    client: Option<ClientCertificate>,
    skip_verify_requested: bool,
}

#[derive(Debug, PartialEq)]
struct ClientCertificate {
    cert: PathBuf,
    key: PathBuf,
}

/// `<config_directory>/contexts/tls/<sha256(context_name)>/docker/` — where
/// `docker context create --docker ca=…,cert=…,key=…` stores a context's
/// `ca.pem`/`cert.pem`/`key.pem` (the Docker CLI's `contextdir.go`/
/// `tlsstore.go` layout).
fn docker_context_tls_directory(config_directory: &Path, context_name: &str) -> PathBuf {
    config_directory
        .join("contexts")
        .join("tls")
        .join(docker_context_id(context_name))
        .join("docker")
}

/// Reads `<config_directory>/contexts/meta/<sha256(context_name)>/meta.json`
/// for `context_name`'s daemon host and `SkipTLSVerify`, and its TLS
/// directory (see [`docker_context_tls_directory`]) for which of
/// `ca.pem`/`cert.pem`/`key.pem` it stores. TLS applies when any of those
/// files exists or `SkipTLSVerify` is set — the Docker CLI's own rule. A
/// missing `meta.json` (or one that doesn't parse as expected) is reported
/// as the named context not existing — matching what `--docker-context`
/// naming an unknown context should feel like to a user, rather than a raw
/// file-not-found error.
///
/// A client certificate without its key, or the reverse, is an error. The
/// Docker CLI silently presents no client certificate then; a daemon that
/// wants one would refuse the connection with nothing pointing at the
/// missing file.
fn docker_context_endpoint(config_directory: &Path, context_name: &str) -> Result<ContextEndpoint> {
    let meta_path = config_directory
        .join("contexts")
        .join("meta")
        .join(docker_context_id(context_name))
        .join("meta.json");
    let contents = fs::read_to_string(&meta_path).with_context(|| {
        format!(
            "Docker context '{context_name}' does not exist (expected to find it at {}).",
            meta_path.display()
        )
    })?;
    let metadata: DockerContextMetadata = serde_json::from_str(&contents).with_context(|| {
        format!(
            "Failed to read Docker context '{context_name}' ({})",
            meta_path.display()
        )
    })?;
    let endpoint = metadata.endpoints.docker;

    let tls_directory = docker_context_tls_directory(config_directory, context_name);
    let stored = |name: &str| Some(tls_directory.join(name)).filter(|path| path.is_file());
    let ca = stored("ca.pem");
    let client = match (stored("cert.pem"), stored("key.pem")) {
        (Some(cert), Some(key)) => Some(ClientCertificate { cert, key }),
        (None, None) => None,
        (Some(cert), None) => return Err(half_client_pair(context_name, &cert, "key.pem")),
        (None, Some(key)) => return Err(half_client_pair(context_name, &key, "cert.pem")),
    };

    let tls =
        (ca.is_some() || client.is_some() || endpoint.skip_tls_verify).then_some(ContextTls {
            ca,
            client,
            skip_verify_requested: endpoint.skip_tls_verify,
        });
    Ok(ContextEndpoint {
        host: endpoint.host,
        tls,
    })
}

/// The subset of the Docker CLI's own `config.json` this needs — just the
/// active context's name.
#[derive(serde::Deserialize, Default)]
struct DockerCliConfig {
    #[serde(rename = "currentContext", default)]
    current_context: Option<String>,
}

/// The Docker CLI's own "currently active" context, from
/// `<config_directory>/config.json`'s `currentContext` field — consulted
/// only when none of `--docker-context`, `--docker-host`/`DOCKER_HOST` or
/// `DOCKER_CONTEXT` says otherwise. `None` (not an error) when the file
/// doesn't exist or sets no `currentContext` — both mean the same thing as
/// the Docker CLI's own fallback: use the `default` context.
fn active_docker_context(config_directory: &Path) -> Option<String> {
    let contents = fs::read_to_string(config_directory.join("config.json")).ok()?;
    let config: DockerCliConfig = serde_json::from_str(&contents).ok()?;
    config.current_context.filter(|name| !name.is_empty())
}

/// `--docker-config`, else `DOCKER_CONFIG`, else `~/.docker` — the
/// directory the Docker CLI's own context store and `config.json` live in.
/// Visible to `docker.rs` (like `connect`, below) so `DockerClient::new` can
/// resolve the same directory a second time to hand to its credential
/// resolver, honoring `--docker-config`/`DOCKER_CONFIG` exactly as
/// everywhere else Ratect reads Docker's own config.
pub(super) fn docker_config_directory(options: &DockerConnectionOptions) -> Result<PathBuf> {
    if let Some(dir) = &options.config_directory {
        return Ok(dir.clone());
    }
    if let Ok(dir) = std::env::var("DOCKER_CONFIG") {
        return Ok(PathBuf::from(dir));
    }
    Ok(crate::user::home_directory()?.join(".docker"))
}

/// `--docker-cert-path`, else `DOCKER_CERT_PATH`, else `~/.docker` — the
/// directory `ca.pem`/`cert.pem`/`key.pem` are read from unless
/// `--docker-tls-ca-cert`/`-cert`/`-key` individually override one.
/// Resolved independently of `docker_config_directory` (its own separate
/// environment variable, even though both happen to share the same
/// hardcoded default) — matching Batect's own two independently-settable
/// options exactly.
fn docker_cert_directory(options: &DockerConnectionOptions) -> Result<PathBuf> {
    if let Some(dir) = &options.cert_path {
        return Ok(dir.clone());
    }
    if let Ok(dir) = std::env::var("DOCKER_CERT_PATH") {
        return Ok(PathBuf::from(dir));
    }
    Ok(crate::user::home_directory()?.join(".docker"))
}

/// Whether this invocation should connect over TLS at all: `--docker-tls`
/// and `--docker-tls-verify` both enable it (Ratect always verifies
/// regardless of which — see `DockerConnectionOptions`'s own doc comment),
/// same as the real `DOCKER_TLS_VERIFY` environment variable (the only one
/// of the two flags Batect gives an environment variable default at all).
fn tls_enabled(options: &DockerConnectionOptions, docker_tls_verify_env: Option<&str>) -> bool {
    options.tls
        || options.tls_verify
        || matches!(
            docker_tls_verify_env
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("1") | Some("true")
        )
}

/// Installs `rustls`'s `ring` cryptographic provider as the process-wide
/// default, exactly once — `bollard::Docker::connect_with_ssl` panics if
/// asked to build a TLS connection before one is installed (there's no
/// provider bundled by default; `ratect-core`'s own `bollard` dependency
/// enables just enough of `ssl_providerless` for that, matching bollard's
/// own `ssl` feature). Idempotent: a later call after the first is a no-op
/// (`install_default` only errors if something else already installed a
/// provider, which never happens here — nothing else in `ratect-core`
/// touches `rustls` directly).
fn ensure_crypto_provider_installed() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The host named by `--docker-host`, else the real `DOCKER_HOST`
/// environment variable (an empty one counts as unset, as in the Docker CLI),
/// else `None`. Step 2 of `connect`'s precedence — a host from either source
/// rules out every context but an explicit `--docker-context` — and then the
/// host a plain or TLS connection uses once no context applies (`None` only
/// valid for a plain, non-TLS connection — see `require_host_for_tls`). Pure
/// (the environment value is injected) so it's unit-testable without
/// depending on whichever real environment variables happen to be set on the
/// machine running the tests.
fn resolve_host(
    options: &DockerConnectionOptions,
    docker_host_env: Option<&str>,
) -> Option<String> {
    options.host.clone().or_else(|| {
        docker_host_env
            .filter(|host| !host.is_empty())
            .map(str::to_string)
    })
}

/// TLS has no platform-default host to fall back to the way the plain path
/// does (`Docker::connect_with_local_defaults`) — an explicit host is
/// required. Pure (takes the already-resolved host, not the environment)
/// purely to keep this one error message unit-testable in isolation.
fn require_host_for_tls(host: Option<String>) -> Result<String> {
    host.ok_or_else(|| {
        anyhow::anyhow!(
            "--docker-tls/--docker-tls-verify requires --docker-host (or the DOCKER_HOST \
             environment variable) to be set."
        )
    })
}

/// Steps 1–4 of `connect`'s own doc comment, as a pure decision: which
/// context (if any) should be looked up in the store. A `None` return means
/// "no context — connect via `host` or the platform default instead",
/// covering both a resolved host (`host` is `resolve_host`'s result, so
/// `--docker-host` and `DOCKER_HOST` alike skip context resolution) and the
/// `default` context name itself (never looked up in the store — it *means*
/// "no context"). An empty `DOCKER_CONTEXT` counts as unset, as in the
/// Docker CLI.
///
/// `active_context` is step 4's fallback — reading the store's own "active
/// context" needs real file I/O, so it's computed by the caller and passed
/// in already-resolved, keeping this function itself pure (and so
/// unit-testable without a filesystem) like `select_builder_version`.
fn resolve_context_name(
    options: &DockerConnectionOptions,
    host: Option<&str>,
    docker_context_env: Option<&str>,
    active_context: Option<String>,
) -> Option<String> {
    let context_name = if let Some(context) = &options.context {
        Some(context.clone())
    } else if host.is_some() {
        None
    } else if let Some(context) = docker_context_env.filter(|context| !context.is_empty()) {
        Some(context.to_string())
    } else {
        active_context
    };

    context_name.filter(|name| name != "default")
}

/// Batect's own `forbiddenOptionsWithDockerContext` set, named one at a
/// time so the error can say exactly which flag conflicts, matching
/// Batect's own message format (`"Cannot use both --docker-context and
/// --docker-host."`) rather than a generic "these are mutually exclusive"
/// dump.
fn conflicting_option_with_context(options: &DockerConnectionOptions) -> Option<&'static str> {
    if options.host.is_some() {
        Some("--docker-host")
    } else if options.tls {
        Some("--docker-tls")
    } else if options.tls_verify {
        Some("--docker-tls-verify")
    } else if options.cert_path.is_some() {
        Some("--docker-cert-path")
    } else if options.tls_ca_cert.is_some() {
        Some("--docker-tls-ca-cert")
    } else if options.tls_cert.is_some() {
        Some("--docker-tls-cert")
    } else if options.tls_key.is_some() {
        Some("--docker-tls-key")
    } else {
        None
    }
}

/// The error for a context storing only one of `cert.pem`/`key.pem`.
fn half_client_pair(context_name: &str, present: &Path, missing: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Docker context '{context_name}' stores {} but no {missing} beside it — a client \
         certificate needs both",
        present.display()
    )
}

/// Reads a file from a context's TLS directory, naming the context if it
/// can't.
fn read_context_file(context_name: &str, path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| {
        format!(
            "Failed to read {} for Docker context '{context_name}'",
            path.display()
        )
    })
}

/// Reads every certificate in the PEM file at `path`, for a context's
/// `ca.pem`/`cert.pem` — an error naming the context and the file when it
/// can't be read, isn't valid PEM, or holds no certificate at all.
fn read_context_certificates(
    context_name: &str,
    path: &Path,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;
    let bytes = read_context_file(context_name, path)?;
    let certificates = rustls::pki_types::CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| {
            format!(
                "Docker context '{context_name}': {} is not a valid PEM certificate file",
                path.display()
            )
        })?;
    if certificates.is_empty() {
        anyhow::bail!(
            "Docker context '{context_name}': {} holds no certificate",
            path.display()
        );
    }
    Ok(certificates)
}

/// Reads the private key in a context's `key.pem` — an error naming the
/// context and the file when it can't be read, is encrypted (a context
/// stores no passphrase to decrypt it with), or holds no usable key.
fn read_context_private_key(
    context_name: &str,
    path: &Path,
) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    use rustls::pki_types::pem::PemObject;
    let bytes = read_context_file(context_name, path)?;
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
        || text.contains("Proc-Type: 4,ENCRYPTED")
    {
        anyhow::bail!(
            "Docker context '{context_name}': {} is an encrypted private key, which Ratect \
             can't use — store it unencrypted",
            path.display()
        );
    }
    rustls::pki_types::PrivateKeyDer::from_pem_slice(&bytes).with_context(|| {
        format!(
            "Docker context '{context_name}': {} holds no valid PEM private key",
            path.display()
        )
    })
}

/// The OS trust store, for a context that stores no `ca.pem`. An entry that
/// fails to load is skipped rather than failing the connection — the same
/// policy as the `bollard` fork's `connect_with_ssl` (see this module's own
/// doc comment for why).
fn system_trust_roots() -> rustls::RootCertStore {
    let loaded = rustls_native_certs::load_native_certs();
    if !loaded.errors.is_empty() {
        tracing::warn!(errors = ?loaded.errors, "ignoring errors loading the system trust store");
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(loaded.certs);
    roots
}

/// The `rustls` client configuration for a context's TLS settings (see
/// [`ContextTls`]). Every stored file is read and checked here, up front,
/// so a bad one is named by this error rather than surfacing later as an
/// opaque handshake failure — a client key most of all, which would
/// otherwise just not be presented. The daemon's certificate is always
/// verified: `skip_verify_requested` plays no part in this.
fn context_tls_config(context_name: &str, tls: &ContextTls) -> Result<rustls::ClientConfig> {
    ensure_crypto_provider_installed();
    let roots = match &tls.ca {
        Some(ca) => {
            let mut roots = rustls::RootCertStore::empty();
            for certificate in read_context_certificates(context_name, ca)? {
                roots.add(certificate).with_context(|| {
                    format!(
                        "Docker context '{context_name}': {} holds a certificate that can't be \
                         used as a trust root",
                        ca.display()
                    )
                })?;
            }
            roots
        }
        None => system_trust_roots(),
    };
    let builder = rustls::ClientConfig::builder().with_root_certificates(roots);
    let Some(client) = &tls.client else {
        return Ok(builder.with_no_client_auth());
    };
    let certificates = read_context_certificates(context_name, &client.cert)?;
    let key = read_context_private_key(context_name, &client.key)?;
    builder
        .with_client_auth_cert(certificates, key)
        .with_context(|| {
            format!(
                "Docker context '{context_name}': the private key in {} can't be used with the \
             certificate in {}",
                client.key.display(),
                client.cert.display()
            )
        })
}

/// Connects to `host` over TLS configured by `config`, through `bollard`'s
/// custom-transport hook: `bollard`'s own `connect_with_ssl` needs a CA, a
/// client certificate and a key, each as a file, so it can't express a
/// context with no CA (the system trust store) or no client certificate.
/// The transport mirrors that function's own — a `hyper-rustls` connector
/// on `hyper-util`'s client, with no idle-connection pooling — and every
/// request, upgrades (attach, exec) included, goes through it the same way.
fn connect_with_tls_config(
    context_name: &str,
    host: &str,
    config: rustls::ClientConfig,
) -> Result<Docker> {
    let Some(address) = host
        .strip_prefix("tcp://")
        .or_else(|| host.strip_prefix("https://"))
    else {
        anyhow::bail!(
            "Docker context '{context_name}' has TLS settings but its host '{host}' isn't a \
             tcp:// or https:// address, the only kinds Ratect connects to over TLS"
        );
    };

    let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
    http.enforce_http(false);
    let https = hyper_rustls::HttpsConnector::from((http, config));
    let mut builder =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new());
    builder.pool_max_idle_per_host(0);
    let client = std::sync::Arc::new(builder.build(https));

    Docker::connect_with_custom_transport(
        move |request: bollard::BollardRequest| {
            let client = std::sync::Arc::clone(&client);
            async move {
                client
                    .request(request)
                    .await
                    .map_err(bollard::errors::Error::from)
            }
        },
        Some(format!("https://{address}")),
        120,
        bollard::API_DEFAULT_VERSION,
    )
    .with_context(|| {
        format!("Failed to connect to Docker context '{context_name}' (host '{host}') over TLS")
    })
}

/// A connected client, plus what's needed to explain its first failure:
/// the name of the context that asked to skip TLS verification, when the
/// connection went through one that did.
#[derive(Debug)]
pub(super) struct Connection {
    pub(super) docker: Docker,
    skip_verify_requested_by: Option<String>,
}

impl Connection {
    fn without_skip_request(docker: Docker) -> Self {
        Self {
            docker,
            skip_verify_requested_by: None,
        }
    }

    /// `err`, from a request over this connection, with an explanation added
    /// when it's a certificate-verification failure and the context asked
    /// to skip verification — the one case where the user's own
    /// configuration says this shouldn't have happened. Anything else is
    /// returned unchanged.
    pub(super) fn explain_failure(&self, err: anyhow::Error) -> anyhow::Error {
        match &self.skip_verify_requested_by {
            Some(context_name) if holds_certificate_error(err.as_ref()) => err.context(format!(
                "Docker context '{context_name}' asks to skip TLS verification (its \
                 SkipTLSVerify setting), but Ratect always verifies the daemon's certificate. \
                 See 'TLS with a private certificate authority' in Ratect's Connecting to \
                 Docker documentation for how to make it verifiable"
            )),
            _ => err,
        }
    }
}

/// Whether `error`'s chain holds a `rustls` certificate error. That error
/// sits inside a `std::io::Error` — two, in fact: `tokio-rustls` wraps it in
/// one and `hyper-rustls` wraps that in another — and an `io::Error`'s own
/// `source` skips straight past its payload, so each one is looked inside as
/// well as past.
fn holds_certificate_error(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(rustls_error) = error.downcast_ref::<rustls::Error>() {
        return matches!(rustls_error, rustls::Error::InvalidCertificate(_));
    }
    let payload = error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref);
    if payload.is_some_and(|payload| holds_certificate_error(payload)) {
        return true;
    }
    error.source().is_some_and(holds_certificate_error)
}

/// Resolves and connects to the Docker daemon, matching Batect's own
/// precedence (`CommandLineOptionsParser.resolveDockerContext`/
/// `DockerClientConfigurationFactory`) — bar the empty-value rule, see
/// `resolve_host`/`resolve_context_name`:
///
/// 1. An explicit `--docker-context` is looked up by name in the context
///    store — except `default`, which connects to the host step 2 resolves
///    (`DOCKER_HOST` alone, since the flag conflicts), else the platform
///    default.
/// 2. Otherwise, a host — `--docker-host`, else `DOCKER_HOST` — connects
///    directly to that host, bypassing the context store entirely, even if
///    `DOCKER_CONTEXT` or an active context is also set (Batect's and the
///    Docker CLI's own rule: a host from either source means "ignore
///    whatever context would otherwise apply").
/// 3. Otherwise, `DOCKER_CONTEXT` (if set) is looked up the same way as 1.
/// 4. Otherwise, the Docker CLI's own "active" context
///    (`~/.docker/config.json`'s `currentContext`) is looked up the same
///    way, falling back to bollard's own platform default (unix
///    socket/named pipe) when that's unset or names the `default` context.
///
/// TLS (`--docker-tls`/`-verify`, `DOCKER_TLS_VERIFY`) only applies once a
/// context is ruled out — Batect rejects combining it with
/// `--docker-context` at all (see `conflicting_option_with_context`), and
/// ignores it when step 3 or 4 picks a context, as this does — and has no
/// platform-default host to fall back to the way the plain path does, so a
/// host is required (see `require_host_for_tls`).
///
/// A context brings its own TLS settings instead — the ones
/// `docker context create` stored for it (see [`docker_context_endpoint`]),
/// connected through [`connect_with_tls_config`] rather than `bollard`'s
/// `connect_with_ssl`. The returned [`Connection`] carries what
/// [`Connection::explain_failure`] needs to explain a verification failure.
pub(super) fn connect(options: &DockerConnectionOptions) -> Result<Connection> {
    if options.context.is_some() {
        if let Some(conflicting) = conflicting_option_with_context(options) {
            anyhow::bail!("Cannot use both --docker-context and {conflicting}.");
        }
    }

    let config_directory = docker_config_directory(options)?;
    let docker_host_env = std::env::var("DOCKER_HOST").ok();
    let host = resolve_host(options, docker_host_env.as_deref());
    let docker_context_env = std::env::var("DOCKER_CONTEXT").ok();
    let context_name = resolve_context_name(
        options,
        host.as_deref(),
        docker_context_env.as_deref(),
        active_docker_context(&config_directory),
    );

    if let Some(context_name) = context_name {
        let ContextEndpoint { host, tls } =
            docker_context_endpoint(&config_directory, &context_name)?;
        let Some(tls) = tls else {
            return Docker::connect_with_host(&host)
                .map(Connection::without_skip_request)
                .with_context(|| {
                    format!("Failed to connect to Docker context '{context_name}' (host '{host}')")
                });
        };
        let config = context_tls_config(&context_name, &tls)?;
        let docker = connect_with_tls_config(&context_name, &host, config)?;
        return Ok(Connection {
            docker,
            skip_verify_requested_by: tls.skip_verify_requested.then_some(context_name),
        });
    }

    let docker_tls_verify_env = std::env::var("DOCKER_TLS_VERIFY").ok();
    if !tls_enabled(options, docker_tls_verify_env.as_deref()) {
        let docker = match host {
            Some(host) => Docker::connect_with_host(&host)
                .with_context(|| format!("Failed to connect to Docker host '{host}'"))?,
            None => Docker::connect_with_local_defaults().context("Failed to connect to Docker")?,
        };
        return Ok(Connection::without_skip_request(docker));
    }

    let host = require_host_for_tls(host)?;
    let cert_directory = docker_cert_directory(options)?;
    let ca = options
        .tls_ca_cert
        .clone()
        .unwrap_or_else(|| cert_directory.join("ca.pem"));
    let cert = options
        .tls_cert
        .clone()
        .unwrap_or_else(|| cert_directory.join("cert.pem"));
    let key = options
        .tls_key
        .clone()
        .unwrap_or_else(|| cert_directory.join("key.pem"));

    ensure_crypto_provider_installed();
    Docker::connect_with_ssl(&host, &key, &cert, &ca, 120, bollard::API_DEFAULT_VERSION)
        .map(Connection::without_skip_request)
        .with_context(|| format!("Failed to connect to Docker host '{host}' over TLS"))
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
