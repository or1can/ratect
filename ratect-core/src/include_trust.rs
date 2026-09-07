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

//! CONTEXT.md's whole **Boundary** — what a bundle a project pulls in over
//! Git is contained within, together with what it's allowed to do.
//! [`Boundary`] carries both halves: the containment (which directory its
//! includes and container paths must stay inside) and the grants (`Bundle`'s
//! `trust`), together, since every containment failure and every grants
//! decision has to name the same bundle.
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
//! The same shape of defect was still possible one level up: both walkers
//! called [`restricting`]/[`refusing_nested_git`] the same way, but each
//! then decided *whether a file may be read at all* by hand-combining that
//! answer with its own containment check, in its own words. [`check_may_declare_git`]
//! and [`boundary_contains`] are the two checks that decision needs — nested-Git
//! first, before anything about the target is resolved, then containment once a
//! candidate path exists — and each already returns `Result<()>` on its own, so
//! neither walker recombines them differently or in a different order: the
//! loader raises whichever fails with `?`, and completion turns either into a
//! decline through [`refuse_read`].
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

use anyhow::{Context, Result};
use path_clean::PathClean;

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

/// The Git-clone boundary a file's own further `include` entries must stay
/// within, once traversal has crossed from the caller's own local project
/// tree into a Git-included bundle's content — see the security note on
/// [`crate::config::Config::load_from_file_with_git_cache`]. Propagated
/// through that function's traversal queue: a local file include inherits
/// its declaring file's own boundary unchanged; a `type: git` include always
/// establishes a fresh one, rooted at its own newly (or previously) cloned
/// repository, regardless of the declaring file's own boundary.
///
/// CONTEXT.md's whole **Boundary**: `repo_dir` is the containment half,
/// `bundle` the grants half, carried together since every containment
/// failure has to name the bundle the path came from.
#[derive(Debug, Clone)]
pub(crate) struct Boundary {
    pub repo_dir: PathBuf,
    /// A grant deliberately does *not* relax the include-`path` containment
    /// ([`check_contains`](Self::check_contains)): that stops a bundle pulling
    /// an arbitrary host *file* into the configuration, which is a separate
    /// concern from where its containers may mount.
    pub bundle: Bundle,
}

/// Which of `Boundary::check_path_allowed`'s two checks refused a path.
/// Named rather than passed as a message fragment so a third check cannot be
/// added by inventing a third string at one call site.
enum Escape {
    /// The path's spelling already leaves both allowed roots.
    Lexical,
    /// Its spelling stays inside, but what it really points at does not.
    ViaSymlink,
}

impl Boundary {
    /// Purely lexical containment check — deliberately runs before
    /// `resolved` is confirmed to exist, so a `path` engineered to escape
    /// (an absolute path, or a `../..` traversal) is rejected without ever
    /// touching the filesystem at the escaped location.
    ///
    /// Normalizes `resolved` itself rather than trusting the caller to. The
    /// comparison is [`Path::starts_with`], which matches components without
    /// interpreting any of them, so `<repo_dir>/../../elsewhere` starts with
    /// `<repo_dir>` and passes — the check is inert on exactly the input it
    /// exists to reject. Two call sites got this wrong (completion's walk, and
    /// [`crate::config::resolve_path`]'s absolute branch), which is one more
    /// than a convention survives; cleaning here makes the mistake
    /// unrepresentable.
    pub(crate) fn check_contains(&self, resolved: &Path) -> Result<()> {
        let resolved = &resolved.clean();
        if resolved.starts_with(&self.repo_dir) {
            return Ok(());
        }
        anyhow::bail!(
            "Included file '{}' escapes the Git repository '{}' at '{}' it was included from \
             — includes reached through a Git include must resolve within that repository.",
            resolved.display(),
            self.bundle.id.remote,
            self.bundle.id.git_ref
        );
    }

    /// A second check against the *canonicalized* (symlink-resolved) form
    /// of both paths, once `resolved` is confirmed to exist — closes the
    /// gap `check_contains` alone can't: a malicious repository planting a
    /// symlink inside its own clone that itself points back outside it
    /// would still lexically "start with" `repo_dir`.
    fn check_contains_canonical(&self, resolved: &Path) -> Result<()> {
        let canonical_resolved = resolved
            .canonicalize()
            .with_context(|| format!("Failed to resolve {resolved:?}"))?;
        let canonical_root = self
            .repo_dir
            .canonicalize()
            .with_context(|| format!("Failed to resolve {:?}", self.repo_dir))?;
        if canonical_resolved.starts_with(&canonical_root) {
            return Ok(());
        }
        anyhow::bail!(
            "Included file '{}' escapes the Git repository '{}' at '{}' it was included from \
             (via a symlink) — includes reached through a Git include must resolve within that \
             repository.",
            resolved.display(),
            self.bundle.id.remote,
            self.bundle.id.git_ref
        );
    }

    /// Containment check for a Git-included container's path-bearing fields
    /// (`volumes` host paths, `build_directory`) — see the security note on
    /// [`crate::config::Config::resolve_expressions_with_boundaries`]. Unlike
    /// `check_contains`/`check_contains_canonical` above (used only for
    /// further `include` resolution, which must stay entirely within the
    /// repository), a shared bundle may reasonably want to reference the
    /// caller's own project directory (e.g.
    /// `<{batect.project_directory}/output:/output`) — so `project_dir` is
    /// accepted as a second allowed root alongside the repository's own
    /// clone directory.
    ///
    /// Checked twice, because either check alone has a hole the other closes.
    /// Lexically first, on the normalized path — that rejects a written-out
    /// escape without touching the filesystem where it points, and it was not
    /// merely theoretical: `<{batect.project_directory}/../../../etc` is an
    /// absolute path a bundle can write knowing nothing about the machine, and
    /// it starts with the project directory component-for-component. Then
    /// against the real locations, because a bundle can commit a *symlink*
    /// inside its own clone, which is lexically within an allowed root while
    /// its target is not — and Docker dereferences it at bind-mount time.
    ///
    /// The second check resolves as far as the path exists rather than
    /// requiring it to, unlike
    /// [`check_contains_canonical`](Self::check_contains_canonical): an
    /// `include` target must already exist to be read, but a
    /// `volumes`/`build_directory` path need not — Ratect or Docker creates
    /// it. Both allowed roots are resolved too, since a project or cache
    /// directory may itself sit under a symlink (`/tmp` does on macOS).
    pub(crate) fn check_path_allowed(&self, resolved: &Path, project_dir: &Path) -> Result<()> {
        let resolved = &resolved.clean();
        if self.bundle.trust.host_paths {
            return Ok(());
        }
        if !(resolved.starts_with(&self.repo_dir) || resolved.starts_with(project_dir)) {
            return Err(self.refuse_escape(resolved, project_dir, Escape::Lexical));
        }
        let real = self.real_path(resolved)?;
        if real.starts_with(self.real_path(&self.repo_dir)?)
            || real.starts_with(self.real_path(project_dir)?)
        {
            return Ok(());
        }
        Err(self.refuse_escape(resolved, project_dir, Escape::ViaSymlink))
    }

    /// [`real_path_as_far_as_it_exists`] with this bundle's name attached: the
    /// helper knows a path, not which include pulled it in, and a refusal has
    /// to say. Worded to read correctly for all three paths it is called on —
    /// the candidate and both allowed roots — since a root failing to resolve
    /// is not the caller's path being at fault.
    fn real_path(&self, path: &Path) -> Result<PathBuf> {
        real_path_as_far_as_it_exists(path).with_context(|| {
            format!(
                "Cannot determine where '{}' really points, so the containment for the \
                 Git repository '{}' at '{}' cannot be checked.",
                path.display(),
                self.bundle.id.remote,
                self.bundle.id.git_ref
            )
        })
    }

    /// The refusal both halves of [`check_path_allowed`](Self::check_path_allowed)
    /// raise. Which half it was changes only how the path is described; the
    /// remedy is the same either way, and is the part that has to name the
    /// field rather than spell its syntax, since the include entry it points
    /// at may be in either format.
    fn refuse_escape(&self, resolved: &Path, project_dir: &Path, escape: Escape) -> anyhow::Error {
        let how = match escape {
            Escape::Lexical => "",
            Escape::ViaSymlink => " (via a symlink)",
        };
        anyhow::anyhow!(
            "Path '{}' escapes both the Git repository '{}' at '{}' it was included from and \
             the project directory '{}'{} — a container reached through a Git include must \
             resolve its 'volumes'/'build_directory' paths within one of the two. If you trust \
             this bundle to reach that path, set 'allow_host_paths' to true on the include entry \
             for '{}' in your own configuration.",
            resolved.display(),
            self.bundle.id.remote,
            self.bundle.id.git_ref,
            project_dir.display(),
            how,
            self.bundle.id.remote
        )
    }
}

/// Whether `resolved` — an include's already-resolved target — stays inside
/// `boundary`'s repository, checked both ways for the same reason
/// [`Boundary::check_path_allowed`] is: lexically first
/// ([`Boundary::check_contains`]), then against the symlink-resolved form
/// ([`Boundary::check_contains_canonical`]), since either alone has a gap
/// the other closes. `None` means no boundary applies at all — an owned
/// file, contained by nothing — so nothing is checked. The two checks the
/// loader and completion each call directly, alongside
/// [`check_may_declare_git`], to decide whether a file may be read at all.
pub(crate) fn boundary_contains(boundary: Option<&Boundary>, resolved: &Path) -> Result<()> {
    let Some(boundary) = boundary else {
        return Ok(());
    };
    boundary.check_contains(resolved)?;
    boundary.check_contains_canonical(resolved)
}

/// `path` with every symlink in it resolved, as far as it exists: the longest
/// existing ancestor canonicalized, with the not-yet-created tail re-appended.
///
/// A plain [`Path::canonicalize`] can't be used because the path may not exist
/// yet — that is the whole reason
/// `Boundary::check_path_allowed` can't simply reuse
/// [`Boundary::check_contains_canonical`]. A component that is merely
/// *missing* is therefore expected, and resolution continues above it: a path
/// with no existing ancestor cannot be pointing anywhere yet, so the caller's
/// lexical check is the only one that can apply to it.
///
/// Any *other* failure is an error rather than a shrug. It means this process
/// cannot see where the path leads — an ancestor it may not search, a symlink
/// loop — while the Docker daemon that will dereference it runs as root and is
/// under no such restriction. Treating that as "resolves to itself" would let
/// a path be judged on its spelling by the one check that exists to look past
/// spelling.
fn real_path_as_far_as_it_exists(path: &Path) -> std::io::Result<PathBuf> {
    let mut missing_tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut existing = path;
    loop {
        match existing.canonicalize() {
            Ok(canonical) => {
                return Ok(missing_tail
                    .iter()
                    .rev()
                    .fold(canonical, |real, part| real.join(part)));
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            Err(_) => {}
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing_tail.push(name);
                existing = parent;
            }
            _ => return Ok(path.to_path_buf()),
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

/// Turns a refusal into completion's decline: no error to raise, just
/// whether something refused this file. Shared by both checks completion
/// makes — [`check_may_declare_git`]'s nested-Git refusal, and, once a
/// candidate target is known, [`boundary_contains`]'s containment escape —
/// so completion reads either the same way it reads the other, and the
/// loader (which raises the same `Result<()>` with `?` instead) can never
/// drift into treating one differently from the other.
pub(crate) fn refuse_read(check: Result<()>) -> bool {
    check.is_err()
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
/// Keyed on the *file*, which is the unit that is loaded once — not on the
/// repository, of which two entries may pull in different files with a `path`
/// each, neither racing the other. A file reachable by more than one route
/// keeps whichever route arrived first; a later entry then carries grants that
/// cannot apply — and silently, which is the one outcome a trust boundary must
/// not have, since the flag is written precisely by someone who has no other
/// way to tell whether it took.
/// `None` against a file means it was loaded under **no boundary at all** —
/// the project owner's own tree — which is not the same as a boundary that
/// grants nothing, and is the distinction [`Trust`] alone cannot carry. An
/// unbounded file is contained by nothing and may resolve any host path, so
/// it is strictly more permissive than any grant; a later route reaching it
/// has lost nothing, however much that route was granted.
#[derive(Debug, Default)]
pub(crate) struct EffectiveGrants(HashMap<PathBuf, Option<Trust>>);

impl EffectiveGrants {
    /// Records what `file` was loaded under, on first arrival — `None` for an
    /// owned file, which has no boundary.
    ///
    /// First arrival wins *here*, not only in the caller's own dedup: this is
    /// the answer every later route is compared against, so a second write
    /// would silently redefine what "already loaded with" means.
    pub(crate) fn record(&mut self, file: PathBuf, trust: Option<Trust>) {
        self.0.entry(file).or_insert(trust);
    }

    /// Errors when an entry carrying `wanted` reaches an already-loaded
    /// `file` that was loaded with less.
    ///
    /// Only ever refuses a permission being *lost*. A route granting less than
    /// the winning one is the ordinary case — a bundle reaching a file the
    /// owner also vouched for — and erroring on it would break
    /// configurations that work today for no gain, since the stricter ask is
    /// already satisfied.
    ///
    /// `repo` is whichever repository the lost grant was written against — the
    /// entry's own for a `type: git` include, and for a local include the
    /// bundle it sits in, whose entry is where the owner wrote the flag. It is
    /// a plain `&str` rather than an `Option` because trust only ever comes
    /// from a boundary, so anything with something to lose has a repository to
    /// name; the caller skips this entirely when there is neither.
    ///
    /// The one rule in this module that shell completion deliberately does
    /// *not* mirror, because it fires on an already-loaded file and so changes
    /// no file's contents — see [`crate::config::task_names_for_completion`].
    pub(crate) fn check(&self, file: &Path, wanted: Trust, repo: &str) -> Result<()> {
        let effective = match self.0.get(file) {
            // Loaded outside every boundary, so it is contained by nothing and
            // a grant would only ever have narrowed it. Nothing to lose.
            Some(None) => return Ok(()),
            Some(Some(trust)) => *trust,
            // Never recorded, which the traversal makes unreachable — assume
            // the strictest thing it could have been rather than the loosest,
            // so a gap here fails loudly instead of waving a grant through.
            None => Trust::NONE,
        };
        // Every lost field, not the first: an entry can carry both, and
        // reporting one would send the reader round the loop again for the
        // other after they had already fixed what they were told about.
        let mut lost = Vec::new();
        if wanted.host_paths && !effective.host_paths {
            lost.push("allow_host_paths");
        }
        if wanted.nested_git && !effective.nested_git {
            lost.push("allow_nested_git_includes");
        }
        if lost.is_empty() {
            return Ok(());
        }
        let named = lost.join("' and '");
        let (were, permission) = if lost.len() == 1 {
            ("was", "the permission")
        } else {
            ("were", "the permissions")
        };
        anyhow::bail!(
            "'{named}' {were} set on the include of '{repo}', but the file it \
             pulls in had already been reached through an earlier include, and \
             a file is only loaded once — so {permission} would have had no \
             effect. Move '{named}' onto whichever include reaches that file \
             first — every include in the root configuration file is resolved \
             before any bundle's own — or remove what cannot apply."
        );
    }
}

#[cfg(test)]
#[path = "include_trust_tests.rs"]
mod tests;
