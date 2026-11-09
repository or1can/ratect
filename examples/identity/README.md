# File ownership and home directory example

Who owns what a container writes through a bind mount, and what `$HOME` is
inside it, with [`identity`](../../docs/ratect-config-reference.md#identity-your-files-and-the-same-home-everywhere)
and without. See
[File Ownership and the Home Directory](../../docs/file-ownership.md) for the
full writeup, including what each task prints on each kind of Docker daemon.

## Tasks

```
$ ratect tasks list
```

- `show-home` — the running user, its groups, `$HOME` and its `/etc/passwd`
  home, from a container with `identity` and an ssh configuration mounted
  below the home
- `show-home-override` — the same container with `HOME` set in `environment`
- `write-as-you` — writes `out/as-you/` through a bind mount, with `identity`
- `write-as-image` — writes `out/as-image/` through the same mount, without
  `identity`
- `clean` — removes both from a container, which is what removes them when a
  rootful Linux daemon has left `out/as-image/` owned by root
- `show-app-home` — the running user and home in an image (`app-image/`) with
  its own non-root `USER` and `ENV HOME`
- `fill-caches` — writes to two cache volumes below that home: one the image
  prepares, which is writable on every daemon, and one it doesn't, which only a
  rootful Linux daemon makes writable, by handing it to you

## Running it

```
ratect run write-as-image
ratect run write-as-you
ls -lnd out/* out/*/*
ratect run clean
```
