# Releasing

Recovery steps for a release that didn't publish. `AGENTS.md`'s guideline 8
is the release process itself, and the trigger for this page.

## The `host` job fails with `HTTP 403`

The `host` job of `.github/workflows/release.yml` fails with a bare
`HTTP 403: Resource not accessible by integration`, after every build has
succeeded, and the release stays a draft. `GITHUB_TOKEN` may not publish a
release whose tagged commit's workflow files differ from `main`'s, so a
`.github/workflows/` change merged while the run was in flight causes it,
and every re-run then fails the same way (ratect#593).

1. Publish the draft from an account whose token has the `workflow` scope:
   `gh release edit <tag> --draft=false`.
2. Delete the release's already-uploaded assets, since the re-run uploads
   them again without `--clobber`.
3. Re-run the failed `host` job, so `publish-homebrew-formula` lands the
   tap formula.
