# 0022 — A written-for refusal says which changes touch the project

**Status:** Accepted — implemented (ratect-compat 0.33.0 · ratect 0.12.0,
[#623](https://github.com/or1can/ratect/issues/623)). Extends
[0015](0015-native-project-records-its-written-for-version.md) and
[0020](0020-bundle-records-its-written-for-version.md); the refusal itself
is unchanged. Refined while implementing #623: a detector reads the files
the trial load found, or, when it fails, the root or a bundle's TOML entry
file alone, which can report a use but never "none found" — so a row partly
each kind needs nothing extra. A YAML file in scope, which a detector can't
read, makes finding nothing "not known" too.

## Context

0015 refuses a `ratect.toml` project whose `ratect_version` predates a row of
`written_for::BREAKING_CHANGES` until someone reviews the changes and raises
the version. 0020 does the same for a Git bundle. The refusal is
unconditional by design. It fires whether or not the project uses what
changed, and it lists the rows without helping anyone decide whether they
apply.

ratect 0.12.0 is the first release to write rows, and it writes nine at once.
On upgrade, every existing native project, and every bundle with no version,
is refused with the same nine-item list. Working out which items matter means
reading the CHANGELOG against the whole project.

While #382 was being implemented, refusing only when a change applies was
considered and rejected. A detector that missed a case would let a project's
meaning change silently, which is the failure the mechanism exists to
prevent. Some changes act through what a project leaves out, and can hardly
be detected at all. And `ratect_version` would stop recording that a review
happened. That rejection stands.

## Decision

- **The refusal stays unconditional.** What's added is advice: each listed
  row gets an outcome on the same line. A wrong outcome can only make the
  advice wrong. It can never let a file be reinterpreted.
- **Each row declares how it is checked**, as a required part of the row, so
  a new row can't be added without saying so:
  - **Fails the load.** The row only makes something that used to load fail
    to. The loader is its detector. After refusing, Ratect loads the project
    once more under the running version's rules, with the written-for checks
    set aside, and reports once whether the project loads. If it does, every
    such row reads as not applying. If it doesn't, the load error is shown
    once, and these rows say the project doesn't load under the new rules,
    without claiming which row caused it.
  - **Detected.** The row changes what a value *means*, so a load that
    succeeds proves nothing. A detector looks for the construct, reading each
    file as a plain TOML document rather than through the current config
    model, so it keeps working however that model changes later. Its outcome
    is "none found" or "found in …", naming the field and the file.
  - **Not checked.** A row no detector covers says so. Then "none found" and
    "didn't look" can never be confused.
- **A row that is partly each kind** (some uses change meaning, others now
  fail) is detected. Its outcome counts as "none found" only if the detector
  finds nothing *and* the project loads. Every detected row gets that for
  free: its detector reads the files the trial load found, and when the
  project doesn't load, which files there are isn't known, so it reads the
  root (or a bundle's entry file) alone. A use found there is "found";
  finding nothing there is "not known" — never "none found".
- **`config validate`, `doctor` and the refusal share one per-row result**,
  so the three commands never disagree about a row.

## Alternatives considered

- **Refuse only when a change applies.** Rejected in #382 for the reasons
  above. Detectors here only advise.
- **A detector per row, the load-failing rows included.** Rejected. Each would
  re-implement a rule the loader already enforces, and would have to keep
  matching it. For those rows the trial load is exact by construction.
- **Detect on the parsed `ConfigFile`s the load keeps.** Rejected as the only
  route. A row's detector must keep recognising old files under every later
  parser, and the typed model is exactly what later releases change. A plain
  TOML document changes only with the TOML specification.
- **No outcome for undetected rows.** Rejected. An unmarked row would read as
  "nothing found".

## Consequences

- **The trial load fetches what a normal load fetches**, including Git
  bundles, which a refused project otherwise never reaches. The user is about
  to need them anyway, and the trial load runs nothing.
- **Every new row carries its check kind, and a detected row carries tests**
  against a file that uses the construct and one that doesn't. A detector's
  false "none found" is the one wrong answer that matters, so it's what those
  tests pin.
- **A row for YAML files** (#721, undecided) would need detectors that read
  YAML. Nothing here assumes every detected file is TOML beyond the rows that
  exist today.
