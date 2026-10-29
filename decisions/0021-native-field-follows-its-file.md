# 0021 — A native field follows its file, on include entries as on containers

**Status:** Accepted — planned (ratect-compat 0.33.0 · ratect 0.12.0,
[#680](https://github.com/or1can/ratect/issues/680)). Supersedes the
include-entry exception in [0003](0003-ratect-native-config-format.md)'s
"Generalized (ratect#214)" paragraph; the rest of 0003 stands.

## Context

[0003](0003-ratect-native-config-format.md) settled that a **container's
file** decides which fields and semantics apply to it: a container declared
in a `.yml` may use no native field and keeps Batect's meaning, even inside a
native project and even when the `.yml` is the root (`ratect -f batect.yml`).
To use a native field, you convert that container's file to TOML.

The same paragraph made an exception for an `include` entry's own native
fields, which it left governed by the **project's** format, "since an entry is
the including file's statement about a bundle rather than the bundle's about
itself". `include_trust::check_dialect` implements that: it refuses
`allow_nested_git_includes`, and since
[0020](0020-bundle-records-its-written-for-version.md)
`reviewed_ratect_version`, only in a Batect-compatible project. Under
`ratect`, any `.yml` may set either field: the root, a local include, or a
bundle file. `docs/includes.md` documents the grant spelled in YAML.

Two things showed up while implementing 0020 (#680). Its own rule, "a
`batect.yml` gains no field", means that the YAML format gains no new field.
The exception contradicts that. And 0003's reason for the exception is about
*whose* statement an entry is, which matters for trust. It doesn't explain
why that statement may be written in Batect's format.

## Decision

- **A native-only field follows the file it is written in**, whether it sits
  on a container or on an include entry, and so does every native-only field
  added later. A `.yml` is Batect's format, so it may set none of them, under
  either binary and whatever the project's format.
- **Today that covers `allow_nested_git_includes` and `reviewed_ratect_version`**
  on a Git include entry. Under `ratect`, a `.yml` that sets either one fails
  the load. That includes a `.yml` root, a local `.yml` include and a YAML
  bundle file. The error names the field and the file, and says to convert
  that file to TOML (`ratect config convert`). It never spells the field in
  either format's syntax.
- **A field both formats accept is unaffected**, such as `allow_host_paths`:
  Ratect's own field ([0004](0004-git-include-host-path-trust.md)), which
  both binaries support. This record is about native-only fields.
- **What an entry *means* is unchanged.** Who may grant, and which entry's
  grant counts, stay as [0004](0004-git-include-host-path-trust.md), 0003 and
  0020 say. This decides only which file format may carry the field.

## Alternatives considered

- **Keep the project rule (0003's exception).** Rejected. It lets `ratect`
  accept a `.yml` that Batect and `ratect-compat` refuse, so a fragment shared
  between a native project and a Batect project breaks only when someone loads
  it under the other tool, long after it was written. Under the file rule,
  `ratect` refuses it while it's being written. That makes "every `.yml` that
  `ratect` accepts is a valid Batect file" something `ratect` enforces, not a
  convention. Incremental migration exists so a project needn't be converted
  in one go, not so a YAML file can grow native features (0003). Converting
  one file is cheap.
- **Decide per field.** Rejected. Each new include-entry field would reopen
  the question, and the answer would depend on which issue happened to add
  the field.

## Consequences

- **Breaking for anyone who grants `allow_nested_git_includes` from a `.yml`
  in a native project.** That form was documented and shipped (ratect 0.4.0),
  but it's expected to be rare. The remedy is to convert that one file. The
  change is to how a YAML file is read, so under
  [AGENTS.md](../AGENTS.md) guideline 7 it takes no `**ratect.toml:**` prefix
  and no `written_for::BREAKING_CHANGES` row.
- **`reviewed_ratect_version` isn't affected** by the Breaking note, because it
  hasn't shipped. 0020's "a `batect.yml` gains no field" now holds as written.
- **A YAML bundle can't acknowledge a TOML bundle it nests.** Only that TOML
  bundle's own `ratect_version` can satisfy the check, since 0020 checks each
  entry that reaches a bundle on its own. The case is narrow: a YAML file that
  isn't the root, declaring a Git include that reaches a TOML bundle (#680).
- **The committed `batect.yml` schema already matches.** It lists neither
  field, and only the `ratect.toml` schema does. The format twins can't
  cover either field: like every Git include field, they're `UNCOVERABLE`,
  because covering them would clone into the real `~/.ratect/incl`. So unit
  tests have to pin the refusal.
