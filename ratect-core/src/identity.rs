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

//! What a container's `identity` does to the container once it exists
//! ([decisions/0011](../../decisions/0011-container-identity.md)), through the
//! [`ContainerRuntime`] calls that read and write a created container. The
//! editing itself is pure and lives with the other passwd code, in
//! [`crate::user::edit_passwd_home`]; `HOME` is
//! `container_spec::derive_spec`'s, since it is part of what the container is
//! created with.
//!
//! On a Desktop or remapped daemon, Ratect sets no process user. Three things
//! shape the rest. An upload rewrites the owner and mode of whatever it lands on,
//! through a bind mount and onto the host as well, so nothing existing goes into
//! one, with one exception (`hand_over_home_made_for_a_mount`). `/etc/passwd` is
//! whatever the image shipped, so it is checked before it is read. And the first
//! archive call makes the daemon create each bind mount's and volume's destination
//! and its parents as root, before anything here can look, so a directory found
//! isn't always one the image shipped.
//!
//! Failures here name the configured container (#345), since this layer's
//! runtime calls don't. CI can't reach any of this against a real daemon: see the
//! note above these tests in `engine_tests.rs`.

use crate::container_spec::{ContainerSpec, IdentityPlan};
use crate::daemon_class::DaemonClass;
use crate::docker::{ContainerFile, ContainerPathKind, ContainerRuntime, CreatedContainerFacts};
use crate::ui::printable;
use crate::user::{edit_passwd_home, HomeOwner};
use anyhow::{Context, Result};
use std::path::Path;

/// The identity file [`apply`] edits.
const PASSWD_PATH: &str = "/etc/passwd";

/// The largest [`PASSWD_PATH`] that is read: the file is the image's, so
/// untrusted, and this is far past any real one.
const MAX_PASSWD_SIZE: u64 = 1024 * 1024;

fn failed_to_apply(name: &str) -> String {
    format!("Failed to apply 'identity' to container '{name}'")
}

/// Refuses `identity` on a daemon class it can't serve yet, before this
/// container's image is pulled or the container created: rootful Linux needs
/// the process user and ownership half of decisions/0011, which #236 delivers.
pub async fn check_supported(docker: &impl ContainerRuntime, name: &str) -> Result<()> {
    let class = docker
        .daemon_class()
        .await
        .with_context(|| failed_to_apply(name))?;
    anyhow::ensure!(
        class != DaemonClass::RootfulLinux,
        "{}: it is not supported yet on a rootful Linux daemon",
        failed_to_apply(name)
    );
    Ok(())
}

/// Applies `spec`'s identity, if it has one, to `container_id`, just created: the
/// running user's passwd entry gets the home, and that home is created if nothing
/// is there. Part of what makes the container startable, so the caller treats a
/// failure as a failed create.
pub async fn apply(
    docker: &impl ContainerRuntime,
    spec: &ContainerSpec,
    container_id: &str,
) -> Result<()> {
    let Some(identity) = &spec.shared.identity else {
        return Ok(());
    };
    apply_plan(docker, container_id, identity)
        .await
        .with_context(|| failed_to_apply(&spec.shared.name))
}

async fn apply_plan(
    docker: &impl ContainerRuntime,
    container_id: &str,
    identity: &IdentityPlan,
) -> Result<()> {
    let facts = docker.created_container_facts(container_id).await?;
    let passwd = read_passwd(docker, container_id, &facts).await?;
    let edit = edit_passwd_home(&passwd.contents, &facts.user, &identity.home);
    // A named user the image lacks can't start, and the runtime reports that
    // when it tries.
    let Some(owner) = edit.home_owner else {
        return Ok(());
    };
    docker
        .write_container_file(
            container_id,
            PASSWD_PATH,
            &ContainerFile {
                contents: edit.contents,
                ..passwd
            },
        )
        .await?;
    prepare_home(docker, container_id, &identity.home, owner, &facts).await
}

/// The container's `/etc/passwd`, an absent one (`scratch`) counting as an empty
/// one owned as a real one would be. One this edit would write through to
/// somewhere else, or that isn't plausibly a passwd file, is refused.
async fn read_passwd(
    docker: &impl ContainerRuntime,
    container_id: &str,
    facts: &CreatedContainerFacts,
) -> Result<ContainerFile> {
    // A mount at `/etc` would take the write to the host just the same.
    if let Some(mount) = facts
        .mount_destinations
        .iter()
        .chain(&facts.tmpfs_destinations)
        .find(|destination| {
            let destination = Path::new(destination);
            destination == Path::new(PASSWD_PATH) || destination == Path::new("/etc")
        })
    {
        anyhow::bail!(
            "{PASSWD_PATH} can't be edited: there is a mount at '{}'",
            printable(mount)
        );
    }
    let Some(stat) = docker
        .stat_container_path(container_id, PASSWD_PATH)
        .await?
    else {
        return Ok(ContainerFile {
            contents: Vec::new(),
            mode: 0o644,
            uid: 0,
            gid: 0,
        });
    };
    match stat.kind {
        ContainerPathKind::RegularFile => {}
        ContainerPathKind::SymbolicLink => {
            anyhow::bail!("{PASSWD_PATH} can't be edited: it is a symbolic link")
        }
        ContainerPathKind::Directory | ContainerPathKind::Other => {
            anyhow::bail!("{PASSWD_PATH} can't be edited: it is not a regular file")
        }
    }
    anyhow::ensure!(
        stat.size <= MAX_PASSWD_SIZE,
        "{PASSWD_PATH} can't be edited: it is over 1 MiB"
    );
    docker.read_container_file(container_id, PASSWD_PATH).await
}

/// Creates `home` only if nothing is there, owned by `owner`, with any missing
/// ancestors as root. A directory or symbolic link at `home` is the project's
/// choice and is left alone, even if the running user can't write to it, unless
/// it was only made for a mount (`hand_over_home_made_for_a_mount`). A symbolic
/// link on the way there is a directory to upload into, which Docker follows
/// inside the container.
///
/// Looks from the top down, since a daemon answers a stat of a path under a file
/// with a bare 500.
async fn prepare_home(
    docker: &impl ContainerRuntime,
    container_id: &str,
    home: &str,
    owner: HomeOwner,
    facts: &CreatedContainerFacts,
) -> Result<()> {
    let home_path = Path::new(home);
    let mut ancestors: Vec<&Path> = home_path.ancestors().collect();
    ancestors.reverse();
    // The deepest ancestor found so far; `/` always exists.
    let mut into = Path::new("/");
    for path in ancestors.into_iter().skip(1) {
        let Some(stat) = docker
            .stat_container_path(container_id, &path.to_string_lossy())
            .await?
        else {
            break;
        };
        if !matches!(
            stat.kind,
            ContainerPathKind::Directory | ContainerPathKind::SymbolicLink
        ) {
            anyhow::bail!(
                "'{}' can't be the home: '{}' is not a directory",
                printable(home),
                printable(&path.to_string_lossy())
            );
        }
        if path == home_path {
            return if stat.kind == ContainerPathKind::Directory {
                hand_over_home_made_for_a_mount(docker, container_id, home, owner, facts).await
            } else {
                Ok(())
            };
        }
        into = path;
    }
    // Created in the image's layer, a `tmpfs` above it would hide it at start.
    if let Some(tmpfs) = facts.tmpfs_destinations.iter().find(|tmpfs| {
        let tmpfs = Path::new(tmpfs);
        home_path != tmpfs && home_path.starts_with(tmpfs)
    }) {
        anyhow::bail!(
            "'{}' can't be created: the tmpfs at '{}' would hide it",
            printable(home),
            printable(tmpfs)
        );
    }
    let relative = home_path
        .strip_prefix(into)
        .expect("an ancestor of an absolute path is a prefix of it");
    docker
        .create_container_directories(
            container_id,
            &into.to_string_lossy(),
            &relative.to_string_lossy(),
            owner,
        )
        .await
}

/// A mount below `home` makes the daemon create `home` as root, and a non-root
/// running user then can't write to its own home. That directory looks like any
/// other by now, so this goes by what is observable: a mount below it, none at
/// it, and root as its owner, which hands it to a non-root running user. A
/// root-owned home the image shipped is handed over too, which is what naming it
/// as this user's home asks for. It is in the container's layer, not on the host,
/// since nothing is mounted at it.
async fn hand_over_home_made_for_a_mount(
    docker: &impl ContainerRuntime,
    container_id: &str,
    home: &str,
    owner: HomeOwner,
    facts: &CreatedContainerFacts,
) -> Result<()> {
    let home_path = Path::new(home);
    let mounted_at_home = facts
        .mount_destinations
        .iter()
        .chain(&facts.tmpfs_destinations)
        .any(|mount| Path::new(mount) == home_path);
    let mounted_below_home = facts.mount_destinations.iter().any(|mount| {
        let mount = Path::new(mount);
        mount != home_path && mount.starts_with(home_path)
    });
    if owner.uid == 0
        || mounted_at_home
        || !mounted_below_home
        || docker.container_directory_uid(container_id, home).await? != 0
    {
        return Ok(());
    }
    let (Some(parent), Some(leaf)) = (home_path.parent(), home_path.file_name()) else {
        return Ok(());
    };
    docker
        .create_container_directories(
            container_id,
            &parent.to_string_lossy(),
            &leaf.to_string_lossy(),
            owner,
        )
        .await
}
