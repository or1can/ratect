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
//! on every platform.
//!
//! # The rewrite is only half of reaching a proxy on the host
//!
//! Rewriting the URL makes it *name* the host. Something still has to make
//! that name resolve, and on Linux nothing does automatically — which is why
//! this module returns more than a map of variables:
//!
//! - [`ProxyEnvironment::host_gateway`] is the `/etc/hosts` entry the run
//!   must add so the rewritten name resolves, and it is `Some` only when a
//!   URL was *actually* rewritten. Injecting it for a run with no proxy at
//!   all would put a name into every container that nothing there asked for.
//! - [`loopback_only_ports_in`], over the tables [`proc_net_tcp_tables`]
//!   reads, answers the case the rewrite cannot fix at all: a proxy bound to
//!   `127.0.0.1` is unreachable from a container however correct its URL is.
//!   The engine warns; it does not fail, since a run may not need the proxy
//!   and `--no-proxy-vars` already turns propagation off.
//!
//! Batect has neither half — it leaves Linux unrewritten
//! ([batect#10](https://github.com/batect/batect/issues/10), open for eight
//! years), because its own recipe predates the `host-gateway` sentinel. See
//! `docs/differences-from-batect.md`.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The proxy-related environment variable names Batect looks for, lowercase.
/// `http_proxy`/`https_proxy`/`ftp_proxy` get their values preprocessed
/// (`preprocess_proxy_value`); `no_proxy` doesn't.
const PROXY_VARIABLE_NAMES_NEEDING_PREPROCESSING: [&str; 3] =
    ["http_proxy", "https_proxy", "ftp_proxy"];
const NO_PROXY_VARIABLE_NAME: &str = "no_proxy";

/// Docker's own sentinel for "the machine running the daemon", which it
/// substitutes for the real address when it writes the container's
/// `/etc/hosts`. Added in Docker Engine 20.10 (December 2020).
const HOST_GATEWAY_ADDRESS: &str = "host-gateway";

/// The extra `/etc/hosts` entry that makes a rewritten proxy URL resolve
/// from inside a container: the name [`preprocess_proxy_value`] rewrote to,
/// mapped to Docker's [`HOST_GATEWAY_ADDRESS`] sentinel.
///
/// Both fields are `'static` because both are constants — the type exists so
/// the pair travels as one named thing from the decision that produces it
/// (`ProxyEnvironment::host_gateway`) to the two places that consume it
/// (`docker::NetworkOptions` and `ContainerRuntime::build_image`), rather
/// than each site re-deriving "should I add a host, and which".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostGateway {
    pub name: &'static str,
    pub address: &'static str,
}

impl HostGateway {
    /// The entry as Docker spells it — `name:address`, the form both
    /// `HostConfig.extra_hosts` and the `/build` endpoint's `extrahosts`
    /// take.
    ///
    /// Here rather than at the two build call sites so the type owns its own
    /// wire format: the pair travelling as one value is the point, and a
    /// caller re-spelling the colon is the pair coming apart again.
    pub fn extra_host(&self) -> String {
        format!("{}:{}", self.name, self.address)
    }
}

/// The hostname a container can reach the Docker host itself through — used
/// to rewrite a proxy value that points at `localhost` (which, from *inside*
/// a container, means the container itself, not the host running Docker) so
/// the proxy is actually reachable.
///
/// Unlike Batect's `DockerHostNameResolver` (which queries the Docker
/// daemon's version and picks between several historical hostnames back to
/// Docker 17.06), this doesn't query the daemon at all. It is a constant:
/// Docker Desktop provides `host.docker.internal` automatically, and on
/// Linux [`HostGateway`] makes the same name resolve. Taking that as given
/// rather than querying the daemon commits Ratect to Docker Engine 20.10+
/// (December 2020), knowingly — the same reasoning that declined Batect's
/// fallback chain ("any actively-maintained Docker install today satisfies
/// the modern case") argues for the floor, and it is the whole reason this
/// stays a pure function.
///
/// Kept returning `Option` even though it is now always `Some`: the caller
/// that matters (`preprocess_proxy_value`) is a port of Batect's, where the
/// resolver genuinely can fail, and its "no name, so change nothing" branch
/// is the behaviour a future platform without one would need.
pub fn docker_host_name() -> Option<&'static str> {
    Some("host.docker.internal")
}

/// What preprocessing one proxy variable produced: the value to propagate,
/// and — only when the value was actually rewritten — the port it named on
/// the host. The port is what [`loopback_only_ports_in`] needs, and its
/// presence is what makes a run ask for a [`HostGateway`] at all.
struct Preprocessed {
    value: String,
    rewritten_port: Option<u16>,
}

impl Preprocessed {
    /// The variable as it stands, rewriting nothing.
    fn unchanged(value: &str) -> Self {
        Self {
            value: value.to_string(),
            rewritten_port: None,
        }
    }
}

/// Rewrites `value` if it's an `http`/`https` URL whose host is
/// `localhost`/`127.0.0.1`/`::1` and `docker_host_name` is available —
/// otherwise returns it unchanged (not a URL, not `http`/`https`, doesn't
/// refer to the local machine, or no Docker host name on this platform).
/// Ported from `ProxyEnvironmentVariablePreprocessor`.
fn preprocess_proxy_value(value: &str, docker_host_name: Option<&str>) -> Preprocessed {
    let Some(docker_host_name) = docker_host_name else {
        return Preprocessed::unchanged(value);
    };
    let Ok(mut parsed) = url::Url::parse(value) else {
        return Preprocessed::unchanged(value);
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Preprocessed::unchanged(value);
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
        return Preprocessed::unchanged(value);
    }

    // Read the port *before* the host is replaced: `port_or_known_default`
    // falls back to the scheme's default (80/443) for a URL that names no
    // port, which is the port the proxy is on either way.
    let rewritten_port = parsed.port_or_known_default();
    match parsed.set_host(Some(docker_host_name)) {
        Ok(()) => Preprocessed {
            value: parsed.to_string(),
            rewritten_port,
        },
        Err(_) => Preprocessed::unchanged(value),
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

/// The proxy configuration one run propagates, and what else that
/// propagation obliges the run to do.
///
/// Returned as one value rather than a bare map because the two extra
/// obligations — adding a [`HostGateway`] and warning about a proxy nothing
/// in a container can reach — are decided by *whether a URL was rewritten*,
/// which only this module can see. Handing back the map alone would leave
/// every call site to re-derive that from the strings, and the site added
/// next would be the one that forgot.
pub struct ProxyEnvironment {
    /// The variables to inject into a container's environment or a build's
    /// `build_args`.
    pub variables: HashMap<String, String>,
    /// The host ports of the proxy URLs that were rewritten. Empty when
    /// nothing was rewritten — which is the whole of the "does this run need
    /// a host gateway" question.
    rewritten_ports: BTreeSet<u16>,
}

impl ProxyEnvironment {
    /// The `/etc/hosts` entry every container and image build in this run
    /// needs so a rewritten URL resolves — `None` when no URL was rewritten,
    /// so a run whose proxy already names a routable host (or which has no
    /// proxy at all) gets no name injected that nothing asked for.
    pub fn host_gateway(&self) -> Option<HostGateway> {
        if self.rewritten_ports.is_empty() {
            return None;
        }
        Some(HostGateway {
            name: docker_host_name()?,
            address: HOST_GATEWAY_ADDRESS,
        })
    }

    /// The rewritten URLs' host ports, for [`loopback_only_ports`].
    pub fn rewritten_ports(&self) -> &BTreeSet<u16> {
        &self.rewritten_ports
    }
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
) -> ProxyEnvironment {
    let docker_host_name = docker_host_name();
    let lowercase_names = PROXY_VARIABLE_NAMES_NEEDING_PREPROCESSING
        .iter()
        .copied()
        .chain(std::iter::once(NO_PROXY_VARIABLE_NAME));

    let mut variables = HashMap::new();
    let mut rewritten_ports = BTreeSet::new();
    for name in lowercase_names {
        let Some(value) = case_or_other_case(&host_env, name) else {
            continue;
        };
        let value = if PROXY_VARIABLE_NAMES_NEEDING_PREPROCESSING.contains(&name) {
            let preprocessed = preprocess_proxy_value(&value, docker_host_name);
            rewritten_ports.extend(preprocessed.rewritten_port);
            preprocessed.value
        } else {
            value
        };
        variables.insert(name.to_string(), value.clone());
        variables.insert(name.to_uppercase(), value);
    }

    if variables.is_empty() || extra_no_proxy_entries.is_empty() {
        return ProxyEnvironment {
            variables,
            rewritten_ports,
        };
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

    ProxyEnvironment {
        variables,
        rewritten_ports,
    }
}

/// The host's `/proc/net/tcp` and `/proc/net/tcp6`, or nothing at all off
/// Linux — the input [`loopback_only_ports_in`] answers from.
///
/// Linux only, and deliberately so on both counts. It is the only platform
/// where the rewrite depends on a [`HostGateway`] at all — Docker Desktop
/// reaches a host loopback service through its own VM gateway, so there is
/// nothing to warn about there — and it is the only one with these files.
///
/// The platform difference lives here, in the *input*, rather than as a
/// branch around the check: one code path then serves every platform, and
/// "empty everywhere else" follows from having no tables to read.
///
/// Errors reading `/proc` are swallowed rather than propagated — a
/// diagnostic that cannot be produced must not fail a run that would
/// otherwise have worked.
///
/// The engine reaches this through a field it can replace in tests rather
/// than calling it directly, so the warning built on it is reachable on a
/// machine with no `/proc` at all — see `TaskEngine::with_proc_net_tcp`.
pub fn proc_net_tcp_tables() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// Which of `ports` the given `/proc/net/tcp`-format `tables` show listening
/// on loopback addresses *only* — the case a rewritten URL cannot fix, since
/// a [`HostGateway`] routes a container to the host's address on the
/// container's own network and a socket bound to `127.0.0.1` never accepts a
/// connection there.
///
/// Takes the tables rather than reading them, so both the parsing and the
/// engine's warning built on it are testable on every platform, not only on
/// the one that has the files.
///
/// A port nothing is listening on is *not* reported: that is a proxy that
/// isn't running, a different complaint from one that is running but bound
/// too narrowly, and only the second is what this exists to catch.
pub fn loopback_only_ports_in(tables: &[String], ports: &BTreeSet<u16>) -> BTreeSet<u16> {
    let listening: Vec<(IpAddr, u16)> = tables
        .iter()
        .flat_map(|table| table.lines())
        .filter_map(listening_socket)
        .collect();

    ports
        .iter()
        .copied()
        .filter(|port| {
            let mut on_this_port = listening
                .iter()
                .filter(|(_, listen_port)| listen_port == port);
            // `all` on an empty iterator is `true`, so the "nothing is
            // listening" case has to be excluded before it, not by it.
            on_this_port
                .next()
                .is_some_and(|(address, _)| address.is_loopback())
                && on_this_port.all(|(address, _)| address.is_loopback())
        })
        .collect()
}

/// The address and port one `/proc/net/tcp`/`tcp6` line is listening on, or
/// `None` for any line that isn't a listening socket — the header, a
/// connected socket, or anything unparseable.
fn listening_socket(line: &str) -> Option<(IpAddr, u16)> {
    /// The kernel's `TCP_LISTEN`, as `/proc/net/tcp` prints it.
    const TCP_LISTEN: &str = "0A";

    let mut fields = line.split_whitespace();
    let (_slot, local, _remote, state) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if state != TCP_LISTEN {
        return None;
    }
    let (address, port) = local.split_once(':')?;
    Some((
        parse_proc_address(address)?,
        u16::from_str_radix(port, 16).ok()?,
    ))
}

/// Parses one `/proc/net/tcp` local address: 8 hex digits for IPv4, 32 for
/// IPv6. An IPv4-mapped IPv6 address comes back as the IPv4 address it maps,
/// so `::ffff:127.0.0.1` answers `is_loopback` the way its writer meant.
///
/// The kernel prints each 32-bit word of the address with `%08X` — of a
/// value it holds in *network* byte order — so recovering the bytes means
/// reading each group as a `u32` and taking its **little-endian** bytes.
/// That is an assumption about the host being little-endian, which every
/// platform Ratect ships for is; on a big-endian Linux host this would read
/// each word backwards and the check would simply stop matching, costing a
/// diagnostic rather than a run.
fn parse_proc_address(hex: &str) -> Option<IpAddr> {
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(hex, 16).ok()?.to_le_bytes(),
        ))),
        32 => {
            let mut bytes = [0u8; 16];
            for (word, group) in bytes.chunks_mut(4).zip(hex.as_bytes().chunks(8)) {
                let group = std::str::from_utf8(group).ok()?;
                word.copy_from_slice(&u32::from_str_radix(group, 16).ok()?.to_le_bytes());
            }
            let address = Ipv6Addr::from(bytes);
            Some(match address.to_ipv4_mapped() {
                Some(mapped) => IpAddr::V4(mapped),
                None => IpAddr::V6(address),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
