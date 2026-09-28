# 0011 — Container identity: state the intent, add to the image

**Status:** Accepted — planned (ratect 0.12.0). Scope is tracked in the
[ratect 0.12.0 milestone](https://github.com/or1can/ratect/milestone/12)
([#227](https://github.com/or1can/ratect/issues/227),
[#107](https://github.com/or1can/ratect/issues/107),
[#225](https://github.com/or1can/ratect/issues/225),
[#226](https://github.com/or1can/ratect/issues/226)). This decision covers
`ratect` (native) only: `ratect-compat` keeps Batect's `run_as_current_user`
behaviour byte-for-byte, whatever this record says — being a drop-in
replacement is its compatibility contract.

## Context

Batect's `run_as_current_user` mode — which `ratect-compat` ports exactly —
bundles three things: it runs the container process as the host user's uid:gid
(`--user`), it **replaces** the image's `/etc/passwd`, `/etc/shadow` and
`/etc/group` with `root` plus the host user, and it creates and chowns the
configured home directory and each writable cache mount. Batect's own rationale
is the rootful-Linux ownership problem (root in the container writes host-root
files into bind mounts), with the mode applied on every platform "for
consistency".

Three issues hit the limits of that bundle from different sides:
[#107](https://github.com/or1can/ratect/issues/107) (users re-declaring
identities the image already shipped, because replacement deleted them),
[#225](https://github.com/or1can/ratect/issues/225) (the collision surface
between the chown step and every kind of volume), and
[#226](https://github.com/or1can/ratect/issues/226) (supplementary groups).
[#227](https://github.com/or1can/ratect/issues/227) is the design round that
produced this record; its probe runs (five daemon/user combinations, results
in the issue's comments) established:

- **Rootful Linux, non-root host user** is the only case where the ownership
  half of the mode does anything: default mode wrote host-root files and
  tripped Git's `safe.directory` check; `--user` fixed both.
- **Rootless Docker refutes the mode destructively**: the daemon already maps
  container root to the host user, and applying `--user <host uid>` made the
  bind mount unreadable outright (the host uid maps to a subordinate uid).
- **macOS runtimes (OrbStack, Docker Desktop) refute the ownership rationale**:
  default mode already wrote host-owned files, and `safe.directory` never
  tripped.
- **Whenever `--user` is applied with no matching passwd entry**, `whoami`
  fails and `$HOME` is `/`, on every daemon.

Why, then, has the mode been useful on macOS in practice? Not ownership: a
**consistent `home_directory` across machines** — the same configuration
producing the same `$HOME` on a macOS laptop and on Linux CI, so cache mounts
(`~/.gradle`, `~/.npm`) and mounted ssh/git configuration can be declared once.
That is a real requirement, and it is not a side effect the redesign may drop.
One mechanism fact shapes how it can be met: git reads the `HOME` environment
variable, but OpenSSH resolves `~/.ssh` from the passwd entry's home field
(`pw_dir`) — so a home guarantee that only sets an environment variable
delivers half of it.

"Don't run as root" was considered as a third intent and **dropped**: the
probes left it as the only remaining reason to apply `--user` on macOS, it was
never a stated goal of the mode, and anyone who wants it can say so directly
with the `user` passthrough below.

The nearest prior art is Podman (Apache-2.0, same licence as Ratect). Its
identity synthesis ([`container_internal_common.go`](https://github.com/containers/podman/blob/main/libpod/container_internal_common.go);
`--passwd`/`--passwd-entry`/`--group-entry` in podman-run(1)) is **append-only,
never replacement**: a passwd entry is added only when a numeric `--user` uid
has no entry in the image's own file (an existing entry is left untouched, a
named user must already exist); a group entry is added only when the gid — and,
on the keep-id path, the group *name* too — is absent from the image's file.
Podman never creates or chowns the home directory it writes into an entry, and
the home it writes is host-varying (the host `$HOME` if mounted, else the
container working directory).

## Decision

### Config surface (native format, per container)

```toml
identity = { home = "/home/me" }  # presence declares the intent; home mandatory
user   = "1000"                   # passthrough: user name or uid
group  = "20"                     # passthrough: primary group; only valid with user
groups = ["docker", 999]          # passthrough: supplementary groups
```

- **`identity`** declares that the container adopts the *host identity* — the
  uid:gid of the user running Ratect — together with a guaranteed home
  directory. What adopting means is resolved against the daemon (below), not
  fixed by the declaration. `home` is mandatory and absolute: `--user` without
  a home breaks `whoami` and `$HOME` everywhere (probe result), so the broken
  half-state is unrepresentable. Both surviving intents travel together
  deliberately — every observed real use wants both at once.
- **`user`, `group`, `groups`** are passthroughs: handed to the runtime
  verbatim, with no per-daemon resolution and no identity-file synthesis.
  Values are strings (name or all-digits id) or bare integers; the runtime
  resolves names against the image's own files, so a named user or group must
  already exist in the image (unknown name is a container start error — the
  runtime's own behaviour, not Ratect's). Three fields rather than Docker's
  `user[:group]` string because the colon packs two meanings into one value;
  Ratect assembles the runtime's forms from them. `group` without `user` is a
  load-time error.
- **`identity` and `user`/`group` are mutually exclusive** — a container
  cannot have two identity sources, and silent precedence is exactly the
  per-call-site divergence this repo's guidelines warn about. `groups` is
  valid with either (supplementary groups on top of an adopted identity is
  the #226 case).

### Daemon classification

Resolved once per connection, from `docker info`:

- **Rootless** — `SecurityOptions` contains `name=rootless`. Container root
  is the host user, so host ownership already holds (probe result).
- **userns-remap** — `SecurityOptions` contains `name=userns`. Every container
  uid, root included, maps to a subordinate host uid (container 0 → e.g.
  `231072`, *n* → `231072 + n`), so **no** container user is the host user and
  host ownership of bind-mount writes cannot be delivered by any `--user`
  value. Docker's own
  [userns-remap page](https://docs.docker.com/engine/security/userns-remap/)
  says bind-mount "file ownership must be pre-arranged". That is Docker's
  documented behaviour; it has not been probed against a real daemon.

  Both strings verified against moby's [`info.go`](https://github.com/moby/moby/blob/master/daemon/info.go)
  (`fillSecurityOptions`, where `name=userns` is added whenever the daemon's
  remapped root pair is not `0:0`); both are bare entries with no
  `,profile=` suffix, so whole-entry exact match is safe. *Remapped* below
  means either of these two.
- **Desktop** — Ratect's own host is macOS or Windows *and* the connection is
  a local socket. This is the heuristic the design owns explicitly: OrbStack,
  Docker Desktop and rootful Ubuntu all report the identical
  `SecurityOptions`, so there is no daemon-side signal. Classifying by
  host-plus-transport covers colima and future runtimes without a vendor list,
  and a remote daemon reached from a Mac (`ssh://`, `tcp://`) correctly falls
  through to what it reports itself.
- **Rootful Linux** — the remainder.

### Resolving `identity`

On **rootful Linux**: apply `--user uid:gid` (host values; a host uid of 0
runs the same pipeline with `--user 0:0`, observably a no-op). Then make the
image's identity files agree, by **reading the created container's own files
and appending or editing — never replacing**:

- Host uid absent from the image's passwd: append one entry — host username
  (falling back to the literal uid digits if the image already uses that name
  for a different uid — Podman's own naming convention for synthesized
  entries), password `*`, the configured `home`, shell `/bin/sh`.
- Host uid already present (`node` at 1000 is the common case): the process
  runs as that existing entry. Keep its name and gid fields untouched; edit
  its home field to the configured `home`. `--user` governs the process gid
  regardless of the entry.
- Group entry: append one, named after the host group, only when the host gid
  *and* that name are both absent from the image's group file; if the gid
  exists, reuse the entry untouched; if only the name is taken, append
  nothing. A group entry is cosmetic (name resolution for `id` and `ls`), so
  the escape hatch on collision is to do nothing rather than invent names.
  This is Podman's keep-id rule.
- Create and chown the home directory, and chown each writable cache mount —
  fresh named volumes are root-owned and unwritable by the host uid here.

On **desktop** and **remapped** daemons: **`--user` is not applied and nothing
is chowned.** On desktop and rootless the ownership outcome already holds
(desktop file sharing maps it; rootless delivers it by construction), and
under rootless, applying `--user` is actively destructive. On userns-remap it
cannot be met at all — the host uid maps to a subordinate uid like every
other — so `--user` would change nothing, and loading such a project emits a
`tracing::warn!` and `ratect doctor` reports a finding that bind-mount
ownership has to be pre-arranged on the host. The home guarantee still holds:
edit the *running* user's passwd entry (the image's default user, usually
root) so its home field is the configured path, and create that directory.
One field's edit, and both `pw_dir` consumers (ssh) and `$HOME` consumers
(git) see the same path on every daemon.

### Passthrough hazards

A non-root `user` on a remapped daemon does not write as the host user: under
rootless it reproduces the unreadable-bind-mount failure the probes found, and
under userns-remap it lands on a subordinate uid like every other container
user. It stays a passthrough — that is what an escape hatch is — but
loading such a project emits a `tracing::warn!`, and `ratect doctor` reports a
finding naming the hazard.

## Alternatives considered

- **Keep replacing the identity files** (Batect's mechanism, today's code).
  Rejected: #107 was users re-declaring what replacement deleted; `node`,
  `postgres`, `nginx` vanish from every image that ships them; and replacement
  *hides* real uid collisions instead of forcing a rule for them. The replace
  path survives in core for `ratect-compat`, so nothing is lost.
- **Keep the `run_as_current_user` name and shape in the native format.**
  Rejected: on every daemon class except rootful Linux the resolved behaviour
  deliberately does *not* run the process as the current user, so the name
  would describe a mechanism the engine no longer performs — the naming bug
  this design escapes.
- **"Don't run as root" as a supported intent.** Dropped (see Context). Its
  loss also removes the one reason cache chown would have been needed on
  macOS.
- **Two independent knobs** (ownership and home as separate fields). Rejected:
  the quarter-states are the known failure modes — home with no owning entry,
  `--user` with no home — and no observed use wants one without the other.
- **Home via environment variable only.** Rejected: ssh resolves `~` from
  `pw_dir`, not `$HOME`, so this delivers git and silently drops ssh — half
  the stated requirement.
- **Apply `--user` plus identity on macOS anyway** (Batect's "harmless"
  claim). Rejected: it reintroduces the whole mechanism everywhere to get one
  field's effect, and the Docker Desktop probe under `--user` showed host
  files as `0:0` inside while git nevertheless passed — behaviour the probe
  could not explain, which is no foundation.
- **Classify desktop daemons by `OperatingSystem` string.** Rejected: a vendor
  list ("Docker Desktop", "OrbStack", …) that misses colima today and every
  future runtime, when host-plus-transport classifies by the thing that
  actually matters (a VM with file sharing).
- **Docker-style `user = "name[:group]"`.** Rejected: two meanings in one
  string; the split fields make "primary group without user" a load-time
  error instead of an unwritable value.
- **A `umask` passthrough.** Out of scope: the runtime has no such option, so
  it is not a passthrough at all — it would need an entrypoint-wrapper
  mechanism of its own.
- **Podman's home for synthesized entries** (host `$HOME` if mounted, else the
  working directory). Rejected: both vary by host, which defeats the
  consistent-home requirement that is the reason `home` exists.
- **Never creating group entries.** Rejected narrowly: the entry costs one
  appended line and spares "cannot find name for group ID" noise; Podman's
  either-name-or-gid-exists skip rule keeps it collision-free.
- **Refusing `identity` on userns-remap**, since half of what it declares
  cannot be met there. Rejected: it would also withhold the home guarantee,
  the half that works on every daemon and the one relied on in practice.
- **Refusing `user` on remapped daemons.** Rejected: a passthrough that
  second-guesses is not an escape hatch. Warn and report via `doctor`, never
  refuse.

## Consequences

- Core carries **both** identity paths: the replace generators (`user.rs`)
  stay for `ratect-compat`, and the native engine gets the read-append-edit
  path. The two binaries' behaviour diverges here by design, and
  [docs/ratect-config-reference.md](../docs/ratect-config-reference.md) gains
  the four fields when they are implemented (with both committed schemas
  regenerated alongside, per the repo's schema rule).
- **Accepted edge:** on daemons where the running user's home field is edited
  (desktop, remapped), image-baked configuration in that user's original home
  (`/root/.npmrc` from a `RUN` step) stops being found via `~`. The files
  remain readable at their old path; only resolution moves. Explicit opt-in,
  and Batect's replacement destroyed such configuration outright, so nothing
  that works today regresses.
- **Held loosely:** the appended passwd entry's name fallback (literal uid
  digits on a name clash) is expected to meet edges only practice will
  uncover; it is the part of this record most likely to be amended, and
  amending it would not disturb the rest of the decision.
- **On userns-remap, `identity` delivers only the home guarantee.** Host
  ownership of bind-mount writes is impossible there by design of the
  daemon, not something Ratect could fix; the warning and `doctor` finding
  say so rather than letting the declared intent go quietly unmet. Its
  behaviour here rests on Docker's documentation, not a probe.
- Effects on open work: [#107](https://github.com/or1can/ratect/issues/107)
  shrinks to "declare users the image lacks" or closes, since the image's
  users now survive; [#225](https://github.com/or1can/ratect/issues/225)'s
  collision surface dissolves with the chown step it catalogued (chown remains
  only for rootful-Linux caches and the created home);
  [#226](https://github.com/or1can/ratect/issues/226) becomes the `groups`
  passthrough plus the identity's own group handling. All three sit in this
  record's milestone; `ratect 0.10.0` carries none of it.
- The glossary terms this design speaks — *host identity*, *image identity*,
  *identity*, *passthrough* — are defined in [CONTEXT.md](../CONTEXT.md).
