---
name: ready
description: Review a change until both reviews, the bug hunt and the two-axis conformance review, cover it. Use before each commit, and before reporting work as done or opening its PR.
---

Two reviews find different things here, and neither replaces the other. The
**bug review** is the built-in `code-review` skill, which hunts for behaviour
that's wrong. The **two-axis review** is `mattpocock-skills:code-review`,
which checks the change against this repo's standards and against its issue.
Both answer to the bare name `code-review`, so always invoke them by these
names. Over the 0.13.0 milestone the bug review found what the two-axis
review missed on six PRs, and only ran when the maintainer asked (ratect#813).

## Steps

1. **Find what's unreviewed.** The change is the branch since
   `git merge-base origin/main HEAD`, plus anything uncommitted. For each
   review, find the last time it ran in this session, including inside
   `mattpocock-skills:implement`: a run over the change that ranked bugs was the bug review, one reporting
   Standards and Spec was the two-axis review. A review **covers** the change
   only if nothing has changed since it ran. Otherwise its gap is the diff
   from the commit it reviewed to now, and an edit made in response to a
   review is part of that gap.
   *Done when* each review is marked covered, or has a gap named as a diff.

2. **Bug review over its gap.** Invoke the built-in `code-review` at `high`
   over the gap, briefed with `agents/review-brief.md`. For a change that
   drives the Docker daemon, brief it to read the daemon's own sources
   (moby and runc in the Go module cache) for side effects, and probe each
   finding against a real daemon before ranking it: `agents/probing.md`
   says how, for each daemon class. Fix what it confirms, test-first.
   *Done when* each finding is fixed, or listed with why not.

3. **Two-axis review over its gap,** after step 2's fixes, so it reads the
   fixed code. Invoke `mattpocock-skills:code-review` and brief both its
   reviewers with `agents/review-brief.md`. Fix what it confirms.
   *Done when* each finding is fixed, or listed with why not.

4. **Review the fixes.** Fixes from steps 2 and 3 are a new gap for both
   reviews. Run each over that diff alone, bug review first, and repeat
   until a pass changes nothing. If a pass finds more of the same kind of
   problem in the mechanism the last pass fixed, stop and put the design to
   the user (the **design stop**; guideline 16: suspect the design)
   as two options, recommending the first: rethink it with `settle-design`
   on that mechanism, or keep fixing. Twice in 0.14.0 the recommendation
   was to carry on, and both times the maintainer reversed it
   (ratect#902). Rework the code to the design `settle-design` settles;
   that rework is a new gap, so go back to step 2 with it. If it leaves
   the part open, stop there: the design stays with the user and the code
   stays as it is. A "keep fixing" answer holds for that mechanism.

   **Before every pass, sweep what the last fixes renamed.** If they
   renamed an identifier or term, or changed what one means, run
   `git grep --untracked` over the whole checkout, not the diff, for the
   old name and the old defining phrases. The reviews read only the diff,
   so they find leftovers elsewhere only a few per pass (ratect#906). Fix
   each hit or say why it stands; a shipped `CHANGELOG.md` or
   `RELEASES.md` entry stands (guideline 9). Grep matches spelling, not
   meaning, so a sentence asserting the old meaning in other words needs
   reading around the hits, not another grep. A round that renamed
   nothing skips the sweep and says so. The `claims` `restatement` check
   doesn't replace this: it runs at commit or through `check-claims`,
   keyed on text you deleted, and guideline 16 says what it misses.

   **Once a pass changes nothing, check the Windows build** with
   `uv run tools/check-windows-build.py`, which runs CI's Windows Check
   locally. Neither review compiles the `cfg(not(unix))` side of a gate,
   and Windows Check isn't a required status, so a break there is found
   after the PR opens unless it's found here (ratect#904). A change to
   nothing cargo reads (Rust sources, manifests, `Cargo.lock`, `.cargo/`
   config, a file a crate includes) and not to the check itself or CI's
   `windows-check` job skips it and says so. A fix it needs is a new gap, so go round this step again.
   *Done when* a pass changes no file, each of its findings listed with
   why not, each sweep hit fixed or listed with why it stands, and the
   Windows check exits 0 over the final tree or was skipped — or the
   design is with the user.

5. **Size the claims work,** before reporting the work as done or opening
   its PR, once the change is committed (before a commit, skip to step 6
   and say the count is pending). Run `git fetch origin main`, then
   `pre-commit run --hook-stage pre-push claims-unmarked > "/tmp/claims-$(git rev-parse --short HEAD).log" 2>&1`
   and read the whole log. Its unmarked count is the one to quote: the commit
   hook's list covers only the last commit.
   *Done when* the count comes from a run over the current `HEAD`, or is
   reported as pending.

6. **Report** which review covered which commit, what each found, what was
   fixed, what wasn't and why, what each step 4 sweep searched for (or
   that a round renamed nothing), the Windows check's result (or that it
   was skipped), and the unmarked claims count.

## Author habits

These are for you, while writing a fix; a reviewer can't act on them, so
they stay out of the reviewers' brief, `agents/review-brief.md`, which holds
the checks a reviewer applies. Each of these has caught something a review
missed:

- **Anchor an inserted item on the preceding item's closing brace, never on
  the new one's attributes.** A Rust item's doc comment sits *above* its
  `#[test]`/`#[derive]` attributes, so anchoring there splices the new item
  into the previous one's documentation — silently, and the compiler is
  happy. The `claims` plugin's `spliced-docs` check finds the ones that
  get through; it's advisory (never blocks a commit), so read each
  candidate rather than treating a report as a defect.
- **Re-read every string you added, in its final control-flow position.**
  Nothing type-checks a log or error message, so they stay as written while
  the code around them moves. An error should name something the user
  actually wrote (the reviewers' brief has why).
- **When a change alters observable behaviour, re-read that behaviour's
  whole doc section and *run* each claim against the binary.** Not grep —
  execute. Every claim: the example output, the flag descriptions, the "this
  does X" sentences. Capture real output and `diff` it rather than editing
  what looks wrong, since what looks wrong is exactly the set you already
  believe. Grep is the fallback for claims nothing can be run against (a
  roadmap entry, a design note); it is not the check (this rule was once
  grep, and reported clean while missing three).

  To find which claims to re-read, the `claims` plugin's `stale-claims`
  check ranks prose by how much the code it names has moved since the
  claim was last touched. Treat a hit as "re-read this", never as "this
  is wrong" — it measures churn, so a true claim about a hot file looks
  suspicious. Don't record its candidate count anywhere: the number rises
  when code moves and falls when a doc is fixed, so it measures neither.
- **Verify a claim before writing it, not after a reviewer questions it** —
  the review-time half of the change loop's write-prose step.
- **A claim copied from the handoff is unverified.** The handoff is a
  *specification* — what to build, and why it is worth building. It is not
  a source for what exists. So every name in it that you carry into a doc
  comment, an error, or a page under `docs/` gets run first: a flag against
  `--help`, a mode against the code that would implement it, a path against
  the filesystem. This is the change loop's "the code includes whatever the
  prose *names*", applied to the one input that most looks like it has
  already been checked (`RELEASES.md`'s 0.26.0 entry corrects one that
  wasn't). Correct the handoff where it is still live;
  where it is already struck through, record the correction in its
  done-summary rather than editing history.
- **Run a self-cleaning test twice, asserting external state (`docker volume
  ls`) rather than the exit code.** One run passes whether or not the
  cleanup matched anything — the first run starts clean by definition.
