FROM alpine:3.18.2
RUN apk add --no-cache openssh-client
ARG CACHE_BUST
RUN --mount=type=ssh echo "cache bust: $CACHE_BUST" && ssh-add -l > /ssh-agent-keys.txt
