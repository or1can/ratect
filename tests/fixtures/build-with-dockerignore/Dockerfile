FROM alpine:3.18.2
COPY . /context
RUN test -f /context/keep.txt
RUN test -f /context/nested/build/output.txt
RUN test ! -e /context/build
RUN test ! -f /context/secret.txt
RUN test ! -f /context/nested/secret.txt
