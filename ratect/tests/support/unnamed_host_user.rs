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

//! Running a Ratect binary as a host user with no passwd or group entry
//! (decisions/0024), the case CI meets running Ratect in a container under
//! `docker run -u $(id -u):$(id -g)`. Shared by `ratect`'s and `ratect-compat`'s
//! `tests/cli.rs`, which each include it by path, since they are separate test
//! crates.
//!
//! The binary runs as uid and gid [`ID`] through `setpriv`, which needs root, so
//! as root or with passwordless `sudo` (which CI's runner has). It keeps the
//! daemon socket's group to reach the daemon, and runs with a cleared
//! environment, so it reaches the default local socket whatever the test's own
//! `docker` uses: the harness skips unless that is the same daemon, a rootful
//! Linux one. The binary and project are copied under `/tmp`, since the ids
//! can't be assumed to reach the checkout or `TMPDIR`, readable but not writable
//! by them. Where the host can't arrange any of that, a test skips, unless
//! [`REQUIRED`] is set, as CI's `docker-integration` job sets it, which makes the
//! skip a failure.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The uid and gid the binary runs as, which no passwd or group entry names.
pub const ID: &str = "4242";

const SOCKET: &str = "/var/run/docker.sock";

/// Set to make a skip fail the test, where the host is known to arrange it.
pub const REQUIRED: &str = "RATECT_REQUIRE_UNNAMED_HOST_USER";

/// Whether the binary has a home: `HOME` set to the project's directory, or no
/// `HOME` at all, which with no passwd entry is no home.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Home {
    Project,
    None,
}

/// Runs `binary` with `args`, as [`ID`], in a fresh project holding `files`
/// (path within the project, contents), with `-f ratect.toml` or `-f batect.yml`
/// left to `args`. `None`, saying why on stderr, where this host can't arrange it.
pub fn run(binary: &Path, files: &[(&str, &str)], home: Home, args: &[&str]) -> Option<Output> {
    let mut setpriv = setpriv()?;
    let Ok(socket) = std::fs::metadata(SOCKET) else {
        return skip(&format!("{SOCKET} can't be read"));
    };
    let socket_group = std::os::unix::fs::MetadataExt::gid(&socket);

    let directory = Path::new("/tmp").join(format!(
        "ratect-unnamed-host-user-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let _removed = RemoveOnDrop(directory.clone());
    let copy = directory.join(binary.file_name().unwrap());
    std::fs::copy(binary, &copy).unwrap();
    for (path, contents) in files {
        std::fs::write(directory.join(path), contents).unwrap();
    }
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        let mode = if path == copy { 0o755 } else { 0o644 };
        set_mode(&path, mode);
    }
    set_mode(&directory, 0o755);

    setpriv
        .args(["--reuid", ID, "--regid", ID, "--groups"])
        .arg(socket_group.to_string())
        .args(["--", "env", "-i", "PATH=/usr/bin:/bin"]);
    if home == Home::Project {
        setpriv.arg(format!("HOME={}", directory.display()));
    }
    let output = setpriv
        .arg(&copy)
        .current_dir(&directory)
        .args(args)
        .output()
        .expect("failed to run setpriv");
    Some(output)
}

/// Says why the harness can't run here, and fails the test instead where
/// [`REQUIRED`] is set.
fn skip<T>(why: &str) -> Option<T> {
    assert!(
        std::env::var_os(REQUIRED).is_none(),
        "{REQUIRED} is set, but {why}"
    );
    eprintln!("skipped: {why}");
    None
}

/// `setpriv`, as root, or `None` where neither the test nor passwordless `sudo`
/// is root, or the daemon isn't the one the harness can reach.
fn setpriv() -> Option<Command> {
    let info = Command::new("docker")
        .args(["info", "--format", "{{.SecurityOptions}}"])
        .output();
    let Some(info) = info.ok().filter(|info| info.status.success()) else {
        return skip("'docker info' failed");
    };
    let security_options = String::from_utf8_lossy(&info.stdout);
    if !cfg!(target_os = "linux")
        || security_options.contains("rootless")
        || security_options.contains("userns")
    {
        return skip("needs a rootful Linux daemon");
    }
    let context = Command::new("docker")
        .args(["context", "show"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    if ["DOCKER_HOST", "DOCKER_CONTEXT"]
        .iter()
        .any(|variable| std::env::var_os(variable).is_some())
        || context.as_deref().ok() != Some("default")
        || !Path::new(SOCKET).exists()
    {
        return skip(&format!("needs the default context's daemon, on {SOCKET}"));
    }
    for database in ["passwd", "group"] {
        let Ok(found) = Command::new("getent").args([database, ID]).output() else {
            return skip("'getent' can't be run");
        };
        if found.status.success() {
            return skip(&format!("{database} has an entry for {ID}"));
        }
    }
    let root = Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "0");
    if root {
        return Some(Command::new("setpriv"));
    }
    let sudo = Command::new("sudo")
        .args(["-n", "true"])
        .status()
        .is_ok_and(|status| status.success());
    if !sudo {
        return skip("needs root or passwordless sudo");
    }
    let mut command = Command::new("sudo");
    command.args(["-n", "setpriv"]);
    Some(command)
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap();
}

/// Removes the project however the test ends.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
