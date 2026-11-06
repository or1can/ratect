# 0011 — Container identity: state the intent, add to the image

**Status:** Accepted — planned (ratect 0.13.0). Scope is tracked in the
[ratect 0.13.0 milestone](https://github.com/or1can/ratect/milestone/12),
as the spec [#233](https://github.com/or1can/ratect/issues/233) and its
sub-issues; the design round was
[#227](https://github.com/or1can/ratect/issues/227). This decision covers
`ratect` (native) only: `ratect-compat` keeps Batect's `run_as_current_user`
behaviour byte-for-byte, whatever this record says — being a drop-in
replacement is its compatibility contract. Refined before implementation
([#237](https://github.com/or1can/ratect/issues/237)): the hazard warnings
are output under [0017](0017-output-streams-and-styles.md), not `tracing`, and
fire during a run, since the daemon class needs a connection loading doesn't
make. Refined while implementing the passthroughs
([#234](https://github.com/or1can/ratect/issues/234)): an empty value, one
holding a `:` and a negative integer are load-time errors, since none is a name
or an id and a `:` would bring back the packed form; and until `run_as_current_user` leaves
the native format, `user` beside an enabled one is refused the way `identity`
beside `user` will be. Refined before implementation
([#235](https://github.com/or1can/ratect/issues/235)): the probes behind this
record varied the daemon and the user on stock images, and further probes on
OrbStack, varying the image and the mounts, found the home guarantee failing. So
identity sets `HOME` as well as editing the entry, a home directory is created
only where none exists, identity files that are absent, unusual or lack the
running user have rules of their own, and cache ownership is taken only on
rootful Linux, and only of a root-owned named volume. Refined while implementing
that ([#235](https://github.com/or1can/ratect/issues/235)): a symbolic link at the
home counts as existing, and a mount at `/etc` is refused as one at `/etc/passwd`
is, since the write would reach the host the same way. A correctness pass on
OrbStack then found a mount below the home making the daemon create it as root,
which left a non-root running user unable to write to its own home, so that one
case is handed over. It also found a `tmpfs` above the home hiding what is created,
which is refused, and a `..` in `home` meaning different directories to Docker's
stat and upload, which is refused at load. Refined while implementing rootful
Linux ([#236](https://github.com/or1can/ratect/issues/236)): host-side mount
pre-creation applies there, and only there, over a local socket, and a remote
daemon's edge is recorded under Consequences. Probing it found a cache at
`~/.cargo/registry` leaving `~/.cargo` root's, so the hand-over reaches every directory
on the way to a mount, keeping its mode, rather than the home alone at `0755`, and
leaves one inside another mount alone. Probed while implementing the hazard warnings
([#237](https://github.com/or1can/ratect/issues/237)), on Docker 29.9.3 rootless and
userns-remap: the mapping below holds, the access it leaves is stated, and an image's
non-root `USER` under `identity` meets it as `user` does, so those are no longer
inferences. Extended before release ([#107](https://github.com/or1can/ratect/issues/107)):
`identity` can declare users the image lacks, under
[Additional users](#additional-users). Probed before release
([#817](https://github.com/or1can/ratect/issues/817)): Docker Desktop on Linux stays
rootful Linux, under [Daemon classification](#daemon-classification), at a cost stated
under Consequences.


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
  says bind-mount "file ownership must be pre-arranged". Probed (#237): writes to a
  world-writable mount landed on `100000` and `101234` for container 0 and 1234,
  container root could neither write to a host-owned `0755` mount nor list a `0700`
  one, and a directory chowned on the host to `101234` took 1234's writes.

  Both strings verified against moby's [`info.go`](https://github.com/moby/moby/blob/master/daemon/info.go)
  (`fillSecurityOptions`, where `name=userns` is added whenever the daemon's
  remapped root pair is not `0:0`); both are bare entries with no
  `,profile=` suffix, so whole-entry exact match is safe. *Remapped* below
  means either of these two.
- **Desktop** — Ratect's own host is macOS or Windows *and* the connection is
  a local socket. This is the heuristic the design owns explicitly: OrbStack,
  Docker Desktop and rootful Ubuntu all report the identical
  `SecurityOptions`, so there is no daemon-side signal short of a vendor list. Classifying by
  host-plus-transport covers colima and future runtimes without a vendor list,
  and a remote daemon reached from a Mac (`ssh://`, `tcp://`) correctly falls
  through to what it reports itself.
- **Rootful Linux** — the remainder. That includes Docker Desktop on Linux,
  deliberately, though it is a VM with file sharing as well. `identity` works there, as
  it does on native Linux CI, and only vendor-specific signals identify it: see
  Alternatives. [Probed](https://github.com/or1can/ratect/issues/817#issuecomment-6661813666)
  (#817) on Docker Desktop 4.98.0, engine 29.9.2, default file sharing, host user 1001:
  it reports neither `name=rootless` nor `name=userns`. The default user, `--user 1001:1001`
  and `--user 1000:1000` each saw a bind mount as their own and wrote to it, with the host
  owner staying 1001:1001, and `identity` through `ratect run` exited 0 with the
  same result. That is one version on one host, a GitHub Actions runner, so under nested
  virtualization, which Docker doesn't support. Older versions, synchronized file shares,
  `identity`'s writable `cache` mounts and a non-nested host weren't probed.

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
- Create the home directory as [below](#the-home-guarantee), and take ownership
  of each writable cache mount that is a named volume with a root-owned root,
  keeping its mode: fresh named volumes are root-owned and unwritable by the
  host uid here. Ratect chose this uid, so it owes that uid a usable cache, and
  every `identity` container one person runs has the same one. A volume another
  user owns is left alone, and so is a directory cache, which Ratect has already
  created as the host user.
- Create what a mount needs on the host first, as the host user, as
  `run_as_current_user` does: a missing bind source, or a mount point nested inside
  a host mount, would otherwise be created by the daemon as root in the user's own
  project. On desktop the files are already host-owned, a rootless daemon creates
  them as the user, and on userns-remap ownership has to be arranged on the host
  anyway.

On **desktop** and **remapped** daemons: **`--user` is not applied, no cache changes
owner, and no existing directory does beyond [the one case below](#the-home-guarantee).** On desktop and rootless the ownership outcome already holds
(desktop file sharing maps it; rootless delivers it by construction), and
under rootless, applying `--user` is actively destructive. On userns-remap it
cannot be met at all — the host uid maps to a subordinate uid like every
other — so `--user` would change nothing, and a run reaching such a container
prints a warning and `ratect doctor` reports a finding that bind-mount
ownership has to be pre-arranged on the host. The home guarantee still holds:
edit the *running* user's passwd entry (the image's configured user, root when
it has none) so its home field is the configured path. A name selects the entry
with that name, and a uid the first entry with that uid, the one the runtime
uses, leaving any other for the same uid untouched. A running user with no entry
gets one appended, since without it `whoami` fails and `$HOME` is `/`: named
with its literal uid digits (`root` for uid 0, since the process is not the host
user here), password `*`, the configured `home`, shell `/bin/sh`, and the numeric
group from the image's `User` or else 0, the gid such a user already runs with
(probed). A *named* user with no entry gets nothing appended, and the runtime
reports it.

### The home guarantee

On every daemon class, however the entry above was reached:

- **`HOME` is set to `home` in the container's environment**, over any value the
  image's own `ENV` gives it, which otherwise beats the passwd edit. A `HOME`
  written in the project's `environment` still wins: its author stated it, so
  this is no silent precedence, and the passwd home stays the configured path.
- **A home directory is created only where none exists**, owned by the running
  user, with any missing ancestors created as root, mode `0755`. An existing
  directory or symbolic link is left untouched even if the running user can't write to it, since
  the project chose that path; Docker's archive API has no create-only form, so
  an upload rewrites the owner and mode of what it lands on, through a bind mount
  and onto the host as well. A file at that path is an error naming the container,
  as is a `tmpfs` above it, which would hide what is created.
- **A mount below the home is the exception to "untouched".** The daemon creates
  each bind mount's and volume's destination and its parents as root on the first
  archive call, even a stat, so by the time Ratect can look, a directory made for
  `~/.ssh` or `~/.cargo/registry` looks like a root-owned one the image shipped. Each
  directory from `home` down to a mount, with root as its owner and a non-root running
  user, goes to the running user, keeping its mode, unless it is at or inside another
  mount. That also hands over a root-owned one an image shipped, which naming that
  path as this user's home asks for, though not what is in it. It is all in the
  container's layer, so the host is untouched. Docker offers nothing here: `--user`
  with such a mount leaves these directories root's (probed). Podman chowns a working
  directory and a fresh volume to the running user, but not their parents (read in
  libpod, not probed).
- **The identity files are read as untrusted bytes.** An absent `/etc/passwd`
  (`scratch`) is an empty one. A symbolic link, a mount at that path or at `/etc`,
  or a file over 1 MiB is an error naming the container. Every line not being edited is
  preserved byte-for-byte.

A relative `cache` destination is refused at load beside `identity`, on every
daemon class, so whether a project loads never depends on the daemon that later
runs it.

### Additional users

`identity = { home, additional_users = [{ name, uid, gid }] }` declares users the image
lacks, for tools that look one up. They sit under `identity` rather than beside the
passthroughs, which never edit identity files. On every daemon class, once the running
user's entry is settled:

- Each gets one appended passwd entry: password `*`, home `/nonexistent`, which nothing
  creates ([#225](https://github.com/or1can/ratect/issues/225)), shell `/bin/sh`, and
  `/etc/shadow` stays untouched, as for the running user. A group entry follows the
  keep-id rule above, named after the user.
- All three fields are required, since an allocated id would differ per image.
- **At load:** the name's shape, uid or gid 0, and additional users sharing a name, uid
  or gid.
- **When the files are read:** a name or uid already in `/etc/passwd`, the image's or the
  running user's, fails the run with nothing written. A gid already in `/etc/group` is
  reused: sharing one is ordinary.
- A named running user with no entry on a desktop or remapped daemon gets nothing, as
  above, even if listed here, and the runtime reports it. The running user's entry is
  settled first, so an additional user can't be taken for it.

### Passthrough hazards

A non-root `user` on a remapped daemon does not write as the host user: it has only
other users' access to a bind mount the host user owns, so it can't write to a `0755`
one or read a `0600` file or `0700` directory there (probed on both, #237). That
includes the host's own uid as `user`, which under rootless is the failure the
original probes found, and under userns-remap container root as well. It stays a
passthrough — that is what an escape hatch is — but a run reaching such a container
prints a warning, and `ratect doctor` reports a finding naming the hazard. An
`identity` whose running user is non-root because the image says so meets the same
mapping (probed on both, #237), so a run prints that warning too. `doctor` can't
report it: the image's user isn't in the configuration, and the image may not be local.

## Alternatives considered

- **Checking additional users against the host user at load**, as #107's first brief
  asked. Rejected: on most daemons the host user isn't the running user, and that
  depends on a daemon loading doesn't reach, so whether a project loaded would depend
  on it. The check runs against the files, where the real running user is.
- **Skipping any clash, or accepting an additional user the image already has.**
  Rejected: skipping leaves a lookup finding a different uid from the one declared, in
  silence. Accepting an exact match is one more branch, to spare deleting a declaration
  once an image starts shipping the user.
- **Additional users as a container field apart from `identity`.** Rejected for now: it
  would bring identity-file edits to containers that have no other reason for any.
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
  loss also removes the one reason Ratect's own choice of user would have needed
  cache chown on macOS. An image's own non-root `USER` still meets a root-owned
  fresh volume there: see Consequences.
- **Taking ownership of caches for an image's non-root `USER`** on desktop and
  remapped daemons. Rejected: it helps only while one uid uses the volume. A second
  uid is still refused on the first uid's volume, and even under `1777` it can create files
  but not change the other's (probed), so sharing a cache between identities
  needs file modes only the tools or the image control. The upload also
  widened a `700` cache to `755`, and on a remapped daemon would hand a host
  directory to a subordinate uid (the mapping probed in #237). Ratect takes ownership only for a user it chose.
- **The passwd edit alone for `$HOME`.** Rejected: an image's `ENV HOME` beats it
  (probed).
- **Refusing a `HOME` in `environment` beside `identity`.** Rejected: written by
  hand, it says what its author wants, and a refusal needs a check across task
  and `customise` overlays to prevent nothing silent.
- **Naming an appended entry on these daemons after the host user**, as rootful
  Linux does. Rejected: the process isn't the host user here, and on `scratch` it
  would name uid 0 after them.
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
  actually matters (a VM with file sharing), on macOS and Windows.
- **Classifying Docker Desktop on Linux as Desktop** (#817). Rejected: what identifies
  it is vendor-specific (`OperatingSystem`, the `com.docker.desktop.address` label, the
  `linuxkit` kernel), so it would be the vendor list rejected above, and one entry
  wouldn't cover other VM-based runtimes on Linux. Its per-user socket path is a
  client-side convention a user can change.
- **Docker-style `user = "name[:group]"`.** Rejected: two meanings in one
  string; the split fields make "primary group without user" a load-time
  error instead of an unwritable value.
- **A `umask` passthrough.** Out of scope: the runtime has no such option, so
  it is not a passthrough at all — it would need an entrypoint-wrapper
  mechanism of its own.
- **A throwaway container to tell a daemon-made home from an image's**, created
  with no mounts so a stat answers exactly. Rejected: a create and remove per
  affected container, and one more thing to clean up on an interrupt, to spare a
  root-owned image home that a non-root user can't write to anyway.
- **Documenting a mount below the home as needing the image to create it.**
  Rejected: `~/.ssh` and cache mounts are why `home` exists, and it would leave them
  broken under a non-root image that doesn't ship that home.
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
- **Accepted edge:** a non-root image `USER` on a desktop or remapped daemon
  can't write a fresh named cache volume, so a project can pass on rootful Linux
  CI and fail on a Mac. It fails on first write, and the fix is in the image: a
  fresh volume takes its owner from an image directory at that path (probed).
- **Not Ratect's to solve:** a cache shared by different uids or groups, which
  its users have to arrange themselves.
- **Accepted edge:** an entrypoint that drops privileges (`postgres`) runs as a
  user other than the running user, whose home doesn't move.
- **Accepted edge:** a `working_directory` below `home` that isn't a mount leaves it
  and `home` root-owned, as Docker creates them (probed). Treating it as
  a mount would hand over a project directory an image shipped, for a rare layout
  that a mount at it avoids.
- **Accepted edge:** a `tmpfs` at `home` masks what was created there, so under a
  non-root running user its options have to allow that user to write.
- **Accepted edge:** a Mac or Linux client reaching a Linux daemon over `ssh://` or
  `tcp://` classifies as rootful Linux, and the host identity stays that of the user
  running Ratect. Ratect can't create directories on a remote host, so it skips the
  pre-creation above unless the connection is a local socket, since on this machine
  it would leave stray directories or fail a run over a path only the daemon has. The
  daemon creates a missing source as root. A Windows client there has no host identity to adopt, so
  the run fails naming `identity` and the container.
- **Accepted cost:** Desktop's own reason applies to Docker Desktop on Linux too, since
  host ownership already holds there (probed, #817). So `--user` and the identity-file
  edits that go with it are unnecessary on that daemon, `ratect doctor` calling it
  `rootful Linux` isn't accurate, and a project runs as the host user there but as the
  image's user on Docker Desktop on a Mac.
- **On userns-remap, `identity` delivers only the home guarantee**, and any additional
  users' entries. Host
  ownership of bind-mount writes is impossible there by design of the
  daemon, not something Ratect could fix; the warning and `doctor` finding
  say so rather than letting the declared intent go quietly unmet.
- Effects on open work: [#107](https://github.com/or1can/ratect/issues/107)
  shrinks to "declare users the image lacks", since the image's users now
  survive, which is [Additional users](#additional-users);
  [#225](https://github.com/or1can/ratect/issues/225)'s
  collision surface dissolves with the chown step it catalogued (ownership is
  taken only of root-owned named cache volumes on rootful Linux, and of a home
  directory by creating it, or when a mount below it left it root-owned);
  [#226](https://github.com/or1can/ratect/issues/226) becomes the `groups`
  passthrough plus the identity's own group handling. All three sit in this
  record's milestone; `ratect 0.10.0` carries none of it.
- The glossary terms this design speaks — *host identity*, *image identity*,
  *identity*, *passthrough*, *running user* — are defined in [CONTEXT.md](../CONTEXT.md).
