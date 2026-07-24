# The build environment for Ratect's own dev tasks (see the repository-root
# batect.yml). The official `rust` image ships without the rustfmt and
# clippy components, so add them here — everything else the build needs (the
# C toolchain for `ring`, git for the patched `bollard` dependency) is
# already in the full (non-slim) image. Pinned, not `latest`, so `ratect
# doctor` stays quiet.
#
# This version is the floor the dogfood build actually runs on, so it must
# stay high enough to build the current dependency tree — some deps (e.g.
# `home`) already require a recent rustc. CI's `dev-env` job reads this exact
# version and runs `cargo build`/`test` on it, so a dependency raising its
# MSRV above this pin fails CI rather than silently breaking the dogfood
# build; when that happens, bump this line. There's no separate MSRV promise
# — this just tracks a recent stable.
FROM rust:1.96-bookworm
RUN rustup component add rustfmt clippy
