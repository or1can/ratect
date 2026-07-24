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

//! A single real-Docker test that measures the daemon's *global* dangling
//! volume count, kept in its own test binary on purpose. It reads state
//! shared by the whole daemon (`docker volume ls -qf dangling=true`), so any
//! other test creating or removing a volume between its two measurements —
//! a redis/postgres dependency's anonymous volume, or a cache test's named
//! one going dangling on cleanup — would skew the delta and fail it
//! spuriously (a real, timing-dependent CI flake). libtest parallelises
//! tests *within* a binary but Cargo runs the binaries themselves one at a
//! time, so isolating this test in its own file gives it exclusive daemon
//! access however it's invoked — no `--test-threads=1` for a contributor to
//! remember. It lives here rather than in `cli.rs` for exactly that reason.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect-compat"))
}

fn sidecar_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sidecar.yml")
}

/// Requires a running Docker daemon with network access to pull `redis:7-alpine`
/// and `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Cleanup has to take a container's *anonymous* volumes with it. `redis`
/// declares `VOLUME /data`, so each of this fixture's three redis
/// dependencies creates one per run; without `v: true` on removal (Docker's
/// own default, and Ratect's behavior before this test existed) every run
/// left three behind, permanently. They're unfixable after the fact — Docker
/// names them with a random hash and they carry no labels, since Docker
/// creates them implicitly — so not creating them is the only remedy there
/// is, and this is what proves it holds.
///
/// Deliberately measures the *delta* rather than an absolute count: a
/// developer machine (or a CI runner between jobs) has dangling volumes from
/// everything else it's ever run, and none of that is this test's business.
#[test]
#[ignore]
fn cleanup_takes_anonymous_volumes_with_it_via_docker() {
    fn dangling_volume_count() -> usize {
        let output = Command::new("docker")
            .args(["volume", "ls", "-qf", "dangling=true"])
            .output()
            .expect("failed to run docker volume ls");
        assert!(output.status.success(), "docker volume ls failed");
        String::from_utf8_lossy(&output.stdout).lines().count()
    }

    let before = dangling_volume_count();

    let output = ratect_command()
        .arg("-f")
        .arg(sidecar_config_path())
        .arg("ping-sidecars")
        .output()
        .expect("failed to run ratect");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        dangling_volume_count(),
        before,
        "the run should have left no anonymous volumes behind (three redis \
         dependencies, each declaring VOLUME /data)"
    );
}
