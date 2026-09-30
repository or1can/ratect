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
//! `connect_with_ssl` used to treat *any* error from the OS trust-store
//! loader as fatal, discarding every certificate that loaded successfully
//! *and* the explicit CA it was handed. So one unreadable entry anywhere in
//! the OS trust store — nothing to do with Docker, nothing the user did
//! wrong — could make a TLS connection impossible even with a correct
//! `--docker-tls-ca-cert`, on a machine where Docker's own CLI connects
//! fine. User-reachable, not just a test artifact — see `CHANGELOG.md` for
//! the fix, patched into the `bollard` fork this crate's
//! `[patch.crates-io]` pins; offered upstream as
//! [fussybeaver/bollard#796](https://github.com/fussybeaver/bollard/pull/796).
//!
//! Nothing here calls `connect_with_ssl` any more. Every TLS connection —
//! `--docker-tls`/`-verify` and a Docker context alike — is configured by
//! `tls_client_config` and made by `connect_over_tls`, and the OS
//! trust store is loaded in one place only, `system_trust_roots`, and only
//! when no CA is configured; it tolerates a partial failure the same way
//! the fork's fix does. `bollard` still reaches its own `connect_with_ssl`
//! by itself, from `connect_with_host` on the plain path (an `https://`
//! host, or `DOCKER_TLS_VERIFY` set to anything at all —
//! [ratect#257](https://github.com/or1can/ratect/issues/257)), which is why
//! the fork's fix is still pinned.

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
/// `SkipTLSVerify` is not honoured either (see `TlsSettings`). Batect's own
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
/// for the supported (verified) alternative.
///
/// A second one, in the other direction: none of `ca.pem`, `cert.pem` and
/// `key.pem` has to exist (see `flag_tls_settings`). Batect requires all
/// three, and the Docker CLI requires `ca.pem`; here a missing `ca.pem`
/// means the system trust store and a missing client pair means no client
/// certificate, as for a Docker context.
///
/// Re-exported from `docker.rs` (`pub use`) so this module's existence is an
/// implementation detail, not a path change.
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
    tls: Option<TlsSettings>,
}

/// What a TLS connection is configured with, whichever of a Docker context
/// or the `--docker-tls*` flags asked for it, in the Docker CLI's own terms
/// (`cli/context/docker/load.go`'s `Endpoint.tlsConfig`): `ca` is the sole
/// trust root when there is one, else the system trust store is used;
/// `client` is presented when there is one, else none is.
/// `skip_verify_requested` is a context's `SkipTLSVerify`, which Ratect does
/// not honour — it is kept only to explain a verification failure (see
/// [`Connection::explain_failure`]), and the flags have no way to set it.
#[derive(Debug, PartialEq)]
struct TlsSettings {
    ca: Option<TlsFile>,
    client: Option<ClientCertificate>,
    skip_verify_requested: bool,
}

/// A file holding TLS material, and what the user wrote that named it —
/// `Docker context 'x'`, a flag (`--docker-tls-ca-cert`), or for a file
/// found in the certificate directory whichever of `--docker-cert-path`,
/// `DOCKER_CERT_PATH` and `~/.docker` chose that directory. Every error
/// about the file leads with `named_by`, so it points at something the
/// user can change.
#[derive(Debug, PartialEq)]
struct TlsFile {
    path: PathBuf,
    named_by: String,
}

impl TlsFile {
    fn new(path: PathBuf, named_by: &str) -> Self {
        Self {
            path,
            named_by: named_by.to_string(),
        }
    }
}

#[derive(Debug, PartialEq)]
struct ClientCertificate {
    cert: TlsFile,
    key: TlsFile,
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
    let named_by = format!("Docker context '{context_name}'");
    let ca = file_in(&tls_directory, "ca.pem", &named_by);
    let client = client_pair_in(&tls_directory, &named_by)?;

    let tls =
        (ca.is_some() || client.is_some() || endpoint.skip_tls_verify).then_some(TlsSettings {
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
/// directory `ca.pem`/`cert.pem`/`key.pem` are looked for in unless
/// `--docker-tls-ca-cert`/`-cert`/`-key` individually override one — and
/// which of those three chose it, the name errors about a file found there
/// use (see `TlsFile`). Resolved independently of `docker_config_directory`
/// (its own separate environment variable, even though both happen to share
/// the same hardcoded default) — matching Batect's own two
/// independently-settable options exactly.
///
/// An empty `DOCKER_CERT_PATH` counts as unset, as in the Docker CLI,
/// rather than naming the working directory. Whether the directory exists
/// is [`flag_tls_settings`]'s question, asked only if it's consulted.
///
/// Pure but for the home directory lookup — the environment value is
/// injected, as for `resolve_host`.
fn docker_cert_directory(
    options: &DockerConnectionOptions,
    docker_cert_path_env: Option<&str>,
) -> Result<(PathBuf, String)> {
    if let Some(dir) = &options.cert_path {
        return Ok((dir.clone(), "--docker-cert-path".to_string()));
    }
    if let Some(dir) = docker_cert_path_env.filter(|dir| !dir.is_empty()) {
        return Ok((PathBuf::from(dir), "DOCKER_CERT_PATH".to_string()));
    }
    Ok((
        crate::user::home_directory()?.join(".docker"),
        "~/.docker".to_string(),
    ))
}

/// `directory`'s `name`, when it is there.
fn file_in(directory: &Path, name: &str, named_by: &str) -> Option<TlsFile> {
    Some(directory.join(name))
        .filter(|path| path.is_file())
        .map(|path| TlsFile::new(path, named_by))
}

/// `directory`'s `cert.pem` and `key.pem`: both, or neither. One without
/// the other is an error — the Docker CLI silently presents no client
/// certificate then (from a context's store and from the default paths
/// alike: `cli/context/docker/load.go`'s `tlsConfig` wants both), and a
/// daemon that wants one would refuse the connection with nothing pointing
/// at the missing file.
fn client_pair_in(directory: &Path, named_by: &str) -> Result<Option<ClientCertificate>> {
    let cert = file_in(directory, "cert.pem", named_by);
    let key = file_in(directory, "key.pem", named_by);
    match (cert, key) {
        (Some(cert), Some(key)) => Ok(Some(ClientCertificate { cert, key })),
        (None, None) => Ok(None),
        (Some(cert), None) => Err(half_client_pair(named_by, &cert.path, "key.pem")),
        (None, Some(key)) => Err(half_client_pair(named_by, &key.path, "cert.pem")),
    }
}

/// The TLS settings `--docker-tls`/`-verify` ask for, from the three file
/// flags and the certificate directory (`cert_directory`, chosen by
/// `cert_directory_named_by` — see [`docker_cert_directory`]). Decides
/// which files are used without reading any of them; [`tls_client_config`]
/// does that.
///
/// - A file a flag names must exist. It need not be a regular file: a pipe
///   (`<(…)`, to keep a key off disk) is read like one, once.
/// - The directory must exist if any file is looked for in it — that is,
///   unless all three flags are given. Every file in it is optional, so a
///   mistyped path would otherwise read as a directory holding none of
///   them, and connect with the system trust store and no client
///   certificate without a word.
/// - Without `--docker-tls-ca-cert`, the directory's `ca.pem` is the CA
///   when it is there; when it isn't there is none, and the system trust
///   store is used.
/// - With either of `--docker-tls-cert`/`-key`, a client certificate is
///   presented, the other half coming from the directory's `cert.pem` or
///   `key.pem`, which must then exist. With neither, the directory's pair
///   is presented when both files are there, and none is when neither is.
///
/// Accepts everything Batect does (all three files required) and the Docker
/// CLI does (`ca.pem` required; a missing default `cert.pem`/`key.pem`
/// dropped), and more than either: no CA at all. What that costs is that a
/// forgotten `ca.pem` shows up as a verification failure rather than a
/// missing file, which [`Connection::explain_failure`] makes up for.
fn flag_tls_settings(
    options: &DockerConnectionOptions,
    cert_directory: &Path,
    cert_directory_named_by: &str,
) -> Result<TlsSettings> {
    let named = |flag: &str, path: &PathBuf| -> Result<TlsFile> {
        if !path.exists() || path.is_dir() {
            anyhow::bail!("{flag}: {} does not exist or is not a file", path.display());
        }
        Ok(TlsFile::new(path.clone(), flag))
    };
    let required = |name: &str| -> Result<TlsFile> {
        file_in(cert_directory, name, cert_directory_named_by).with_context(|| {
            format!(
                "{cert_directory_named_by}: {} does not exist or is not a file — a client \
                 certificate needs both a certificate and a key",
                cert_directory.join(name).display()
            )
        })
    };

    let consults_directory =
        options.tls_ca_cert.is_none() || options.tls_cert.is_none() || options.tls_key.is_none();
    if consults_directory && !cert_directory.is_dir() {
        anyhow::bail!(
            "{cert_directory_named_by}: {} does not exist or is not a directory",
            cert_directory.display()
        );
    }

    let ca = match &options.tls_ca_cert {
        Some(path) => Some(named("--docker-tls-ca-cert", path)?),
        None => file_in(cert_directory, "ca.pem", cert_directory_named_by),
    };
    let client = match (&options.tls_cert, &options.tls_key) {
        (None, None) => client_pair_in(cert_directory, cert_directory_named_by)?,
        (cert, key) => Some(ClientCertificate {
            cert: match cert {
                Some(path) => named("--docker-tls-cert", path)?,
                None => required("cert.pem")?,
            },
            key: match key {
                Some(path) => named("--docker-tls-key", path)?,
                None => required("key.pem")?,
            },
        }),
    };
    Ok(TlsSettings {
        ca,
        client,
        skip_verify_requested: false,
    })
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
/// default, exactly once — `rustls::ClientConfig::builder()`, which
/// [`tls_client_config`] calls, uses the installed provider, and panics
/// when there is none unless `rustls`'s own crate features name exactly
/// one (`ratect-core`'s `bollard` dependency turns on `ring`, through its
/// `ssl` feature; installing it here means nothing depends on no other
/// crate ever turning on a second). Idempotent: a later call after the first is a no-op
/// (`install_default` only errors if something else already installed a
/// provider, which never happens here — nothing else in `ratect-core`
/// installs one).
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

/// The error for only one of `cert.pem`/`key.pem` being there.
fn half_client_pair(named_by: &str, present: &Path, missing: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{named_by}: {} has no {missing} beside it — a client certificate needs both",
        present.display()
    )
}

/// Reads `file`, naming what named it if it can't.
fn read_tls_file(file: &TlsFile) -> Result<Vec<u8>> {
    fs::read(&file.path)
        .with_context(|| format!("{}: {} can't be read", file.named_by, file.path.display()))
}

/// Reads every certificate in the PEM file `file`, a CA or a client
/// certificate — an error naming the file and what named it when it can't
/// be read, isn't valid PEM, or holds no certificate at all.
fn read_certificates(file: &TlsFile) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;
    let bytes = read_tls_file(file)?;
    let certificates = rustls::pki_types::CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| {
            format!(
                "{}: {} is not a valid PEM certificate file",
                file.named_by,
                file.path.display()
            )
        })?;
    if certificates.is_empty() {
        anyhow::bail!(
            "{}: {} holds no certificate",
            file.named_by,
            file.path.display()
        );
    }
    Ok(certificates)
}

/// Reads the private key in `file` — an error naming the file and what
/// named it when it can't be read, is encrypted (there is no passphrase to
/// decrypt it with: a context stores none, and no flag takes one), or holds
/// no usable key.
fn read_private_key(file: &TlsFile) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    use rustls::pki_types::pem::PemObject;
    let bytes = read_tls_file(file)?;
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
        || text.contains("Proc-Type: 4,ENCRYPTED")
    {
        anyhow::bail!(
            "{}: {} is an encrypted private key, which Ratect can't use — store it unencrypted",
            file.named_by,
            file.path.display()
        );
    }
    rustls::pki_types::PrivateKeyDer::from_pem_slice(&bytes).with_context(|| {
        format!(
            "{}: {} holds no valid PEM private key",
            file.named_by,
            file.path.display()
        )
    })
}

/// The OS trust store, for a connection with no CA of its own. An entry
/// that fails to load is skipped rather than failing the connection: one
/// unreadable entry has nothing to do with Docker, and the Docker CLI
/// connects regardless (see this module's own doc comment).
fn system_trust_roots() -> rustls::RootCertStore {
    let loaded = rustls_native_certs::load_native_certs();
    if !loaded.errors.is_empty() {
        tracing::warn!(errors = ?loaded.errors, "ignoring errors loading the system trust store");
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(loaded.certs);
    roots
}

/// The `rustls` client configuration for `tls`. Every file is read and
/// checked here, up front, so a bad one is named by this error rather than
/// surfacing later as an opaque handshake failure — a client key most of
/// all, which would otherwise just not be presented. The daemon's
/// certificate is always verified: `skip_verify_requested` plays no part
/// in this.
fn tls_client_config(tls: &TlsSettings) -> Result<rustls::ClientConfig> {
    ensure_crypto_provider_installed();
    let roots = match &tls.ca {
        Some(ca) => {
            let mut roots = rustls::RootCertStore::empty();
            for certificate in read_certificates(ca)? {
                roots.add(certificate).with_context(|| {
                    format!(
                        "{}: {} holds a certificate that can't be used as a trust root",
                        ca.named_by,
                        ca.path.display()
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
    let certificates = read_certificates(&client.cert)?;
    let key = read_private_key(&client.key)?;
    builder
        .with_client_auth_cert(certificates, key)
        .with_context(|| {
            format!(
                "{}: the private key in {} can't be used with the certificate in {}",
                client.key.named_by,
                client.key.path.display(),
                client.cert.path.display()
            )
        })
}

/// The address in `host` a TLS connection is made to: a `tcp://` or
/// `https://` address, or a bare `host:port` — what `connect_with_ssl`
/// took, so what `--docker-host` has always accepted with `--docker-tls`.
/// Anything else with a scheme is refused; `connect_with_ssl` built a
/// client for it that could never connect. A pure string check, so it's
/// asked first — before any certificate is read or the system trust store
/// loaded for a connection that could never be made.
///
/// `target` is what errors call the daemon: `Docker context 'x' (host
/// 'h')` or `Docker host 'h'`.
fn tls_address<'a>(target: &str, host: &'a str) -> Result<&'a str> {
    match host
        .strip_prefix("tcp://")
        .or_else(|| host.strip_prefix("https://"))
    {
        Some(address) => Ok(address),
        None if !host.contains("://") => Ok(host),
        None => anyhow::bail!(
            "Can't connect to {target} over TLS: the host isn't a tcp:// or https:// address, \
             the only kinds Ratect connects to over TLS"
        ),
    }
}

/// Connects to `address` (see [`tls_address`]) over TLS as `tls` says
/// (see [`tls_client_config`]), through `bollard`'s custom-transport hook
/// — the one route every TLS connection takes, and the one place a
/// failure to set it up is worded, whichever of the flags or a context
/// asked for it.
/// `bollard`'s own `connect_with_ssl` can't express what `TlsSettings`
/// does: it needs a CA, a client certificate and a key, each as a file,
/// adds the CA to the system trust store rather than using it alone, and
/// reads the client pair only during the handshake, where a failure means
/// no certificate is presented. The transport mirrors that function's own
/// — a `hyper-rustls` connector on `hyper-util`'s client, with no
/// idle-connection pooling — and every request, upgrades (attach, exec)
/// included, goes through it the same way.
///
fn connect_over_tls(target: &str, address: &str, tls: &TlsSettings) -> Result<Docker> {
    let config = tls_client_config(tls)
        .with_context(|| format!("Failed to connect to {target} over TLS"))?;

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
    .with_context(|| format!("Failed to connect to {target} over TLS"))
}

/// Why a verification failure on a connection may not be what the user
/// expected, from how the connection was configured.
#[derive(Debug)]
enum VerificationNote {
    /// The named context sets `SkipTLSVerify`, which Ratect doesn't honour.
    ContextAskedToSkip(String),
    /// `--docker-tls`/`-verify` with no CA: no `--docker-tls-ca-cert`, and
    /// nothing at `looked_for`, the certificate directory's `ca.pem`.
    SystemTrustStore { looked_for: PathBuf },
}

/// A connected client, plus what's needed to explain its first failure.
#[derive(Debug)]
pub(super) struct Connection {
    pub(super) docker: Docker,
    verification_note: Option<VerificationNote>,
}

impl Connection {
    fn without_note(docker: Docker) -> Self {
        Self {
            docker,
            verification_note: None,
        }
    }

    /// `err`, from a request over this connection, with an explanation added
    /// when it's a certificate-verification failure and the connection has
    /// a [`VerificationNote`] — the cases where the user's own
    /// configuration says this shouldn't have happened, or doesn't say
    /// which roots the daemon was verified against. Anything else is
    /// returned unchanged.
    pub(super) fn explain_failure(&self, err: anyhow::Error) -> anyhow::Error {
        let Some(note) = &self.verification_note else {
            return err;
        };
        if !holds_certificate_error(err.as_ref()) {
            return err;
        }
        const SEE: &str = "See 'TLS with a private certificate authority' in Ratect's \
                           Connecting to Docker documentation for how to make it verifiable";
        match note {
            VerificationNote::ContextAskedToSkip(context_name) => err.context(format!(
                "Docker context '{context_name}' asks to skip TLS verification (its \
                 SkipTLSVerify setting), but Ratect always verifies the daemon's certificate. \
                 {SEE}"
            )),
            VerificationNote::SystemTrustStore { looked_for } => err.context(format!(
                "No --docker-tls-ca-cert was given and no ca.pem was found at {}, so the \
                 daemon's certificate was verified against the system trust store. {SEE}",
                looked_for.display()
            )),
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
/// `docker context create` stored for it (see [`docker_context_endpoint`]).
/// Either way the settings are one [`TlsSettings`], turned into a
/// connection by [`connect_over_tls`]; the flags' come from
/// [`flag_tls_settings`]. The returned [`Connection`]
/// carries what [`Connection::explain_failure`] needs to explain a
/// verification failure.
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
                .map(Connection::without_note)
                .with_context(|| {
                    format!("Failed to connect to Docker context '{context_name}' (host '{host}')")
                });
        };
        let target = format!("Docker context '{context_name}' (host '{host}')");
        let address = tls_address(&target, &host)?;
        let docker = connect_over_tls(&target, address, &tls)?;
        return Ok(Connection {
            docker,
            verification_note: tls
                .skip_verify_requested
                .then_some(VerificationNote::ContextAskedToSkip(context_name)),
        });
    }

    let docker_tls_verify_env = std::env::var("DOCKER_TLS_VERIFY").ok();
    if !tls_enabled(options, docker_tls_verify_env.as_deref()) {
        let docker = match host {
            Some(host) => Docker::connect_with_host(&host)
                .with_context(|| format!("Failed to connect to Docker host '{host}'"))?,
            None => Docker::connect_with_local_defaults().context("Failed to connect to Docker")?,
        };
        return Ok(Connection::without_note(docker));
    }

    let host = require_host_for_tls(host)?;
    let target = format!("Docker host '{host}'");
    let address = tls_address(&target, &host)?;
    let docker_cert_path_env = std::env::var("DOCKER_CERT_PATH").ok();
    let (cert_directory, named_by) =
        docker_cert_directory(options, docker_cert_path_env.as_deref())?;
    let tls = flag_tls_settings(options, &cert_directory, &named_by)
        .with_context(|| format!("Failed to connect to {target} over TLS"))?;
    let docker = connect_over_tls(&target, address, &tls)?;
    Ok(Connection {
        docker,
        verification_note: tls
            .ca
            .is_none()
            .then(|| VerificationNote::SystemTrustStore {
                looked_for: cert_directory.join("ca.pem"),
            }),
    })
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
