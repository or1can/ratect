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

//! Resolves `cache` volume mounts ([`crate::config::CacheVolumeMount`]) to an
//! actual Docker bind-mount string — a named volume that persists between
//! separate `ratect` invocations ([`CacheType::Volume`], the default) or a
//! host directory ([`CacheType::Directory`], `--cache-type=directory`) — and
//! implements `--clean`/`--clean-cache`
//! ([`clean_volume_caches`]/[`clean_directory_caches`]), which remove them.
//!
//! Ported from Batect's own `CacheManager`/`VolumeMountResolver`/`CacheType`/
//! `CleanupCachesCommand`, and kept byte-for-byte compatible with its
//! `.batect/caches/` location and `batect-cache-<project key>-<name>` volume
//! naming *on purpose*: this is `ratect-compat`'s territory, so a project
//! migrating from real `batect` finds its existing caches reused rather than
//! orphaned.
//!
//! One deliberate divergence: a freshly generated [`project_cache_key`] is a
//! full `uuid::Uuid::new_v4()`, not Batect's 6-character `a-z0-9` id, whose
//! alphabet is meaningfully more collision-prone across many projects on one
//! machine. An existing Batect-written key file is still read and reused
//! byte-for-byte, tolerant of its `#`-comment header — nothing depends on
//! matching the *generation* format, only on the file's path and layout.
//!
//! The removal *decision* — which volumes or directories match this project,
//! restricted to `--clean-cache`'s allowlist — lives in plain synchronous
//! functions ([`matching_cache_volumes`]/[`matching_cache_directories`]),
//! deliberately separate from the async I/O around them, so it is testable
//! against plain `Vec<String>`/tempdir fixtures with no fake
//! `ContainerRuntime`.

use crate::config::CacheVolumeMount;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Where a `cache` mount's contents actually live. Selected by `--cache-type`
/// (default `Volume`), matching Batect's own `CacheType` — except Batect
/// additionally forces `Directory` for Windows containers; Ratect has no
/// Windows support to special-case yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheType {
    #[default]
    Volume,
    Directory,
}

/// Bundles the per-invocation settings [`resolve_cache_mount`] needs beyond
/// the mount itself.
#[derive(Debug, Clone)]
pub struct CacheOptions {
    pub cache_type: CacheType,
    pub project_directory: PathBuf,
}

/// The project-local directory Ratect's cache mechanism uses — `.batect/`,
/// not `.ratect/`, deliberately: this is where an existing Batect project
/// already keeps its own `key` file and any `directory`-type cache contents,
/// and reusing them (rather than starting cold under a Ratect-only
/// directory name) is the entire point of `ratect-compat`'s parity goal.
pub fn cache_directory(project_directory: &Path) -> PathBuf {
    project_directory.join(".batect").join("caches")
}

/// The per-project key embedded in every cache volume's name
/// (`batect-cache-<key>-<name>`) — without it, two unrelated projects that
/// happen to declare a same-named cache (e.g. `gradle-cache`) would collide
/// on the exact same Docker volume, since Docker volumes live in one flat,
/// global namespace, not scoped by project directory.
///
/// Reads `<project_directory>/.batect/caches/key` if it already exists —
/// tolerating Batect's own file format exactly (skip blank lines and any
/// line starting with `#`, take the one remaining line as the key,
/// mirroring `CacheManager.projectCacheKey`'s own read logic), so a project
/// already run under real Batect has its existing key discovered and
/// reused, preserving the exact volume names Batect itself would use.
///
/// When no file exists yet, generates and persists a new one: a full
/// `uuid::Uuid::new_v4()` rather than Batect's 6-char `a-z0-9` id. This
/// doesn't affect compatibility — Batect's reader has no length/charset
/// check, it just takes whatever's on that one line, so the value's shape
/// is opaque to both tools; only the file's *path* and *read-compatible
/// format* matter for interop. A full UUID is simply safer: Batect's own
/// 6-char alphabet only has ~2.18 billion combinations, meaningfully more
/// collision-prone across many projects on one machine than a UUID, with no
/// upside since nothing depends on matching that format for a freshly
/// generated key.
pub fn project_cache_key(project_directory: &Path) -> Result<String> {
    let key_path = cache_directory(project_directory).join("key");

    if let Ok(contents) = fs::read_to_string(&key_path) {
        if let Some(key) = contents
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
        {
            return Ok(key.to_string());
        }
    }

    let key = uuid::Uuid::new_v4().to_string();
    let parent = key_path
        .parent()
        .expect("cache_directory() always returns a path with a parent");
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create cache directory {parent:?}"))?;
    fs::write(
        &key_path,
        format!(
            "# This file was autogenerated to track which Docker volumes are associated with \
             this project.\n# Do not modify it, and do not commit it to source control.\n{key}\n"
        ),
    )
    .with_context(|| format!("Failed to write cache key file {key_path:?}"))?;

    Ok(key)
}

/// The Docker volume name a `cache` mount named `name` resolves to under
/// `CacheType::Volume` — `batect-cache-<project_cache_key>-<name>`, Batect's
/// own literal prefix (see the module's own doc comment for why it isn't
/// `ratect-cache-`).
pub fn cache_volume_name(project_cache_key: &str, name: &str) -> String {
    format!("batect-cache-{project_cache_key}-{name}")
}

/// The Docker volume name a **shared** cache resolves to —
/// `ratect-shared-cache-<name>`, with no project key, which is the whole
/// point: every project naming it gets the same storage.
///
/// The `ratect-` prefix is deliberate on two counts. Batect has no shared
/// cache, so there is no naming convention to stay compatible with; and
/// because it differs from [`cache_volume_name`]'s `batect-cache-` prefix,
/// [`matching_cache_volumes`] cannot match a shared cache even by accident,
/// so a bare `--clean` can never discard storage other projects are using.
pub fn shared_cache_volume_name(name: &str) -> String {
    format!("ratect-shared-cache-{name}")
}

/// The host directory a **shared** cache resolves to under
/// `CacheType::Directory` — `~/.ratect/caches/<name>`, beside
/// `~/.ratect/incl`'s Git-include clones, for the same reason: it belongs to
/// the user's machine rather than to any one project.
pub fn shared_cache_directory(name: &str) -> Result<PathBuf> {
    Ok(shared_cache_root()?.join(name))
}

/// Where every shared cache directory lives — `~/.ratect/caches`.
pub fn shared_cache_root() -> Result<PathBuf> {
    Ok(crate::user::home_directory()?
        .join(".ratect")
        .join("caches"))
}

/// Resolves `mount` to a Docker bind-mount string (`"source:container[:options]"`,
/// the same shape `docker.rs`'s `HostConfig.binds` already expects) —
/// `source` is a bare Docker volume name under `CacheType::Volume` (Docker
/// itself auto-creates a named volume on first use), or an absolute host
/// directory under `.batect/caches/<name>/` under `CacheType::Directory`
/// (created here if missing, matching Batect's own
/// `Files.createDirectories`).
pub fn resolve_cache_mount(
    options: &CacheOptions,
    project_cache_key: &str,
    mount: &CacheVolumeMount,
) -> Result<String> {
    let shared = mount.scope() == crate::config::CacheScope::Shared;
    let source = match options.cache_type {
        CacheType::Volume if shared => shared_cache_volume_name(&mount.name),
        CacheType::Volume => cache_volume_name(project_cache_key, &mount.name),
        CacheType::Directory => {
            let dir = if shared {
                shared_cache_directory(&mount.name)?
            } else {
                cache_directory(&options.project_directory).join(&mount.name)
            };
            fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create cache directory {dir:?}"))?;
            dir.display().to_string()
        }
    };

    Ok(match &mount.options {
        Some(mount_options) => format!("{source}:{}:{mount_options}", mount.container),
        None => format!("{source}:{}", mount.container),
    })
}

/// Which shared cache volumes `only` names — the removal *decision*, kept
/// synchronous and separate from the I/O around it, the same way
/// [`matching_cache_volumes`] is.
///
/// **An empty `only` matches nothing**, the opposite of the project-scoped
/// rule where empty means "all of this project's". A shared cache holds
/// storage other projects are still using, so a bare `caches clean` must
/// never reach one. Stating that as a precondition on the caller was not
/// enough — it read as satisfied and wasn't, and every shared cache on the
/// machine was swept.
fn matching_shared_cache_volumes<'a>(
    existing_volumes: &'a [String],
    only: &HashSet<String>,
) -> Vec<&'a str> {
    if only.is_empty() {
        return Vec::new();
    }
    matching_cache_volumes(existing_volumes, &shared_cache_volume_name(""), only)
}

/// Every cache volume this project can see, with the scope each one's name
/// implies — one `list_volumes` call covering both, rather than
/// [`list_volume_caches`] listing the daemon once per scope for the same
/// answer.
pub async fn list_all_volume_caches(
    runtime: &impl crate::docker::ContainerRuntime,
    project_cache_key: &str,
) -> Result<Vec<(String, crate::config::CacheScope)>> {
    use crate::config::CacheScope;

    let existing = runtime.list_volumes().await?;
    let project_prefix = cache_volume_name(project_cache_key, "");
    let shared_prefix = shared_cache_volume_name("");

    let mut found: Vec<(String, CacheScope)> = existing
        .iter()
        .filter_map(|volume| {
            if let Some(name) = volume.strip_prefix(&project_prefix) {
                Some((name.to_string(), CacheScope::Project))
            } else {
                volume
                    .strip_prefix(&shared_prefix)
                    .map(|name| (name.to_string(), CacheScope::Shared))
            }
        })
        .collect();
    found.sort();
    Ok(found)
}

/// Removes the named shared cache volumes, returning the cache names
/// actually removed.
///
/// **An empty `only` removes nothing**, the opposite of
/// [`clean_volume_caches`], where empty means "this project's, all of
/// them". A shared cache holds storage other projects are still using, so
/// sweeping them can never be the default — and stating that as a
/// precondition on the caller was not enough: it read as satisfied and
/// wasn't, so a bare `caches clean` removed every shared cache on the
/// machine. The rule lives here now, where it cannot be forgotten.
pub async fn clean_shared_volume_caches(
    runtime: &impl crate::docker::ContainerRuntime,
    only: &HashSet<String>,
) -> Result<Vec<String>> {
    let existing = runtime.list_volumes().await?;
    let matched: Vec<String> = matching_shared_cache_volumes(&existing, only)
        .into_iter()
        .map(str::to_string)
        .collect();

    for name in &matched {
        runtime.remove_volume(name).await?;
    }

    let prefix = shared_cache_volume_name("");
    Ok(matched
        .iter()
        .map(|name| name.strip_prefix(&prefix).unwrap_or(name).to_string())
        .collect())
}

/// Every **shared** cache directory, by name — the counterpart to
/// [`list_directory_caches`] under `CacheType::Directory`.
pub fn list_shared_directory_caches() -> Result<Vec<String>> {
    // A host with no passwd entry for the current uid has no home, and so
    // no shared caches — that is an empty list, not a failure. `caches
    // --cache-type directory` worked on such a host before shared caches
    // existed, and has no reason to stop.
    let Ok(root) = shared_cache_root() else {
        return Ok(Vec::new());
    };
    matching_cache_directories(&root, &HashSet::new())
}

/// Removes the named shared cache directories, returning those removed.
///
/// An empty `only` removes nothing — see
/// [`clean_shared_volume_caches`] for why that is the opposite of the
/// project-scoped rule.
pub fn clean_shared_directory_caches(only: &HashSet<String>) -> Result<Vec<String>> {
    if only.is_empty() {
        return Ok(Vec::new());
    }
    // Same tolerance as [`list_shared_directory_caches`]: no home means no
    // shared caches to remove.
    let Ok(root) = shared_cache_root() else {
        return Ok(Vec::new());
    };
    let matched = matching_cache_directories(&root, only)?;
    for name in &matched {
        let dir = root.join(name);
        fs::remove_dir_all(&dir).with_context(|| format!("Failed to remove {dir:?}"))?;
    }
    Ok(matched)
}

/// Filters `existing_volumes` (from [`crate::docker::ContainerRuntime::list_volumes`])
/// down to those whose name starts with `prefix`, further restricted to
/// `only` when non-empty (the `--clean-cache <name>` allowlist; empty means
/// "everything under this prefix").
///
/// Takes the prefix rather than a project key so both scopes share it —
/// `batect-cache-<key>-` for a project's own, `ratect-shared-cache-` for
/// the machine's. **The empty-`only` rule differs by scope and is decided by
/// the caller**: for a project cache empty means "all of them", and for a
/// shared one it must mean "none", which is why
/// [`matching_shared_cache_volumes`] guards it before delegating here.
///
/// A pure, synchronous decision function deliberately kept separate from
/// the I/O in [`clean_volume_caches`], so it's unit-testable against plain
/// `Vec<String>` fixtures without needing a fake `ContainerRuntime`.
fn matching_cache_volumes<'a>(
    existing_volumes: &'a [String],
    prefix: &str,
    only: &HashSet<String>,
) -> Vec<&'a str> {
    existing_volumes
        .iter()
        .filter_map(|name| {
            let cache_name = name.strip_prefix(prefix)?;
            (only.is_empty() || only.contains(cache_name)).then_some(name.as_str())
        })
        .collect()
}

/// This project's own existing cache volumes, by their *cache* name (what
/// a `volumes` entry calls them) rather than the prefixed Docker volume
/// name — for `ratect caches list`, which has no equivalent in
/// `ratect-compat`/Batect (both only ever offered removal).
///
/// Knowing what's there is the prerequisite for removing one by name, so
/// this is a deliberate addition rather than a parity gap. Sorted, so
/// repeated invocations agree with each other; Docker's own volume listing
/// order isn't specified.
pub async fn list_volume_caches(
    runtime: &impl crate::docker::ContainerRuntime,
    project_cache_key: &str,
) -> Result<Vec<String>> {
    let existing = runtime.list_volumes().await?;
    let prefix = cache_volume_name(project_cache_key, "");
    let mut names: Vec<String> = matching_cache_volumes(
        &existing,
        &cache_volume_name(project_cache_key, ""),
        &HashSet::new(),
    )
    .into_iter()
    .map(|volume| volume.strip_prefix(&prefix).unwrap_or(volume).to_string())
    .collect();
    names.sort();
    Ok(names)
}

/// The `CacheType::Directory` counterpart of [`list_volume_caches`] —
/// already sorted, by [`matching_cache_directories`].
pub fn list_directory_caches(project_directory: &Path) -> Result<Vec<String>> {
    matching_cache_directories(&cache_directory(project_directory), &HashSet::new())
}

/// Removes this project's own cache volumes (or, with `only` non-empty,
/// just the named ones) — `--clean`/`--clean-cache` under
/// `CacheType::Volume`. Mirrors Batect's own `CleanupCachesCommand.runForVolumes`.
/// Returns the names actually removed.
pub async fn clean_volume_caches(
    runtime: &impl crate::docker::ContainerRuntime,
    project_cache_key: &str,
    only: &HashSet<String>,
) -> Result<Vec<String>> {
    let existing = runtime.list_volumes().await?;
    let matched: Vec<String> =
        matching_cache_volumes(&existing, &cache_volume_name(project_cache_key, ""), only)
            .into_iter()
            .map(str::to_string)
            .collect();

    for name in &matched {
        runtime.remove_volume(name).await?;
    }

    Ok(matched)
}

/// The synchronous counterpart of [`matching_cache_volumes`] for
/// `CacheType::Directory`: this project's own cache directories are exactly
/// [`cache_directory`]'s own subdirectories (the `key` file living
/// alongside them is a plain file, not a directory, so it's never matched
/// here) — restricted to `only` when non-empty, same convention as above.
fn matching_cache_directories(cache_dir: &Path, only: &HashSet<String>) -> Result<Vec<String>> {
    if !cache_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut matched = Vec::new();
    for entry in fs::read_dir(cache_dir).with_context(|| format!("Failed to read {cache_dir:?}"))? {
        let entry = entry.with_context(|| format!("Failed to read an entry in {cache_dir:?}"))?;
        if !entry
            .file_type()
            .with_context(|| format!("Failed to inspect {:?}", entry.path()))?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if only.is_empty() || only.contains(&name) {
            matched.push(name);
        }
    }
    matched.sort();

    Ok(matched)
}

/// Removes this project's own cache directories (or, with `only` non-empty,
/// just the named ones) — `--clean`/`--clean-cache` under
/// `CacheType::Directory`. Mirrors Batect's own
/// `CleanupCachesCommand.runForDirectories`. Returns the names actually
/// removed.
pub fn clean_directory_caches(
    project_directory: &Path,
    only: &HashSet<String>,
) -> Result<Vec<String>> {
    let cache_dir = cache_directory(project_directory);
    let matched = matching_cache_directories(&cache_dir, only)?;

    for name in &matched {
        let dir = cache_dir.join(name);
        fs::remove_dir_all(&dir).with_context(|| format!("Failed to remove {dir:?}"))?;
    }

    Ok(matched)
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
