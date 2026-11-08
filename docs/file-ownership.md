# File Ownership and the Home Directory

A task that writes through a bind mount, such as a build writing its output or
a formatter its fixes, writes onto your machine. Who owns those files on the
host depends on the daemon and on the user the container runs as. On a rootful
Linux daemon, an image with no `USER` writes them as root: files you can't edit,
and directories you can't remove.
Separately, a program inside the container that looks under `~` for its
configuration finds the home of the image's user, which differs from image to
image.

[`identity`](ratect-config-reference.md#identity-your-files-and-the-same-home-everywhere)
does both jobs. What a container writes through a bind mount is yours wherever
the daemon allows it, and `$HOME` is the path you name, on every daemon and in
every image, so ssh configuration and caches can be mounted below it once:

```toml
{{#include ../examples/identity/ratect.toml:as-you}}
```

That container is from
[`examples/identity`](https://github.com/or1can/ratect/tree/main/examples/identity),
which the rest of this page walks through. The reference has every rule
`identity` follows; this page is what those rules add up to on each daemon.

## Who owns what a container writes

Which kind of daemon you have is the `Docker daemon class` line of
[`ratect doctor`](ratect-cli.md#doctor). On each, a container writing to a
directory of yours through a bind mount gives this:

| Daemon class | Image's `USER` | Runs as | Owner on the host, without `identity` | Owner on the host, with `identity` |
| --- | --- | --- | --- | --- |
| `rootful Linux` | root, or none | root, or you with `identity` | root | you |
| `rootful Linux` | non-root, such as `app` | that user, or you with `identity` | that user's uid, where the directory lets others write | you |
| `desktop` | any | the image's `USER`, or root | you | you |
| `rootless` | root, or none | root | you | you |
| `rootless` | non-root | that user | a subordinate uid, where the directory lets others write | the same, and the run warns |
| `userns-remap` | any | the image's `USER`, or root | a subordinate uid, where the directory lets others write | the same, and the run warns |

How each daemon gets there:

- **On a rootful Linux daemon** a file's owner is the uid of the process that
  wrote it, so `identity` runs the container as your uid and gid. Without it, a
  non-root image `USER` can only write where the directory lets anyone write,
  and a directory you created with the usual `0755` refuses it.
- **Docker Desktop, OrbStack and similar on macOS or Windows** share files with
  the host as you, whoever wrote them in the container. `identity` leaves the
  image's `USER` running.
- **A rootless daemon** maps root in the container to you, so an image that runs
  as root writes your files without `identity`. Any other container user maps to a
  subordinate uid, which has only other users' access to your directory.
- **A userns-remap daemon** maps every container user to a subordinate uid,
  root included. Root in the container writes as uid `100000`, say, so a
  directory you own with `0755` refuses it.

Where the owner is a subordinate uid, nothing Ratect can apply makes it yours, so
a run warns about each `identity` container that meets it, naming the container,
and `ratect doctor` reports the userns-remap case. That ownership has to be
arranged on the host. The home is still guaranteed there.

## Writing through a bind mount

The example has a second container, the same image and mount without
`identity`, and a task that writes through each:

```toml
{{#include ../examples/identity/ratect.toml:as-image}}

{{#include ../examples/identity/ratect.toml:write}}
```

`ratect run write-as-image -o quiet && ratect run write-as-you -o quiet && ls
-lnd out/* out/*/*`, captured on a terminal in `examples/identity`, on a
`rootful Linux` daemon:

```ansi
{{#include captures/identity-ownership-rootful.ansi}}
```

`alpine` has no `USER`, so without `identity` its directory and file are
root's. With `identity` they belong to uid 1001, the user who ran Ratect. The
same command on a `desktop` daemon, OrbStack on macOS, where both containers ran
as root:

```ansi
{{#include captures/identity-ownership-desktop.ansi}}
```

Both are yours there, `identity` or not, since file sharing makes them so.

### Removing what root left behind

A root-owned directory can't be removed without root, even in a directory you
own: removing a directory means removing what is in it, and that takes write
access to the directory itself. The `clean` task does it from a container
without `identity`, which runs as root. `rm -rf out/as-image; ratect run clean
-o quiet && ls -A out`, captured on a terminal in `examples/identity`, on a
`rootful Linux` daemon:

```ansi
{{#include captures/identity-clean-rootful.ansi}}
```

`out/.gitignore` is the one file the project keeps there, so `out` itself is
yours from the moment you clone it. Without it, the first task to write there
would create `out`, and on a rootful Linux daemon a container without
`identity` would create it as root, leaving the next one with `identity` unable
to write inside it.

To see all of this on your own daemon:

```sh
ratect run write-as-image
ratect run write-as-you
ls -lnd out/* out/*/*
ratect run clean
```

## The same home on every daemon

```toml
{{#include ../examples/identity/ratect.toml:show-home}}
```

`ratect run show-home -o quiet`, captured on a terminal in `examples/identity`,
on a `desktop` daemon:

```ansi
{{#include captures/identity-show-home-desktop.ansi}}
```

and on a `rootful Linux` daemon:

```ansi
{{#include captures/identity-show-home-rootful.ansi}}
```

The running user differs, root on one and you on the other, but `$HOME` and that
user's `/etc/passwd` entry agree on `/home/me` on both, so Git, which reads
`$HOME`, and ssh, which reads the passwd entry, both look there. On the rootful
daemon the image had no entry for uid 1001, so one was added with your username.

The read-only mount at `~/.ssh` is below the home, so Docker created `/home/me`
before Ratect could, as root. Where the running user isn't root, the home is
handed over to them, which is why it is `runner`'s on the rootful daemon. `groups`
works alongside `identity`, adding `users` to whoever runs.

### An image with its own user and home

Some images run as a non-root user with a home of their own. The
example builds one:

```dockerfile
{{#include ../examples/identity/app-image/Dockerfile:dockerfile}}
```

and runs it with `identity` and two cache volumes below the home:

```toml
{{#include ../examples/identity/ratect.toml:app}}

{{#include ../examples/identity/ratect.toml:show-app-home}}
```

`ratect run show-app-home -o quiet`, captured on a terminal in
`examples/identity`, on a `desktop` daemon:

```ansi
{{#include captures/identity-show-app-home-desktop.ansi}}
```

and on a `rootful Linux` daemon:

```ansi
{{#include captures/identity-show-app-home-rootful.ansi}}
```

The image's `app` user runs on one and you on the other, and `identity`'s home replaces the
image's `ENV HOME` on both.

### Cache volumes below the home

```toml
{{#include ../examples/identity/ratect.toml:fill-caches}}
```

`ratect run fill-caches -o quiet`, captured on a terminal in
`examples/identity`, on a `desktop` daemon, where the task fails:

```ansi
{{#include captures/identity-fill-caches-desktop.ansi}}
```

and on a `rootful Linux` daemon, where it passes:

```ansi
{{#include captures/identity-fill-caches-rootful.ansi}}
```

A fresh volume is root-owned unless the image has a directory at the same
path, whose owner and mode it then takes. On a rootful Linux daemon, Ratect
hands a root-owned cache volume to you before the container starts, so both
caches are writable there. Everywhere else, `app` can't write to the
`unprepared` volume. The `prepared` one takes mode `777` from the image's
directory, so it is writable on every daemon.

`chown app` in the image would fix the `desktop` daemon and break the rootful
one: there you run instead of `app`, and Ratect leaves a volume another uid owns
alone. A volume that has already been created keeps its owner, so after changing
the image, [remove the cache](ratect-cli.md#caches-options) to have it created
again.

### Setting `HOME` yourself

A `HOME` in `environment` wins over `identity`'s, though the passwd entry still
says `home`:

```toml
{{#include ../examples/identity/ratect.toml:show-home-override}}
```

`ratect run show-home-override -o quiet`, captured on a terminal in
`examples/identity`, on a `desktop` daemon:

```ansi
{{#include captures/identity-show-home-override.ansi}}
```

So Git looks in `/code` and ssh in `/home/me`. Leave `HOME` out of `environment`
unless that split is what you want.

## What `identity` leaves to you

- **It is opt-in.** A container without it on a rootful Linux daemon writes as
  its image's `USER`, or root if it has none. Unless it sets a non-root `user`,
  `ratect doctor` warns about each of its writable bind mounts, since it doesn't
  pull the image to see its `USER`.
- **`home` is required**, even if ownership is all you want.
- **An entrypoint that drops privileges**, as `postgres` does, writes as the
  user it switches to, whoever the container started as.
- **A daemon on another machine** (`ssh://`, `tcp://`) still gets the uid and gid
  of the user running Ratect, which needn't be yours on that machine. Ratect
  creates no host directories for it, so the daemon creates a missing bind mount
  source as root.
- **Docker Desktop on Linux** is a `rootful Linux` daemon to Ratect, so
  `identity` runs the container as you there too, although its file sharing may
  make what it writes yours anyway. See
  [On a rootful Linux daemon](ratect-config-reference.md#on-a-rootful-linux-daemon).

Coming from Batect? `identity` has the same goal as `run_as_current_user`, and
[Coming from `run_as_current_user`](ratect-config-reference.md#coming-from-run_as_current_user)
says which daemons get which mechanism.

## Where to go next

- Every rule `identity` follows, including adding service users an image
  lacks, is in the
  [`identity` reference](ratect-config-reference.md#identity-your-files-and-the-same-home-everywhere).
- Which daemon Ratect connects to, and how to choose another, is
  [Connecting to Docker](connecting-to-docker.md).
- The next page, [Interactive Mode](interactive-mode.md), covers when a task's
  container gets a terminal.
