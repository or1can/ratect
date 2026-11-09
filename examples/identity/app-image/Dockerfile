FROM alpine:3.24

# ANCHOR: dockerfile
# A non-root user with its own home, as many language images have.
RUN adduser -D -u 1000 app
ENV HOME=/home/app

# A fresh cache volume takes its owner and mode from the image's directory at
# the same path, here below the `app` container's `identity` home in
# ratect.toml. Writable by anyone, so whoever the container runs as can fill
# the `prepared` cache. The `unprepared` one has no directory here.
RUN mkdir -p /home/me/.cache/prepared && chmod 777 /home/me/.cache/prepared

USER app
# ANCHOR_END: dockerfile
