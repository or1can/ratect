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

//! Which kind of Docker daemon Ratect is talking to —
//! [ADR 0011](../../decisions/0011-container-identity.md)'s daemon
//! classification, as a pure function so each class is testable without a
//! daemon. [`ContainerRuntime::daemon_class`](crate::docker::ContainerRuntime::daemon_class)
//! is the one place a caller gets a class from; nothing else re-derives one.
//!
//! **Desktop is a heuristic, and owned as one.** OrbStack, Docker Desktop and
//! rootful Ubuntu report identical `SecurityOptions`, so no daemon-side signal
//! short of a vendor list tells them apart. Ratect's own host plus the transport covers colima and
//! future runtimes without a vendor list, and leaves a remote daemon reached
//! from a Mac to what it reports about itself. It also leaves Docker Desktop on
//! Linux as rootful Linux, deliberately: that is a VM with file sharing too, but
//! only vendor-specific signals identify it (#817).

use std::fmt;

/// A class of Docker daemon. See [`classify`] for what makes each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonClass {
    Rootless,
    UsernsRemap,
    Desktop,
    RootfulLinux,
}

impl fmt::Display for DaemonClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rootless => "rootless",
            Self::UsernsRemap => "userns-remap",
            Self::Desktop => "desktop",
            Self::RootfulLinux => "rootful Linux",
        })
    }
}

impl DaemonClass {
    /// Whether container uids map to other uids on the host, as they do on
    /// [`Rootless`](Self::Rootless) and [`UsernsRemap`](Self::UsernsRemap).
    pub fn is_remapped(self) -> bool {
        matches!(self, Self::Rootless | Self::UsernsRemap)
    }
}

/// How a connection reaches its daemon, as far as classifying it cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    LocalSocket,
    Network,
}

impl Transport {
    /// The transport of a daemon host as the Docker CLI spells it. Only
    /// `unix://` and `npipe://` are local sockets: a `tcp://` host is
    /// [`Network`](Self::Network) even when it names this machine.
    pub fn of_host(host: &str) -> Self {
        if host.starts_with("unix://") || host.starts_with("npipe://") {
            Self::LocalSocket
        } else {
            Self::Network
        }
    }
}

/// The class of a daemon reporting `security_options`, reached over
/// `transport` from a host whose [`std::env::consts::OS`] is `host_os`.
///
/// What the daemon reports wins over where it is: `name=rootless` first, then
/// `name=userns`, each matched against a whole entry, since moby emits both
/// bare. Only then Desktop (macOS or Windows, over a local socket), and
/// [`RootfulLinux`](DaemonClass::RootfulLinux) for the remainder, which includes
/// Docker Desktop on Linux by decision.
pub fn classify(security_options: &[String], host_os: &str, transport: Transport) -> DaemonClass {
    let reports = |entry: &str| security_options.iter().any(|option| option == entry);
    if reports("name=rootless") {
        DaemonClass::Rootless
    } else if reports("name=userns") {
        DaemonClass::UsernsRemap
    } else if matches!(host_os, "macos" | "windows") && transport == Transport::LocalSocket {
        DaemonClass::Desktop
    } else {
        DaemonClass::RootfulLinux
    }
}

#[cfg(test)]
#[path = "daemon_class_tests.rs"]
mod tests;
