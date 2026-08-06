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

//! Proxy environment variable detection/propagation
//! (`--no-proxy-vars` to disable) — ported from Batect's
//! `ProxyEnvironmentVariablesProvider`/`ProxyEnvironmentVariablePreprocessor`.
//! Rewrites `localhost`/`127.0.0.1`/`::1` proxy URLs to `host.docker.internal`
//! (macOS/Windows only — `None` on Linux).

use std::collections::{BTreeSet, HashMap};

/// The proxy-related environment variable names Batect looks for, lowercase.
/// `http_proxy`/`https_proxy`/`ftp_proxy` get their values preprocessed
/// (`preprocess_proxy_value`); `no_proxy` doesn't.
const PROXY_VARIABLE_NAMES_NEEDING_PREPROCESSING: [&str; 3] =
    ["http_proxy", "https_proxy", "ftp_proxy"];
const NO_PROXY_VARIABLE_NAME: &str = "no_proxy";

/// The hostname a container can reach the Docker host itself through, if
/// any — used to rewrite a proxy value that points at `localhost` (which,
/// from *inside* a container, means the container itself, not the host
/// running Docker) so the proxy is actually reachable.
///
/// Unlike Batect's `DockerHostNameResolver` (which queries the Docker
/// daemon's version and picks between several historical hostnames back to
/// Docker 17.06), this doesn't query the daemon at all — it just returns
/// `host.docker.internal` on the platforms where a reasonably current
/// Docker Desktop provides it automatically, and `None` on Linux (not
/// automatic there) or anywhere else. A known, accepted gap versus Batect's
/// fallback chain: not worth porting since any actively-maintained Docker
/// install today satisfies the modern case that chain converges to anyway.
pub fn docker_host_name() -> Option<&'static str> {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        Some("host.docker.internal")
    } else {
        None
    }
}

/// Rewrites `value` if it's an `http`/`https` URL whose host is
/// `localhost`/`127.0.0.1`/`::1` and `docker_host_name` is available —
/// otherwise returns it unchanged (not a URL, not `http`/`https`, doesn't
/// refer to the local machine, or no Docker host name on this platform).
/// Ported from `ProxyEnvironmentVariablePreprocessor`.
fn preprocess_proxy_value(value: &str, docker_host_name: Option<&str>) -> String {
    let Some(docker_host_name) = docker_host_name else {
        return value.to_string();
    };
    let Ok(mut parsed) = url::Url::parse(value) else {
        return value.to_string();
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return value.to_string();
    }
    // `Url::host_str` returns an IPv6 literal wrapped in brackets (e.g.
    // `"[::1]"`), matching how it appears in the URL itself — not the bare
    // `"::1"` Batect's own check (`parsed.host in setOf("localhost",
    // "127.0.0.1", "::1")`) compares against, since OkHttp's `HttpUrl.host`
    // strips them. Both bracketed and unbracketed forms are accepted here
    // so this doesn't silently miss the IPv6 case.
    let refers_to_local_machine = matches!(
        parsed.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    );
    if !refers_to_local_machine {
        return value.to_string();
    }

    match parsed.set_host(Some(docker_host_name)) {
        Ok(()) => parsed.to_string(),
        Err(_) => value.to_string(),
    }
}

/// Looks `name` up in the host environment case-insensitively: `name`
/// itself, then its uppercase form, then its lowercase form — matching
/// Batect's `getMatchingCaseOrOtherCase`.
fn case_or_other_case(host_env: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    host_env(name)
        .or_else(|| host_env(&name.to_uppercase()))
        .or_else(|| host_env(&name.to_lowercase()))
}

/// Builds the proxy-related environment variables to inject into a
/// container's environment or a build's `build_args` — ported from
/// Batect's `ProxyEnvironmentVariablesProvider`. Detects
/// `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` from the host
/// environment (both cases, via `host_env`), preprocessing the three
/// URL-bearing ones (see `preprocess_proxy_value`), and appends
/// `extra_no_proxy_entries` (comma-joined) to `no_proxy`/`NO_PROXY` — but
/// only when at least one proxy variable was actually found; if none were,
/// an empty map is returned even when `extra_no_proxy_entries` is
/// non-empty, matching Batect's own short-circuit (there's nothing to
/// exempt from proxying if nothing is being proxied).
pub fn proxy_environment_variables(
    host_env: impl Fn(&str) -> Option<String>,
    extra_no_proxy_entries: &BTreeSet<String>,
) -> HashMap<String, String> {
    let docker_host_name = docker_host_name();
    let lowercase_names = PROXY_VARIABLE_NAMES_NEEDING_PREPROCESSING
        .iter()
        .copied()
        .chain(std::iter::once(NO_PROXY_VARIABLE_NAME));

    let mut variables = HashMap::new();
    for name in lowercase_names {
        let Some(value) = case_or_other_case(&host_env, name) else {
            continue;
        };
        let value = if PROXY_VARIABLE_NAMES_NEEDING_PREPROCESSING.contains(&name) {
            preprocess_proxy_value(&value, docker_host_name)
        } else {
            value
        };
        variables.insert(name.to_string(), value.clone());
        variables.insert(name.to_uppercase(), value);
    }

    if variables.is_empty() || extra_no_proxy_entries.is_empty() {
        return variables;
    }

    let extra_entries = extra_no_proxy_entries
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    for key in [NO_PROXY_VARIABLE_NAME.to_string(), "NO_PROXY".to_string()] {
        let combined = match variables.get(&key) {
            Some(existing) if !existing.is_empty() => format!("{existing},{extra_entries}"),
            _ => extra_entries.clone(),
        };
        variables.insert(key, combined);
    }

    variables
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
