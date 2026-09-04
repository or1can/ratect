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
//! lookup, TLS/cert-directory resolution, and the actual `bollard::Docker`
//! connect call ([`connect`], reached only through
//! [`DockerClient::new`](super::DockerClient::new)). Split out of `docker.rs`
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
//! cert-loading, one layer below anything here.

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
/// active-context resolution, exactly matching Batect's own precedence
/// (`CommandLineOptionsParser.resolveDockerContext`).
///
/// One deliberate divergence from Batect, documented in
/// [Differences from Batect](../../../docs/differences-from-batect.md): there's
/// no way to skip TLS verification here. Batect's own `--docker-tls`
/// (without `-verify`) sets Go's `tls.Config.InsecureSkipVerify`, which
/// disables *all* server certificate verification — chain of trust,
/// expiry, and hostname matching, not just hostname matching — while still
/// doing the TLS handshake and any configured client-certificate auth.
/// `tls` and `tls_verify` are both accepted here (for command-line
/// compatibility) but behave identically: connecting always fully
/// verifies the daemon's certificate. This matches `rustls` itself (the
/// library this is built on) rather than fighting it: `rustls` has no
/// boolean toggle for this either — disabling verification means
/// implementing its own `ServerCertVerifier` trait from scratch, a
/// deliberate hurdle against careless misuse, not a config flag. See
/// [CLI reference](../../../docs/cli-reference.md#tls-with-a-private-certificate-authority)
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

/// The subset of a context's `meta.json` this needs — just the daemon host
/// to connect to. Field names/casing match the Docker CLI's own format
/// exactly (`Endpoints.docker.Host`; `docker` itself is lowercase, unlike
/// its sibling fields).
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
}

/// Reads `<config_directory>/contexts/meta/<sha256(context_name)>/meta.json`
/// for `context_name`'s daemon host. A missing file (or one that doesn't
/// parse as expected) is reported as the named context not existing —
/// matching what `--docker-context` naming an unknown context should feel
/// like to a user, rather than a raw file-not-found error.
fn docker_context_host(config_directory: &Path, context_name: &str) -> Result<String> {
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
    Ok(metadata.endpoints.docker.host)
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
/// only when neither `--docker-context`/`--docker-host` nor `DOCKER_CONTEXT`
/// says otherwise. `None` (not an error) when the file doesn't exist or sets
/// no `currentContext` — both mean the same thing as the Docker CLI's own
/// fallback: use the `default` context.
fn active_docker_context(config_directory: &Path) -> Option<String> {
    let contents = fs::read_to_string(config_directory.join("config.json")).ok()?;
    let config: DockerCliConfig = serde_json::from_str(&contents).ok()?;
    config.current_context.filter(|name| !name.is_empty())
}

/// `--docker-config`, else `DOCKER_CONFIG`, else `~/.docker` — the
/// directory the Docker CLI's own context store and `config.json` live in.
fn docker_config_directory(options: &DockerConnectionOptions) -> Result<PathBuf> {
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

/// The host to connect to once no context applies (step 2's own
/// resolution, used both for the plain and TLS paths): an explicit
/// `--docker-host`, else the real `DOCKER_HOST` environment variable, else
/// `None` (only valid for a plain, non-TLS connection — see `connect`,
/// which requires an explicit host for TLS). Pure (the environment value is
/// injected) so it's unit-testable without depending on whichever real
/// environment variables happen to be set on the machine running the
/// tests.
fn resolve_host(
    options: &DockerConnectionOptions,
    docker_host_env: Option<&str>,
) -> Option<String> {
    options
        .host
        .clone()
        .or_else(|| docker_host_env.map(str::to_string))
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

/// Step 1–3 of `connect`'s own doc comment, as a pure decision: which
/// context (if any) should be looked up in the store. A `None` return means
/// "no context — connect via `options.host`/`DOCKER_HOST`/the platform
/// default instead", covering both an explicit `--docker-host` (which skips
/// context resolution entirely) and the `default` context name itself
/// (never looked up in the store — it *means* "no context").
///
/// `active_context` is step 4's fallback — reading the store's own "active
/// context" needs real file I/O, so it's computed by the caller and passed
/// in already-resolved, keeping this function itself pure (and so
/// unit-testable without a filesystem) like `select_builder_version`.
fn resolve_context_name(
    options: &DockerConnectionOptions,
    docker_context_env: Option<&str>,
    active_context: Option<String>,
) -> Option<String> {
    let context_name = if let Some(context) = &options.context {
        Some(context.clone())
    } else if options.host.is_some() {
        None
    } else if let Some(context) = docker_context_env {
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

/// Resolves and connects to the Docker daemon, matching Batect's own
/// precedence (`CommandLineOptionsParser.resolveDockerContext`/
/// `DockerClientConfigurationFactory`) exactly:
///
/// 1. An explicit `--docker-context` is looked up by name in the context
///    store.
/// 2. Otherwise, an explicit `--docker-host` connects directly to that host
///    — bypassing the context store entirely, even if `DOCKER_CONTEXT` or
///    an active context is also set (Batect's own rule: an explicit host
///    always means "ignore whatever context would otherwise apply").
/// 3. Otherwise, `DOCKER_CONTEXT` (if set) is looked up the same way as 1.
/// 4. Otherwise, the Docker CLI's own "active" context
///    (`~/.docker/config.json`'s `currentContext`) is looked up the same
///    way, falling back to connecting via `DOCKER_HOST`/bollard's own
///    platform default (unix socket/named pipe) when that's unset or names
///    the `default` context.
///
/// TLS (`--docker-tls`/`-verify`) only applies once a context is ruled out
/// — Batect rejects combining it with `--docker-context` at all (see
/// `conflicting_option_with_context`) — and has no platform-default host to
/// fall back to the way the plain path does, so an explicit host is
/// required (see `require_host_for_tls`).
pub(super) fn connect(options: &DockerConnectionOptions) -> Result<Docker> {
    if options.context.is_some() {
        if let Some(conflicting) = conflicting_option_with_context(options) {
            anyhow::bail!("Cannot use both --docker-context and {conflicting}.");
        }
    }

    let config_directory = docker_config_directory(options)?;
    let docker_context_env = std::env::var("DOCKER_CONTEXT").ok();
    let context_name = resolve_context_name(
        options,
        docker_context_env.as_deref(),
        active_docker_context(&config_directory),
    );

    if let Some(context_name) = context_name {
        let host = docker_context_host(&config_directory, &context_name)?;
        return Docker::connect_with_host(&host).with_context(|| {
            format!("Failed to connect to Docker context '{context_name}' (host '{host}')")
        });
    }

    let docker_host_env = std::env::var("DOCKER_HOST").ok();
    let host = resolve_host(options, docker_host_env.as_deref());

    let docker_tls_verify_env = std::env::var("DOCKER_TLS_VERIFY").ok();
    if !tls_enabled(options, docker_tls_verify_env.as_deref()) {
        return match host {
            Some(host) => Docker::connect_with_host(&host)
                .with_context(|| format!("Failed to connect to Docker host '{host}'")),
            None => Docker::connect_with_local_defaults().context("Failed to connect to Docker"),
        };
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
        .with_context(|| format!("Failed to connect to Docker host '{host}' over TLS"))
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
