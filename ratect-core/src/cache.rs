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
//! actual Docker bind-mount string — either a named volume that persists
//! between separate `ratect` invocations, or a host directory under
//! `--cache-type=directory` — and implements `--clean`/`--clean-cache`
//! ([`clean_volume_caches`]/[`clean_directory_caches`]), which remove them.
//! Ported from Batect's own `CacheManager`/`VolumeMountResolver`/`CacheType`/
//! `CleanupCachesCommand`, with one deliberate divergence: the project cache
//! key is a full UUID rather than Batect's 6-char `a-z0-9` id (see
//! [`project_cache_key`]'s own doc comment for why) — everything else,
//! including the `.batect/caches/` location and `batect-cache-` volume
//! prefix, is kept byte-for-byte compatible with Batect's own convention on
//! purpose: this is `ratect-compat`'s territory (see `ROADMAP.md`'s
//! `## Two Binaries` section), and a project migrating from real `batect`
//! should find its existing cache volumes/directories reused, not orphaned.
//!
//! Resolves a `VolumeMount::Cache`
//! (`config.rs`) into an actual Docker bind-mount string — a named volume
//! (`CacheType::Volume`, the default) or a host directory
//! (`CacheType::Directory`, `--cache-type=directory`) — and implements
//! `--clean`/`--clean-cache` (`clean_volume_caches`/`clean_directory_caches`),
//! which remove them. Ported from Batect's own `CacheManager`/
//! `VolumeMountResolver`/`CacheType`/`CleanupCachesCommand`, kept
//! byte-for-byte compatible with Batect's own `.batect/caches/` location and
//! `batect-cache-<project-key>-<name>` volume-naming convention *on purpose*
//! — this is `ratect-compat`'s territory (see `ROADMAP.md`'s two-binaries
//! section), so a project migrating from real `batect` should find its
//! existing cache volumes/directories reused, not orphaned. The one
//! deliberate divergence: a freshly generated `project_cache_key` is a full
//! `uuid::Uuid::new_v4()`, not Batect's 6-char `a-z0-9` id — an existing
//! Batect-created key file is still read and reused byte-for-byte (tolerant
//! of its `#`-comment-header format), since nothing depends on matching the
//! *generation* format, only the file's path and read-compatible layout, and
//! Batect's own alphabet is meaningfully more collision-prone across many
//! projects on one machine. The actual removal *decision* (which
//! volumes/directories match this project's prefix, restricted to
//! `--clean-cache`'s allowlist) is split into plain synchronous functions
//! (`matching_cache_volumes`/`matching_cache_directories`), deliberately kept
//! separate from the async I/O around them, so they're unit-testable against
//! plain `Vec<String>`/tempdir fixtures without needing a fake
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
    let source = match options.cache_type {
        CacheType::Volume => cache_volume_name(project_cache_key, &mount.name),
        CacheType::Directory => {
            let dir = cache_directory(&options.project_directory).join(&mount.name);
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

/// Filters `existing_volumes` (from [`crate::docker::ContainerRuntime::list_volumes`])
/// down to this project's own cache volumes — those with the
/// `batect-cache-<project_cache_key>-` prefix — further restricted to
/// `only` when non-empty (the `--clean-cache <name>` allowlist; empty means
/// "every one of this project's cache volumes", matching plain `--clean`).
/// A pure, synchronous decision function deliberately kept separate from
/// the I/O in [`clean_volume_caches`], so it's unit-testable against plain
/// `Vec<String>` fixtures without needing a fake `ContainerRuntime`.
fn matching_cache_volumes<'a>(
    existing_volumes: &'a [String],
    project_cache_key: &str,
    only: &HashSet<String>,
) -> Vec<&'a str> {
    let prefix = cache_volume_name(project_cache_key, "");
    existing_volumes
        .iter()
        .filter_map(|name| {
            let cache_name = name.strip_prefix(&prefix)?;
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
    let mut names: Vec<String> =
        matching_cache_volumes(&existing, project_cache_key, &HashSet::new())
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
    let matched: Vec<String> = matching_cache_volumes(&existing, project_cache_key, only)
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
