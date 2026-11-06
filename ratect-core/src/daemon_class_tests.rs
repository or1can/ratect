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

fn options(entries: &[&str]) -> Vec<String> {
    entries.iter().map(ToString::to_string).collect()
}

/// A rootful daemon's options as moby reports them, `,profile=` suffix
/// included (`docker info` against Docker 29).
const ROOTFUL: &[&str] = &["name=seccomp,profile=builtin", "name=cgroupns"];

#[test]
fn name_rootless_is_rootless() {
    let reported = options(&["name=seccomp,profile=builtin", "name=rootless"]);
    assert_eq!(
        classify(&reported, "linux", Transport::LocalSocket),
        DaemonClass::Rootless
    );
}

#[test]
fn name_userns_is_userns_remap() {
    let reported = options(&["name=seccomp,profile=builtin", "name=userns"]);
    assert_eq!(
        classify(&reported, "linux", Transport::LocalSocket),
        DaemonClass::UsernsRemap
    );
}

/// ADR 0011's order decides a daemon reporting both.
#[test]
fn rootless_wins_over_userns() {
    let reported = options(&["name=userns", "name=rootless"]);
    assert_eq!(
        classify(&reported, "linux", Transport::LocalSocket),
        DaemonClass::Rootless
    );
}

/// Exact entries: a suffix, or a longer name, is a different entry.
#[test]
fn only_a_whole_entry_matches() {
    for near_miss in [
        "name=rootless,profile=builtin",
        "name=userns,profile=builtin",
        "name=rootlessly",
        "name=userns-remap",
        "rootless",
        "userns",
    ] {
        assert_eq!(
            classify(&options(&[near_miss]), "linux", Transport::LocalSocket),
            DaemonClass::RootfulLinux,
            "{near_miss}"
        );
    }
}

#[test]
fn a_local_socket_from_macos_or_windows_is_desktop() {
    for host_os in ["macos", "windows"] {
        assert_eq!(
            classify(&options(ROOTFUL), host_os, Transport::LocalSocket),
            DaemonClass::Desktop,
            "{host_os}"
        );
    }
}

/// ADR 0011: a remote daemon reached from a Mac falls through to what it
/// reports about itself.
#[test]
fn a_network_daemon_from_macos_or_windows_is_classified_by_what_it_reports() {
    for host_os in ["macos", "windows"] {
        assert_eq!(
            classify(&options(ROOTFUL), host_os, Transport::Network),
            DaemonClass::RootfulLinux,
            "{host_os}"
        );
        assert_eq!(
            classify(&options(&["name=rootless"]), host_os, Transport::Network),
            DaemonClass::Rootless,
            "{host_os}"
        );
        assert_eq!(
            classify(&options(&["name=userns"]), host_os, Transport::Network),
            DaemonClass::UsernsRemap,
            "{host_os}"
        );
    }
}

/// What a daemon reports wins over the desktop heuristic.
#[test]
fn a_remapped_daemon_on_a_local_socket_from_macos_is_not_desktop() {
    assert_eq!(
        classify(
            &options(&["name=rootless"]),
            "macos",
            Transport::LocalSocket
        ),
        DaemonClass::Rootless
    );
    assert_eq!(
        classify(&options(&["name=userns"]), "macos", Transport::LocalSocket),
        DaemonClass::UsernsRemap
    );
}

#[test]
fn a_rootful_daemon_on_linux_is_rootful_linux_over_either_transport() {
    for transport in [Transport::LocalSocket, Transport::Network] {
        assert_eq!(
            classify(&options(ROOTFUL), "linux", transport),
            DaemonClass::RootfulLinux,
            "{transport:?}"
        );
    }
}

/// #817: Docker Desktop on Linux stays rootful Linux by decision, though it is a
/// VM with file sharing as Desktop is. `ROOTFUL` is exactly what it reported when
/// probed (ADR 0011).
#[test]
fn docker_desktop_on_linux_is_rootful_linux() {
    assert_eq!(
        classify(&options(ROOTFUL), "linux", Transport::LocalSocket),
        DaemonClass::RootfulLinux
    );
}

#[test]
fn a_daemon_reporting_no_options_is_classified_by_host_and_transport() {
    assert_eq!(
        classify(&[], "linux", Transport::LocalSocket),
        DaemonClass::RootfulLinux
    );
    assert_eq!(
        classify(&[], "macos", Transport::LocalSocket),
        DaemonClass::Desktop
    );
}

#[test]
fn only_unix_and_npipe_hosts_are_local_sockets() {
    for host in [
        "unix:///var/run/docker.sock",
        "unix:///Users/me/.orbstack/run/docker.sock",
        "npipe:////./pipe/docker_engine",
    ] {
        assert_eq!(Transport::of_host(host), Transport::LocalSocket, "{host}");
    }
    for host in [
        "tcp://127.0.0.1:2375",
        "tcp://build.example.com:2376",
        "http://build.example.com:2375",
        "https://build.example.com:2376",
        "ssh://me@build.example.com",
    ] {
        assert_eq!(Transport::of_host(host), Transport::Network, "{host}");
    }
}

#[test]
fn each_class_has_the_name_doctor_prints() {
    assert_eq!(DaemonClass::Rootless.to_string(), "rootless");
    assert_eq!(DaemonClass::UsernsRemap.to_string(), "userns-remap");
    assert_eq!(DaemonClass::Desktop.to_string(), "desktop");
    assert_eq!(DaemonClass::RootfulLinux.to_string(), "rootful Linux");
}
