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

//! Resolving private registry credentials via real Docker credential
//! helpers — [`RegistryCredentialResolver`] and its
//! [`DockerCredentialHelperResolver`] production implementation, plus
//! [`registry_hostname`] (pull's own registry) and
//! [`configured_registries`] (build's — every registry `config.json`
//! declares, since neither Batect nor Ratect parses Dockerfile `FROM`
//! lines). A filesystem-and-subprocess concern of its own, kept out of
//! `docker.rs` (which stays focused on bollard/daemon interaction and wires
//! this module into `pull_image`/`build_image`).

use anyhow::Context;

const DOCKER_IO: &str = "docker.io";

/// The registry hostname an image reference names — `"docker.io"` when the
/// reference has no explicit registry. Matches the real `docker` CLI's own
/// reference-parsing rule (`distribution/reference`'s `splitDockerDomain`,
/// verified directly against its source): the first path segment (before
/// the first `/`) is a registry domain only if it is exactly `localhost`,
/// equals `index.docker.io` (canonicalized here to `docker.io`), contains a
/// `.` or `:`, or has any uppercase character; otherwise there is no domain
/// and the whole reference is an image name under `docker.io`'s implicit
/// `library/` prefix — including a bare, single-segment reference, which
/// never has a domain regardless of its own spelling.
///
/// A lightweight community crate (`docker-image`, `sunsided/docker-image-rs`)
/// was considered and rejected during scoping: its regex requires a
/// dot-plus-TLD in the first segment, so it misses the `localhost`/
/// `index.docker.io`/uppercase cases entirely — a real correctness gap on a
/// common form (a local dev/CI registry), not a theoretical one. This rule
/// needs no regex, so it's hand-written instead.
pub fn registry_hostname(image_reference: &str) -> String {
    let Some((first_segment, _rest)) = image_reference.split_once('/') else {
        return DOCKER_IO.to_string();
    };

    if first_segment == "index.docker.io" {
        return DOCKER_IO.to_string();
    }

    let looks_like_domain = first_segment == "localhost"
        || first_segment.contains('.')
        || first_segment.contains(':')
        || first_segment.chars().any(|c| c.is_ascii_uppercase());

    if looks_like_domain {
        first_segment.to_string()
    } else {
        DOCKER_IO.to_string()
    }
}

/// The union of registry keys `config_json` (raw `config.json` contents)
/// declares under `auths` and `credHelpers` — every registry Ratect's own
/// Docker config knows about. Parses just those two fields itself rather
/// than replicating `docker_credential`'s own fuller config parsing, since
/// that crate has no way to enumerate configured registries through its
/// public API (only look up one already-known server name at a time — see
/// [`RegistryCredentialResolver`]). Used for build, which — since neither
/// Batect nor Ratect parses Dockerfile `FROM` lines to scope this more
/// precisely — resolves every registry Ratect's config declares rather than
/// just the one image field pull can single out.
///
/// Empty for missing/malformed JSON or a config declaring neither field —
/// silent, not an error, matching a fresh machine's absent `config.json`
/// being "nothing configured" rather than a problem worth surfacing.
/// Sorted and deduplicated so callers (and tests) get a stable order.
pub fn configured_registries(config_json: &str) -> Vec<String> {
    #[derive(serde::Deserialize, Default)]
    struct ConfigRegistries {
        #[serde(default)]
        auths: std::collections::HashMap<String, serde::de::IgnoredAny>,
        #[serde(default, rename = "credHelpers")]
        cred_helpers: std::collections::HashMap<String, serde::de::IgnoredAny>,
    }

    let config: ConfigRegistries = serde_json::from_str(config_json).unwrap_or_default();
    let mut registries: Vec<String> = config
        .auths
        .into_keys()
        .chain(config.cred_helpers.into_keys())
        .collect();
    registries.sort();
    registries.dedup();
    registries
}

/// Resolves the credential configured for a registry — the seam pull (and,
/// later, build) credential wiring depends on, so tests can inject a fake
/// instead of invoking a real credential helper.
///
/// `Ok(None)` means nothing is configured for `registry` — the common,
/// anonymous case, and not a failure. `Err` means resolution itself failed
/// (a broken helper, a timeout, a malformed response, ...); callers treat
/// that as non-fatal to the run — see `docker.rs`'s pull-credential wiring —
/// but do surface it as a warning naming the registry, since a pull that
/// then fails three layers down for an unrelated-looking reason is exactly
/// the confusing-failure shape this project avoids elsewhere.
#[async_trait::async_trait]
pub trait RegistryCredentialResolver: Send + Sync {
    async fn resolve(
        &self,
        registry: &str,
    ) -> anyhow::Result<Option<bollard::auth::DockerCredentials>>;
}

/// How long a credential-helper subprocess gets before its resolution
/// attempt is treated as failed. Neither Batect nor the real `docker` CLI
/// impose one (verified directly against Batect's own helper-invocation
/// call path) — a deliberate divergence: a hung, misconfigured helper (one
/// that unexpectedly tries to prompt interactively, say) failing just the
/// registry it belongs to is a far better outcome than it hanging the
/// entire task run. `docker_credential` gives no handle to kill the
/// subprocess itself once spawned, so this timeout stops Ratect from
/// *waiting* on it, not the process from running.
const CREDENTIAL_HELPER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolves via `~/.docker/config.json`'s `auths`/`credsStore`/
/// `credHelpers`, exactly as the real `docker` CLI does — invoking the
/// configured `docker-credential-<name>` helper binary where one applies.
/// Reads Ratect's own resolved config path
/// (`docker::connection::docker_config_directory`), not `docker_credential`'s
/// own internal path resolution, so `--docker-config`/`DOCKER_CONFIG` are
/// honored here exactly as everywhere else Ratect reads Docker's config.
pub struct DockerCredentialHelperResolver {
    config_path: std::path::PathBuf,
}

impl DockerCredentialHelperResolver {
    /// `config_directory` is the directory `config.json` lives in (e.g.
    /// `~/.docker`) — not necessarily where it exists yet: a missing file
    /// resolves like an empty one (nothing configured), matching the real
    /// `docker` CLI's own behavior on a fresh machine.
    pub fn new(config_directory: std::path::PathBuf) -> Self {
        Self {
            config_path: config_directory.join("config.json"),
        }
    }
}

#[async_trait::async_trait]
impl RegistryCredentialResolver for DockerCredentialHelperResolver {
    async fn resolve(
        &self,
        registry: &str,
    ) -> anyhow::Result<Option<bollard::auth::DockerCredentials>> {
        let config_path = self.config_path.clone();
        let registry = registry.to_string();

        let attempt = tokio::task::spawn_blocking(move || {
            let file = match std::fs::File::open(&config_path) {
                Ok(file) => file,
                Err(_) => return Ok(None),
            };
            match docker_credential::get_credential_from_reader(file, &registry) {
                Ok(credential) => Ok(Some(to_bollard_credentials(credential))),
                Err(
                    docker_credential::CredentialRetrievalError::NoCredentialConfigured
                    | docker_credential::CredentialRetrievalError::ConfigNotFound,
                ) => Ok(None),
                Err(docker_credential::CredentialRetrievalError::HelperFailure {
                    stdout,
                    stderr,
                    ..
                }) if is_credentials_not_found(&stdout) || is_credentials_not_found(&stderr) => {
                    Ok(None)
                }
                Err(error) => Err(anyhow::Error::from(error)),
            }
        });

        match tokio::time::timeout(CREDENTIAL_HELPER_TIMEOUT, attempt).await {
            Ok(join_result) => join_result.context("The credential-resolution task panicked")?,
            Err(_elapsed) => Err(anyhow::anyhow!(
                "Timed out after {CREDENTIAL_HELPER_TIMEOUT:?} waiting for the credential helper"
            )),
        }
    }
}

/// Whether a credential helper's failure output is really just "nothing is
/// stored for this server" rather than a genuine problem with the helper.
///
/// A globally-configured `credsStore` (the Docker Desktop/OrbStack default,
/// present even if `docker login` has never been run) makes
/// `docker_credential` invoke the helper for *every* registry, including
/// ones with nothing stored — the overwhelmingly common case for an
/// ordinary, already-public pull. Real helpers (`osxkeychain`, `wincred`,
/// `secretservice`, `pass`, and third-party ones like `ecr-login`/`gcloud`
/// that import the same shared library for exactly this compatibility) all
/// write that case's error as the literal string `"credentials not found in
/// native keychain"` to stdout and exit non-zero — verified directly against
/// `docker-credential-helpers`'s own `credentials/error.go`
/// (`NewErrCredentialsNotFound`) and `credentials/credentials.go`'s `Serve`
/// function. `docker_credential` (this crate) surfaces that as a generic
/// `HelperFailure` with no distinction from a real one, so this recognizes
/// the sentinel itself — the same way the real `docker` CLI's own
/// `IsErrCredentialsNotFound` does — to avoid warning on every anonymous
/// pull on any machine with a `credsStore` configured. Checked against both
/// `stdout` and `stderr` since nothing requires a conformant helper to use
/// exactly one.
fn is_credentials_not_found(output: &str) -> bool {
    output.contains("credentials not found in native keychain")
}

fn to_bollard_credentials(
    credential: docker_credential::DockerCredential,
) -> bollard::auth::DockerCredentials {
    match credential {
        docker_credential::DockerCredential::UsernamePassword(username, password) => {
            bollard::auth::DockerCredentials {
                username: Some(username),
                password: Some(password),
                ..Default::default()
            }
        }
        docker_credential::DockerCredential::IdentityToken(token) => {
            bollard::auth::DockerCredentials {
                identitytoken: Some(token),
                ..Default::default()
            }
        }
    }
}

#[cfg(test)]
#[path = "registry_auth_tests.rs"]
mod tests;
