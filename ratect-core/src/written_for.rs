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

//! A native project's **written-for version** (`CONTEXT.md`) — the
//! `ratect_version` its root `ratect.toml` declares — and the check `ratect`
//! runs against it: before loading anything else for a TOML root, and after
//! the load for a YAML root, which can't declare one. See
//! [decisions/0015](../../decisions/0015-native-project-records-its-written-for-version.md)
//! for the why; what follows is how, and what is easy to undo by accident.
//!
//! - **For a TOML root it runs before the configuration is parsed**, and
//!   reads nothing but that one field. That ordering is the point: a project written for a
//!   newer Ratect most likely uses a field this binary doesn't know, and the
//!   full parse would reject that field first, with a message that says
//!   nothing about versions. So `read` parses the root file as a bare TOML
//!   table, never as a [`crate::config`] type.
//! - **A file it can't read, or that isn't TOML at all, is not its problem.**
//!   `read` answers `None` and the loader reports the file in its own
//!   words — duplicating those messages here would only let them drift.
//! - **Only major and minor are compared.** A pre-1.0 breaking change lands
//!   on a minor version, so a patch number can't change what a project means.
//! - **[`BREAKING_CHANGES`] is the table the check consults**, one row per
//!   change to what a `ratect.toml` means or accepts. A row is added in the
//!   same commit as its `CHANGELOG.md` entry, which is marked as one —
//!   `ratect`'s own tests fail if a marked entry has no row (see AGENTS.md
//!   guideline 7 for the marker). Nothing released before this mechanism
//!   existed was retrofitted, since every unmarked project would then have
//!   refused on the spot; the first row (0.12.0, #382) is what made them
//!   refuse.
//! - **Every row declares how it is checked** — its [`Check`], a required
//!   field, so the compiler stops a row added without one. A refusal is a
//!   [`Refusal`], which `ratect` [assesses](Refusal::assess) after a trial
//!   load to give each row an outcome; `doctor` and `config validate` report
//!   the same result through `diagnostics::load_failure_findings`
//!   ([decisions/0022](../../decisions/0022-written-for-refusal-says-what-applies.md)).
//!   A row that only makes something fail to load is [`Check::FailsTheLoad`];
//!   one that changes what a value means is [`Check::Detected`], or
//!   [`Check::NotChecked`] until it has a detector. **A detected row carries
//!   tests** against a file that uses the construct and one that doesn't: a
//!   false "none found" is the one wrong answer that matters. Its detector
//!   reads plain TOML tables, never the config model, so it keeps recognising
//!   old files however that model changes.
//! - **A Git bundle is checked too, by `check_bundle`**, against the later
//!   of its entry file's own `ratect_version` and its include entry's
//!   `reviewed_ratect_version` — never the root's
//!   ([decisions/0020](../../decisions/0020-bundle-records-its-written-for-version.md)).
//!   The loader runs it, since only the loader finds bundles, at each Git
//!   include entry as it's reached: before parsing a TOML entry file, for the
//!   same reason as the root, and after the load for a YAML one, which —
//!   like a YAML root — is judged only once it reaches a TOML file.
//! - **Native dialect only.** `ratect-compat` never calls this, and a
//!   `batect.yml` can't declare the field; a native project whose root file
//!   is YAML is `WrittenFor::YamlRoot`. Its YAML files follow Batect's
//!   rules, so it is refused only when it loads a TOML file — see
//!   [`check_yaml_root`] — which converting the root fixes.

use anyhow::{Context, Result};
use std::path::Path;

/// The top-level field a root `ratect.toml` declares its version in.
pub const FIELD: &str = "ratect_version";

/// What an unmarked project counts as written for: 0.3.0, the first release
/// with `ratect.toml` — so it predates every row the table can hold.
const FIRST_NATIVE_RELEASE: Version = Version {
    major: 0,
    minor: 3,
    patch: 0,
};

/// One change to what a `ratect.toml` means or accepts.
#[derive(Debug, Clone, Copy)]
pub struct BreakingChange {
    /// The `ratect` version it landed in, as `major.minor.patch`.
    pub version: &'static str,
    /// What changed, as one line a user can act on.
    pub summary: &'static str,
    /// How a refusal finds out whether it touches the project
    /// ([decisions/0022](../../decisions/0022-written-for-refusal-says-what-applies.md)).
    pub check: Check,
}

/// How a refusal tells whether a [`BreakingChange`] touches the project it
/// refuses. Only advice: the refusal itself never depends on it.
#[derive(Debug, Clone, Copy)]
pub enum Check {
    /// The change only makes something that used to load fail to, so the
    /// trial load — the project loaded once more with the written-for checks
    /// set aside — is its detector.
    FailsTheLoad,
    /// The change alters what a value means, so a load that succeeds proves
    /// nothing: the detector returns every use of the construct in the
    /// files in scope, each read as a plain TOML document. It reads only
    /// files a successful trial load reached, so "none found" always also
    /// means the project loads — which covers a change some uses of which
    /// fail the load instead.
    Detected(fn(&[TomlFile]) -> Vec<Hit>),
    /// No detector covers the change, and the refusal says so.
    NotChecked,
}

/// One file a [`Check::Detected`] row's detector reads.
#[derive(Debug, Clone)]
pub struct TomlFile {
    /// As a refusal names it.
    pub name: String,
    /// The whole file as a generic TOML table, never through the config
    /// model, so a detector keeps recognising an old file however that
    /// model changes later.
    pub document: toml::Table,
}

/// One use a detector found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The field it was found in.
    pub field: String,
    /// The file, as [`TomlFile::name`] names it.
    pub file: String,
}

/// The bundle row's detector: every file declaring a `type = "git"`
/// include, in either of the forms `include` takes — an inline array or
/// `[[include]]` tables, which TOML reads alike.
fn declares_git_include(files: &[TomlFile]) -> Vec<Hit> {
    files
        .iter()
        .filter(|file| {
            file.document
                .get("include")
                .and_then(toml::Value::as_array)
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry.get("type").and_then(toml::Value::as_str) == Some("git"))
                })
        })
        .map(|file| Hit {
            field: "include".to_string(),
            file: file.name.clone(),
        })
        .collect()
}

/// The home-directory row's detector: every file declaring a local include
/// — a bare path, or a table whose `type` is absent or `"file"` — whose
/// `path` has `~` as its first component, by the loader's own rule
/// (`config::expand_home_directory`'s): `~` and `~/x`, never `~user/x`. A
/// Git include's `path` is inside its repository, so the change leaves it
/// alone.
fn includes_from_home_directory(files: &[TomlFile]) -> Vec<Hit> {
    fn local_path(entry: &toml::Value) -> Option<&str> {
        match entry {
            toml::Value::String(path) => Some(path),
            toml::Value::Table(table) => match table.get("type").map(toml::Value::as_str) {
                None | Some(Some("file")) => table.get("path").and_then(toml::Value::as_str),
                Some(_) => None,
            },
            _ => None,
        }
    }
    let from_home = |path: &str| {
        matches!(
            Path::new(path).components().next(),
            Some(std::path::Component::Normal(first)) if first == "~"
        )
    };
    files
        .iter()
        .filter(|file| {
            file.document
                .get("include")
                .and_then(toml::Value::as_array)
                .is_some_and(|entries| entries.iter().filter_map(local_path).any(from_home))
        })
        .map(|file| Hit {
            field: "include".to_string(),
            file: file.name.clone(),
        })
        .collect()
}

/// Every breaking change to what a `ratect.toml` means or accepts, oldest
/// first. See the module documentation before adding one.
pub const BREAKING_CHANGES: &[BreakingChange] = &[
    BreakingChange {
        version: "0.12.0",
        summary: "a '\\' in an expression escapes the character after it and is dropped, and \
                  a trailing '\\' fails the load",
        check: Check::NotChecked,
    },
    BreakingChange {
        version: "0.12.0",
        summary: "every '$' or '<' in an expression starts a reference, so a malformed one \
                  ('$$', 'a < b', '${X:default}') fails the load, and an unbraced name ends at \
                  '.' or '-' (write '<{batect.project_directory}')",
        check: Check::NotChecked,
    },
    BreakingChange {
        version: "0.12.0",
        summary: "a task or config variable name Batect refuses fails the load: a task name \
                  is letters, digits, ':', '-', '.' and '_', starting and ending with a letter \
                  or digit; a config variable name is letters, digits, '-', '.' and '_', \
                  starting with a letter and not with 'batect'",
        check: Check::FailsTheLoad,
    },
    BreakingChange {
        version: "0.12.0",
        summary: "'project_name' and every container name must be a Docker image-name \
                  component, or the load fails: lowercase letters and digits, joined by a \
                  single '.', one or two '_', or any number of '-'",
        check: Check::FailsTheLoad,
    },
    BreakingChange {
        version: "0.12.0",
        summary: "an included file that sets 'forbid_telemetry' to true fails the load; only \
                  the root file may",
        check: Check::FailsTheLoad,
    },
    BreakingChange {
        version: "0.12.0",
        summary: "a string 'volumes' or 'devices' entry with an empty options segment \
                  ('a:b:') fails the load",
        check: Check::FailsTheLoad,
    },
    BreakingChange {
        version: "0.12.0",
        summary: "a local include's 'path' whose first component is '~' is looked for in the \
                  home directory, not in a '~' directory beside the file declaring it",
        check: Check::Detected(includes_from_home_directory),
    },
    BreakingChange {
        version: "0.12.0",
        summary: "a 'command', 'entrypoint', 'run.command', 'run.entrypoint' or setup command \
                  that can't be split into arguments (an unbalanced quote, a trailing '\\') \
                  fails the load, even where no task runs it",
        check: Check::FailsTheLoad,
    },
    BreakingChange {
        version: "0.12.0",
        summary: "a Git bundle is refused until the 'ratect_version' of the file its include \
                  names, or the include's 'reviewed_ratect_version', covers every change since; \
                  the root's 'ratect_version' doesn't cover a bundle",
        check: Check::Detected(declares_git_include),
    },
];

/// A `major.minor.patch` version, optionally followed by a `-pre-release`
/// suffix that is accepted and then ignored — a development build of 0.11.0
/// is 0.11.0 here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// The part of a version a written-for comparison looks at.
    fn release_line(self) -> (u64, u64) {
        (self.major, self.minor)
    }
}

impl std::str::FromStr for Version {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let release = match text.split_once('-') {
            Some((release, pre)) if !pre.is_empty() => release,
            Some(_) => anyhow::bail!("'{text}' is not a version"),
            None => text,
        };
        let parts: Vec<&str> = release.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            anyhow::bail!("'{text}' is not a version (expected major.minor.patch, e.g. 0.11.0)");
        };
        // Digits only: `u64`'s own parser also takes a leading `+`.
        let number = |part: &str| {
            part.bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| part.parse::<u64>().ok())
                .flatten()
                .with_context(|| {
                    format!("'{text}' is not a version (expected major.minor.patch, e.g. 0.11.0)")
                })
        };
        Ok(Version {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What a project's root file says about the version it was written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WrittenFor {
    /// It sets [`FIELD`].
    Declared(Version),
    /// It's TOML and doesn't — so it counts as written for 0.3.0.
    Unmarked,
    /// It's a Batect-format YAML file, which keeps Batect's own shape and so
    /// can't say. Its own meaning is Batect's; a TOML file it includes counts
    /// as unmarked — see [`check_yaml_root`].
    YamlRoot,
}

/// Reads what `root` — a native project's root configuration file — says
/// about its written-for version. `None` when it can't be read or isn't
/// TOML, which the loader goes on to report; an error only when [`FIELD`]
/// is present but isn't a version.
pub(crate) fn read(root: &Path) -> Result<Option<WrittenFor>> {
    let is_yaml = root
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml"));
    if is_yaml {
        return Ok(root.is_file().then_some(WrittenFor::YamlRoot));
    }
    let Ok(text) = std::fs::read_to_string(root) else {
        return Ok(None);
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Ok(None);
    };
    let Some(value) = table.get(FIELD) else {
        return Ok(Some(WrittenFor::Unmarked));
    };
    let version = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("'{FIELD}' must be a version string, e.g. \"0.11.0\""))
        .and_then(str::parse)
        .with_context(|| format!("Invalid '{FIELD}' in {}", root.display()))?;
    Ok(Some(WrittenFor::Declared(version)))
}

/// Refuses to go on when `root` was written for a newer `ratect` than
/// `running` (this binary's own version), or when a change in
/// [`BREAKING_CHANGES`] has altered what it means since it was written.
///
/// A YAML root passes here: its own meaning is Batect's, so only a TOML file
/// it includes can have changed — which [`check_yaml_root`] judges once the
/// load has found them.
pub fn check(root: &Path, running: Version) -> Result<()> {
    let Some(written_for) = read(root)? else {
        return Ok(());
    };
    if written_for == WrittenFor::YamlRoot {
        return Ok(());
    }
    check_against(
        format!("Refusing to load {}", root.display()),
        Some(root),
        &written_for,
        running,
        BREAKING_CHANGES,
    )
}

/// Refuses a project whose root is YAML when the load read any TOML file —
/// a local include or a Git bundle — and [`BREAKING_CHANGES`] has altered
/// what a TOML file means since 0.3.0: nothing in the project can say it
/// was reviewed. `toml_files` are those files, in the order the load read
/// them, as [`crate::config::LoadedProject::toml_files`] names them. A
/// project of YAML files alone follows Batect's rules throughout, so it is
/// never refused; nor is a TOML root, which [`check`] judged.
///
/// After the load, so it can't explain a load that fails: a TOML include
/// using a field this binary doesn't know, or a value an escape now
/// refuses, gets the loader's own error instead.
pub fn check_yaml_root(root: &Path, toml_files: &[String], running: Version) -> Result<()> {
    if read(root)? != Some(WrittenFor::YamlRoot) {
        return Ok(());
    }
    check_yaml_root_against(root, toml_files, running, BREAKING_CHANGES)
}

/// [`check_yaml_root`]'s decision, over an explicit table.
fn check_yaml_root_against(
    root: &Path,
    toml_files: &[String],
    running: Version,
    table: &[BreakingChange],
) -> Result<()> {
    let Some(first) = toml_files.first() else {
        return Ok(());
    };
    check_against(
        format!(
            "Refusing to load {root:?}, which includes {first}: a TOML file follows ratect's \
             own rules"
        ),
        None,
        &WrittenFor::YamlRoot,
        running,
        table,
    )
}

/// [`check`]'s decision, over an explicit table so it can be tested with
/// rows the real one doesn't have yet. `heading` says what is refused, and
/// `root` is the root file when it is TOML — what a refusal's detectors can
/// still read if the trial load fails.
fn check_against(
    heading: String,
    root: Option<&Path>,
    written_for: &WrittenFor,
    running: Version,
    table: &[BreakingChange],
) -> Result<()> {
    let declared = match written_for {
        WrittenFor::Declared(version) => *version,
        WrittenFor::Unmarked | WrittenFor::YamlRoot => FIRST_NATIVE_RELEASE,
    };
    if declared.release_line() > running.release_line() {
        return Err(anyhow::anyhow!(
            "This project's '{FIELD}' is {declared}, so it needs ratect {}.{} or later; \
             this is ratect {running}.",
            declared.major,
            declared.minor
        )
        .context(heading));
    }
    let rows = changes_since(declared, table);
    if rows.is_empty() {
        return Ok(());
    }
    let written = match written_for {
        WrittenFor::Declared(version) => format!("was written for ratect {version}"),
        WrittenFor::Unmarked => {
            format!("doesn't set '{FIELD}', so it counts as written for ratect {declared}")
        }
        WrittenFor::YamlRoot => format!(
            "can't set '{FIELD}' in its YAML root, so it counts as written for ratect \
             {declared}"
        ),
    };
    let remedy = match written_for {
        // Only a YAML root that includes a TOML file gets here — see
        // `check_yaml_root` — and `config convert` reads every file as YAML,
        // so it can't carry a local one over.
        WrittenFor::YamlRoot => format!(
            "Its root file is Batect-format YAML, which can't declare a version: make the \
             root a ratect.toml — `ratect config convert` converts its YAML files, but a \
             local .toml include has to be carried over by hand — review the changes, then \
             set '{FIELD}' to {running} in it."
        ),
        _ => format!(
            "Review them (see CHANGELOG.md), then set '{FIELD}' to {running} to run it with \
             this version."
        ),
    };
    Err(Refusal {
        opening: format!("{heading}: This project {written}"),
        rows,
        remedy,
        running,
        scope: Scope::Project,
        fallback: root.map(|root| {
            let root = crate::config::absolute_path(root).unwrap_or_else(|_| root.to_path_buf());
            (format!("{root:?}"), root)
        }),
        assessment: None,
    }
    .into())
}

/// Every row of `table` newer than `declared`, in the table's order.
fn changes_since(declared: Version, table: &[BreakingChange]) -> Vec<BreakingChange> {
    table
        .iter()
        .filter(|row| {
            row.version
                .parse::<Version>()
                .is_ok_and(|version| version.release_line() > declared.release_line())
        })
        .copied()
        .collect()
}

/// One Git include entry reaching a bundle, as [`check_bundle`] judges it
/// (decisions/0020). Every entry is judged on its own, even one reaching a
/// bundle another entry already loaded.
pub(crate) struct BundleEntry<'a> {
    /// The bundle's entry file — the file the include names — as an error
    /// names it, with the bundle's repository and ref.
    pub(crate) entry_file: &'a str,
    /// The file declaring the include, as an error names it.
    pub(crate) declared_in: &'a str,
    /// Where that file sits if it is Batect-format (YAML), which can't set
    /// [`REVIEWED_FIELD`] (decisions/0021); `None` for a TOML file.
    pub(crate) declared_in_batect: Option<crate::include_trust::BatectFile>,
    /// What the entry file says, as [`read`] reads it: `YamlRoot` for a
    /// Batect-format entry file, which can't say.
    pub(crate) written_for: WrittenFor,
    /// The first TOML file a YAML entry file reaches through its own local
    /// includes, if any. A bundle of YAML files alone follows Batect's rules,
    /// so it is judged by no row, as a YAML root is.
    pub(crate) first_toml: Option<&'a str>,
    /// The include entry's `reviewed_ratect_version`.
    pub(crate) reviewed: Option<Version>,
    /// The bundle, which a refusal's detectors are scoped to.
    pub(crate) bundle: &'a crate::include_trust::BundleId,
    /// Where the entry file is — what a refusal's detectors can still read
    /// if the trial load fails, when it is TOML.
    pub(crate) entry_path: &'a Path,
}

/// The include-entry field a bundle's user acknowledges a review in.
pub const REVIEWED_FIELD: &str = "reviewed_ratect_version";

/// Refuses a bundle written or reviewed for a newer `ratect` than `running`,
/// or one a row of `table` has changed the meaning of since the later of
/// its own written-for version and its entry's reviewed-for version. An
/// unmarked bundle counts as written for 0.3.0, as an unmarked root does;
/// the root's own version never covers it.
///
/// The loader calls this before parsing a TOML entry file, so a bundle
/// written for a newer Ratect is refused for that rather than for whichever
/// of its fields this binary doesn't know.
pub(crate) fn check_bundle(
    entry: &BundleEntry<'_>,
    running: Version,
    table: &[BreakingChange],
) -> Result<()> {
    let BundleEntry {
        entry_file,
        declared_in,
        declared_in_batect,
        written_for,
        first_toml,
        reviewed,
        bundle,
        entry_path,
    } = entry;
    let refusing = || format!("Refusing to load {entry_file}, included by {declared_in}");
    let declared = match written_for {
        WrittenFor::Declared(version) => Some(*version),
        WrittenFor::Unmarked | WrittenFor::YamlRoot => None,
    };
    if let Some(declared) = declared.filter(|v| v.release_line() > running.release_line()) {
        anyhow::bail!(
            "{}: its '{FIELD}' is {declared}, so it needs ratect {}.{} or later; this is \
             ratect {running}. Upgrade ratect, or set the include's 'ref' to a release of the \
             bundle written for this version.",
            refusing(),
            declared.major,
            declared.minor
        );
    }
    if let Some(reviewed) = reviewed.filter(|v| v.release_line() > running.release_line()) {
        anyhow::bail!(
            "{}: the include's '{REVIEWED_FIELD}' is {reviewed}, newer than this ratect \
             ({running}), which can't check a review against changes it doesn't know. Upgrade \
             ratect, or set '{REVIEWED_FIELD}' to the version you reviewed the bundle against.",
            refusing()
        );
    }
    if *written_for == WrittenFor::YamlRoot && first_toml.is_none() {
        return Ok(());
    }
    let effective = [declared, *reviewed]
        .into_iter()
        .flatten()
        .max_by_key(|version| version.release_line())
        .unwrap_or(FIRST_NATIVE_RELEASE);
    let rows = changes_since(effective, table);
    if rows.is_empty() {
        return Ok(());
    }
    let mut written = match (written_for, first_toml) {
        (WrittenFor::Declared(version), _) => format!("was written for ratect {version}"),
        (WrittenFor::YamlRoot, Some(toml)) => format!(
            "has a Batect-format (YAML) entry file, which can't set '{FIELD}', and includes \
             {toml}, so it counts as written for ratect {FIRST_NATIVE_RELEASE}"
        ),
        _ => format!(
            "doesn't set '{FIELD}', so it counts as written for ratect {FIRST_NATIVE_RELEASE}"
        ),
    };
    if let Some(reviewed) = reviewed {
        written.push_str(&format!(
            ", and its include was reviewed for ratect {reviewed}"
        ));
    }
    let author = match written_for {
        WrittenFor::YamlRoot => format!(
            "its author can review them and give it a TOML entry file whose '{FIELD}' is \
             {running}"
        ),
        _ => format!("its author can review them and raise its '{FIELD}' to {running}"),
    };
    let convert_first = match declared_in_batect {
        Some(place) => format!(
            ", which a Batect-format (YAML) file can't set: first, {}",
            place.remedy()
        ),
        None => String::new(),
    };
    Err(Refusal {
        opening: format!("{}: the bundle {written}", refusing()),
        rows,
        remedy: format!(
            "Either {author}, or — once you have reviewed the bundle against them yourself — \
             set '{REVIEWED_FIELD}' to {running} on its include in {declared_in}{convert_first}."
        ),
        running,
        scope: Scope::Bundle((*bundle).clone()),
        fallback: (*written_for != WrittenFor::YamlRoot)
            .then(|| (entry_file.to_string(), entry_path.to_path_buf())),
        assessment: None,
    }
    .into())
}

/// A refusal for the rows of [`BREAKING_CHANGES`] a project or a bundle
/// predates — the error [`check`], [`check_yaml_root`] and a bundle's own
/// check during the load return for it, and which `ratect` then [`assess`](Self::assess)es: each
/// row gets an outcome, from a trial load and the row's [`Check`]
/// ([decisions/0022](../../decisions/0022-written-for-refusal-says-what-applies.md)).
/// The outcomes are advice; the refusal stands whatever they say.
#[derive(Debug, Clone)]
pub struct Refusal {
    /// What is refused, and the version it counts as written for.
    opening: String,
    rows: Vec<BreakingChange>,
    /// What to do about it.
    remedy: String,
    running: Version,
    scope: Scope,
    /// The file a detector can still read when the trial load fails — the
    /// root, or a bundle's entry file, when it is TOML — as a refusal names
    /// it and where it is. `None` for a YAML one.
    fallback: Option<(String, std::path::PathBuf)>,
    assessment: Option<Assessment>,
}

/// The files a refusal's detectors read: those its written-for version
/// covers.
#[derive(Debug, Clone)]
pub(crate) enum Scope {
    /// The root and every local include reached without crossing a Git
    /// include.
    Project,
    /// One bundle's own files — not those of a bundle it includes, which has
    /// a version of its own.
    Bundle(crate::include_trust::BundleId),
}

impl Scope {
    /// Whether a file of `bundle` (`None` for an owned one) is in scope.
    pub(crate) fn covers(&self, bundle: Option<&crate::include_trust::BundleId>) -> bool {
        match (self, bundle) {
            (Scope::Project, None) => true,
            (Scope::Bundle(scope), Some(bundle)) => {
                scope.remote == bundle.remote && scope.git_ref == bundle.git_ref
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
struct Assessment {
    /// One per row, in the same order.
    outcomes: Vec<Outcome>,
    /// The trial load's error, if it failed.
    load_error: Option<String>,
}

/// Whether one row touches what a refusal refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A [`Check::FailsTheLoad`] row, and the trial load succeeded.
    Loads,
    /// A [`Check::FailsTheLoad`] row, and the trial load failed — never a
    /// claim that this row is why.
    DoesNotLoad,
    /// A [`Check::Detected`] row whose detector found nothing.
    NoneFound,
    /// A [`Check::Detected`] row's uses — in every file in scope, or, when
    /// the trial load failed, in the root or a bundle's entry file alone.
    Found(Vec<Hit>),
    /// A [`Check::Detected`] row whose detector found nothing, but not
    /// every file could be looked in: the trial load failed, so only the
    /// root or a bundle's entry file was; a YAML file is in scope, which a
    /// detector can't read; or a file it read couldn't be read back.
    Unknown,
    /// A [`Check::NotChecked`] row.
    NotChecked,
}

impl Outcome {
    /// The row doesn't touch the project — the outcomes that need no review.
    pub fn rules_out(&self) -> bool {
        matches!(self, Outcome::Loads | Outcome::NoneFound)
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Loads => write!(f, "doesn't apply: it loads under these rules"),
            Outcome::DoesNotLoad => write!(f, "may apply: it doesn't load under these rules"),
            Outcome::NoneFound => write!(f, "none found"),
            Outcome::Found(hits) => {
                let hits: Vec<String> = hits
                    .iter()
                    .map(|hit| format!("'{}' in {}", hit.field, hit.file))
                    .collect();
                write!(f, "found: {}", hits.join("; "))
            }
            Outcome::Unknown => write!(f, "not known: not every file could be looked in"),
            Outcome::NotChecked => write!(f, "not checked"),
        }
    }
}

impl Refusal {
    /// This refusal with an outcome for each row. `trial` is the project
    /// loaded once more with the written-for checks set aside — for a bundle
    /// too, so whether it loads is the whole project's answer — and its
    /// files are what each [`Check::Detected`] row reads, as far as this
    /// refusal's version covers them. When it doesn't load, which files
    /// there are isn't known, so a detector reads the root, or the bundle's
    /// entry file, alone: a use found there is still found.
    pub fn assess(
        &self,
        trial: std::result::Result<&crate::config::LoadedProject, &anyhow::Error>,
    ) -> Refusal {
        match trial {
            Ok(project) => match project.toml_documents(&self.scope) {
                Ok((files, true)) => self.with_outcomes(None, Readable::Every(&files)),
                Ok((files, false)) => self.with_outcomes(None, Readable::Only(&files)),
                Err(_) => self.with_outcomes(None, Readable::Only(&[])),
            },
            Err(error) => {
                let fallback: Vec<TomlFile> = self
                    .fallback
                    .iter()
                    .filter_map(|(name, path)| {
                        let document = std::fs::read_to_string(path).ok()?.parse().ok()?;
                        Some(TomlFile {
                            name: name.clone(),
                            document,
                        })
                    })
                    .collect();
                self.with_outcomes(Some(format!("{error:#}")), Readable::Only(&fallback))
            }
        }
    }

    /// [`assess`](Self::assess) once the trial load is known: its error, if
    /// it failed, and the files a detector can read.
    fn with_outcomes(&self, load_error: Option<String>, readable: Readable<'_>) -> Refusal {
        let outcomes = self
            .rows
            .iter()
            .map(|row| match row.check {
                Check::NotChecked => Outcome::NotChecked,
                Check::FailsTheLoad if load_error.is_none() => Outcome::Loads,
                Check::FailsTheLoad => Outcome::DoesNotLoad,
                Check::Detected(detect) => {
                    let (Readable::Every(files) | Readable::Only(files)) = readable;
                    let hits = detect(files);
                    match readable {
                        _ if !hits.is_empty() => Outcome::Found(hits),
                        Readable::Every(_) => Outcome::NoneFound,
                        Readable::Only(_) => Outcome::Unknown,
                    }
                }
            })
            .collect();
        Refusal {
            assessment: Some(Assessment {
                outcomes,
                load_error,
            }),
            ..self.clone()
        }
    }

    /// What is refused, and why, up to the list of changes.
    pub fn opening(&self) -> String {
        format!(
            "{}, and ratect has changed what a configuration means since:",
            self.opening
        )
    }

    /// One line per change, each with its outcome once assessed.
    pub fn rows(&self) -> Vec<(String, Option<&Outcome>)> {
        let mut outcomes = self
            .assessment
            .iter()
            .flat_map(|assessment| assessment.outcomes.iter());
        self.rows
            .iter()
            .map(|row| {
                let line = format!("ratect {}: {}", row.version, row.summary);
                match outcomes.next() {
                    Some(outcome) => (format!("{line} — {outcome}"), Some(outcome)),
                    None => (line, None),
                }
            })
            .collect()
    }

    /// What the trial load found, said once — `None` until assessed, then
    /// `Ok` when the project loads and `Err` when it doesn't.
    pub fn trial_load(&self) -> Option<std::result::Result<String, String>> {
        let assessment = self.assessment.as_ref()?;
        let rules = "With this check set aside, the project";
        Some(match &assessment.load_error {
            None => Ok(format!(
                "{rules} loads under ratect {}'s rules.",
                self.running
            )),
            Some(error) => Err(format!(
                "{rules} doesn't load under ratect {}'s rules: {error}",
                self.running
            )),
        })
    }

    /// What to do about it.
    pub fn remedy(&self) -> &str {
        &self.remedy
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.opening())?;
        for (line, _) in self.rows() {
            write!(f, "\n  - {line}")?;
        }
        if let Some(Ok(trial) | Err(trial)) = self.trial_load() {
            write!(f, "\n{trial}")?;
        }
        write!(f, "\n{}", self.remedy)
    }
}

impl std::error::Error for Refusal {}

/// The files a refusal's detectors can read.
#[derive(Clone, Copy)]
enum Readable<'a> {
    /// Every file in scope, so finding nothing means there is nothing.
    Every(&'a [TomlFile]),
    /// Only some of them, so finding nothing proves nothing.
    Only(&'a [TomlFile]),
}

#[cfg(test)]
#[path = "written_for_tests.rs"]
mod tests;
