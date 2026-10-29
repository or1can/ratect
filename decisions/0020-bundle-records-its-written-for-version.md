# 0020 — A bundle records the Ratect version it was written for

**Status:** Accepted — planned (ratect-compat 0.33.0 · ratect 0.12.0,
[#559](https://github.com/or1can/ratect/issues/559)). Takes up the per-bundle
alternative [0015](0015-native-project-records-its-written-for-version.md)
deferred. Clarified on 2026-10-29, before that release shipped: "a
`batect.yml` gains no field" means that the YAML format gains none, so a
`.yml` can't set `reviewed_ratect_version` under either binary
([0021](0021-native-field-follows-its-file.md), rework
[#680](https://github.com/or1can/ratect/issues/680)). As first implemented,
a `.yml` in a native project could set it.

## Context

[0015](0015-native-project-records-its-written-for-version.md) gave a native
project a written-for version: its root `ratect.toml` declares
`ratect_version`, and `ratect` refuses the project when a later release has
changed what its configuration means. Only the root file is checked. A Git
include pulls a bundle into the project, and that bundle's files are
reinterpreted by a newer binary exactly as the root's would be — with no
warning at all, since the root's version says nothing about them.

Raising your own `ratect_version` is a statement that you reviewed your own
configuration, not every bundle it includes. And for a bundle you don't own,
you can't fix it at the source: you depend on its author to.

No report of this has been seen. Neither had one been for the root case when
0015 was decided; the reasoning there — a silent change in meaning is the
failure worth preventing in advance — applies to a bundle unchanged.

## Decision

- **A bundle entry file declares its own written-for version**, with the same
  `ratect_version` field a root `ratect.toml` uses. It covers every bundle file
  reached from that entry file, as a root's covers its project
  ([`CONTEXT.md`](../CONTEXT.md)'s *bundle entry file*). The field stays refused
  in any other included file, owned or bundle, and in any YAML file: a
  declaration partway down an include tree would leave what it covers
  ambiguous.
- **A Git include entry may declare a reviewed-for version**,
  `reviewed_ratect_version`: its author's statement that they checked the
  bundle against the changes up to that version. It lets someone using a
  bundle they don't own answer a refusal without waiting for its author. It
  is a separate field from `ratect_version` because it states a review, not
  authorship.
- **A bundle is checked against the later of the two.** With neither, it is
  unmarked and counts as written for 0.3.0, as an unmarked root does — refused
  once any row applies.
- **The root's `ratect_version` never covers a bundle.** Otherwise raising it
  would silently vouch for every bundle the project includes — the gap this
  closes.
- **The reviewed-for version counts on any Git include entry**, including a
  bundle's own entry for a nested bundle. Unlike a grant it widens no
  permission, and a bundle's author is the user of the bundle it nests;
  restricting it to owned files would make you vouch for a bundle you may not
  know is there.
- **Each Git include entry reaching a bundle is checked on its own.** A bundle
  file is loaded once, under the first entry to reach it, but the check needs
  no load: every entry must be satisfied by the later of the bundle's version
  and its own reviewed-for version.
- **A YAML bundle entry file follows the YAML-root rule.** It can't declare the
  field, and its meaning is Batect's, so it is checked only once the bundle
  loads a TOML file — and then counts as unmarked unless its entry acknowledges
  it.
- **A version from the future is refused, on either side.** A bundle declaring
  a version newer than the binary is refused before it is parsed, so its new
  fields don't surface as an unknown-field error; the remedies are a newer
  `ratect`, or pinning the include's ref to a release of the bundle written for
  this one. A reviewed-for version newer than the binary is refused too: a
  review against rules this binary doesn't have can't be checked, and
  accepting one would let a typo silence every row.
- **The refusal names the bundle** — its repository, ref and entry path, and
  the file declaring the include — lists the rows since its effective version,
  and gives both remedies: the bundle's author raising its `ratect_version`, or
  the include's author setting `reviewed_ratect_version` after reviewing.
- **No tool writes the reviewed-for version.** `ratect config convert` writes a
  root's `ratect_version`, never an entry's acknowledgement: a review can't be
  automated, and filling the field in would make the check a formality.
- **Native dialect only**, as 0015. `ratect-compat` runs no such check, and a
  `batect.yml` gains no field. A native project with a YAML root still has its
  bundles checked.

## Alternatives considered

- **One declaration per repository**, in a separate file. A repository can
  hold several bundle entry files reached by different includes, and 0015
  already rejected a `.ratect-version`-style file for keeping the version away
  from the configuration it describes.
- **Reusing `ratect_version` on the include entry.** It would read as "this
  bundle was written for X", which the entry's author can't claim, and blur the
  existing refusal of the field outside a root.
- **No acknowledgement from the bundle's user.** Leaves them stuck on the
  bundle's author, which is the risk this exists to remove.
- **Acknowledgement only in owned files**, as grants are. Grants widen what a
  bundle may do; this widens nothing.
- **The first-entry rule boundaries use.** A boundary has to be single because
  a file is loaded under one containment; a version check has no such
  constraint, and per-entry checking doesn't depend on the order of entries.
- **Shipping after the first rows.** The 0.12.0 rows would then refuse
  bundles a release later, after users had already reviewed their roots for
  them — a second migration for the same changes.

## Consequences

- A bundle that is unmarked and unacknowledged is refused under any release
  with a row, which is a breaking change for anyone including one. It ships in
  the same release as the first rows, so that review happens once.
- A bundle's author can declare a version for every project using it; a
  bundle's user can only acknowledge one, and owns that acknowledgement.
- A reviewed-for version outlives a change of ref: bumping a bundle's ref is
  already a change you review, and nothing ties the acknowledgement to the
  content it was given for.
