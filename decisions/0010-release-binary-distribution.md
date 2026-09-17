# 0010 — Automated release pipeline: prebuilt binaries, SBOM, provenance

## Status

Accepted — implemented (ratect-compat 0.28.0 · ratect 0.7.0).

## Context

Before this, Ratect published no binaries at all — `docs/installation.md`
said so explicitly. Every user needed a Rust toolchain and `cargo build
--release` just to run either binary, and cutting a release meant a
maintainer hand-building artifacts and hand-creating the GitHub Release.
There was also no way for anyone to verify that a downloaded binary
actually came from Ratect's own CI, built from the source it claims to.

Two things this repo's own conventions had to accommodate rather than
work around:

- **Independent version lines** ([0001](0001-two-binaries.md)) — `ratect`
  and `ratect-compat` tag and release independently
  (`PACKAGE/vX.Y.Z`), never in lockstep.
- **A shared, combined-heading `CHANGELOG.md`** (guideline 8) — a release
  heading names every version in that release at once, e.g.
  `## [ratect-compat 0.27.0 · ratect 0.6.0]`, since a shared-core change
  is released for both binaries together.

## Decision

`cargo-dist`, configured via a root `dist-workspace.toml`, with a
GitHub Actions workflow it generates (`.github/workflows/release.yml`)
plus three hand-added customizations that file's own header comment
documents in full and `dist-workspace.toml`'s `allow-dirty = ["ci"]`
keeps from being overwritten on regeneration.

- **Trigger**: pushing a binary's own version tag (the existing
  guideline-8 process) runs the pipeline end to end and publishes the
  GitHub Release with every artifact attached — no remaining manual
  step. `dist`'s native "Singular" (`PACKAGE/vX.Y.Z`) tag support means
  each push announces exactly one binary, matching the independent
  version lines with no workaround needed.
- **Target matrix**: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`.
  Every target builds on a native GitHub-hosted runner for its own
  architecture — confirmed via `dist plan`'s own manifest, not assumed —
  so nothing here is QEMU- or Rosetta-emulated.
- **`create-release = false`** plus a hand-added `create-draft-release`
  job: `dist`'s own changelog matching understands only an exact
  version-string heading and can't find this repo's combined ones. Rather
  than restructure the changelog (rejected below), `tools/changelog-section.py`
  (ticket #30) extracts the right section given a version string, and
  that job creates the draft GitHub Release itself (title, notes,
  prerelease flag) before `dist`'s own `host` phase attaches artifacts
  and undrafts it.
- **SBOM**: `cargo-cyclonedx`, CycloneDX 1.5 JSON, generated once per
  release (in `build-global-artifacts`, not once per target — the
  dependency graph doesn't vary by target for this workspace) and
  attached as a release asset. Fail-closed: a generation failure, or a
  bare (Unified-announcement) tag naming more than one package, fails
  the job outright.
- **Provenance**: GitHub Artifact Attestations
  (`github-attestations = true`, `github-attestations-phase = "host"`,
  `github-attestations-filters` covering every binary archive, the
  SBOM, and the aggregate checksums manifest — not the source tarball
  or the many per-artifact `.sha256` sidecars, which #35's own
  acceptance criteria didn't ask for). `github-attestations-phase` is
  load-bearing, not redundant with `dist`'s own default: verified
  directly (three separate `dist generate` runs, diffed against each
  other) that the default phase alone attests only the per-target
  binaries, never the SBOM or checksums manifest, since both are built
  later than that phase runs.
- **Post-build smoke test**: a hand-added step per target, after `dist`
  builds that target's archive and before it's uploaded, extracting it
  and running the real binary with `--version`/`--help` — the real
  cross-compiled artifact, not the dev-profile build `cargo test
  --workspace` already covers.
- **CI guard**: a `Release Pipeline Config` job in the existing
  `ci.yml`, running `dist plan` on every PR — catching a broken
  `dist-workspace.toml` (or a Cargo.toml version `dist` can't reconcile)
  before it would otherwise only surface mid-release. It reports today
  rather than blocks (see Consequences).
- **Rollout validation** (ticket #36): before relying on this for a real
  version cut, two real `-rc.1` tags (`ratect-compat/v0.28.0-rc.1`,
  `ratect/v0.7.0-rc.1`) were pushed against live GitHub infrastructure on
  a throwaway branch, then deleted once verified. Both pipelines
  completed successfully: all five targets built and smoke-tested for
  each binary, both SBOMs validated as real CycloneDX with populated
  dependency components, `sha256.sum` verified against the downloaded
  archives, `gh attestation verify` succeeded for every archive/SBOM/
  checksums-manifest against a real Sigstore transparency log entry, and
  release notes matched the extracted `CHANGELOG.md` section. This
  validation run found and fixed three real defects the design above
  couldn't have caught any other way (an invalid GitHub Actions
  expression that broke the whole workflow file's parsing; the smoke
  test reading `dist`'s *known*-artifact set instead of its narrower
  *produced* one, twice, for two different reasons) — each shipped as
  its own fix, referenced from the commits touching
  `.github/workflows/release.yml`.

## Alternatives considered

- **A hand-rolled GitHub Actions matrix** — what most popular Rust CLIs
  actually do. Rejected: Ratect had zero existing release automation to
  preserve, and `cargo-dist` gets GitHub Attestations support for
  comparatively little hand-written YAML. Two of its other headline
  benefits — generated install scripts and `cargo-binstall` metadata —
  weren't realized or verified in this pass; see Consequences.
- **Restructuring `CHANGELOG.md` to per-package headings**, so `dist`'s
  own changelog matching would just work. Rejected: the combined-heading
  convention is deliberate and already documented (guideline 8's own
  changelog section), and every already-released heading is append-only
  — rewriting the convention would either break old links or leave two
  conventions coexisting. A small, standalone script was cheaper than
  either.
- **sigstore/cosign signing directly**, instead of GitHub Artifact
  Attestations. Rejected: Attestations are native to the pipeline
  already running in GitHub Actions, with no extra CLI for a verifying
  user to install beyond `gh` itself.
- **Publishing to crates.io** — deferred, not merely postponed by
  choice: `ratect-core/Cargo.toml` requires `bollard = "0.22"`, but the
  latest release actually published to crates.io is `0.21.1` — only the
  root `Cargo.toml`'s git-sourced `[patch.crates-io]` pin (see that
  file's own comment) satisfies the requirement at all, and a patch
  doesn't follow a published crate to its own consumers. `ratect-core`'s
  unversioned, path-based dependency on the local `dockerignore` crate is
  a second, independent blocker — crates.io requires every dependency of
  a published crate to carry a version requirement. Removing the first
  blocker needs a new upstream `bollard` release incorporating
  already-landed fixes plus this repo's still-open native-certs PR
  (fussybeaver/bollard#796) — not, as originally scoped, blocked on open
  PRs, which have since landed.
- **A Homebrew tap** (`or1can/homebrew-tap`) — deferred; not attempted
  in this pass at all (`installers = []`), rather than built and held
  back.
- **Windows binaries** — deferred pending the isolation-mode
  (`process`/`hyperv`) and terminal-resize-forwarding gaps `ROADMAP.md`'s
  "First-class Cross-platform Support" entry already tracks; Windows
  support here would be premature ahead of those.
- **Reproducible/bit-for-bit builds** — deferred. Attestations answer
  "did this come from our CI," a different question from "can a third
  party rebuild this exactly." Revisit only if it turns out to be
  near-free as a byproduct of the toolchain already chosen, not designed
  for here.

## Consequences

- **Future revisit risk**: the changelog-extraction workaround's
  stability depends on the `ratect-core` coupling between the two
  binaries. Today a shared-core change is always released for both at
  once, so one script resolving one version string against one shared
  changelog is enough. If a change ever forces releasing them on
  genuinely different cadences with separate changelog sections, this
  assumption needs re-examining — either the script still resolves
  correctly per version, or the combined-heading convention itself would
  need to fork, which is a bigger decision than this ADR's scope.
- **`cargo-binstall` support is unrealized, not delivered** — still
  blocked on the crates.io deferral above. The install-script and
  Homebrew-tap halves of this same original benefit *are* now delivered:
  `dist-workspace.toml`'s `installers` list gained `"shell"` in ticket
  #58 and `"homebrew"` in ticket #59, once the real 0.28.0/0.7.0 tags
  were safely out of the way of `dist`'s no-milestone-awareness (see
  both tickets). Verified for #58 that turning on the shell installer
  adds zero new jobs/steps to `release.yml`. #59's `publish-jobs =
  ["homebrew"]`/`tap = "or1can/homebrew-tap"` DOES add a real new job
  (`publish-homebrew-formula`, after `host` and before `announce`) —
  validated end-to-end against a throwaway `or1can/homebrew-tap-test`
  repo and a real (later-deleted) rc-tag release: the formula was pushed
  with correct per-platform URLs/checksums, and `brew tap`/`brew
  install`/running the installed binary all genuinely worked. Whether
  `cargo-binstall` itself already works off the plain release-asset/
  manifest shape `dist` produces regardless of `installers` still wasn't
  verified.
- **Two non-obvious GitHub Actions gotchas surfaced by #59's real rc-tag
  validation**, worth knowing before ever repeating this kind of
  validation: (1) the built-in `GITHUB_TOKEN` refuses (`403 Resource not
  accessible by integration`) to retarget a release's `target_commitish`
  to a commit that isn't reachable from any pushed branch — pushing only
  the tag (not the branch it's on) leaves the tagged commit dangling and
  breaks `host`'s own `gh release edit --target`; a personal token has
  no such restriction, which is what made this confusing to diagnose.
  Push the branch too. (2) Once a tag name has been deleted from a repo
  with an associated release, GitHub permanently refuses to recreate
  that exact tag name (`Cannot create ref due to creations being
  restricted`) — a throwaway validation that needs a retry must bump to
  a new suffix (`-rc.2`, `-rc.3`, ...), never reuse the same one.
- **`Release Pipeline Config` (ci.yml) now gates, not just reports.**
  Added to the `main branch protection` ruleset's required status checks
  after this ADR's own initial gap (a broken `dist-workspace.toml` used
  to show a red X without blocking a merge) — a live repo-settings
  change, made by a maintainer rather than unilaterally while
  implementing the ticket that added the check.
- **A narrow, accepted attestation gap**: `actions/attest`'s own
  completeness check rejects only a *total* zero-subject count across
  all three filters, not each individually — so if `*.cdx.json` or
  `sha256.sum` ever matched nothing (an upstream rename in
  `cargo-cyclonedx` or `dist` itself), the binaries would still attest
  successfully while the release shipped with the SBOM/checksums
  silently unattested. Recorded in `dist-workspace.toml`'s own comment
  rather than guarded against with a fourth hand-added step, since
  today's job graph makes the more common failure mode (a skipped
  `build-global-artifacts` while `host` still runs) unreachable — the
  realistic trigger is an upstream rename, not a normal run.
- **New external tool dependency.** `dist-workspace.toml`'s
  `cargo-dist-version` pin and `release.yml`'s own installer step both
  name the same `cargo-dist` version — two places, not one, a known,
  accepted duplication (`dist generate` owns `release.yml`'s copy; `ci.yml`'s
  own `Release Pipeline Config` job reads `dist-workspace.toml`'s copy
  directly rather than hardcoding a third, so it can't drift from that
  one independently).
- **Now user-facing** (ticket #38, deliberately not folded into this
  one): `docs/installation.md` documents the GitHub Releases download
  path as primary, and `AGENTS.md`'s release-process guideline describes
  the tag-push-triggers-release flow. Until a real version is tagged
  after this lands, `docs/installation.md`'s instructions point at a
  Releases page with nothing yet to download for this pipeline
  specifically — a one-release transition, not a permanent gap.
  (The install script and Homebrew tap are documented as of tickets #58
  and #59, once each existed to document — see the bullets above.
  `cargo-binstall` support still isn't, matching the crates.io deferral
  above — its normal discovery needs the package resolvable via the
  crates.io index, which is exactly what that deferral blocks.)
