# 0015 — A native project records the Ratect version it was written for

**Status:** Accepted — planned (ratect-compat 0.32.0 · ratect 0.11.0,
[#149](https://github.com/or1can/ratect/issues/149)).

## Context

Nothing in a configuration says which Ratect it was written for, so two things
go wrong silently or unhelpfully:

1. **An older binary meets a newer config.** A field it doesn't know is
   rejected at load — both formats deny unknown fields — but as a parse error
   (`unknown field: …`), not as "this project needs a newer Ratect".
2. **A newer binary meets an older config whose meaning has changed.** Under
   [0013](0013-when-ratect-compat-copies-batect.md), a parity fix in shared code
   applies to the native binary by default, and where it changes what a
   `ratect.toml` means or accepts — a backslash in an expression starting to
   escape, say — it's a breaking change. No field is added, so nothing fails:
   the newer binary applies today's rules to a file written under yesterday's.

The JSON schemas only help in an editor, and only for the first case.

Batect's answer was its committed `./batect` wrapper script, which pinned the
exact version a project ran. Ratect ships no wrapper, deliberately
([`docs/ratect-cli.md`](../docs/ratect-cli.md)).

## Decision

- **Native dialect only.** `ratect-compat`'s meaning is Batect's
  ([0013](0013-when-ratect-compat-copies-batect.md)): its changes move it
  towards Batect, so a `batect.yml` has no earlier Ratect meaning to protect,
  and it gains no Ratect-only field.
- **A top-level `ratect_version` field in the root `ratect.toml`** holds the
  version the project was written for — a full version (`"0.11.0"`), compared
  by major and minor, since a pre-1.0 breaking change lands on a minor version.
  Only the root file may set it, as with `project_name`; it's the project's
  version, not a file's. `ratect config convert` writes the converting binary's
  own version into what it generates.
- **The binary carries a table of breaking changes**: one row per change to
  what a `ratect.toml` means or accepts, giving the version it landed in, a
  one-line summary and its `CHANGELOG.md` entry. The table starts empty.
- **The check runs before the configuration is parsed**, reading only
  `ratect_version` from the root file, so a newer project's unknown field gets
  this explanation rather than a parse error. It refuses in two cases:
  - the written-for version is newer than the binary — "this project needs
    ratect ≥0.13; you have 0.11";
  - the table has rows newer than the written-for version — listing each,
    and saying to set `ratect_version` to the binary's version once they've
    been reviewed.

  Otherwise it says nothing. An upgrade crossing no breaking change is silent;
  one crossing a change refuses then, when the project's meaning actually
  changes, and updating `ratect_version` is the record that someone looked.
- **A project with no `ratect_version` is written for 0.3.0**, the first release
  with `ratect.toml`. While the table is empty that changes nothing; when the
  first row lands, every unmarked project refuses the same way.
  `ratect config validate` and `ratect doctor` suggest adding the field
  meanwhile.
- **A native project whose root file is YAML counts as unmarked.** `ratect`
  accepts a `batect.yml` root, as a step in migrating; that file stays exactly
  Batect's shape, so it can't declare `ratect_version`. Once a row lands, such a
  project's refusal says to convert it with `ratect config convert` and declare
  the field there.
- **A row's `CHANGELOG.md` entry is tagged** as a configuration change, and a
  test checks that every tagged entry has a row — so a missed row fails the
  build rather than shipping.

## Alternatives considered

- **A `.ratect-version` file beside the config** (in the style of `.nvmrc`).
  Its case was keeping `batect.yml` free of a Ratect-only field while serving
  both formats; covering the native dialect only removes it, and a field keeps
  the version in the file it describes.
- **A minimum version only.** Fixes the first case, not the second: it can say
  the binary is too old, never that the project is.
- **Applying the old rules to an old project.** Every breaking change would
  carry both behaviours forever.
- **Warning rather than refusing.** A warning on every run gets ignored, which
  leaves exactly the silent change this exists to prevent.
- **Requiring the field.** Every existing `ratect.toml` lacks it, so requiring
  it would itself be a breaking change, before any change it protects against
  exists.
- **A separate configuration-format version.** A second number tracking the
  same thing: with the table, the binary's own version already says which
  rules a project was written for.
- **Accepting `ratect_version` in a YAML root under the native dialect.**
  The file would carry a field Batect and `ratect-compat` reject, undoing the
  reason the marker is native-only.
- **Exempting a YAML root from the check.** The projects part-way through
  migrating would be reinterpreted silently — the case this exists for.
- **A written-for version per Git bundle.** A bundle written for 0.10 and used
  under 0.13 is reinterpreted too, and the project owner updating their own
  `ratect_version` doesn't mean they reviewed the bundle. Deferred, not
  rejected — see Consequences.

## Consequences

- A `### Breaking` entry that changes what a `ratect.toml` means or accepts
  adds a table row in the same commit.
- A bundle's own meaning changing under it is not caught: only the root file's
  written-for version is checked. A bundle carrying its own is follow-on work.
- Per-task evaluation ([0014](0014-config-checked-at-load-evaluated-per-task.md),
  #314) is a relaxation — nothing that ran stops running — so it adds no row.
