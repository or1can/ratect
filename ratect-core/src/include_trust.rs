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

//! What a bundle a project pulls in over Git is allowed to do — the *grants*
//! half of CONTEXT.md's **Boundary**. The containment half (which directory a
//! bundle's includes and container paths must stay inside) lives with the
//! paths it checks, in [`crate::config`].
//!
//! Crate-internal, so `cargo doc` renders this only with
//! `--document-private-items`: a binary picks a trust policy by choosing a
//! `config` entry point, never by naming these types.
//!
//! # Why this is a module
//!
//! Two walkers decide this: [`crate::config::Config::load_from_file`], which
//! loads a project, and [`crate::config::task_names_for_completion`], which
//! must offer the same task names on `<TAB>` that a load would produce. They
//! used to spell the rule out separately —
//!
//! ```text
//! allow_host_paths          && owner_declared        // loader
//! allow_nested.unwrap(false) && owner_declared        // loader
//! granted.unwrap_or(false)   && self.owner            // completion
//! ```
//!
//! — with nothing making them agree, and a test named as though it compared
//! them that in fact only asserted one of the two. Every trust defect found
//! while this shipped was a site guarded correctly in isolation next to one
//! that wasn't. Deriving the rule once, here, is what makes the divergence
//! unrepresentable rather than remembered.
//!
//! # The rule
//!
//! A grant counts only when the file *declaring* the include is one the
//! project owner controls: the root configuration file, or something it
//! reached without crossing a Git include. Inside a bundle the flags are
//! ignored, so a bundle can neither grant itself anything nor pass on what it
//! was granted. That is what keeps a grant one level deep rather than a
//! subtree — the bundle it admits gets [`Trust::NONE`], and no owner-controlled
//! file exists further down to re-grant.
//!
//! [`Grants`] is what an include entry *asks for*; [`Trust`] is what the file
//! it reaches *ends up with*. They are different types on purpose: conflating
//! the two is how a grant came to be silently discarded (see
//! [`EffectiveGrants`]).
//!
//! # Two asymmetries worth knowing
//!
//! `allow_host_paths` is enforced in both dialects — see
//! [decisions/0004](../../decisions/0004-git-include-host-path-trust.md).
//! `allow_nested_git_includes` is native-only: a `batect.yml` has no gate at
//! all, matching Batect, which is why [`restricting`] takes the dialect and
//! every native-only behaviour keys off its single answer.
//!
//! # Errors live here too
//!
//! All four refusals are constructed in this module rather than at their call
//! sites. They are one policy explained four ways, and holding them together
//! is what keeps them consistent about the two rules they share: name the
//! *field*, never spell the config syntax (a native project can include a
//! `.yml`, so there is no one right spelling), and name something the reader
//! actually wrote — which for a bundle means naming the bundle, since a
//! `ratect-compat` user cannot edit a file they have never seen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::ConfigFormat;

/// What a `type: git` include entry asks for. Built from the entry's own
/// fields at the single place each walker destructures one, so neither passes
/// two bare `bool`s in the wrong order.
///
/// `nested_git` is `Option` because absent and `false` differ: writing it in a
/// `batect.yml` is refused either way (see [`check_dialect`]), and only an
/// entry that wrote *something* can be refused for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Grants {
    pub host_paths: bool,
    pub nested_git: Option<bool>,
}

/// The grants a file is actually loaded under. Distinct from [`Grants`]: an
/// entry asks, a file carries, and the two differ whenever a bundle asked for
/// something it is not in a position to be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Trust {
    /// This bundle's containers may resolve `volumes`/`build_directory`/
    /// `build_secrets` paths anywhere, rather than only within its own clone
    /// or the project directory.
    pub host_paths: bool,
    /// This bundle's own `type: git` includes may be followed, so it may
    /// redirect the load to remotes the project owner never named.
    pub nested_git: bool,
}

impl Trust {
    /// Granted nothing — what every bundle carries unless an owner-controlled
    /// file said otherwise, and what a bundle admitted by another bundle
    /// always carries.
    pub(crate) const NONE: Self = Self {
        host_paths: false,
        nested_git: false,
    };
}

/// How a bundle is named back to whoever has to act on an error: exactly the
/// two things they wrote, and nothing derived (a clone directory is Ratect's
/// business, not theirs).
#[derive(Debug, Clone)]
pub(crate) struct BundleId {
    pub remote: String,
    pub git_ref: String,
}

/// A bundle as the trust rules see it — how it is named, and what it carries.
///
/// Identity travels with the grants deliberately. Every decision this module
/// makes has an error that must name the bundle responsible, and keeping the
/// two apart is what previously forced the caller to re-derive the decision in
/// order to render the message.
#[derive(Debug, Clone)]
pub(crate) struct Bundle {
    pub id: BundleId,
    pub trust: Trust,
}

impl Bundle {
    /// The bundle `id` as reached by an entry asking for `asked`, declared by
    /// a file whose own origin is `declaring` (`None` for an owned file — the
    /// root configuration, or anything it reached without crossing a Git
    /// include).
    ///
    /// This is the whole rule. Both walkers call it and neither restates it.
    pub(crate) fn granted(declaring: Option<&Bundle>, id: BundleId, asked: Grants) -> Bundle {
        let owner_declared = declaring.is_none();
        Bundle {
            id,
            trust: Trust {
                host_paths: asked.host_paths && owner_declared,
                nested_git: asked.nested_git.unwrap_or(false) && owner_declared,
            },
        }
    }
}

/// The bundle whose permission a `type: git` include needs, or `None` when
/// nothing restricts it — an owned file declared it, or the project is
/// Batect-compatible and has no such gate.
///
/// The single derived value every native-only behaviour keys off. The first
/// cut of this gate guarded the refusal and left [`hide_clone_detail`]
/// unguarded, silently changing `ratect-compat`; no test caught it, because
/// the redaction's tests were all native. Returning the bundle rather than a
/// `bool` is what lets both consumers share one answer instead of each
/// deriving its own.
pub(crate) fn restricting(declaring: Option<&Bundle>, format: ConfigFormat) -> Option<&Bundle> {
    match format {
        ConfigFormat::Native => declaring,
        ConfigFormat::Compat => None,
    }
}

/// Refuses `allow_nested_git_includes` in a Batect-compatible project, where
/// it is not merely inert but a field Batect has never had.
///
/// Rejects `false` as well as `true`: the field is unsupported, not the
/// permission. Names *where it was written*, because it can appear inside a
/// bundle — and an error about "the Git include of X" would then describe a
/// file the `ratect-compat` user has never seen.
pub(crate) fn check_dialect(
    asked: Grants,
    declaring: Option<&Bundle>,
    repo: &str,
    format: ConfigFormat,
) -> Result<()> {
    if asked.nested_git.is_none() || !matches!(format, ConfigFormat::Compat) {
        return Ok(());
    }
    let subject = match declaring {
        Some(declaring) => format!(
            "The bundle '{}' at '{}' sets 'allow_nested_git_includes' on its \
             Git include of '{repo}'",
            declaring.id.remote, declaring.id.git_ref
        ),
        None => {
            format!("The Git include of '{repo}' sets 'allow_nested_git_includes'")
        }
    };
    anyhow::bail!(
        "{subject}, which is a ratect-native field not supported in \
         Batect-compatible configuration. A bundle may declare Git includes \
         of its own here regardless, matching Batect."
    );
}

/// The bundle that refuses this Git include, or `None` if it may proceed —
/// [`restricting`]'s answer narrowed to those that withhold the grant.
///
/// Returns the bundle rather than a `bool` so [`check_may_declare_git`] can
/// name it without asking twice. Completion takes the `bool` view (it has no
/// error to raise — it just declines to offer tasks that `ratect run` would
/// then refuse), and takes it from *here*, so neither walker restates when an
/// include is permitted.
pub(crate) fn refusing_nested_git(restricted: Option<&Bundle>) -> Option<&Bundle> {
    restricted.filter(|declaring| !declaring.trust.nested_git)
}

/// [`refusing_nested_git`], rendered as the refusal with its remedy.
///
/// Called *before* the clone, so a bundle naming an unreachable remote cannot
/// even be used to probe for one.
pub(crate) fn check_may_declare_git(restricted: Option<&Bundle>, repo: &str) -> Result<()> {
    let Some(declaring) = refusing_nested_git(restricted) else {
        return Ok(());
    };
    anyhow::bail!(
        "The bundle '{}' at '{}' declares a Git include of its own \
         ('{repo}'), which would fetch and run configuration from a \
         remote you have not named. Set 'allow_nested_git_includes' to \
         true on that bundle's own include entry to accept this.",
        declaring.id.remote,
        declaring.id.git_ref
    );
}

/// Attributes a failed Git-include clone, keeping `git`'s own stderr only
/// when the project owner named the remote themselves.
///
/// `restricted` is [`restricting`]'s answer, so this and
/// [`check_may_declare_git`] can never disagree about which includes are a
/// bundle's own. `None` covers both the root config's own includes and every
/// include in a `batect.yml`, where withholding git's error would be a parity
/// break rather than hardening. There, the full error is what someone
/// debugging their own typo needs, and it reveals nothing they didn't write.
///
/// Otherwise the remote was chosen by a third-party bundle, and git's stderr
/// distinguishes host-unreachable from connection-refused from
/// repository-not-found from auth-failed. A bundle able to name hosts and
/// read the resulting CI log can walk an internal network one include at a
/// time; the same log is often visible to whoever can propose a change to
/// that bundle. The detail goes to `RUST_LOG=debug` instead, which is a
/// deliberate trade: the person who can read the debug log is the person
/// running the build, not the person who wrote the bundle.
pub(crate) fn hide_clone_detail(
    error: anyhow::Error,
    repo: &str,
    git_ref: &str,
    restricted: Option<&Bundle>,
) -> anyhow::Error {
    let Some(declaring) = restricted else {
        return error.context(format!(
            "Failed to resolve Git include '{repo}' at '{git_ref}'"
        ));
    };
    tracing::debug!(
        error = format!("{error:#}"),
        repo,
        git_ref,
        "Failed to resolve a nested Git include"
    );
    anyhow::anyhow!(
        "Failed to resolve the Git include of '{repo}' at '{git_ref}', declared by the bundle \
         '{}' at '{}'. The underlying error is not shown here because that remote was named by \
         the bundle rather than by you — re-run with RUST_LOG=debug to see it.",
        declaring.id.remote,
        declaring.id.git_ref
    )
}

/// The trust each already-loaded file was loaded under — CONTEXT.md's
/// **effective boundary**, recorded so a grant that arrived too late to take
/// effect can be reported instead of silently doing nothing.
///
/// A file is loaded once, so a repository reachable by more than one route
/// keeps whichever route arrived first. A later entry then carries grants that
/// cannot apply — and silently, which is the one outcome a trust boundary must
/// not have, since the flag is written precisely by someone who has no other
/// way to tell whether it took.
#[derive(Debug, Default)]
pub(crate) struct EffectiveGrants(HashMap<PathBuf, Trust>);

impl EffectiveGrants {
    /// Records the trust `file` was loaded under, on first arrival.
    pub(crate) fn record(&mut self, file: PathBuf, trust: Trust) {
        self.0.insert(file, trust);
    }

    /// Errors when an entry carrying `wanted` reaches an already-loaded
    /// `file` that was loaded with less.
    ///
    /// Only ever refuses a permission being *lost*. A route granting less than
    /// the winning one is the ordinary case — a bundle reaching a repository
    /// the owner also vouched for — and erroring on it would break
    /// configurations that work today for no gain, since the stricter ask is
    /// already satisfied.
    ///
    /// Asked only for a `type: git` entry, which is the only kind that carries
    /// a grant of its own — hence `repo` being a plain `&str` rather than an
    /// `Option`. A local include inherits its declaring file's boundary and
    /// asks for nothing, so it can arrive second carrying more trust than the
    /// winner without any grant having been written, let alone lost.
    pub(crate) fn check(&self, file: &Path, wanted: Trust, repo: &str) -> Result<()> {
        let effective = self.0.get(file).copied().unwrap_or(Trust::NONE);
        let lost = match (wanted, effective) {
            (
                Trust {
                    host_paths: true, ..
                },
                Trust {
                    host_paths: false, ..
                },
            ) => "allow_host_paths",
            (
                Trust {
                    nested_git: true, ..
                },
                Trust {
                    nested_git: false, ..
                },
            ) => "allow_nested_git_includes",
            _ => return Ok(()),
        };
        anyhow::bail!(
            "'{lost}' was set on the include of '{repo}', but that repository was \
             already reached through an earlier include, and a file is only \
             loaded once — so the permission would have had no effect. Move \
             '{lost}' onto whichever include of '{repo}' is resolved first — \
             every include in the root configuration file is resolved before \
             any bundle's own — or remove the '{lost}' that cannot apply."
        );
    }
}

#[cfg(test)]
#[path = "include_trust_tests.rs"]
mod tests;
