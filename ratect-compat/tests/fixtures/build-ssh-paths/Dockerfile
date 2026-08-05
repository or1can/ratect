FROM alpine:3.18.2
RUN apk add --no-cache openssh-client
ARG CACHE_BUST
# `ssh-add -l` exits non-zero when the agent holds nothing, so this `RUN`
# failing is itself part of the assertion: reaching the next step at all
# means the named agent was reachable and served an identity.
RUN --mount=type=ssh,id=deploy echo "deploy keyring, build $CACHE_BUST" && ssh-add -l > /deploy-agent-keys.txt
