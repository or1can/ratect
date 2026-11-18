#!/usr/bin/env python3
# Copyright 2026 Orican Ltd.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///

"""Reads and writes an issue's one design record.

    uv run tools/design-record.py get ISSUE
    uv run tools/design-record.py create ISSUE --by settle-design       < record
    uv run tools/design-record.py rewrite ISSUE --by settle-design --why WHY < record
    uv run tools/design-record.py append ISSUE gap --by TOOL            < text
    uv run tools/design-record.py append ISSUE doubt DN --by TOOL       < reason
    uv run tools/design-record.py amend ISSUE DN --by settle-design --why WHY < decision

Each takes `--repo OWNER/REPO`, the current repository by default, and
prints the record as JSON: `issue`, `source`, `comment_id`, `url`,
`updated_at`, `body` and `pointers`. TOOL is `settle-design`,
`check-design` or `ready`. Exits 1 when the rules below refuse a request,
2 when `gh` can't be run, fails, or replies with something other than
GitHub's JSON.

It is meant to be the only way the three review tools of ADR 0028 read or
write a design record. ratect#924's design record (D5-D12 and D21) settles
what follows, and ratect#926 settles the parts that record left open. The
skills move to it as ratect#927, #928 and #930 land; until then
`settle-design` still posts a `## settle-design record`, which this reads
as a legacy record.

# Which record

An issue has at most one design record: the comment holding a line that is
exactly `## Design record`, outside a code fence, so a comment quoting the
format is not one, and only from someone who can write to the repository
(an owner, member or collaborator): anyone can comment on a public issue.
Two of them is an error: the maintainer deletes all but
one, and settle-design merges what the others held into it with `rewrite`.
With none, a comment holding a line exactly `## settle-design record` (the
form before ADR 0028) serves, if there is one: its section of the comment,
up to the next `##` heading, since triage posts one inside its brief. With
several of those, settle-design merges them into a design record before
any tool reads it, and the maintainer confirms any conflict between them. A
legacy record posted after the design record is refused the same way until
settle-design merges it in with `rewrite`, whose log line names its URL;
an edit to the record that doesn't name it doesn't count. With none of either, the issue body's own `## Design record` section,
up to its next heading, is the record (`source` `body`), if someone who can
write to the repository opened the issue: triage writes the body, so it
is never edited, and the first append starts the comment from it.
Otherwise the record is empty (`source` `none`), which is also what a
change with no ticket is reviewed against; there, anything that would
change a decision goes to the maintainer as "reopen?". Appending to an
empty record posts a design record comment holding only what was appended,
so that a design gap check-design or ready finds isn't lost; settle-design
later `rewrite`s it into a whole record.

A record can say that another issue's record governs one of its
mechanisms ("The lock: governed by #31's record"). Each line that says so
first thing, after at most a list marker and the mechanism's name, is
returned under `pointers`, as a pointer to read with
`get N`, not as decisions of this record: a mechanism shared across issues
has its decisions in one owning record only. Nothing here checks that a
record pointing elsewhere for a mechanism holds no decisions on it; that is
settle-design's to keep.

# What a record holds

`## Design record`, then, as `###` sections:

- `Mechanisms`: each mechanism the issue changes, its actors, and a table
  of state by actor where one fits; or a pointer to its owning record.
- `Decisions`: each a paragraph starting `**D1. <what>.**`, saying why, with
  the alternatives rejected and why (`- *Rejected:* ...`) and its accepted
  cost (`- *Accepted cost:* ...`). The rejected alternatives are what let a
  reviewer tell new evidence from an old argument. A decision that changes
  an ADR says so, and the ADR is updated in that ticket's pull request; the
  record links to an ADR's decisions rather than restating them.
- `Accepted costs`, where they are the record's rather than one decision's.
- `Open points`: each saying who settles it.
- `Design gaps`: added by `append gap`, as `- **G1.** ...`.
- `Amendment log`: one dated line per change, naming the tool and what
  changed. An amend's or rewrite's line says why; a gap's or doubt's why
  is its own entry, which the line names.

ratect#924's record is the worked example.

# Who writes what

- settle-design creates the record and may change anything; the
  maintainer confirms its decisions. `amend` replaces one decision, from
  its `**DN.` line to the next decision or heading, with text holding no
  other decision or heading, and logs why. `rewrite` replaces the whole
  record, for anything else: a new decision, a settled open point, a
  merge.
- check-design reads it, and may append a design gap or doubt a decision
  (an open point for the maintainer, through settle-design). It changes no
  existing decision.
- ready reads it, and may append a design gap, marked as added at
  implementation, which its report lists. It changes no existing decision.

Changing a decision goes back to settle-design through the maintainer.

# What outranks what

ADR 0028's Decision 2, stated here too because this is where the tools read
the record. The record outranks the acceptance criteria: settle-design writes them from
it, check-design checks they still agree, and a disagreement ready finds is
a design flaw, not a choice for ready to make by picking a side. Where the
record and an ADR that hasn't shipped disagree, that too is a design flaw for
settle-design.

# Two writers at once

GitHub has no conditional edit of a comment, so a write re-reads the
comment's `updated_at` just before editing it and refuses if it moved since
it was read, then reads it back and fails if it isn't what was written
(line endings and trailing blank lines aside). A
create re-counts the records after posting. That leaves a window of about a
second in which a second write can still be lost: an accepted cost, since
one session works an issue at a time.
"""

import sys

# Before any other import: the PEP 723 header above is what `uv run` reads,
# and this is what a plain `python3` older than it meets instead.
if sys.version_info < (3, 11):
    sys.exit(
        "tools/design-record.py needs Python 3.11 or later, and this is %d.%d. "
        "Run it as `uv run tools/design-record.py`, which fetches one." % sys.version_info[:2]
    )

import argparse
import datetime
import json
import re
import subprocess

HEADING = "## Design record"
LEGACY_HEADING = "## settle-design record"
OPEN_HEADING = "### Open points"
GAPS_HEADING = "### Design gaps"
LOG_HEADING = "### Amendment log"
TOOLS = ("settle-design", "check-design", "ready")
# Who can write to the repository. Anyone can comment on a public issue, so a
# comment from anyone else is never a record, or a second one, and nor is the
# body of an issue they opened.
TRUSTED = {"OWNER", "MEMBER", "COLLABORATOR"}
HEADING_LINE = re.compile(r"^(#{1,6}) ")
FENCE = re.compile(r"^\s*(```|~~~)")
DECISION_LINE = re.compile(r"^\*\*D\d+\.")
GAP_ID = re.compile(r"\*\*G(\d+)\.\*\*")
# A line that is a pointer and nothing else first: "Governed by #N…", or a
# mechanism's label followed by it ("- **The lock.** governed by #N…",
# "- The lock: governed by #N…"). Not the phrase inside other prose.
POINTER = re.compile(
    r"^\s*(?:[-*]\s+)?(?:\*\*[^*]+\*\*[.:]?\s+|[^\s*:][^:]*:\s+)?governed by\s+\[?#(\d+)",
    re.IGNORECASE,
)


class Refused(Exception):
    """A request the record's rules refuse, or a record in a state they forbid."""


def gh(*args, input=None):
    """`gh` with `args`, returning its stdout; the one place this script reaches GitHub."""
    return subprocess.run(
        ["gh", *args], input=input, check=True, capture_output=True, encoding="utf-8"
    ).stdout


def today():
    return datetime.datetime.now(datetime.timezone.utc).date().isoformat()


def outside_fences(lines):
    """For each of `lines`, whether it is Markdown text rather than inside a code fence."""
    flags, fence = [], None
    for line in lines:
        match = FENCE.match(line)
        if fence is None and match:
            fence = match.group(1)
            flags.append(False)
        elif fence is not None:
            if match and match.group(1) == fence:
                fence = None
            flags.append(False)
        else:
            flags.append(True)
    return flags


def heading_level(line, text):
    match = HEADING_LINE.match(line) if text else None
    return len(match.group(1)) if match else None


def index_of(lines, wanted):
    """The first line of `lines` that is exactly `wanted`, outside code fences, or `None`."""
    flags = outside_fences(lines)
    return next((n for n, line in enumerate(lines) if flags[n] and line.rstrip() == wanted), None)


def has_line(text, wanted):
    return index_of(text.splitlines(), wanted) is not None


def section_end(lines, start, level):
    """Where the section headed at `start` ends: its next heading of `level` or above."""
    flags = outside_fences(lines)
    return next(
        (n for n in range(start + 1, len(lines)) if (heading_level(lines[n], flags[n]) or 7) <= level),
        len(lines),
    )


def section_span(text, heading):
    """The (start, end) character offsets of the `##` section `heading` in `text`, or `None`."""
    lines = text.splitlines(keepends=True)
    start = index_of(lines, heading)
    if start is None:
        return None
    end = section_end(lines, start, 2)
    offset = sum(len(line) for line in lines[:start])
    return offset, offset + len("".join(lines[start:end]))


def json_stream(text):
    """Each JSON value in `text`, as `gh api --paginate` writes one per page."""
    decoder, at, values = json.JSONDecoder(), 0, []
    while True:
        while at < len(text) and text[at].isspace():
            at += 1
        if at == len(text):
            return values
        value, at = decoder.raw_decode(text, at)
        values.append(value)


def comments(repo, issue):
    pages = json_stream(gh("api", "--paginate", f"repos/{repo}/issues/{issue}/comments"))
    return [comment for page in pages for comment in page]


def pointers(text):
    lines = text.splitlines()
    flags = outside_fences(lines)
    found = []
    for line, is_text in zip(lines, flags):
        match = POINTER.match(line) if is_text else None
        if match:
            found.append({"issue": int(match.group(1)), "line": line.strip()})
    return found


def scan(repo, issue):
    """The issue's design-record comments, and its legacy `## settle-design record` ones."""
    every = [c for c in comments(repo, issue) if c.get("author_association") in TRUSTED]
    return (
        [c for c in every if has_line(c["body"], HEADING)],
        [c for c in every if has_line(c["body"], LEGACY_HEADING)],
    )


def refuse_several(issue, records):
    if len(records) > 1:
        raise Refused(
            f"#{issue} has {len(records)} design records, where an issue has one: "
            "the maintainer deletes all but one, and settle-design merges what they held into it with `rewrite`"
        )


def found(issue, source, comment, text, span=None):
    """The record; `get` prints all but the `_`-prefixed keys, which are for writing it back."""
    return {
        "issue": issue,
        "source": source,
        "comment_id": comment and comment["id"],
        "url": comment and comment["html_url"],
        "updated_at": comment and comment["updated_at"],
        "body": text,
        "pointers": pointers(text),
        "_comment_body": comment and comment["body"],
        "_span": span,
    }


def find(repo, issue, merging=False):
    """The issue's design record: where it came from, its comment, and its text.

    `merging` is settle-design's `rewrite`, which is how a legacy record
    posted after the design record gets merged into it.
    """
    records, legacy = scan(repo, issue)
    refuse_several(issue, records)
    if records:
        record = records[0]
        # Merged is what the log says, not an edit's timestamp: `rewrite`
        # names each legacy record it merges.
        unmerged = [
            c for c in legacy if c["created_at"] > record["created_at"] and c["html_url"] not in record["body"]
        ]
        if unmerged and not merging:
            raise Refused(
                f"#{issue} has a `{LEGACY_HEADING}` comment posted after its design record "
                f"({unmerged[0]['html_url']}): settle-design merges it in with `rewrite`"
            )
        result = found(issue, "comment", record, record["body"])
        result["_unmerged"] = [c["html_url"] for c in unmerged]
        return result
    if len(legacy) > 1:
        raise Refused(
            f"#{issue} has {len(legacy)} legacy `{LEGACY_HEADING}` comments and no design record: "
            "settle-design must merge them into one design record before any tool reads it"
        )
    if legacy:
        span = section_span(legacy[0]["body"], LEGACY_HEADING)
        text = legacy[0]["body"][span[0] : span[1]].rstrip() + "\n"
        return found(issue, "legacy", legacy[0], text, span)
    reply = json.loads(gh("api", f"repos/{repo}/issues/{issue}"))
    body = (reply["body"] or "") if reply.get("author_association") in TRUSTED else ""
    span = section_span(body, HEADING)
    text = body[span[0] : span[1]].rstrip() + "\n" if span else ""
    return found(issue, "body" if text else "none", None, text)


def post(repo, issue, text):
    """Posts `text` as the issue's design record comment."""
    comment = json.loads(
        gh("api", "-X", "POST", f"repos/{repo}/issues/{issue}/comments", "--input", "-",
           input=json.dumps({"body": text}))
    )
    # Another tool may have posted one between the read and the post: say so
    # rather than leave two for the next reader to find.
    refuse_several(issue, scan(repo, issue)[0])
    return found(issue, "comment", comment, comment["body"])


def normalised(text):
    return text.replace("\r\n", "\n").rstrip()


def write(repo, issue, record, text):
    """Replaces `record`'s text with `text`, refusing if its comment changed since it was read."""
    whole = text
    if record["_span"]:
        start, end = record["_span"]
        rest = record["_comment_body"][end:]
        whole = record["_comment_body"][:start] + text + ("\n" + rest if rest else "")
    path = f"repos/{repo}/issues/comments/{record['comment_id']}"
    if json.loads(gh("api", path))["updated_at"] != record["updated_at"]:
        raise Refused(f"#{issue}'s design record changed since it was read: read it again and retry")
    gh("api", "-X", "PATCH", path, "--input", "-", input=json.dumps({"body": whole}))
    landed = json.loads(gh("api", path))
    if normalised(landed["body"]) != normalised(whole):
        raise Refused(
            f"#{issue}'s design record is not what was written: another write landed at the same time. "
            f"Read {record['url']} before retrying, since this write may be part of it"
        )
    return find(repo, issue, merging=True)


def only(tool, allowed, action):
    if tool not in allowed:
        raise Refused(f"{tool} may not {action}: only {' or '.join(allowed)} may")


def whole_record(text):
    if not has_line(text, HEADING) or not has_line(text, LOG_HEADING):
        raise Refused(f"a design record has a `{HEADING}` heading and a `{LOG_HEADING}` section")


def create(repo, issue, tool, text):
    """Posts `text` as the issue's design record; only settle-design does."""
    only(tool, ("settle-design",), "create a design record")
    whole_record(text)
    records, _ = scan(repo, issue)
    if records:
        raise Refused(f"#{issue} already has a design record, {records[0]['html_url']}: rewrite it instead")
    return post(repo, issue, text)


def rewrite(repo, issue, tool, text, why):
    """Replaces the whole record with `text`, and logs why; only settle-design does."""
    only(tool, ("settle-design",), "rewrite a design record")
    whole_record(text)
    record = find(repo, issue, merging=True)
    if record["source"] != "comment":
        raise Refused(f"#{issue} has no design record comment to rewrite: create one instead")
    text = log(text, tool, f"rewrote the record, because {reason(why)}")
    for url in record["_unmerged"]:
        text = log(text, tool, f"merged the legacy record {url}")
    return write(repo, issue, record, text)


def decision(lines, decision_id):
    """The index in `lines` where decision `decision_id` starts, or `None`."""
    pattern = re.compile(rf"^\*\*{re.escape(decision_id)}\.")
    flags = outside_fences(lines)
    return next((n for n, line in enumerate(lines) if flags[n] and pattern.match(line)), None)


def item(text):
    """`text` as one Markdown list item: every line after the first indented."""
    first, *rest = text.strip().splitlines()
    return [first, *("  " + line if line.strip() else "" for line in rest)]


def add_to_section(text, heading, entry, before=()):
    """`text` with `entry`'s lines at the end of `###` section `heading`, made if missing.

    A missing section goes just before the first of the `before` headings
    present, or at the end.
    """
    lines = text.rstrip("\n").splitlines()
    start = index_of(lines, heading)
    if start is not None:
        end = section_end(lines, start, 3)
        while end - 1 > start and not lines[end - 1].strip():
            end -= 1
        lines[end:end] = ([""] if end == start + 1 else []) + entry
    else:
        at = next((n for n in (index_of(lines, name) for name in before) if n is not None), None)
        block = [heading, "", *entry]
        if at is None:
            lines += ["", *block]
        else:
            lines[at:at] = [*block, ""]
    return "\n".join(lines) + "\n"


def log(text, tool, what):
    return add_to_section(text, LOG_HEADING, [f"- {today()}: {tool}: {what}."])


def reason(why):
    return why.strip().rstrip(".")


def append(repo, issue, tool, kind, text, decision_id=None):
    """Appends a design gap, or a doubt about `decision_id`, and logs it."""
    if kind == "doubt":
        only(tool, ("settle-design", "check-design"), "doubt a decision")
    if not text.strip():
        raise Refused(f"an appended {kind} needs its text on stdin")
    record = find(repo, issue)
    body = record["body"] or HEADING + "\n"
    if kind == "doubt":
        if decision(body.splitlines(), decision_id) is None:
            raise Refused(
                f"#{issue}'s design record has no {decision_id}: doubt a decision where it is recorded, "
                "following the record's pointer if it has one"
            )
        entry = item(
            f"- **{decision_id} doubted** by {tool}, {today()}: {text.strip()}\n"
            "Who settles it: the maintainer, through settle-design."
        )
        what = f"doubted {decision_id}"
        heading, before = OPEN_HEADING, (GAPS_HEADING, LOG_HEADING)
    else:
        number = max((int(n) for n in GAP_ID.findall(body)), default=0) + 1
        stage = "at implementation " if tool == "ready" else ""
        entry = item(f"- **G{number}.** {text.strip()}\n*(added {stage}by {tool}, {today()})*")
        what = f"appended design gap G{number}"
        heading, before = GAPS_HEADING, (LOG_HEADING,)
    body = add_to_section(body, heading, entry, before)
    if record["source"] == "body":
        body = log(body, tool, "started this comment from the issue body's design record")
    body = log(body, tool, what)
    if record["comment_id"] is None:
        return post(repo, issue, body)
    return write(repo, issue, record, body)


def amend(repo, issue, tool, decision_id, text, why):
    """Replaces decision `decision_id` with `text`, and logs why; only settle-design does."""
    only(tool, ("settle-design",), "change a decision")
    new = text.strip().splitlines()
    if not new or not new[0].startswith(f"**{decision_id}."):
        raise Refused(f"the new text of {decision_id} starts `**{decision_id}.`, as the decision it replaces does")
    flags = outside_fences(new)
    if any(flags[n] and (DECISION_LINE.match(line) or HEADING_LINE.match(line)) for n, line in enumerate(new[1:], 1)):
        raise Refused(f"`amend` replaces one decision; this text holds another decision or a heading: use `rewrite`")
    record = find(repo, issue)
    lines = record["body"].splitlines()
    start = decision(lines, decision_id)
    if start is None or record["comment_id"] is None:
        raise Refused(f"#{issue} has no design record comment holding {decision_id}")
    flags = outside_fences(lines)
    end = next(
        (n for n in range(start + 1, len(lines))
         if flags[n] and (DECISION_LINE.match(lines[n]) or HEADING_LINE.match(lines[n]))),
        len(lines),
    )
    while end - 1 > start and not lines[end - 1].strip():
        end -= 1
    body = "\n".join([*lines[:start], *new, *lines[end:]]) + "\n"
    return write(repo, issue, record, log(body, tool, f"amended {decision_id}, because {reason(why)}"))


def by(command):
    command.add_argument("--by", required=True, choices=TOOLS, help="The tool writing.")


def parser():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    get = commands.add_parser("get", help="Print the issue's design record as JSON.")
    get.add_argument("issue", type=int)
    create_ = commands.add_parser("create", help="Post the record on stdin as the issue's design record.")
    create_.add_argument("issue", type=int)
    by(create_)
    rewrite_ = commands.add_parser("rewrite", help="Replace the whole record with the one on stdin.")
    rewrite_.add_argument("issue", type=int)
    rewrite_.add_argument("--why", required=True, help="Why it changed, for the amendment log.")
    by(rewrite_)
    append_ = commands.add_parser("append", help="Append a design gap, or a doubt, read from stdin.")
    append_.add_argument("issue", type=int)
    kinds = append_.add_subparsers(dest="kind", required=True)
    gap = kinds.add_parser("gap", help="A case the record is silent on.")
    doubt = kinds.add_parser("doubt", help="A decision that may no longer hold.")
    doubt.add_argument("decision", metavar="DN")
    by(gap)
    by(doubt)
    amend_ = commands.add_parser("amend", help="Replace one decision with the text on stdin.")
    amend_.add_argument("issue", type=int)
    amend_.add_argument("decision", metavar="DN")
    amend_.add_argument("--why", required=True, help="Why it changed, for the amendment log.")
    by(amend_)
    for command in (get, create_, rewrite_, gap, doubt, amend_):
        command.add_argument("--repo", default="{owner}/{repo}", help="OWNER/REPO; the current repository by default.")
    return parser


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        if args.command == "get":
            result = find(args.repo, args.issue)
        elif args.command == "create":
            result = create(args.repo, args.issue, args.by, sys.stdin.read())
        elif args.command == "rewrite":
            result = rewrite(args.repo, args.issue, args.by, sys.stdin.read(), args.why)
        elif args.command == "amend":
            result = amend(args.repo, args.issue, args.by, args.decision, sys.stdin.read(), args.why)
        else:
            result = append(
                args.repo, args.issue, args.by, args.kind, sys.stdin.read(), getattr(args, "decision", None)
            )
    except Refused as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except subprocess.CalledProcessError as error:
        print(f"error: gh failed: {error.stderr or error}", file=sys.stderr)
        return 2
    except FileNotFoundError as error:
        print(f"error: can't run gh: {error}", file=sys.stderr)
        return 2
    except json.JSONDecodeError as error:
        print(f"error: gh's reply isn't what GitHub's API returns: {error}", file=sys.stderr)
        return 2
    print(json.dumps({k: v for k, v in result.items() if not k.startswith("_")}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
