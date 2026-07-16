# Security Policy

## Supported versions

Ratect is pre-1.0: only the **latest released `0.x` version** receives security
fixes. There are no maintenance branches for older releases — a fix ships as (part
of) the next release.

## Reporting a vulnerability

Please **do not** open a public issue for a suspected vulnerability.

Instead, use GitHub's private vulnerability reporting: **[Security →
Advisories → Report a vulnerability](https://github.com/or1can/ratect/security/advisories/new)**
on this repository. You'll get an acknowledgement within a few days, and the report
stays private while a fix is prepared.

## Scope worth knowing about

The areas of Ratect most likely to matter for a security report, and where its
existing hardening lives:

- **Git includes** (`type: git`): fetched bundles are treated as untrusted input —
  path containment within the clone, restricted Git transports, and argv hygiene
  are enforced (see the
  [config reference](docs/config-reference.md#git-includes)). Anything that lets a
  fetched bundle read or write outside its clone (or the project directory) is a
  vulnerability.
- **Container/volume path resolution**: escapes of the documented containment
  rules via config values (volumes, `build_directory`, `build_secrets.path`, …).
- **`run_as_current_user`**: the generated `/etc/passwd`/`/etc/shadow`/`/etc/group`
  content — injection via config-controlled values is a vulnerability.
- **`build_secrets`/`build_ssh`**: a secret's value or the forwarded agent leaking
  into image layers, logs, error messages, or cache keys.
