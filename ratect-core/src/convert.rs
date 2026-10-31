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

//! `ratect config convert`'s translation of a Batect-format project into
//! native files — **one `.toml` per source file**, never one merged result.
//!
//! The loader resolves a relative host path (`volumes`, `build_directory`,
//! `build_secrets`, `build_ssh`) against the directory of the file that
//! declared it. Flattening every include into the root's `ratect.toml`
//! re-anchored those paths at the root (ratect#574), and no check on the
//! merged [`Config`](crate::config::Config) could see it, since that holds
//! paths as written. So [`convert_project`] works from
//! [`LoadedConfig`]'s per-file record instead: the root becomes
//! `ratect.toml`, each local include becomes a `.toml` with the same stem in
//! its own directory, and every `include` entry naming a converted file is
//! rewritten to its `.toml`. Nothing is re-based; every value is copied as
//! written, which is what keeps each path resolving where it did.
//!
//! A `type: git` include stays an include entry (`repo`, `ref`, `path` and
//! `allow_host_paths` carried across): a bundle's files are its own, known
//! only by their clone-cache path, so nothing in one is copied out and no
//! file in one is converted. Two things about a bundle are checked instead
//! and reported as [`ConversionCheck`]s. A pathless entry whose clone offers
//! a `ratect-bundle.toml` gets `path` set to `batect-bundle.yml` — a native
//! load would otherwise pick a file the Batect-format load never read — and
//! an item saying so. A bundle declaring Git includes of its own gets an
//! item, since a native project refuses those without
//! `allow_nested_git_includes`, which convert never writes: it is a grant
//! only the owner can make.
//!
//! Each document is proved to **round-trip** before it is returned: parsed
//! straight back and compared as `toml::Value`s, so a lossy conversion —
//! most plausibly in a hand-written (de)serializer, or in this module's own
//! inline rendering of `include` — can't be written out silently. The
//! `include` list is rendered by hand because `toml`'s pretty serializer
//! writes an array of tables as `[[include]]` blocks, which would land among
//! the `[containers.*]` tables; the round trip is what checks that
//! rendering too. `ratect_version` is prepended to the root's text after
//! the check, since it isn't part of the document.
//!
//! What this doesn't do: evaluate the project, compare resolved values
//! before and after, or delete the source files — a converted project is
//! still a starting point to review.

use crate::config::{
    absolute_path, local_include_target, ConfigFile, ConfigVariable, Container, IncludeEntry,
    LoadedConfig, LoadedFile, RunAsCurrentUser, Task, VolumeMount,
};
use crate::diagnostics::{conversion_checks, ConversionCheck, ConversionCheckKind};
use crate::git_include::describe_include;
use crate::include_trust::BundleId;
use crate::ui::printable;
use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The file the root converts to — `ratect`'s own default config file.
pub const NATIVE_CONFIG_FILE: &str = "ratect.toml";

/// The bundle file a native load prefers; a pathless Git include whose
/// clone offers one is pinned to the Batect one instead — see the module
/// header.
const NATIVE_BUNDLE_FILE: &str = "ratect-bundle.toml";
const BATECT_BUNDLE_FILE: &str = "batect-bundle.yml";

/// Every file [`convert_project`] produced, root first, then in the order
/// the load reached each include.
#[derive(Debug)]
pub struct ConvertedProject {
    pub files: Vec<ConvertedFile>,
}

/// One converted file.
#[derive(Debug)]
pub struct ConvertedFile {
    /// The source file, absolute.
    pub source: PathBuf,
    /// Where it converts to, absolute: `ratect.toml` beside the root, or the
    /// source with a `.toml` extension.
    pub target: PathBuf,
    /// The document, without any header — `ratect_version` first for the
    /// root, then the file's own configuration.
    pub toml: String,
    /// What to check in this file: its `include` entries' items in list
    /// order, then its containers' as [`conversion_checks`] orders them.
    pub checks: Vec<ConversionCheck>,
}

/// Converts every file the project owner controls — the root and each
/// local include, however deeply nested — and keeps each Git include as an
/// entry. Fails, before anything is rendered, when two sources would
/// convert to the same target (`c.yml` and `c.yaml` beside each other, or
/// an include named `ratect.yml` beside the root), or a source *is* its own
/// target (a `.toml`-named include, which the Batect-format load still
/// reads as YAML); the caller writes nothing then.
///
/// `written_for` is declared as the root's `ratect_version`
/// ([`crate::written_for`]): the converting binary's own version, since
/// that is whose rules the result was converted under.
pub fn convert_project(
    loaded: &LoadedConfig,
    written_for: crate::written_for::Version,
) -> Result<ConvertedProject> {
    let owned: Vec<&LoadedFile> = loaded
        .files
        .iter()
        .filter(|file| file.boundary.is_none())
        .collect();
    let (root, includes) = owned
        .split_first()
        .expect("a loaded project has a root file");

    let targets: Vec<PathBuf> = std::iter::once(
        root.path
            .parent()
            .unwrap_or(Path::new(""))
            .join(NATIVE_CONFIG_FILE),
    )
    .chain(includes.iter().map(|file| file.path.with_extension("toml")))
    .collect();
    // One table of source → target, which the clash check and the `include`
    // rewriting both read — so an entry can only ever name a file this
    // conversion writes, however the entry spells it (an include naming the
    // root gets `ratect.toml`, not `batect.toml`).
    let mut target_of: HashMap<&Path, &Path> = HashMap::new();
    let mut claimed: HashMap<&Path, &Path> = HashMap::new();
    for (file, target) in owned.iter().zip(&targets) {
        if *target == file.path {
            anyhow::bail!(
                "Converting '{}' would overwrite it with its own conversion: a Batect-format \
                 file is YAML whatever its extension, so rename it before converting.",
                printable(&file.path.display().to_string())
            );
        }
        if let Some(previous) = claimed.insert(target, &file.path) {
            anyhow::bail!(
                "Converting '{}' and '{}' would both write '{}' — rename one of them first.",
                printable(&previous.display().to_string()),
                printable(&file.path.display().to_string()),
                printable(&target.display().to_string())
            );
        }
        target_of.insert(&file.path, target);
    }

    let mut container_checks = checks_by_file(loaded)?;
    let files = owned
        .iter()
        .zip(&targets)
        .enumerate()
        .map(|(index, (file, target))| {
            // Exhaustive, so a field added to `ConfigFile` is a compile error
            // here rather than a setting every converted file silently drops.
            let ConfigFile {
                // Root-only: the merged `Config`'s, below.
                project_name: _,
                containers,
                tasks,
                config_variables,
                include,
                forbid_telemetry,
                // Refused in a Batect-format file at load, so never set here;
                // the root's is declared by `render` instead.
                ratect_version: _,
            } = &file.file;
            let mut checks = Vec::new();
            let include = include
                .iter()
                .map(|entry| convert_include(entry, &file.path, loaded, &target_of, &mut checks))
                .collect::<Result<Vec<IncludeEntry>>>()?;
            checks.extend(container_checks.remove(&file.path).unwrap_or_default());
            let document = Document {
                // The root's own, or the one `ratect-compat` infers for it,
                // since `ratect.toml` requires the field; an include's is
                // refused at load.
                project_name: (index == 0).then(|| loaded.config.project_name.clone()),
                containers: containers
                    .iter()
                    .map(|(name, container)| (name.clone(), literal_fields_escaped(container)))
                    .collect(),
                tasks: tasks.clone(),
                config_variables: config_variables.clone(),
                include,
                forbid_telemetry: *forbid_telemetry,
            };
            let toml = render(&document, (index == 0).then_some(written_for))
                .with_context(|| format!("converting {}", file.path.display()))?;
            Ok(ConvertedFile {
                source: file.path.clone(),
                target: target.clone(),
                toml,
                checks,
            })
        })
        .collect::<Result<Vec<ConvertedFile>>>()?;
    anyhow::ensure!(
        container_checks.is_empty(),
        "an item to check was placed in no converted file: {container_checks:?} — this is a \
         bug; please report it"
    );
    Ok(ConvertedProject { files })
}

/// `container` with each value `ratect.toml` reads as an expression and a
/// Batect-format file doesn't rewritten to mean what the source did. A
/// compact volume's host path and `run_as_current_user.home_directory` are
/// rewritten as
/// [`literal_as_expression`](crate::expressions::literal_as_expression)
/// says, the one rule [`conversion_checks`] also judges them by: so a
/// reference the converted file holds is exactly one a check lists. An
/// `image` is kept wholly literal instead: a Batect-format file refuses a
/// reference there at load, so a `$` or `<` that loaded (`alpine\$HOME`)
/// was never meant as one, and no check would list it.
fn literal_fields_escaped(container: &Container) -> Container {
    let escaped = |value: &mut String| {
        *value = crate::expressions::literal_as_expression(value);
    };
    let mut container = container.clone();
    // `true`: convert reads a Batect-format file.
    if !Container::image_is_expression(true) {
        for image in container.image.iter_mut() {
            *image = crate::expressions::literal_kept(image);
        }
    }
    for volume in container.volumes.iter_mut().flatten() {
        if let VolumeMount::Local(local) = volume {
            if !local.local_is_expression(true) {
                escaped(&mut local.local);
            }
        }
    }
    if !RunAsCurrentUser::home_directory_is_expression(true) {
        let home = container
            .run_as_current_user
            .iter_mut()
            .flat_map(|user| user.home_directory.as_mut());
        home.for_each(escaped);
    }
    container
}

/// [`conversion_checks`]' items, each under the file whose header it belongs
/// in: the file declaring its container, or — for a container declared in a
/// bundle, whose files aren't converted — the owned file whose Git include
/// reached that bundle. A bundle's file stays YAML, so a Batect-format
/// literal (ratect#462, ratect#379) stays literal there and those two kinds
/// don't apply; the readiness gate follows the binary, not the file
/// (decisions/0012), so that one still does.
fn checks_by_file(loaded: &LoadedConfig) -> Result<HashMap<PathBuf, Vec<ConversionCheck>>> {
    let mut by_file: HashMap<PathBuf, Vec<ConversionCheck>> = HashMap::new();
    for check in conversion_checks(&loaded.config) {
        let container = check
            .container
            .as_deref()
            .expect("a container check names its container");
        let file = match loaded.container_bundle(container) {
            None => loaded
                .container_origin(container)
                .expect("a container check names a container the load declared")
                .to_path_buf(),
            Some(bundle) => {
                if check.kind != ConversionCheckKind::UngatedTaskContainer {
                    continue;
                }
                owned_file_reaching(loaded, bundle)?
            }
        };
        by_file.entry(file).or_default().push(check);
    }
    Ok(by_file)
}

/// The owned file whose Git include reached `bundle` — through the bundle
/// that included it, where one bundle includes another.
fn owned_file_reaching(loaded: &LoadedConfig, bundle: &BundleId) -> Result<PathBuf> {
    let mut wanted = bundle.clone();
    // Each step climbs one bundle; more steps than files is a cycle.
    for _ in 0..loaded.files.len() {
        let declaring = loaded
            .files
            .iter()
            .filter(|file| {
                file.file.include.iter().any(|entry| {
                    matches!(entry, IncludeEntry::Git { repo, git_ref, .. }
                        if *repo == wanted.remote && *git_ref == wanted.git_ref)
                })
            })
            // An owned file over a bundle's, where both declare it.
            .min_by_key(|file| file.boundary.is_some())
            .with_context(|| {
                format!(
                    "no file declares the Git include {wanted} — this is a bug; please \
                     report it"
                )
            })?;
        match &declaring.boundary {
            None => return Ok(declaring.path.clone()),
            Some(boundary) => wanted = boundary.bundle.id.clone(),
        }
    }
    anyhow::bail!(
        "the Git include {bundle} is reached from no owned file — this is a bug; please \
         report it"
    )
}

/// `entry`, declared in `declaring`, as the converted file declares it. A
/// local include keeps its spelling but names the converted file —
/// resolved the way the loader resolves it, then looked up in `target_of`;
/// a Git include is carried across, pinned to the bundle file the
/// Batect-format load read where a native load would pick another, and
/// never granted `allow_nested_git_includes` — with an item in `checks` for
/// each of those two.
fn convert_include(
    entry: &IncludeEntry,
    declaring: &Path,
    loaded: &LoadedConfig,
    target_of: &HashMap<&Path, &Path>,
    checks: &mut Vec<ConversionCheck>,
) -> Result<IncludeEntry> {
    let declaring_dir = declaring.parent().unwrap_or(Path::new(""));
    match entry {
        IncludeEntry::File { path } => {
            let resolved = absolute_path(&local_include_target(declaring_dir, path)?)?;
            let target = target_of.get(resolved.as_path()).with_context(|| {
                format!(
                    "the load didn't reach '{}' from '{}' — this is a bug; please report it",
                    resolved.display(),
                    declaring.display()
                )
            })?;
            let file_name = target.file_name().expect("a target is a file");
            Ok(IncludeEntry::File {
                path: Path::new(path)
                    .with_file_name(file_name)
                    .to_string_lossy()
                    .into_owned(),
            })
        }
        IncludeEntry::Git {
            repo,
            git_ref,
            path,
            allow_host_paths,
            allow_nested_git_includes: _,
            // A Batect-format source can't set it, and no tool writes one: a
            // review can't be automated (decisions/0020).
            reviewed_ratect_version: _,
        } => {
            // For messages only: the entry written keeps `repo` in full.
            let include = describe_include(repo, git_ref);
            let clone = loaded
                .git_clones
                .get(&(repo.clone(), git_ref.clone()))
                .with_context(|| {
                    format!("the load left Git include {include} uncloned — this is a bug; please report it")
                })?;
            let path = match path {
                Some(path) => Some(path.clone()),
                None if clone.join(NATIVE_BUNDLE_FILE).is_file() => {
                    checks.push(ConversionCheck {
                        container: None,
                        kind: ConversionCheckKind::BundleOffersNativeFile,
                        message: format!(
                            "Git include {include} offers a {NATIVE_BUNDLE_FILE}, \
                             which ratect would load in place of the {BATECT_BUNDLE_FILE} \
                             batect.yml loads — 'path' was set to {BATECT_BUNDLE_FILE} so the \
                             same file loads; remove 'path' to switch to the native one."
                        ),
                    });
                    Some(BATECT_BUNDLE_FILE.to_string())
                }
                None => None,
            };
            if bundle_declares_git_includes(loaded, clone, path.as_deref())? {
                checks.push(ConversionCheck {
                    container: None,
                    kind: ConversionCheckKind::BundleDeclaresGitIncludes,
                    message: format!(
                        "Git include {include} declares Git includes of its own, \
                         which ratect refuses unless 'allow_nested_git_includes' is set to true \
                         on this entry (see https://github.com/or1can/ratect/blob/main/docs/includes.md#nested-git-includes)."
                    ),
                });
            }
            Ok(IncludeEntry::Git {
                repo: repo.clone(),
                git_ref: git_ref.clone(),
                path,
                allow_host_paths: *allow_host_paths,
                allow_nested_git_includes: None,
                reviewed_ratect_version: None,
            })
        }
    }
}

/// Whether the bundle file a Git include loads from `clone` — `path`, or
/// the Batect default — or any file it reaches through local includes of
/// its own, declares a Git include. Keyed on the files this entry reaches,
/// not on the bundle: two entries into one clone can load different files,
/// and the item would otherwise ask for a grant one of them doesn't need.
/// Every file reached was loaded, so one missing from the load is a bug,
/// not a file with no includes.
fn bundle_declares_git_includes(
    loaded: &LoadedConfig,
    clone: &Path,
    path: Option<&str>,
) -> Result<bool> {
    let mut queue = vec![absolute_path(
        &clone.join(path.unwrap_or(BATECT_BUNDLE_FILE)),
    )?];
    let mut seen = HashSet::new();
    while let Some(file_path) = queue.pop() {
        if !seen.insert(file_path.clone()) {
            continue;
        }
        let file = loaded
            .files
            .iter()
            .find(|file| file.path == file_path)
            .with_context(|| {
                format!(
                    "the load didn't reach '{}' in the clone of a Git include — this is a bug; \
                     please report it",
                    file_path.display()
                )
            })?;
        for nested in &file.file.include {
            match nested {
                IncludeEntry::Git { .. } => return Ok(true),
                IncludeEntry::File { path } => {
                    queue.push(absolute_path(&local_include_target(
                        file_path.parent().unwrap_or(Path::new("")),
                        path,
                    )?)?);
                }
            }
        }
    }
    Ok(false)
}

/// One file's configuration as the converted file holds it — the per-file
/// shape (`project_name` optional, an `include` list), unlike the merged
/// [`Config`](crate::config::Config). `ratect_version` is not a field: it is
/// prepended to the root's text, after the round-trip check. An empty
/// `containers`/`tasks` isn't written, so a file that only includes others
/// doesn't carry two empty tables.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    project_name: Option<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    containers: IndexMap<String, Container>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    tasks: IndexMap<String, Task>,
    config_variables: Option<IndexMap<String, ConfigVariable>>,
    #[serde(default)]
    include: Vec<IncludeEntry>,
    forbid_telemetry: Option<bool>,
}

/// The order an `include` entry's keys are written in — what a reader
/// expects, rather than the alphabetical order [`sorted`] gives every
/// table, an entry included until this reorders it.
const INCLUDE_KEY_ORDER: [&str; 6] = [
    "type",
    "repo",
    "ref",
    "path",
    "allow_host_paths",
    "allow_nested_git_includes",
];

/// `value` with every table's keys sorted, at every depth, tables inside
/// arrays included — so a converted file lists its containers, tasks and
/// every field by name, as it did before `toml`'s `preserve_order` (which
/// the loader needs) made tables keep insertion order. Without it,
/// containers would follow the source file's order, a struct's fields its
/// declaration order, and a `HashMap` field such as `environment` a
/// different order each run.
fn sorted(value: toml::Value) -> toml::Value {
    match value {
        toml::Value::Table(table) => {
            let mut entries: Vec<(String, toml::Value)> = table.into_iter().collect();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            toml::Value::Table(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sorted(value)))
                    .collect(),
            )
        }
        toml::Value::Array(values) => toml::Value::Array(values.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// Renders `document` as native TOML and proves it round-trips, then
/// declares `written_for` first when given (the root).
///
/// Goes through `toml::Value` rather than serializing the struct directly,
/// so scalar fields are emitted before table fields (raw struct
/// serialization would produce a value after a `[table]` header — invalid
/// TOML — because `Container` interleaves the two). The top level is
/// rendered in three parts for the same reason: its scalars, then the
/// `include` list inline, then its tables.
fn render(document: &Document, written_for: Option<crate::written_for::Version>) -> Result<String> {
    let value = sorted(toml::Value::try_from(document).context("serializing the configuration")?);
    let toml::Value::Table(table) = &value else {
        unreachable!("a struct serializes to a table");
    };
    let mut head = toml::Table::new();
    let mut body = toml::Table::new();
    let mut include = Vec::new();
    for (key, value) in table {
        match value {
            toml::Value::Array(entries) if key == "include" => include = entries.clone(),
            toml::Value::Table(_) => {
                body.insert(key.clone(), value.clone());
            }
            _ => {
                head.insert(key.clone(), value.clone());
            }
        }
    }

    let mut text = toml::to_string_pretty(&head).context("rendering TOML")?;
    if !include.is_empty() {
        text.push_str("include = [\n");
        for entry in &include {
            let toml::Value::Table(entry) = entry else {
                unreachable!("an include entry serializes to a table");
            };
            let pairs: Vec<String> = INCLUDE_KEY_ORDER
                .iter()
                .filter_map(|key| entry.get(*key).map(|value| format!("{key} = {value}")))
                .collect();
            text.push_str(&format!("    {{ {} }},\n", pairs.join(", ")));
        }
        text.push_str("]\n");
    }
    if !body.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&toml::to_string_pretty(&body).context("rendering TOML")?);
    }

    let reparsed: Document = toml::from_str(&text).context("re-parsing the generated TOML")?;
    let reparsed_value =
        toml::Value::try_from(&reparsed).context("re-serializing the generated TOML")?;
    anyhow::ensure!(
        reparsed_value == value,
        "the conversion did not round-trip losslessly — this is a bug; please report it"
    );
    Ok(match written_for {
        Some(version) => format!(
            "{} = {}\n{text}",
            crate::written_for::FIELD,
            toml::Value::String(version.to_string())
        ),
        None => text,
    })
}

#[cfg(test)]
#[path = "convert_tests.rs"]
mod tests;
