# The build environment for Ratect's own dev tasks (see the repository-root
# batect.yml). The official `rust` image ships without the rustfmt and
# clippy components, so add them here — everything else the build needs (the
# C toolchain for `ring`, git for the patched `bollard` dependency) is
# already in the full (non-slim) image. Pinned, not `latest`, so `ratect
# doctor` stays quiet.
FROM rust:1.83-bookworm
RUN rustup component add rustfmt clippy
