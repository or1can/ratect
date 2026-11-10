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
   `mattpocock-skills:implement`: a run that ranked bugs was the bug review, one reporting
   Standards and Spec was the two-axis review. A review **covers** the change
   only if nothing has changed since it ran. Otherwise its gap is the diff
   from the commit it reviewed to now, and an edit made in response to a
   review is part of that gap.
   *Done when* each review is marked covered, or has a gap named as a diff.

2. **Bug review over its gap.** Invoke the built-in `code-review` at `high`
   over the gap. For a change that drives the Docker daemon, brief it to read
   the daemon's own sources (moby and runc in the Go module cache) for side
   effects, and probe each finding against a real daemon before ranking it:
   `agents/probing.md` says how, for each daemon class. Fix what it confirms,
   test-first.
   *Done when* each finding is fixed, or listed with why not.

3. **Two-axis review over its gap,** after step 2's fixes, so it reads the
   fixed code. Invoke `mattpocock-skills:code-review` and give both its
   reviewers the review rules below. Fix what it confirms.
   *Done when* each finding is fixed, or listed with why not.

4. **Review the fixes.** Fixes from steps 2 and 3 are a new gap for both
   reviews. Run each over that diff alone, bug review first, and repeat
   until a pass changes nothing. If a pass finds more of the same kind of
   problem in the mechanism the last pass fixed, stop and put the design to
   the user instead (guideline 16: the design is the finding).
   *Done when* a pass changes no file, each of its findings listed with why
   not, or the design is with the user.

5. **Size the claims work,** before reporting the work as done or opening
   its PR, once the change is committed (before a commit, skip to step 6
   and say the count is pending). Run `git fetch origin main`, then
   `pre-commit run --hook-stage pre-push claims-unmarked > "/tmp/claims-$(git rev-parse --short HEAD).log" 2>&1`
   and read the whole log. Its unmarked count is the one to quote: the commit
   hook's list covers only the last commit.
   *Done when* the count comes from a run over the current `HEAD`, or is
   reported as pending.

6. **Report** which review covered which commit, what each found, what was
   fixed, what wasn't and why, and the unmarked claims count.

## Review rules

For both reviewers, alongside guideline 15's checks in `AGENTS.md`:

- **A comment the change made false means an invariant moved.** Check the
  invariant before accepting a reworded comment.
- **A test or comment that presents the fake's behaviour as the daemon's**
  is coverage shaped by the harness. Ask what the daemon does there.
- **A mutation check counts once it has shown red.** A green result from a
  mutation that disabled nothing proves nothing.
- **A design record names the axes its probes didn't vary.**
- **A comment calling a behaviour deliberate should say whose decision it
  was.** Behaviour carried over from Batect can be a platform default it
  inherited (the JVM's `user.home`,
  [decisions/0024](../../../decisions/0024-host-identity.md)) or a bug it
  shipped (no final newline), rather than a choice. Ask where it came from,
  then judge it by
  [decisions/0013](../../../decisions/0013-when-ratect-compat-copies-batect.md)'s
  rules like any other Batect behaviour, since knowing its origin doesn't
  settle whether to copy it. 0024 declined to copy Batect's `user.home` home into
  `ratect-compat` under rule 3, since it reads a different `~/.docker` from
  the one `docker login` wrote (ratect#813, the comment of 2026-11-10).
- **A brief is a specification, not a source for what exists.** A ticket's
  "settled" or "don't" may only flag something as hard; check what was
  actually decided.
