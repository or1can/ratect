# 0024 — Host identity: what Ratect needs from the host user

**Status:** Accepted — planned (ratect-compat 0.34.0 · ratect 0.13.0). Scope is
tracked as the spec [#844](https://github.com/or1can/ratect/issues/844) and its
sub-issues, in the
[ratect 0.13.0 milestone](https://github.com/or1can/ratect/milestone/12).
Settled in a design discussion on 2026-11-09, prompted by
[#818](https://github.com/or1can/ratect/issues/818).

## Context

Ratect looks up the host user it runs as in several places, and until now
assumed that user has a passwd entry, a group entry and a home directory.
Nothing said so, and each lookup decided on its own what a missing entry
meant — almost always an error:

- `user::current_user`, for `identity` on a rootful Linux daemon
  ([0011](0011-container-identity.md)) and `run_as_current_user`, failed on a
  uid or gid with no name.
- `user::home_directory` read only the passwd entry, deliberately not `$HOME`,
  and failed without one. It is reached by the Docker connection's `~/.docker`
  lookup on every run that connects without `--docker-config` or
  `DOCKER_CONFIG`, in both binaries, and by `~` expansion,
  shared directory caches and the Git include cache.

The environment where those entries are missing is ordinary: CI that runs
Ratect in a container with `-u $(id -u):$(id -g)`, which is the case
`identity` exists for. There, unless `--docker-config` or `DOCKER_CONFIG` named
a directory, every connecting command of both binaries failed before any task
started. The fixes arrived one at a time — #818 fixed
`identity`'s lookup and the `~/.docker` one failed next; #843 is the same
failure in `run_as_current_user` — because each was a different call site of
the same unstated assumption.

The probes behind 0011 varied the daemon, the container's user, the image and
the mounts, but never Ratect's own host identity: `agents/probing.md` told the
prober to use a user with real entries, since one without failed the harness.

Batect runs in this environment. Its user name, looked up on every run
(`SystemInfo.kt`), and its group name fall back to the ids' digits when the
lookup finds no entry (`UnixNativeMethods.kt`, `getUserName`/`getGroupName`).
Its home is the JVM's `user.home` (`SystemInfo.kt`), which Batect
never chose: it is the JDK's. Probed on 2026-11-09 with `HOME=/tmp/x`:

| JDK | passwd entry present | no passwd entry (`-u 4242`) |
| --- | --- | --- |
| Temurin 11, Linux | the passwd home | `?` |
| Temurin 21, Linux | the passwd home | `/tmp/x` |
| Homebrew OpenJDK 27, macOS | the passwd home | not probed |

Axes the probe left fixed: the no-entry case on one vendor's builds (Temurin),
and the JVM alone rather than Batect running on it.

The Docker CLI reads `$HOME` first, and the passwd entry only when `$HOME` is
unset (`pkg/homedir`'s `Get`), so `docker login` writes to `$HOME/.docker`.

## Decision

**Both binaries run under a uid or gid the host has no passwd or group entry
for.**

**One place decides every host-identity fact.** `user.rs` determines the uid,
the gid, each name (which may be absent) and the home. A caller consumes
those facts and states what an absent one means for it; no caller queries the
system itself or turns an absence into an error on its own.

**The home is an absolute `$HOME`, else the passwd entry's.** An empty `$HOME`
counts as unset, as in the Docker CLI, and so does a relative one, which the
Docker CLI takes as written: every use of the home here builds an absolute path
from it, and a relative one would make `~/.ratect` a different directory from
each working directory. Supporting it can wait for someone to ask. A passwd
entry's home that isn't absolute is no home either, for the same reason. This
gives Ratect the same home as the `docker` it orchestrates wherever `$HOME` is
absolute or unset, and the one a user overrides it with.

**When there is no home at all** (neither source):

- the Docker config directory is absent: no active context, no stored
  credentials, and the run goes on;
- `~` in a path, shared directory caches, the Git include cache, a named Docker
  context and TLS certificates read from `~/.docker` are refused, naming the
  feature, `HOME` and the uid; the last two only while no `--docker-config` or
  `DOCKER_CONFIG` (for certificates, `--docker-cert-path` or `DOCKER_CERT_PATH`
  too) names a directory instead;
- the background Git-include sweep is skipped.

**An absent name is the id's digits**, as in Batect, wherever a name is needed
— `run_as_current_user`'s generated files included (#843). `identity`'s edits
keep #818's rules: an appended passwd entry for an unnamed uid is named with
its digits, and an unnamed gid gets no group entry.

## Alternatives considered

- **Passwd entry first, `$HOME` as the fallback.** The JDK's rule on current
  versions, so Batect's. Rejected: it gives Ratect a different home from the
  `docker` CLI beside it wherever the two disagree, and makes the home hard to
  override (`HOME=<scratch>` did not isolate `~/.ratect`). The doc comment that
  chose passwd-only gave one reason, that `$HOME` could be overridden to
  something inconsistent; overriding is the point.
- **Copying Batect's home rule in `ratect-compat` only.**
  [0013](0013-when-ratect-compat-copies-batect.md)'s
  rule 1 asks for this where an outcome differs. Rejected under its rule 3:
  where `$HOME` and the passwd entry disagree, Batect reads a different
  `~/.docker` from the one `docker login` wrote, so credentials and contexts
  the user set up go missing, and on JDK 11 an unnamed uid's home is `?`, a
  relative directory. Those are failures on valid input. `ratect-compat`'s
  caches are already in Ratect's own `~/.ratect`, so no Batect outcome there is
  lost.
- **Refusing an unnamed host user once, at startup.** One clear error instead
  of a failure at whichever lookup comes first. Rejected: Batect supports the
  environment, so `ratect-compat` must, and it is the environment `identity`
  is meant for.
- **Fixing each call site as it is found.** What #818 and #843 started. It
  leaves the next call site wrong by default, which is how they were found.

## Consequences

- Where `$HOME` and the passwd entry disagree, `~/.ratect` and `~/.docker` move
  to `$HOME`'s. Caches rebuild and Git includes re-clone, leaving the old ones
  under the passwd entry's home; `~/.docker` then matches the `docker` CLI's.
  Listed as Changed for both binaries.
- `~` in a `ratect.toml` path changes meaning there too, so for `ratect` it is a
  breaking change under 0013's last paragraph, with a `written_for` row that
  detects a local include or host path starting with `~`, and a host path
  starting with a reference, whose value may.
- Run as root with another user's `$HOME` kept (`sudo -E`), Ratect creates
  root-owned entries under that user's `~/.ratect`, which their own runs then
  can't write to. The Docker CLI does the same under `~/.docker`; accepted, and
  said in the changelog.
- `ratect-compat` diverges from Batect there, recorded in
  `docs/differences-from-batect.md` under 0013's rule 3.
- A project using `~`, shared directory caches or Git includes can't run
  without any home; one that uses none of them can, unless its Docker connection
  needs a named context or TLS certificates from `~/.docker`.
- `agents/probing.md` gains a probe for a host user with no entries, and the
  real-daemon tests run both binaries as one, so the axis 0011's probes left
  fixed is varied from now on.
- A lookup that *fails*, as opposed to finding no entry — an unreachable
  directory service — still fails the run, as in Batect, and `ratect caches`,
  which used to swallow it, now reports it where it reads shared directory
  caches.
- The real-daemon tests run the binaries as a host user with no entries on
  rootful Linux only: the harness needs `setpriv` and the daemon's local socket,
  which CI's runner has. On another class `identity` doesn't adopt the host
  user, so only the home rules apply, and those are unit-tested;
  `run_as_current_user` still does, and its digits there are unit-tested only.
