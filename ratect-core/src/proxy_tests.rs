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

fn no_host_env(_: &str) -> Option<String> {
    None
}

fn host_env_with(
    pairs: &'static [(&'static str, &'static str)],
) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    }
}

#[test]
fn no_proxy_vars_set_returns_an_empty_map() {
    let variables = proxy_environment_variables(no_host_env, &BTreeSet::new()).variables;
    assert!(variables.is_empty());
}

#[test]
fn lowercase_host_vars_populate_both_cases() {
    let host_env = host_env_with(&[("http_proxy", "http://proxy.example.com:8080")]);
    let variables = proxy_environment_variables(host_env, &BTreeSet::new()).variables;

    assert_eq!(
        variables.get("http_proxy"),
        Some(&"http://proxy.example.com:8080".to_string())
    );
    assert_eq!(
        variables.get("HTTP_PROXY"),
        Some(&"http://proxy.example.com:8080".to_string())
    );
    assert_eq!(variables.len(), 2);
}

#[test]
fn uppercase_host_vars_populate_both_cases() {
    let host_env = host_env_with(&[("HTTPS_PROXY", "https://proxy.example.com:8443")]);
    let variables = proxy_environment_variables(host_env, &BTreeSet::new()).variables;

    assert_eq!(
        variables.get("https_proxy"),
        Some(&"https://proxy.example.com:8443".to_string())
    );
    assert_eq!(
        variables.get("HTTPS_PROXY"),
        Some(&"https://proxy.example.com:8443".to_string())
    );
}

#[test]
fn mixed_case_host_vars_are_all_detected() {
    let host_env = host_env_with(&[
        ("http_proxy", "http://http-proxy.example.com"),
        ("FTP_PROXY", "http://ftp-proxy.example.com"),
        ("no_proxy", "example.com"),
    ]);
    let variables = proxy_environment_variables(host_env, &BTreeSet::new()).variables;

    assert_eq!(variables.len(), 6);
    assert_eq!(
        variables.get("ftp_proxy"),
        Some(&"http://ftp-proxy.example.com".to_string())
    );
    assert_eq!(variables.get("no_proxy"), Some(&"example.com".to_string()));
}

#[test]
fn extra_no_proxy_entries_are_appended_when_proxy_vars_exist() {
    let host_env = host_env_with(&[("http_proxy", "http://proxy.example.com")]);
    let extra = BTreeSet::from(["app".to_string(), "database".to_string()]);
    let variables = proxy_environment_variables(host_env, &extra).variables;

    assert_eq!(variables.get("no_proxy"), Some(&"app,database".to_string()));
    assert_eq!(variables.get("NO_PROXY"), Some(&"app,database".to_string()));
}

#[test]
fn extra_no_proxy_entries_are_appended_to_an_existing_no_proxy_value() {
    let host_env = host_env_with(&[
        ("http_proxy", "http://proxy.example.com"),
        ("no_proxy", "existing.example.com"),
    ]);
    let extra = BTreeSet::from(["app".to_string()]);
    let variables = proxy_environment_variables(host_env, &extra).variables;

    assert_eq!(
        variables.get("no_proxy"),
        Some(&"existing.example.com,app".to_string())
    );
}

#[test]
fn extra_no_proxy_entries_are_ignored_when_no_proxy_vars_are_set_at_all() {
    let extra = BTreeSet::from(["app".to_string()]);
    let variables = proxy_environment_variables(no_host_env, &extra).variables;

    assert!(variables.is_empty());
}

#[test]
fn preprocess_proxy_value_leaves_a_non_local_url_unchanged() {
    assert_eq!(
        preprocess_proxy_value(
            "http://proxy.example.com:8080",
            Some("host.docker.internal")
        )
        .value,
        "http://proxy.example.com:8080"
    );
}

#[test]
fn preprocess_proxy_value_leaves_an_invalid_url_unchanged() {
    assert_eq!(
        preprocess_proxy_value("not a url", Some("host.docker.internal")).value,
        "not a url"
    );
}

#[test]
fn preprocess_proxy_value_leaves_a_non_http_scheme_unchanged() {
    assert_eq!(
        preprocess_proxy_value("socks5://localhost:1080", Some("host.docker.internal")).value,
        "socks5://localhost:1080"
    );
}

#[test]
fn preprocess_proxy_value_does_nothing_without_a_docker_host_name() {
    assert_eq!(
        preprocess_proxy_value("http://localhost:8080", None).value,
        "http://localhost:8080"
    );
}

#[test]
fn preprocess_proxy_value_rewrites_localhost() {
    assert_eq!(
        preprocess_proxy_value("http://localhost:8080", Some("host.docker.internal")).value,
        "http://host.docker.internal:8080/"
    );
}

#[test]
fn preprocess_proxy_value_rewrites_127_0_0_1() {
    assert_eq!(
        preprocess_proxy_value("http://127.0.0.1:8080", Some("host.docker.internal")).value,
        "http://host.docker.internal:8080/"
    );
}

#[test]
fn preprocess_proxy_value_rewrites_ipv6_localhost() {
    assert_eq!(
        preprocess_proxy_value("http://[::1]:8080", Some("host.docker.internal")).value,
        "http://host.docker.internal:8080/"
    );
}

#[test]
fn preprocess_proxy_value_preserves_path() {
    assert_eq!(
        preprocess_proxy_value("http://localhost:8080/proxy", Some("host.docker.internal")).value,
        "http://host.docker.internal:8080/proxy"
    );
}

/// Linux included, which is the change: it used to answer `None` there, so
/// a `localhost` proxy URL travelled into the container verbatim and named
/// the container itself. The name resolves there because the run adds a
/// `HostGateway`, not because the daemon supplies it.
#[test]
fn a_docker_host_name_is_available_on_every_platform() {
    assert_eq!(docker_host_name(), Some("host.docker.internal"));
}

#[test]
fn a_rewritten_url_asks_for_the_host_gateway_and_reports_its_port() {
    let host_env = host_env_with(&[("http_proxy", "http://localhost:3333")]);
    let proxy = proxy_environment_variables(host_env, &BTreeSet::new());

    assert_eq!(
        proxy.host_gateway(),
        Some(HostGateway {
            name: "host.docker.internal",
            address: "host-gateway",
        })
    );
    assert_eq!(proxy.rewritten_ports(), &BTreeSet::from([3333]));
}

/// The port is what the loopback check needs, and a URL naming no port is
/// still on one — the scheme's default.
#[test]
fn a_rewritten_url_without_a_port_reports_its_schemes_default() {
    let host_env = host_env_with(&[
        ("http_proxy", "http://localhost"),
        ("https_proxy", "https://localhost"),
    ]);
    let proxy = proxy_environment_variables(host_env, &BTreeSet::new());

    assert_eq!(proxy.rewritten_ports(), &BTreeSet::from([80, 443]));
}

/// A proxy that already names a routable host needs nothing added: injecting
/// `host.docker.internal` into every container regardless would put a name
/// there that nothing in the run asked for.
#[test]
fn a_proxy_that_was_not_rewritten_asks_for_no_host_gateway() {
    let host_env = host_env_with(&[("http_proxy", "http://proxy.example.com:8080")]);
    let proxy = proxy_environment_variables(host_env, &BTreeSet::new());

    assert_eq!(proxy.host_gateway(), None);
    assert!(proxy.rewritten_ports().is_empty());
}

/// The form Docker takes on both endpoints. Pinned here because the build
/// paths that use it have no unit test of their own — only the end-to-end
/// one, which needs a real daemon.
#[test]
fn a_host_gateway_spells_itself_the_way_docker_reads_it() {
    let gateway = HostGateway {
        name: "host.docker.internal",
        address: "host-gateway",
    };

    assert_eq!(gateway.extra_host(), "host.docker.internal:host-gateway");
}

#[test]
fn no_proxy_vars_at_all_ask_for_no_host_gateway() {
    let proxy = proxy_environment_variables(no_host_env, &BTreeSet::new());

    assert_eq!(proxy.host_gateway(), None);
}

/// `no_proxy` is never preprocessed (it holds hostnames, not a URL), so a
/// `localhost` entry in it must not be read as a rewrite that happened.
#[test]
fn a_localhost_entry_in_no_proxy_is_not_a_rewrite() {
    let host_env = host_env_with(&[
        ("http_proxy", "http://proxy.example.com:8080"),
        ("no_proxy", "localhost"),
    ]);
    let proxy = proxy_environment_variables(host_env, &BTreeSet::new());

    assert_eq!(proxy.host_gateway(), None);
}

/// A `/proc/net/tcp` table listening on `port` at `address`, with the header
/// line the kernel writes and a connected socket on the same port that must
/// not be read as a listener.
fn proc_net_tcp(address: &str, port: u16) -> String {
    format!(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
            0: {address}:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 34567 1 0000000000000000 100 0 0 10 0\n\
            1: {address}:{port:04X} 0100007F:C001 01 00000000:00000000 00:00000000 00000000  1000        0 34568 1 0000000000000000 20 4 30 10 -1\n"
    )
}

#[test]
fn a_port_bound_to_ipv4_loopback_only_is_reported() {
    let tables = vec![proc_net_tcp("0100007F", 3333)];

    assert_eq!(
        loopback_only_ports_in(&tables, &BTreeSet::from([3333])),
        BTreeSet::from([3333])
    );
}

#[test]
fn a_port_bound_to_all_ipv4_addresses_is_not_reported() {
    let tables = vec![proc_net_tcp("00000000", 3333)];

    assert!(loopback_only_ports_in(&tables, &BTreeSet::from([3333])).is_empty());
}

/// One reachable binding is enough — a proxy listening on both `127.0.0.1`
/// and `0.0.0.0` is reachable, and warning about it would be the generic
/// caveat this check exists to avoid.
#[test]
fn a_port_bound_to_both_loopback_and_everything_is_not_reported() {
    let tables = vec![
        proc_net_tcp("0100007F", 3333),
        proc_net_tcp("00000000", 3333),
    ];

    assert!(loopback_only_ports_in(&tables, &BTreeSet::from([3333])).is_empty());
}

#[test]
fn ipv6_loopback_is_reported() {
    let tables = vec![proc_net_tcp("00000000000000000000000001000000", 3333)];

    assert_eq!(
        loopback_only_ports_in(&tables, &BTreeSet::from([3333])),
        BTreeSet::from([3333])
    );
}

/// A socket bound to `127.0.0.1` through the IPv6 stack appears in
/// `/proc/net/tcp6` as an IPv4-mapped address, which `Ipv6Addr::is_loopback`
/// answers `false` for — so it has to be unmapped before the question is
/// asked, or the commonest binding of all reads as reachable.
#[test]
fn an_ipv4_mapped_loopback_address_in_tcp6_is_reported() {
    let tables = vec![proc_net_tcp("0000000000000000FFFF00000100007F", 3333)];

    assert_eq!(
        loopback_only_ports_in(&tables, &BTreeSet::from([3333])),
        BTreeSet::from([3333])
    );
}

#[test]
fn an_ipv6_wildcard_binding_is_not_reported() {
    let tables = vec![proc_net_tcp("00000000000000000000000000000000", 3333)];

    assert!(loopback_only_ports_in(&tables, &BTreeSet::from([3333])).is_empty());
}

/// Nothing listening is a proxy that isn't running — a different complaint,
/// and not this check's. Pinned because `Iterator::all` answers `true` for
/// an empty iterator, so the obvious implementation reports every unused
/// port as loopback-bound.
#[test]
fn a_port_nothing_is_listening_on_is_not_reported() {
    let tables = vec![proc_net_tcp("0100007F", 3333)];

    assert!(loopback_only_ports_in(&tables, &BTreeSet::from([8080])).is_empty());
}

#[test]
fn ports_are_checked_across_both_the_ipv4_and_ipv6_tables() {
    let tables = vec![
        proc_net_tcp("0100007F", 3333),
        proc_net_tcp("00000000000000000000000001000000", 8080),
    ];

    assert_eq!(
        loopback_only_ports_in(&tables, &BTreeSet::from([3333, 8080])),
        BTreeSet::from([3333, 8080])
    );
}
