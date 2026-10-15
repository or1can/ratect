FROM alpine:3.18.2
COPY . /context
RUN test "$(stat -c %u:%g /context/file.txt)" = 0:0
RUN test "$(stat -c %u:%g /context/nested/file.txt)" = 0:0
