# Records the proxy build args this build was given, for the task to print.
# CACHE_BUST is there because Docker leaves its predefined proxy args out of
# the build cache key, so without it a run with them could reuse the layer
# of a run without.
FROM alpine:3.18.2
ARG CACHE_BUST
ARG HTTP_PROXY
ARG http_proxy
RUN echo "cache bust: $CACHE_BUST" && \
    echo "HTTP_PROXY=$HTTP_PROXY http_proxy=$http_proxy" > /proxies.txt
