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
}

/// Every breaking change to what a `ratect.toml` means or accepts, oldest
/// first. See the module documentation before adding one.
pub const BREAKING_CHANGES: &[BreakingChange] = &[
    BreakingChange {
        version: "0.12.0",
        summary: "a '\\' in an expression escapes the character after it and is dropped, and \
                  a trailing '\\' fails the load",
    },
    BreakingChange {
        version: "0.12.0",
        summary: "every '$' or '<' in an expression starts a reference, so a malformed one \
                  ('$$', 'a < b', '${X:default}') fails the load, and an unbraced name ends at \
                  '.' or '-' (write '<{batect.project_directory}')",
    },
    BreakingChange {
        version: "0.12.0",
        summary: "a task or config variable name Batect refuses fails the load: a task name \
                  is letters, digits, ':', '-', '.' and '_', starting and ending with a letter \
                  or digit; a config variable name is letters, digits, '-', '.' and '_', \
                  starting with a letter and not with 'batect'",
    },
    BreakingChange {
        version: "0.12.0",
        summary: "'project_name' and every container name must be a Docker image-name \
                  component, or the load fails: lowercase letters and digits, joined by a \
                  single '.', one or two '_', or any number of '-'",
    },
    BreakingChange {
        version: "0.12.0",
        summary: "an included file that sets 'forbid_telemetry' to true fails the load; only \
                  the root file may",
    },
    BreakingChange {
        version: "0.12.0",
        summary: "a string 'volumes' or 'devices' entry with an empty options segment \
                  ('a:b:') fails the load",
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
    check_against(&written_for, running, BREAKING_CHANGES)
        .with_context(|| format!("Refusing to load {}", root.display()))
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
    check_against(&WrittenFor::YamlRoot, running, table).with_context(|| {
        format!(
            "Refusing to load {:?}, which includes {first}: a TOML file follows ratect's own \
             rules",
            root
        )
    })
}

/// [`check`]'s decision, over an explicit table so it can be tested with
/// rows the real one doesn't have yet.
fn check_against(
    written_for: &WrittenFor,
    running: Version,
    table: &[BreakingChange],
) -> Result<()> {
    let declared = match written_for {
        WrittenFor::Declared(version) => *version,
        WrittenFor::Unmarked | WrittenFor::YamlRoot => FIRST_NATIVE_RELEASE,
    };
    if declared.release_line() > running.release_line() {
        anyhow::bail!(
            "This project's '{FIELD}' is {declared}, so it needs ratect {}.{} or later; \
             this is ratect {running}.",
            declared.major,
            declared.minor
        );
    }
    let since: Vec<&BreakingChange> = table
        .iter()
        .filter(|row| {
            row.version
                .parse::<Version>()
                .is_ok_and(|version| version.release_line() > declared.release_line())
        })
        .collect();
    if since.is_empty() {
        return Ok(());
    }
    let changes = since
        .iter()
        .map(|row| format!("\n  - ratect {}: {}", row.version, row.summary))
        .collect::<String>();
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
    let next = match written_for {
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
    anyhow::bail!(
        "This project {written}, and ratect has changed what a configuration means since:\
         {changes}\n{next}"
    )
}

#[cfg(test)]
#[path = "written_for_tests.rs"]
mod tests;
