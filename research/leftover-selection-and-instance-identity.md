# Leftover selection and instance identity: how comparable tools do it

Research for [ratect#103](https://github.com/or1can/ratect/issues/103)
(service groups, `services up`/`down`),
[ratect#124](https://github.com/or1can/ratect/issues/124) (detached mode),
[ratect#265](https://github.com/or1can/ratect/issues/265) (what `resources clean`
selects) and
[decisions/0025](../decisions/0025-task-run-lock.md) and
[decisions/0026](../decisions/0026-service-instances.md). It answers
two questions. How do comparable tools identify a long-lived group of
containers across separate CLI invocations, including one that has no
supervising process? And how do they decide which of the containers and
networks they created are safe to remove, and which are still wanted? It
records facts and precedents only. It does not choose a design for ratect.

Every claim cites the source that owns it, pinned to a tag or commit: Docker
Compose `v5.5.0`, compose-go `v2.14.0` (the version Compose `v5.5.0`'s
`go.mod` requires), Docker's documentation `e8cdfe4b` (docker/docs `main`,
2026-11-12), moby `docker-v29.8.2`, docker/cli `v29.8.2`, moby-ryuk `0.14.0`,
testcontainers-java `2.0.5`, testcontainers-go `v0.45.0`, Dev Containers CLI
`v0.90.0`, Skaffold `v2.25.0`, Tilt `v0.37.8`, Dagger `v0.21.11`, Earthly
`v0.8.16`, Batect `67c94224` (0.85.0 plus 3 commits). Where this document
reasons from a source without quoting it, it says so ("inference"). Three
claims rest on a local probe and not on a source. They are marked
"observation", and [The probe](#the-probe) says what was run and what it did
not vary. Anything that could not be checked is in
[Not verified](#not-verified).

## Summary

1. **Compose identifies a project by its name and nothing else.** Every
   lookup filters on the `com.docker.compose.project` label. The absolute
   working directory and config file paths are written to labels too, but no
   lookup filter reads them. Observation: two directories with the same base
   name, at different absolute paths, were one project, and `down` in the
   second removed the container the first had started.
2. **The tools that add a path to the identity do it to find one container
   again, not to group several.** The Dev Containers CLI looks for
   `devcontainer.local_folder` plus `devcontainer.config_file`, both absolute
   paths, and falls back to the folder alone for containers created before
   the second label existed.
3. **A session id identifies one process tree, so it cannot be recomputed by
   a later, unrelated invocation.** testcontainers-java uses a random UUID per
   JVM. testcontainers-go hashes its parent process's PID and creation time,
   so that the packages of one `go test` run share an id.
4. **No tool read here asks whether a supervising process is alive before
   removing a long-lived group.** Compose's `down` removes whatever carries
   the project label, running or not. Compose's `up` in the foreground keeps
   no record of itself: observation, its container kept running after the
   `up` process was killed with SIGKILL. Compose does write a PID file, but
   only for `watch`, and only to stop a second `watch` on the same project.
5. **The one liveness scheme found is a held connection, and it is built for
   the opposite case.** Ryuk removes everything matching a session's labels
   10 seconds after its last client disconnects, force-removing running
   containers. Its purpose is to remove what a dead process left, so by
   design nothing outlives the process.
6. **"Stopped" in `docker container prune` means "not running", which is
   three states.** It removes containers in the created, exited and dead
   states. It leaves running, paused and restarting ones.
7. **Compose's own notion of a leftover is narrow.** An orphan is a container
   whose service is no longer in the model, or a one-off (`compose run`)
   container that has exited or died. A running one-off is never an orphan.
8. **Networks are removed by use, not by ownership.** Compose's `down` skips a
   network that still has a container attached and reports it as in use.
   `docker network prune` removes only networks with no endpoints.

## 1. Identity across invocations

| Tool | What identifies the group | Where it is recorded |
| --- | --- | --- |
| Docker Compose | Project name | `com.docker.compose.project` label on each container, network and volume |
| Dev Containers CLI | Absolute workspace folder and absolute config file path | `devcontainer.local_folder` and `devcontainer.config_file` labels |
| Testcontainers (Java) | Random UUID per JVM | `org.testcontainers.sessionId` label |
| Testcontainers (Go) | SHA-256 of the parent process's PID and creation time | `org.testcontainers.sessionId` label |
| Dagger | Engine image id or version | Container name `dagger-engine-<id>` |
| Earthly | Installation name | Container name `<installation>-buildkitd` |
| Batect | Nothing durable: the container and network ids held in memory | Not recorded |

**Docker Compose.** `withNamePrecedenceLoad` takes the name from `-p`, then
`COMPOSE_PROJECT_NAME`, and, where the Compose file sets no `name:`, from the
base name of the working directory
([options.go](https://github.com/compose-spec/compose-go/blob/v2.14.0/cli/options.go#L554-L572)).
A `name:` in a Compose file applies only when the name was not "imperatively
set" by one of the first two
([loader.go](https://github.com/compose-spec/compose-go/blob/v2.14.0/loader/loader.go#L686-L758)).
The working directory is `--project-directory`, or the directory of the first
Compose file, or the current directory
([options.go](https://github.com/compose-spec/compose-go/blob/v2.14.0/cli/options.go#L420-L440)).
Docker's documentation gives the same order in five steps
([project-name.md](https://github.com/docker/docs/blob/e8cdfe4be9af10a8a5a18d429a7f9a51e34514a0/content/manuals/compose/how-tos/project-name.md?plain=1#L34-L40)).
A directory name is normalised to lowercase letters, digits, `-` and `_`
([loader.go](https://github.com/compose-spec/compose-go/blob/v2.14.0/loader/loader.go#L760-L765)).

Compose `v5.5.0` defines eighteen labels
([labels.go](https://github.com/docker/compose/blob/v5.5.0/pkg/api/labels.go#L25-L65)).
Those that bear on identity and selection:

| Label | Holds |
| --- | --- |
| `com.docker.compose.project` | Project name |
| `com.docker.compose.service` | Service name |
| `com.docker.compose.container-number` | Index of a replica |
| `com.docker.compose.oneoff` | `True` for a `compose run` container, `False` otherwise |
| `com.docker.compose.slug` | "unique slug used for one-off container identity" |
| `com.docker.compose.config-hash` | Hash of the service's configuration |
| `com.docker.compose.image` | Digest of the image the container runs |
| `com.docker.compose.project.working_dir` | "absolute path to compose project working directory" |
| `com.docker.compose.project.config_files` | "absolute path to compose project configuration files" |
| `com.docker.compose.depends_on` | The service's dependencies |
| `com.docker.compose.network`, `.volume` | The model's name for a network or volume |
| `com.docker.compose.version` | The Compose version that created the resource |

The rest are `.project.environment_file`, `.image-volume-digest`,
`.image.builder`, `.replace` and `.engine`.

Every container lookup goes through `getDefaultFilters`, which filters on the
project label, requires the config-hash label to be present, and optionally
adds the service and one-off labels
([containers.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/containers.go#L45-L88),
[filters.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/filters.go#L33-L55)).
`WorkingDirLabel` appears in two places in the non-test source: its
definition, and the map of labels written to each service
([loader.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/loader.go#L130-L139)).
`ConfigFilesLabel` is also read once, by `compose ls`, to print a project's
config files
([ls.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/ls.go#L72-L74)).
Neither is ever part of a filter. Observation: a project started in one
directory was listed, reconciled and removed from a second directory with the
same base name at a different absolute path (see [The probe](#the-probe)).
Docker's documentation presents the name as the isolation mechanism, "Compose
uses a project name to isolate environments from each other", and suggests
setting it "to a unique build number" on a CI server
([project-name.md](https://github.com/docker/docs/blob/e8cdfe4be9af10a8a5a18d429a7f9a51e34514a0/content/manuals/compose/how-tos/project-name.md?plain=1#L21-L25)).

**Dev Containers CLI.** `findContainerAndIdLabels` builds two label sets from
the workspace folder and config file, both resolved to absolute paths: the
"new" set `devcontainer.local_folder=<folder>` plus
`devcontainer.config_file=<file>`, and the "old" set with the folder alone. It
looks up the new set first and falls back to the old one, where it ignores any
container that carries a `devcontainer.config_file` label
([utils.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/utils.ts#L721-L766),
[label names](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/singleContainer.ts#L17-L18)).
A caller can replace both with `--id-label`, "used to query for an existing
container"
([devContainersSpecCLI.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/devContainersSpecCLI.ts#L143)).
The lookup includes stopped containers: `findDevContainer` calls
`listContainers` with `all` set, which adds `-a` to `docker ps`, and discards
only containers whose status is `removing`
([singleContainer.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/singleContainer.ts#L321-L325),
[dockerUtils.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-shutdown/dockerUtils.ts#L162-L175)).
It takes the first match. On Windows only, a second pass compares the paths
case-insensitively on the drive letter
([utils.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/utils.ts#L656-L698)).

**Testcontainers.** Both implementations label what they create with
`org.testcontainers.sessionId`
([DockerClientFactory.java](https://github.com/testcontainers/testcontainers-java/blob/2.0.5/core/src/main/java/org/testcontainers/DockerClientFactory.java#L57-L65),
[labels.go](https://github.com/testcontainers/testcontainers-go/blob/v0.45.0/internal/core/labels.go#L14-L27)).
They derive the value differently. Java's `SESSION_ID` is
`UUID.randomUUID()`, a static field, so it is one value per JVM. Go's is the
hex SHA-256 of `testcontainers-go:<parent pid>:<parent creation time>`, with a
random UUID as the fallback when the process list or the creation time can't
be read. The source gives the reason: "each Go package will be run in a
separate process", and the parent PID aggregates "the test execution of
multiple packages in the same test session"
([bootstrap.go](https://github.com/testcontainers/testcontainers-go/blob/v0.45.0/internal/core/bootstrap/bootstrap.go#L13-L94)).
Inference: neither id can be derived by an invocation outside that process
tree, so a session id finds a group for cleanup and not for a later `down`.

**Dagger and Earthly** each keep one long-lived engine container and find it
by name. Dagger names it `dagger-engine-<id>` after the engine image, starts a
container that already has that name, and treats every other container with
the `dagger-engine-` prefix as "leftover from previous executions of the
engine at different versions"
([container.go](https://github.com/dagger/dagger/blob/v0.21.11/engine/client/drivers/container.go#L234-L272)).
Earthly's is `<installation name>-buildkitd`
([global.go](https://github.com/earthly/earthly/blob/v0.8.16/cmd/earthly/flag/global.go#L15-L16)).

**Batect** records no identity. The only labels it sets on a container are
the ones the project's own configuration declares
([DockerContainerCreationSpecFactory.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/docker/DockerContainerCreationSpecFactory.kt#L62)).
Its cleanup works from the container and network ids of the run in progress
(see [section 3](#3-selecting-what-is-safe-to-remove)).

## 2. A second invocation against a running group

**Compose `up` reconciles and leaves a matching container alone.**
`mustRecreate` decides per container. `RecreateNever` (`--no-recreate`) always
keeps and `RecreateForce` (`--force-recreate`) always recreates. Otherwise a
container is recreated when a parent whose namespace or volumes it shares is
being recreated, when its config-hash label differs from the expected hash,
when its image digest label differs, when its image-volume digest label
differs, when it is running and its networks don't match, or when its volumes
don't match
([reconcile.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/reconcile.go#L753-L780)).
The parent rule is narrower than "a recreated dependency". It is derived from
`network_mode`, `ipc`, `pid` and `volumes_from`, "rather than depends_on, so
the cascade fires only when a stale "container:<id>" reference would otherwise
be left behind"
([reconcile.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/reconcile.go#L782-L800)).
Observation: `up -d` against a running container with an unchanged Compose
file printed `Running` and kept the same container id.

**Dev Containers `up` adopts what it finds.** When the lookup returns a
container, `startExistingContainer` runs `docker start` on it unless its
status is `running`, and the CLI goes on to use it. It builds and creates a
container only when the lookup returns none
([singleContainer.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/singleContainer.ts#L27-L39),
[startExistingContainer](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/singleContainer.ts#L304-L315)).
`--remove-existing-container` "Removes the dev container if it already exists"
([devContainersSpecCLI.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/devContainersSpecCLI.ts#L152)).
The CLI at this tag has no command that stops or removes a dev container on
its own: its commands are `up`, `set-up`, `build`, `run-user-commands`,
`read-configuration`, `outdated`, `upgrade`, `features`, `templates` and
`exec`
([devContainersSpecCLI.ts](https://github.com/devcontainers/cli/blob/v0.90.0/src/spec-node/devContainersSpecCLI.ts#L95-L116)).

**Earthly compares a label, as Compose does.** `maybeRestart` reuses the
running BuildKit container when its image id equals the available image's and
its `dev.earthly.settingshash` label matches the settings in force, and
restarts it otherwise
([buildkitd.go](https://github.com/earthly/earthly/blob/v0.8.16/buildkitd/buildkitd.go#L250-L300),
[label](https://github.com/earthly/earthly/blob/v0.8.16/buildkitd/buildkitd.go#L836-L844)).

## 3. Selecting what is safe to remove

**Compose `down` removes by project label, running or not.** `down` lists the
project's containers with `all` set, excluding one-off containers unless
`--remove-orphans` is given. It stops and then removes each service's
containers in reverse dependency order
([down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L44-L108)).
Removal passes `Force: true`
([down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L332-L353)).
It needs no Compose file. When no project model is passed in, it builds one
from the listed containers' labels, taking service names from the service
label and dependencies from the `depends_on` label, and reads the project's
networks and volumes from the daemon
([down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L56-L62),
[getProjectWithResources](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L355-L387),
[projectFromName](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/compose.go#L358-L408)).
Nothing in this path asks whether an `up` process is attached to the project.

**Compose's orphans.** `isOrphaned` selects two kinds of container. A one-off
is an orphan only when its state is exited or dead. Any other container is an
orphan when its service label names a service that is neither in the model
nor disabled in it
([containers.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/containers.go#L145-L158)).
`up` warns about orphans and removes them only with `--remove-orphans`, by
planning a stop and a remove for each
([create.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/create.go#L115-L120),
[reconcile.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/reconcile.go#L1036-L1053)).
Inference: because `down` builds its model from the containers it lists when
it has no Compose file, every service it finds is in that model, and only
exited or dead one-offs can be orphans there.

**Compose and networks.** `removeNetwork` inspects each of the project's
networks and, where the network still has containers attached, emits the
warning "Resource is still in use" and leaves it
([down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L190-L245)).
External networks and volumes are never candidates
([down.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/down.go#L135-L188)).

**Ryuk removes by label once nobody is connected.** A client connects over
TCP and sends lines of URL-encoded filters such as
`label=testing=true&label=testing.sessionid=mysession`. Each line is
acknowledged
([README.md](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/README.md?plain=1#L33-L46),
[reaper.go](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/reaper.go#L236-L280)).
`pruneWait` counts connections. When the count returns to zero it schedules a
prune check after `RYUK_RECONNECTION_TIMEOUT` (default `10s`), and a client
that connects in that window cancels it. With no connection at all the check
runs after `RYUK_CONNECTION_TIMEOUT` (default `60s`), which the README
describes as "The duration without receiving any connections which will
trigger a shutdown"
([reaper.go](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/reaper.go#L304-L376),
[config.go](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/config.go#L12-L45),
[README.md](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/README.md?plain=1#L71-L81)).
The prune lists containers "including stopped ones", networks, volumes and
images matching the filters, and removes containers with
`RemoveVolumes: true, Force: true`, so a running container is removed
([reaper.go](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/reaper.go#L29-L36),
[affectedContainers](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/reaper.go#L436-L460),
[prune](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/reaper.go#L673-L700)).
It guards one race. A matching resource created later than one second before
the check began (`RYUK_RETRY_OFFSET`, default `-1s`) makes the check wait and
retry, "to ensure in use resources are not removed"
([config.go](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/config.go#L26-L33)).
It skips containers labelled `org.testcontainers.ryuk=true`, which are other
reapers
([reaper.go](https://github.com/testcontainers/moby-ryuk/blob/0.14.0/reaper.go#L455-L460)).

The Java client sends the default labels plus the session id label as its
filter, over a socket it holds open for the life of the JVM
([ResourceReaper.java](https://github.com/testcontainers/testcontainers-java/blob/2.0.5/core/src/main/java/org/testcontainers/utility/ResourceReaper.java#L44-L52),
[RyukResourceReaper.java](https://github.com/testcontainers/testcontainers-java/blob/2.0.5/core/src/main/java/org/testcontainers/utility/RyukResourceReaper.java#L84-L125)).
It starts Ryuk as a container named `testcontainers-ryuk-<session id>` with
auto-remove, the Docker socket bound in, and privileged mode taken from
configuration
([RyukContainer.java](https://github.com/testcontainers/testcontainers-java/blob/2.0.5/core/src/main/java/org/testcontainers/utility/RyukContainer.java#L14-L28)).
The Java documentation says both that "In some environments ryuk must be
started in privileged mode to work properly" and, under "Disabling Ryuk",
that "Ryuk must be started as a privileged container". It offers
`TESTCONTAINERS_RYUK_DISABLED` for an environment that "already implements
automatic cleanup of containers after the execution, but does not allow
starting privileged containers", and adds that without Ryuk "Testcontainers
will continue doing the cleanup at JVM's shutdown, unless you `kill -9` your
JVM process"
([configuration.md](https://github.com/testcontainers/testcontainers-java/blob/2.0.5/docs/features/configuration.md?plain=1#L74-L89)).

**`docker container prune` removes what is not running.** The daemon's
`ContainerPrune` walks every container and acts on those for which
`State.IsRunning()` is false, then applies the `until` and label filters and
removes the container without force
([prune.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/prune.go#L34-L95)).
`IsRunning` returns the `Running` field. `State()` shows how that field maps
to the states the API reports: with `Running` set, a container is paused,
restarting or running, and with it clear, removing, dead, created or exited
([state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L118-L146),
[IsRunning](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L226-L230)).
A restarting container has `Running` set on purpose: "we should consider the
container running when it is restarting because of all the checks in docker
around rm/stop/etc"
([state.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/container/state.go#L296-L300)).

| State | Removed by `container prune`? |
| --- | --- |
| created (never started) | Yes |
| exited | Yes |
| dead | Yes |
| removing | Attempted |
| running | No |
| paused | No |
| restarting | No |

The accepted filters are `label`, `label!` and `until`
([prune.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/prune.go#L27-L31)).
The API reference calls the operation "Delete stopped containers" and
describes `until` as pruning "containers created before this timestamp", so it
is an age-since-creation test, not time since exit
([swagger.yaml](https://github.com/moby/moby/blob/docker-v29.8.2/api/swagger.yaml#L9722-L9737)).
The CLI reference says that two different filters are combined with "and",
and that two `label` filters select containers with "**either**" label
([container_prune.md](https://github.com/docker/cli/blob/v29.8.2/docs/reference/commandline/container_prune.md?plain=1#L47-L55)).

**`docker network prune` and `docker system prune`.** The daemon's local
network prune skips config-only networks, networks that don't match the
filters, networks that aren't pruneable, and any network with at least one
endpoint
([prune.go](https://github.com/moby/moby/blob/docker-v29.8.2/daemon/prune.go#L97-L129)).
The CLI warns that it "will remove all custom networks not used by at least
one container"
([network_prune.md](https://github.com/docker/cli/blob/v29.8.2/docs/reference/commandline/network_prune.md?plain=1#L18-L26)).
`docker system prune` adds, to "all stopped containers", "all networks not
used by at least one container"
([system_prune.md](https://github.com/docker/cli/blob/v29.8.2/docs/reference/commandline/system_prune.md?plain=1#L34-L36)).
The daemon's test is endpoints, and the CLI's wording is containers. Whether a
network whose only containers are stopped has endpoints, and so whether it
counts as used, was not established (see [Not verified](#not-verified)).

**Batect cleans up within the run and has no command for what is left.**
Cleanup is a stage of the task's own state machine. `--no-cleanup-after-failure`,
`--no-cleanup-after-success` and `--no-cleanup` (both) turn it off
([CommandLineOptionsParser.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/cli/CommandLineOptionsParser.kt#L135-L141),
[RunOptions.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/execution/RunOptions.kt#L29-L32)).
When cleanup is disabled or fails, Batect prints commands for a person to
run: `docker rm --force --volumes <container id>` for each container and
`docker network rm <network id>` for the network
([RemoveContainerStepRule.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/execution/model/rules/cleanup/RemoveContainerStepRule.kt#L46),
[DeleteTaskNetworkStepRule.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/execution/model/rules/cleanup/DeleteTaskNetworkStepRule.kt#L53),
[FailureErrorMessageFormatter.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/ui/FailureErrorMessageFormatter.kt#L90-L168)).
`--clean` is "Cleanup caches created on previous runs and exit", and
`CleanupCachesCommand` touches only cache volumes, by the name prefix
`batect-cache-<project cache key>-`, or cache directories
([CommandLineOptionsParser.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/cli/CommandLineOptionsParser.kt#L70),
[CleanupCachesCommand.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/cli/commands/CleanupCachesCommand.kt#L51-L90)).
`CommandFactory` lists every command Batect has, and none lists or removes
containers or networks from an earlier run
([CommandFactory.kt](https://github.com/batect/batect/blob/67c942241c7d52b057c5268278d6252301c48087/app/src/main/kotlin/batect/cli/commands/CommandFactory.kt#L26-L36)).
Inference: a Batect process killed before its cleanup stage leaves containers
that nothing in Batect can find again, since they carry no label of
Batect's.

**Dagger removes by name prefix.** `garbageCollectEngines` removes every
container whose name starts with `dagger-engine-` other than the one it is
about to use, when cleanup is enabled
([container.go](https://github.com/dagger/dagger/blob/v0.21.11/engine/client/drivers/container.go#L334-L400)).

## 4. Whether a supervising process is alive

**Compose `up` in the foreground traps two signals and records nothing.**
`up` registers a handler for `SIGINT` and `SIGTERM` only. The first signal
stops the project's containers ("Gracefully Stopping... press Ctrl+C again to
force") and the second kills them
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L67-L73),
[up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L132-L179)).
Observation: after `SIGKILL` to a foreground `docker compose up` and its
plugin process, the service's container was still running 15 seconds later,
and a later `docker compose ps`, `up -d` and `down` treated it as an ordinary
member of the project. Nothing marked it as having lost its `up`.

**Compose does have a PID file, for `watch` alone.** `compose watch` takes
"the lockfile for this project before doing work": a file named
`<project name>.pid` in `XDG_RUNTIME_DIR`, or a per-OS fallback directory,
holding its PID
([watch.go](https://github.com/docker/compose/blob/v5.5.0/cmd/compose/watch.go#L96-L103),
[pidfile.go](https://github.com/docker/compose/blob/v5.5.0/internal/locker/pidfile.go#L28-L35),
[runtime.go](https://github.com/docker/compose/blob/v5.5.0/internal/locker/runtime.go#L23-L35)).
`Write` fails if the file already holds the PID of a live process. Liveness is
`kill(pid, 0)` on macOS and the existence of `/proc/<pid>` elsewhere on Unix
([pidfile.go](https://github.com/docker/compose/blob/v5.5.0/internal/pidfile/pidfile.go#L32-L67),
[pidfile_unix.go](https://github.com/docker/compose/blob/v5.5.0/internal/pidfile/pidfile_unix.go#L30-L42)).
On Windows the lock checks again with a second process library, because "Some
users faced issues on Windows where the process written in the pidfile was
identified as still existing"
([pidfile_windows.go](https://github.com/docker/compose/blob/v5.5.0/internal/locker/pidfile_windows.go#L28-L48)).
`watch.go` is the only caller. No other command reads the file, and `down`
does not consult it. Inference: it is keyed by project name, like everything
else in Compose, and a reused PID makes a dead `watch` look alive.

**Ryuk's liveness signal is the connection** (see
[section 3](#3-selecting-what-is-safe-to-remove)). A killed client's socket
closes with its process, so it needs no handler in the client. The price is a
second container with the Docker socket mounted.

**Skaffold cleans up on a signal it catches.** "When running `skaffold dev` or
`skaffold debug`, pressing `Ctrl+C` (`SIGINT` signal) will kick off the
cleanup process which will mimic the behavior of `skaffold delete`. If for
some reason the Skaffold process was unable to catch the `SIGINT` signal,
`skaffold delete` can always be run later to clean up the deployed Kubernetes
resources." Cleanup "can be turned off by `--cleanup=false`"
([cleanup.md](https://github.com/GoogleContainerTools/skaffold/blob/v2.25.0/docs-v2/content/en/docs/cleanup.md?plain=1#L24-L32)).

**Tilt leaves resources running on exit, by design.** `tilt up`'s own help
says "When you exit Tilt (using Ctrl+C), Kubernetes resources and Docker
Compose resources continue running; you can use tilt down … to delete these
resources. Any long-running local resources--i.e. those using serve_cmd--are
terminated when you exit Tilt"
([up.go](https://github.com/tilt-dev/tilt/blob/v0.37.8/internal/cli/up.go#L79-L81)).
`tilt down` "Deletes resources specified in the Tiltfile", which makes the
Tiltfile, not a record of what `up` created, its source of what to remove
([down.go](https://github.com/tilt-dev/tilt/blob/v0.37.8/internal/cli/down.go)).

**Batect** has no liveness scheme, as it has no identity scheme: see
[section 3](#3-selecting-what-is-safe-to-remove).

## The probe

Run on 2026-11-12 on macOS (Darwin, arm64) against OrbStack's Docker daemon:
Docker client and server `29.8.2`, Docker Compose `v5.5.0`.

Two directories with the same base name were created under one temporary
directory, at `…/a/<name>` and `…/b/<name>`, each holding the same
`compose.yaml`: one service, image `alpine`, command `sleep 600`. No `-p`,
`COMPOSE_PROJECT_NAME` or `name:` was set.

1. `docker compose up` (foreground, no TTY) was started in `a`. Once the
   container was running, the `docker` process and its `docker-compose`
   plugin child were sent `SIGKILL`.
2. 15 seconds later both processes were gone and `docker ps` showed the
   container `running`. Its labels included
   `com.docker.compose.project=<name>` and
   `com.docker.compose.project.working_dir=…/a/<name>`.
3. From `b`: `docker compose ps` listed that container. `docker compose up -d`
   printed `Container <name>-sleeper-1 Running` and the container id was
   unchanged. `docker compose ls` showed one project, with `a`'s config file.
   `docker compose down` stopped and removed the container and removed the
   network `<name>_default`.

Not varied: the daemon (only OrbStack, not Docker Desktop or a Linux
engine), the Compose version, a TTY (none was attached, so the keyboard menu
was not active), `SIGKILL` of only one of the two processes, a service with a
`restart` policy, more than one service, differing Compose files in the two
directories (which would exercise the config-hash recreation in
[section 2](#2-a-second-invocation-against-a-running-group)), and `up`
running in the foreground in both directories at once.

## Synthesis

Identity schemes:

| Scheme | Used by | Documented or observed failure mode |
| --- | --- | --- |
| Name only | Docker Compose (project name); Dagger and Earthly (container name) | Two checkouts whose names resolve the same are one project: observed, `down` in one removed the other's container. Docker's documentation leaves uniqueness to the user ("setting the project name to a unique build number") |
| Name plus absolute path | Dev Containers CLI (absolute folder plus absolute config file, as labels). Compose records both paths but does not select on them | Path spelling: the CLI carries a Windows-only second pass because the labels "should compare case-insensitively on Windows". A lookup that matches several containers takes the first. Inference: a moved or renamed checkout no longer matches its container |
| Session id | Testcontainers (random UUID per JVM; hash of parent PID and creation time in Go) | Not derivable by a later invocation (inference), so it serves cleanup and not `down`. Inference: Go's fallback to a random UUID gives each package's process its own session |

Liveness schemes:

| Scheme | Used by | Documented or observed failure mode |
| --- | --- | --- |
| None, or container state alone | Compose `up`/`down`; Dev Containers CLI; Tilt; `docker container prune` (not running); Batect | Nothing distinguishes a group whose supervisor died from one that is wanted: observed, Compose's container ran on after `SIGKILL` of `up`. Compose's `down` removes running containers without asking. `container prune` treats a created, never-started container as stopped. Skaffold and Testcontainers both document that a kill they can't catch leaves resources behind |
| Held connection | Ryuk (TCP, 10 s grace after the last client, 60 s with no client) | Needs a second container with the Docker socket, documented as needing privileged mode in some environments, and so disabled where that isn't allowed. Removes running containers by force. By design nothing outlives the client |
| PID or lock file | Compose `watch` only (`<project>.pid`, checked with `kill(pid, 0)` or `/proc/<pid>`) | Documented in source: on Windows a dead process "was identified as still existing". Inference: PID reuse, and a file keyed by name alone. It guards against a second `watch`, and no removal consults it |

## Not verified

- **go-task and just.** Not read. Neither was checked for any container or
  long-lived process management.
- **Dagger sessions.** Only the engine container's naming and removal were
  read. How a Dagger session identifies and removes the service containers it
  starts inside the engine, and whether `ContainerLs` in the Docker driver
  includes stopped containers, were not.
- **Earthly.** Only the BuildKit daemon container was read. How Earthly
  handles containers started by `WITH DOCKER`, and `RemoveExited`
  ([buildkitd.go](https://github.com/earthly/earthly/blob/v0.8.16/buildkitd/buildkitd.go#L337)),
  were not.
- **Skaffold's selection.** The documentation was read, not the source. How
  `skaffold delete` selects resources (rendered manifests, or a run-id label),
  and which signals `skaffold dev` handles besides `SIGINT`, were not checked.
- **Tilt.** The help text was read, not the code path. Whether `tilt down`
  passes a project name to Compose, and what happens to resources when
  `tilt up` is killed with `SIGKILL`, were not checked. The help text covers
  only exit by Ctrl+C.
- **Compose on other daemons and with a restart policy.** The probe ran once,
  on one daemon. See [The probe](#the-probe) for what it did not vary.
- **Compose's `depends_on` and recreation.** Whether a dependent service is
  restarted, as distinct from recreated, when its dependency is recreated
  (`restart: true` under `depends_on`) was not traced.
- **Compose `down` with `--remove-orphans` and no Compose file.** The
  statement that only exited or dead one-offs can be orphans there is an
  inference from `isOrphaned` and `projectFromName`, not a test.
- **Ryuk with no filters.** What the prune check does when the connection
  timeout fires with no filter received was not traced past `pruneWait`. The
  README says only that it triggers a shutdown.
- **testcontainers-go's reaper client.** The session id's derivation was
  read. The Go client's connection handling and its conditions for skipping
  Ryuk were not.
- **`container prune` and a container in the removing state.** `ContainerPrune`
  calls `containerRm` on it. What `containerRm` then does was not read.
- **A network whose containers are all stopped.** `localNetworkPrune` skips a
  network with any endpoint, and Compose's `down` skips one whose inspect
  result lists containers. Whether a stopped (exited or created) container
  holds an endpoint on its network, and so protects the network from either,
  was neither read in libnetwork nor probed.
- **`docker system prune`'s order of operations**, and whether its `until`
  filter is applied to networks by creation time as the CLI reference says.
  The CLI's implementation was not read.
- **Label filter semantics in the daemon.** That two `label` filters select
  either label is the CLI reference's statement. `matchLabels` in moby was not
  read.
- **Podman.** `podman compose`, `podman pod` and `podman kube play`/`down`
  were not researched.
