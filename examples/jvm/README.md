# JVM (Gradle) worked example

A minimal Gradle project — one `Hello.java` with a `greet()` method, a
JUnit test for it, and a `batect.yml` (not `ratect.toml`) driving it. See
[Worked Examples](../../docs/worked-examples.md#jvm-gradle)
for why this one's `batect.yml`, and for the full writeup.

## Tasks

```
$ ratect-compat --list-tasks
```

- `build` — `./gradlew assemble`
- `test` — `./gradlew test` (checks `greet()` returns the right string)
- `run` — `./gradlew run`, prints "Hello, world!" for real
- `lint` — `./gradlew checkstyleMain checkstyleTest`
- `shell` — an interactive shell in the build environment

## Running it

```
ratect-compat build
ratect-compat test
ratect-compat run
ratect-compat lint
```

The task set relies on the project's own checked-in Gradle wrapper
(`./gradlew`), so the image itself needs nothing but a JDK. The Gradle cache
(`~/.gradle`) is its own `cache` volume mount, so only the first run of any
task is slow.
