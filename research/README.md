# Research

Findings checked against primary sources (a tool's own code, specification
or documentation, or its maintainers' discussion), written to inform a
decision. Records in [`decisions/`](../decisions/) and GitHub issues cite
them. A research file is evidence, not a decision: it reports what other
tools do and why, and leaves the choice to the record or issue that cites it.

Kept apart from `decisions/` because research doesn't map one-to-one onto
records. A file can inform an issue brief and never lead to a record, or
support several.

## Each file is a snapshot

A research file records what its sources said when it was written, pinned
to the tag or commit it read, and says so at the top. It is never revised to
follow its sources as they change: a record citing it relies on it saying
what it said. When it goes out of date, write a new file and add one line at
the top of the old one pointing to it. Only corrections are made in place,
where a file misreported a source at the version it pinned, and each says
what it corrected.

Name a file for its question, not its date or the issue that asked it, as
in [`shared-cache-concurrency.md`](shared-cache-concurrency.md). A file that replaces an
older one takes a name that tells the two apart.
