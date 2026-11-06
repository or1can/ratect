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
//! [`crate::user`]; `HOME` and the process user are
//! `container_spec::derive_spec`'s, since they are part of what the container is
//! created with.
//!
//! On a rootful Linux daemon the process runs as the host user, and the image's
//! identity files are made to agree. On a Desktop or remapped daemon, Ratect sets
//! no process user, and only the running user's home moves. Additional users are
//! added on both. Three things shape the rest. An upload rewrites the owner and mode of whatever it lands on,
//! through a bind mount and onto the host as well, so nothing existing goes into
//! one, except for the two cases that ask for exactly that
//! (`hand_over_directories_made_for_mounts`, `claim_caches`). The identity files are
//! whatever the image shipped, so they are checked before they are read. And the
//! first archive call makes the daemon create each bind mount's and volume's
//! destination and its parents as root, before anything here can look, so a
//! directory found isn't always one the image shipped.
//!
//! Failures here name the configured container (#345), since this layer's
//! runtime calls don't. Only the rootful half has a real daemon behind it in CI:
//! see the note above these tests in `engine_tests.rs`.

use crate::container_spec::{BindSource, ContainerSpec, IdentityPlan};
use crate::daemon_class::DaemonClass;
use crate::diagnostics::REMAPPED_MOUNT_ACCESS;
use crate::docker::{ContainerFile, ContainerPathKind, ContainerRuntime, CreatedContainerFacts};
use crate::ui::printable;
use crate::user::{
    edit_group_for_host_user, edit_passwd_for_host_user, edit_passwd_home, with_additional_groups,
    with_additional_users, CurrentUser, DirectoryOwner,
};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;

/// The identity file [`apply`] always edits.
const PASSWD_PATH: &str = "/etc/passwd";

/// The identity file [`apply`] edits only for a host user or additional users.
const GROUP_PATH: &str = "/etc/group";

/// The largest identity file that is read: the file is the image's, so untrusted,
/// and this is far past any real one.
const MAX_IDENTITY_FILE_SIZE: u64 = 1024 * 1024;

fn failed_to_apply(name: &str) -> String {
    format!("Failed to apply 'identity' to container '{name}'")
}

/// What a run says about `name`, a container with `identity` whose image runs it
/// as `image_user` (`Config.User`), on a daemon of `class`. `doctor` has no
/// equivalent, since the image's user isn't in the configuration and the image may
/// not be local.
pub(crate) fn image_user_warning(
    name: &str,
    image_user: &str,
    class: DaemonClass,
) -> Option<String> {
    (class.is_remapped() && !crate::user::is_root_user(image_user)).then(|| {
        format!(
            "Container '{name}' has 'identity', and its image runs it as the non-root user \
             '{image_user}' on a {class} daemon — {REMAPPED_MOUNT_ACCESS}."
        )
    })
}

/// The user `name`'s `identity` makes the process run as: the host's, on a rootful
/// Linux daemon, and on any other class nobody, since there the host's ownership
/// already holds or can't be had. Asked before this container's image is pulled
/// or the container created, so a client with no host user to adopt finds out
/// first.
pub async fn host_user(docker: &impl ContainerRuntime, name: &str) -> Result<Option<CurrentUser>> {
    let class = docker
        .daemon_class()
        .await
        .with_context(|| failed_to_apply(name))?;
    if class != DaemonClass::RootfulLinux {
        return Ok(None);
    }
    crate::user::current_user("'identity' on a rootful Linux daemon")
        .map(Some)
        .with_context(|| failed_to_apply(name))
}

/// Applies `spec`'s identity, if it has one, to `container_id`, just created: the
/// running user's passwd entry gets the home, and that home is created if nothing
/// is there. A host user also gets a group entry if there is room for one, and
/// each writable named cache it can't otherwise use. Additional users get their
/// entries on every class, except where a named running user has none and the
/// container won't start anyway. Part of what makes the container startable, so the caller treats a failure as a failed create.
///
/// `warn` is told what `image_user_warning` finds as soon as the image's user
/// is known, so the warning doesn't depend on anything here going on to succeed.
pub async fn apply(
    docker: &impl ContainerRuntime,
    spec: &ContainerSpec,
    container_id: &str,
    warn: impl Fn(String) + Send,
) -> Result<()> {
    let Some(identity) = &spec.shared.identity else {
        return Ok(());
    };
    apply_plan(docker, spec, container_id, identity, warn)
        .await
        .with_context(|| failed_to_apply(&spec.shared.name))
}

async fn apply_plan(
    docker: &impl ContainerRuntime,
    spec: &ContainerSpec,
    container_id: &str,
    identity: &IdentityPlan,
    warn: impl Fn(String) + Send,
) -> Result<()> {
    let facts = docker.created_container_facts(container_id).await?;
    if identity.host_user.is_none() {
        let class = docker.daemon_class().await?;
        if let Some(warning) = image_user_warning(&spec.shared.name, &facts.user, class) {
            warn(warning);
        }
    }
    let passwd = read_identity_file(docker, container_id, &facts, PASSWD_PATH).await?;
    if let Some(user) = &identity.host_user {
        return adopt_host_user(docker, spec, container_id, identity, user, &facts, passwd).await;
    }
    let edit = edit_passwd_home(&passwd.contents, &facts.user, &identity.home);
    // A named user the image lacks can't start, and the runtime reports that
    // when it tries.
    let Some(owner) = edit.home_owner else {
        return Ok(());
    };
    let contents = with_additional_users(&edit.contents, &identity.additional_users)?;
    // Only additional users have a reason to touch this file here, and an image's
    // odd one shouldn't fail a container that has none.
    let group = if identity.additional_users.is_empty() {
        None
    } else {
        Some(read_identity_file(docker, container_id, &facts, GROUP_PATH).await?)
    };
    docker
        .write_container_file(
            container_id,
            PASSWD_PATH,
            &ContainerFile { contents, ..passwd },
        )
        .await?;
    if let Some(group) = group {
        let contents = with_additional_groups(&group.contents, &identity.additional_users);
        write_group_if_changed(docker, container_id, group, contents).await?;
    }
    prepare_home(docker, container_id, &identity.home, owner, &facts).await
}

/// Makes the container's identity files agree with `user`, whom it runs as, and
/// gets them and its caches ready for that user. Nothing is written until both
/// files have passed [`read_identity_file`].
async fn adopt_host_user(
    docker: &impl ContainerRuntime,
    spec: &ContainerSpec,
    container_id: &str,
    identity: &IdentityPlan,
    user: &CurrentUser,
    facts: &CreatedContainerFacts,
    passwd: ContainerFile,
) -> Result<()> {
    let group = read_identity_file(docker, container_id, facts, GROUP_PATH).await?;
    let contents = with_additional_users(
        &edit_passwd_for_host_user(&passwd.contents, user, &identity.home),
        &identity.additional_users,
    )?;
    docker
        .write_container_file(
            container_id,
            PASSWD_PATH,
            &ContainerFile { contents, ..passwd },
        )
        .await?;
    let contents = with_additional_groups(
        &edit_group_for_host_user(&group.contents, user).unwrap_or_else(|| group.contents.clone()),
        &identity.additional_users,
    );
    write_group_if_changed(docker, container_id, group, contents).await?;
    let owner = DirectoryOwner {
        uid: user.uid,
        gid: user.gid,
    };
    prepare_home(docker, container_id, &identity.home, owner, facts).await?;
    claim_caches(docker, spec, container_id, owner).await
}

/// Writes `contents` over `group`, the file they were made from, unless they are
/// what it already holds.
async fn write_group_if_changed(
    docker: &impl ContainerRuntime,
    container_id: &str,
    group: ContainerFile,
    contents: Vec<u8>,
) -> Result<()> {
    if contents == group.contents {
        return Ok(());
    }
    docker
        .write_container_file(
            container_id,
            GROUP_PATH,
            &ContainerFile { contents, ..group },
        )
        .await
}

/// Takes ownership of each writable named volume whose root is root's, keeping
/// its mode, and leaving what is in it alone: a fresh one is, and `owner` couldn't
/// write to it. Ratect chose this
/// user, so it owes it a usable cache. A volume another uid owns is left alone,
/// since a second uid gains nothing from a first's cache however it is arranged,
/// and a directory cache is already the host user's, whose own process created
/// it. A host root has nothing to claim: root can already write to all of them.
async fn claim_caches(
    docker: &impl ContainerRuntime,
    spec: &ContainerSpec,
    container_id: &str,
    owner: DirectoryOwner,
) -> Result<()> {
    if owner.uid == 0 {
        return Ok(());
    }
    for mount in spec.shared.volumes.iter().flatten() {
        if !matches!(mount.source, BindSource::Volume(_))
            || crate::container_spec::is_read_only(&mount.options)
        {
            continue;
        }
        // The upload names the mount by this path, so it is spelled as Docker
        // spells a mount's destination.
        let path = path_clean::clean(&mount.container);
        let path = path.to_string_lossy();
        let ownership = docker
            .container_directory_ownership(container_id, &path)
            .await?;
        if ownership.uid == 0 {
            docker
                .set_container_directory_ownership(container_id, &path, owner, ownership.mode)
                .await?;
        }
    }
    Ok(())
}

/// The container's identity file at `path`, an absent one (`scratch`) counting as
/// an empty one owned as a real one would be. One this edit would write through
/// to somewhere else, or that isn't plausibly such a file, is refused.
async fn read_identity_file(
    docker: &impl ContainerRuntime,
    container_id: &str,
    facts: &CreatedContainerFacts,
    path: &str,
) -> Result<ContainerFile> {
    // A mount at `/etc` would take the write to the host just the same.
    if let Some(mount) = facts
        .mount_destinations
        .iter()
        .chain(&facts.tmpfs_destinations)
        .find(|destination| {
            let destination = Path::new(destination);
            destination == Path::new(path) || destination == Path::new("/etc")
        })
    {
        anyhow::bail!(
            "{path} can't be edited: there is a mount at '{}'",
            printable(mount)
        );
    }
    let Some(stat) = docker.stat_container_path(container_id, path).await? else {
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
            anyhow::bail!("{path} can't be edited: it is a symbolic link")
        }
        ContainerPathKind::Directory | ContainerPathKind::Other => {
            anyhow::bail!("{path} can't be edited: it is not a regular file")
        }
    }
    anyhow::ensure!(
        stat.size <= MAX_IDENTITY_FILE_SIZE,
        "{path} can't be edited: it is over 1 MiB"
    );
    docker.read_container_file(container_id, path).await
}

/// Creates `home` only if nothing is there, owned by `owner`, with any missing
/// ancestors as root. A directory or symbolic link at `home` is the project's
/// choice and is left alone, even if the running user can't write to it, unless
/// it was only made for a mount (`hand_over_directories_made_for_mounts`). A symbolic
/// link on the way there is a directory to upload into, which Docker follows
/// inside the container.
///
/// Looks from the top down, since a daemon answers a stat of a path under a file
/// with a bare 500.
async fn prepare_home(
    docker: &impl ContainerRuntime,
    container_id: &str,
    home: &str,
    owner: DirectoryOwner,
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
                hand_over_directories_made_for_mounts(docker, container_id, home, owner, facts)
                    .await
            } else {
                Ok(())
            };
        }
        into = path;
    }
    // A `tmpfs` above the home hides what is created, unless a deeper mount, which
    // goes on top of it at start, is where that lands, or one at the home is the home.
    let deepest_tmpfs = facts
        .tmpfs_destinations
        .iter()
        .map(Path::new)
        .filter(|tmpfs| home_path.starts_with(tmpfs))
        .max_by_key(|tmpfs| tmpfs.components().count());
    if let Some(tmpfs) = deepest_tmpfs {
        let hides_nothing = tmpfs == home_path
            || facts
                .mount_destinations
                .iter()
                .map(Path::new)
                .any(|mount| home_path.starts_with(mount) && mount.starts_with(tmpfs));
        anyhow::ensure!(
            hides_nothing,
            "'{}' can't be created: the tmpfs at '{}' would hide it",
            printable(home),
            printable(&tmpfs.to_string_lossy())
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

/// A mount below `home` makes the daemon create `home`, and anything between the
/// two, as root, and a non-root running user then can't write to its own home, or
/// beside its own cache in `~/.cargo`. Those directories look like any others by
/// now, so this goes by what is observable: each is on the way from `home` to a
/// mount, is a directory, and has root as its owner, which hands it to a non-root
/// running user, keeping its mode. A root-owned one the image shipped is handed
/// over too, which is what naming this path as the user's home asks for. A directory
/// at or inside a mount is left alone, since that is the mount's and may be the
/// host's, so this stays in the container's layer.
async fn hand_over_directories_made_for_mounts(
    docker: &impl ContainerRuntime,
    container_id: &str,
    home: &str,
    owner: DirectoryOwner,
    facts: &CreatedContainerFacts,
) -> Result<()> {
    if owner.uid == 0 {
        return Ok(());
    }
    let home_path = Path::new(home);
    // Sorted, so from the top down: see `prepare_home`.
    let on_the_way: BTreeSet<&Path> = facts
        .mount_destinations
        .iter()
        .flat_map(|mount| Path::new(mount).ancestors().skip(1))
        .filter(|directory| directory.starts_with(home_path))
        .collect();
    for directory in on_the_way {
        let in_a_mount = facts
            .mount_destinations
            .iter()
            .chain(&facts.tmpfs_destinations)
            .any(|mount| directory.starts_with(mount));
        if in_a_mount {
            continue;
        }
        let path = directory.to_string_lossy();
        // An upload would replace a symbolic link with a directory.
        let is_directory = docker
            .stat_container_path(container_id, &path)
            .await?
            .is_some_and(|stat| stat.kind == ContainerPathKind::Directory);
        if !is_directory {
            continue;
        }
        let ownership = docker
            .container_directory_ownership(container_id, &path)
            .await?;
        if ownership.uid == 0 {
            docker
                .set_container_directory_ownership(container_id, &path, owner, ownership.mode)
                .await?;
        }
    }
    Ok(())
}
