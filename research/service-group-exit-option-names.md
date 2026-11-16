# Naming a service group's exit options: what comparable tools call them

Research for [ratect#103](https://github.com/or1can/ratect/issues/103)
(service groups, `services up`/`down`) and
[decisions/0026](../decisions/0026-service-instances.md). It answers one
question: what do tools that bring up a set of containers together call
(a) the setting for when the whole set ends on its own, and (b) the setting
for what the client exits with? At the commit below Ratect spelled them
`exit_trigger` and `exit_code`. It records names, values, defaults and
naming patterns. It does not choose names for ratect; decisions/0026
records the choice.

Every claim cites the source that owns it, pinned to a tag or commit:
Docker Compose `v5.5.0`; the Compose Specification at commit
[`3a4a699`](https://github.com/compose-spec/compose-spec/tree/3a4a6995e4a971f6173dff9130cb3e38e6738651);
HashiCorp Nomad `v2.1.1`; Kubernetes `v1.37.2`; the Dev Containers
specification at commit
[`c95ffee`](https://github.com/devcontainers/spec/tree/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421);
Tilt `v0.37.9`; GitHub's docs at commit
[`28d2706`](https://github.com/github/docs/tree/28d27064d49fe856efee59fbaa017c0c0d4aa2ff);
testcontainers-go `v0.45.0`; Earthly at commit `4ac1b124` (`v0.8.6-3`, a
local checkout); Batect at commit `67c94224` (`0.85.0-3`) and batect.dev at
commit `2d2e6f6f`, both local checkouts; and Ratect itself at commit
`80972f0c` on `feat/103-services`. Where this document reasons from a source
without quoting it, it says so ("inference"). Anything that could not be
checked is in [Not verified](#not-verified).

## Summary

1. **Only Docker Compose has both settings, and both are CLI flags, not
   file fields.** The trigger is a pair of booleans,
   `--abort-on-container-exit` and `--abort-on-container-failure`; the exit
   code is `--exit-code-from <service>`. Internally Compose models the trigger
   as one enum field, `OnExit` of type `Cascade`
   (`CascadeIgnore`/`CascadeStop`/`CascadeFail`), and the code as
   `ExitCodeFrom`. The Compose file has no equivalent of either.
2. **Compose couples the two; Ratect doesn't.** `--exit-code-from` implies
   `--abort-on-container-exit`, and `--abort-on-container-failure` folds a
   failure condition into the trigger. A plain `compose up` waits for every
   container and then exits 0 whatever they exited with.
3. **The orchestrators name a role, not a policy.** Nomad marks one task
   `leader = true` (its exit stops the rest); Kubernetes marks helpers as
   sidecars (`restartPolicy: Always` on an init container) and ends a Pod when
   every *regular* container has terminated. Neither has a client that exits
   with a container's code, so neither has a name for (b).
4. **Kubernetes' Job API is the closest to a structured (b)**: rules with
   `action` plus `onExitCodes { containerName, operator, values }`. It selects
   *by* exit code rather than selecting *which* code to report.
5. **Exit-condition nouns exist**: Tilt's `exitCondition` (`manual`/`ci`) is a
   single noun naming when the session ends. Dev Containers'
   `shutdownAction` names what happens at disconnect. GitHub Actions has
   `fail-fast` (boolean) and `wait`/`wait-all` steps.
6. **Naming patterns seen**: verb phrases as flags (`abort-on-…`), `*-from`
   for "take the code from" (`--exit-code-from`), `on*` for a condition
   (`OnExit`, `onExitCodes`, `onPodConditions`), `*Condition`/`*Policy`/
   `*Action` nouns for fields (`exitCondition`, `successPolicy`,
   `podFailurePolicy`, `shutdownAction`). No tool surveyed uses `until` or
   `when` as a field name. No surveyed tool calls a field `exit_code` whose
   value is not itself an exit code.
7. **No tool combines (a) and (b) into one setting**, except by role: Nomad's
   `leader` and Compose's `--exit-code-from` both make one container's exit
   decide the end (Compose's also supplies the code).

## 1. Docker Compose

**The flags.** `docker compose up` declares, in
[up.go](https://github.com/docker/compose/blob/v5.5.0/cmd/compose/up.go#L160-L162):

| Flag | Values | Default | Help text (verbatim) |
| --- | --- | --- | --- |
| `--abort-on-container-exit` | boolean | `false` | "Stops all containers if any container was stopped. Incompatible with -d" |
| `--abort-on-container-failure` | boolean | `false` | "Stops all containers if any container exited with failure. Incompatible with -d" |
| `--exit-code-from` | a service name | `""` | "Return the exit code of the selected service container. Implies --abort-on-container-exit" |

**Their internal names.** The two booleans become one field:
[`OnExit()`](https://github.com/docker/compose/blob/v5.5.0/cmd/compose/up.go#L102-L111)
maps them to `api.CascadeStop`, `api.CascadeFail` or `api.CascadeIgnore`.
`StartOptions` carries `OnExit Cascade` ("defines behavior when a container
stops") and `ExitCodeFrom string` ("return exit code from specified
service"), and `Cascade` has exactly the three values
([api.go](https://github.com/docker/compose/blob/v5.5.0/pkg/api/api.go#L293-L311)).

**How they combine.** `--exit-code-from` sets the abort flag unless
`--abort-on-container-failure` is given, and the two abort flags are
mutually exclusive
([up.go](https://github.com/docker/compose/blob/v5.5.0/cmd/compose/up.go#L194-L199)).
Under either abort flag, the first container to exit (for `CascadeFail`, the
first to exit non-zero) records its code and triggers a stop of the rest;
`ExitCodeFrom` records the first exit of the named service
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L204-L236)).
Those two assignments are the only ones; `up` returns a `StatusError` only
when the recorded code is non-zero
([up.go](https://github.com/docker/compose/blob/v5.5.0/pkg/compose/up.go#L295-L300)).
So a plain `up` exits 0 whatever its containers exit with (this matches the
observation in [concurrent-clients-on-one-container-group.md](concurrent-clients-on-one-container-group.md#summary),
item 4). Inference: when both listeners fire, the named service's exit is
the same event as or later than the first exit, so `--exit-code-from`'s
value wins.

Mapped to Ratect's values: `--abort-on-container-exit` is `exit_trigger =
"any"` with `exit_code = "first_to_exit"`; no flags is `exit_trigger =
"all"` with a code that is always 0 (Ratect has no such value; its default
`any_non_zero` differs); `--exit-code-from` is `{ container = … }` with an
`"any"` trigger forced. `--abort-on-container-failure` (trigger on the first
non-zero exit) has no Ratect equivalent.

**The Compose file.** The specification has no abort or exit-code setting:
`abort` and `exit-code`/`exit_code` occur nowhere in
[spec.md](https://github.com/compose-spec/compose-spec/blob/3a4a6995e4a971f6173dff9130cb3e38e6738651/spec.md).
The nearest service fields are `restart` (`"no"`, the default, `always`,
`on-failure[:max-retries]`, `unless-stopped`;
[05-services.md](https://github.com/compose-spec/compose-spec/blob/3a4a6995e4a971f6173dff9130cb3e38e6738651/05-services.md#restart)),
`depends_on`'s `condition` (`service_started`, `service_healthy`,
`service_completed_successfully`;
[spec.md](https://github.com/compose-spec/compose-spec/blob/3a4a6995e4a971f6173dff9130cb3e38e6738651/spec.md#depends_on)),
and `attach` (boolean, default `true`: whether `up` collects the service's
logs;
[05-services.md](https://github.com/compose-spec/compose-spec/blob/3a4a6995e4a971f6173dff9130cb3e38e6738651/05-services.md#attach)).
Ratect's `stop_signal` and `stop_grace_period` are Compose's own service
field names (see [section 8](#8-ratects-own-vocabulary)).

## 2. Docker Swarm / `docker stack`

Swarm services are long-running and `docker stack deploy` has no client that
waits on them, so there is no group exit or exit-code option. The per-service
analogue is `deploy.restart_policy.condition`: `none`, `on-failure`, or
`any` (the default)
([deploy.md](https://github.com/compose-spec/compose-spec/blob/3a4a6995e4a971f6173dff9130cb3e38e6738651/deploy.md#restart_policy)).

## 3. HashiCorp Nomad

| Option | Values | Default | Meaning |
| --- | --- | --- | --- |
| `task.leader` | boolean | `false` | "When the leader task exits, other tasks will be gracefully terminated." |
| `task.lifecycle.hook` | `prestart`, `poststart`, `poststop` | none (a main task) | When in the group's life the task runs |
| `task.lifecycle.sidecar` | boolean | `false` | Whether a hook task keeps running alongside the main tasks |

Sources: the HCL names are `leader`, `hook` and `sidecar`
([tasks.go](https://github.com/hashicorp/nomad/blob/v2.1.1/api/tasks.go#L753-L754),
[L785](https://github.com/hashicorp/nomad/blob/v2.1.1/api/tasks.go#L785));
the leader comment is
[structs.go](https://github.com/hashicorp/nomad/blob/v2.1.1/nomad/structs/structs.go#L7974-L7976),
the hook values
[structs.go](https://github.com/hashicorp/nomad/blob/v2.1.1/nomad/structs/structs.go#L6152-L6154),
and at most one task may be leader
([structs.go](https://github.com/hashicorp/nomad/blob/v2.1.1/nomad/structs/structs.go#L7264-L7265)).

**What ends a group.** The allocation runner kills the remaining live tasks
when a task has *failed* (`TaskSiblingFailed`), when the leader has exited
(`TaskLeaderDead`), or when only sidecars remain (`TaskMainDead`)
([alloc_runner.go](https://github.com/hashicorp/nomad/blob/v2.1.1/client/allocrunner/alloc_runner.go#L590-L640)).
So Nomad's default is closer to "any failure, or all main tasks done" than to
either of Ratect's values, and the only knob is a *role* (`leader`), not a
policy. Nomad's CLI does not exit with a task's code, so there is no
counterpart of `exit_code` (inference from the absence of any such field in
[tasks.go](https://github.com/hashicorp/nomad/blob/v2.1.1/api/tasks.go); not exhaustively checked, see [Not verified](#not-verified)).

## 4. Kubernetes

**Pod.** `spec.restartPolicy`: `Always` (the default), `OnFailure`, `Never`
([types.go](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/core/v1/types.go#L3885-L3895),
[L4404-L4409](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/core/v1/types.go#L4404-L4409)).
A container-level `restartPolicy` of `Always` on an init container makes it a
sidecar: restarted "until all regular containers have terminated", then shut
down
([L3204-L3220](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/core/v1/types.go#L3204-L3220)).
The container-level `restartPolicyRules` (feature gate `ContainerRestartRules`)
are `{ action, exitCodes: { operator: In|NotIn, values } }` with actions
`Restart` and `RestartAllContainers`
([L3221-L3235](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/core/v1/types.go#L3221-L3235),
[L3907-L3957](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/core/v1/types.go#L3907-L3957)).

**A Pod's outcome follows its containers** as a phase, not a code: `Succeeded`
when all containers "have voluntarily terminated with a container exit code
of 0", `Failed` when all have terminated and at least one failed
([L3721-L3730](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/core/v1/types.go#L3721-L3730)).
That is Ratect's `exit_trigger = "all"` with an `any_non_zero`-shaped verdict,
reduced to a boolean. No Pod field picks one container's code.

**Job.** Two policies
([types.go](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/batch/v1/types.go#L341),
[L350](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/batch/v1/types.go#L350)):

| Option | Values | Default | Meaning |
| --- | --- | --- | --- |
| `podFailurePolicy.rules[].action` | `FailJob`, `FailIndex`, `Ignore`, `Count` | required | What a matching Pod failure does to the Job ([L120-L143](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/batch/v1/types.go#L120-L143), [L220-L245](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/batch/v1/types.go#L220-L245)) |
| `podFailurePolicy.rules[].onExitCodes` | `{ containerName, operator: In\|NotIn, values }` | none | Matches by exit code, optionally of one named container; 0 is excluded ([L173-L201](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/batch/v1/types.go#L173-L201)) |
| `successPolicy.rules[]` | `{ succeededIndexes, succeededCount }` | none (all completions) | When an indexed Job counts as succeeded early ([L260-L301](https://github.com/kubernetes/kubernetes/blob/v1.37.2/staging/src/k8s.io/api/batch/v1/types.go#L260-L301)) |

Naming precedent: `on<Thing>` for a match condition (`onExitCodes`,
`onPodConditions`), `containerName` for "which container", and
`*Policy`/`action` nouns for the setting itself.

## 5. Dev Containers specification

`shutdownAction`, in two variants
([devContainer.base.schema.json](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/schemas/devContainer.base.schema.json#L522-L528),
[L686-L692](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/schemas/devContainer.base.schema.json#L686-L692)):

| Config kind | Values | Default | Meaning |
| --- | --- | --- | --- |
| image/Dockerfile | `none`, `stopContainer` | `stopContainer` | "Action to take when the user disconnects from the container in their editor." |
| Compose | `none`, `stopCompose` | `stopCompose` | "Action to take when the user disconnects from the primary container in their editor. The default is to stop all of the compose containers." |

The Compose variant's `runServices` ("an array of services that should be
started and stopped",
[L675-L677](https://github.com/devcontainers/spec/blob/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421/schemas/devContainer.base.schema.json#L675-L677))
is the closest thing to a member list. The trigger is the client
disconnecting, not a container exiting, and there is no exit-code setting.

## 6. Tilt

`tilt up` and `tilt ci` differ only by the session's `exitCondition`:
`manual` ("will not exit based on resource status", used by `tilt up`) or
`ci` ("terminates upon the first encountered build or runtime failure or
after all resources have been started successfully", used by `tilt ci`)
([session_types.go](https://github.com/tilt-dev/tilt/blob/v0.37.9/pkg/apis/core/v1alpha1/session_types.go#L61-L100)).
The `ci` settings beside it are `k8sGracePeriod`, `timeout` (default 30m) and
`readinessTimeout` (default 5m). `tilt ci` "Exits with failure if any build
fails or any server crashes" and with success once all tasks completed and
all servers are healthy
([ci.go](https://github.com/tilt-dev/tilt/blob/v0.37.9/internal/cli/ci.go#L32-L38)).
It reports pass/fail, not a resource's code. Naming precedent: one noun,
`exitCondition`, for "when this ends on its own".

## 7. GitHub Actions, Testcontainers, Earthly, Batect

**GitHub Actions `services`.** The keys under
`jobs.<job_id>.services.<service_id>` are `image`, `credentials`, `env`,
`ports`, `volumes`, `options`, `command` and `entrypoint`; "the runner …
manages the life cycle of the service containers"
([workflow-syntax.md](https://github.com/github/docs/blob/28d27064d49fe856efee59fbaa017c0c0d4aa2ff/content/actions/reference/workflows-and-actions/workflow-syntax.md#L1419-L1542)).
No exit option: services live for the job. Two adjacent names: background
steps are joined with `wait` (named steps) or `wait-all`, which fail if a
step they wait on failed
([L1090-L1180](https://github.com/github/docs/blob/28d27064d49fe856efee59fbaa017c0c0d4aa2ff/content/actions/reference/workflows-and-actions/workflow-syntax.md#L1090-L1180));
and a matrix's `strategy.fail-fast` (boolean, default `true`) cancels the
rest "if any job in the matrix fails"
([section-using-a-build-matrix-for-your-jobs-failfast.md](https://github.com/github/docs/blob/28d27064d49fe856efee59fbaa017c0c0d4aa2ff/data/reusables/actions/jobs/section-using-a-build-matrix-for-your-jobs-failfast.md)).

**Testcontainers (Go).** The Compose module's `Up` options are
`RunServices`, `IgnoreOrphans`, `Recreate`, `RecreateDependencies`,
`RemoveOrphans` and a `Wait` flag; it embeds Compose's own `api.StartOptions`
but sets neither `OnExit` nor `ExitCodeFrom` (`OnExit`, `ExitCodeFrom` and
`Cascade` occur nowhere in
[compose_api.go](https://github.com/testcontainers/testcontainers-go/blob/v0.45.0/modules/compose/compose_api.go)).
The test process decides the outcome.

**Earthly.** `WITH DOCKER` takes `--pull`, `--load`, `--compose`,
`--service` and others; the result is the `RUN` inside it, and the Compose
services are support (the Earthfile reference in the local checkout,
"`WITH DOCKER`" options, lines 1135-1170). No exit options.

**Batect.** Both settings are only roadmap items:
"exit options (close all after any container stops, wait for all to stop)"
and "return code options (any non-zero, particular container, first to
exit)" (`batect` `ROADMAP.md`, lines 89-90). Batect's field names are
lower snake-case nouns or noun phrases (`build_directory`,
`image_pull_policy`, `run_as_current_user`, `setup_commands`;
batect.dev's container reference page), with PascalCase enum
values where it has an enum (`image_pull_policy`: `IfNotPresent`, `Always`;
lines 549-560).

## 8. Ratect's own vocabulary

**The two fields today.** `ServiceGroup` has `containers`, `exit_trigger`
(`ExitTrigger`: `any`, `all`; default `all`) and `exit_code` (`ExitCodeRule`:
`any_non_zero`, the default, `first_to_exit`, `{ container = "<member>" }`)
(`ratect-core/src/config.rs`, lines 2813-2862). Note the type is already
named `ExitCodeRule`, not `ExitCode`. CONTEXT.md calls them "an exit trigger
and an exit-code rule" (`CONTEXT.md`, lines 148-151), and
`docs/ratect-cli.md` says `services up` "exits with the code the group's
`exit_code` rule selects" (line 175): the prose already needs the word
*rule* that the field name drops.

**Field-naming patterns elsewhere in the native format** (all from
`ratect-core/src/config.rs`):

- `stop_signal` and `stop_grace_period` (lines 482, 496) are Compose's
  service field names, taken verbatim; `stop_*` already names "how a
  container is stopped".
- `run_to_completion` (line 343) is a boolean verb phrase, the one field
  that describes a container *ending*.
- Readiness uses Docker's health-check nouns: `interval`, `retries`,
  `start_period`, `timeout` (lines 1897-1920), and
  `external_health_check.kind`.
- A container reference is spelled `container`: `run.container` on a task
  (line 3012), and `{ container = "<member>" }` in `ExitCodeRule` reuses it.
  `run_in` (line 2303) names a target container by preposition.
- Enum values are lower snake case where Ratect chose them (`CacheScope`:
  `project`, `shared`, line 1137; `ExitTrigger`, `ExitCodeRule`) and Batect's
  PascalCase where inherited (`ImagePullPolicy`, line 1655).
- A setting with parts of its own is a table (`identity = { home = …,
  additional_users = … }`; `health_check = { command = … }`). The native
  format refuses Batect's `run_as_current_user`, which `identity` replaces
  (`docs/ratect-config-reference.md`).
- `*_policy` exists once, inherited from Batect (`image_pull_policy`, line
  212). No native field uses `on_*`, `*_from`, `when` or `until`.

## Candidate name pairs

Each pair names (a) the trigger and (b) the code selection. Values are kept
as today unless the name forces a change. None is recommended here.

1. **`exit_trigger` + `exit_code_from`.**
   `exit_code_from = { container = "api" }`, `exit_code_from = "first_to_exit"`.
   Fit: keeps the CONTEXT.md term for (a); the `*_from` suffix tells the
   reader the value is a source, not a number, so `exit_code_from = 3` reads
   wrong on sight. Precedent: Compose `--exit-code-from` / `ExitCodeFrom`.
   Caveat: Compose's flag takes only a service, so `"any_non_zero"` is a
   Ratect extension of the meaning.

2. **`exit_trigger` + `exit_code_rule`.**
   `exit_code_rule = "any_non_zero"`. Fit: matches the existing type name
   `ExitCodeRule` and CONTEXT.md's "exit-code rule" exactly, so code, docs
   and glossary use one word. Precedent: weaker; nearest is Kubernetes'
   `podFailurePolicy.rules` (`*Policy`/`rules` nouns for "how exits are
   judged").

3. **`stop_when` + `exit_code_from`.**
   `stop_when = "any"` reads "stop when any [member stops]". Fit: joins the
   `stop_signal`/`stop_grace_period` family, which already describes stopping;
   but `when` would be the format's first conjunction-style name, and "stop
   when all" describes the group ending by itself rather than Ratect stopping
   anything. Precedent: Compose's own help text ("Stops all containers if any
   container was stopped"); Batect's roadmap wording ("close all after any
   container stops").

4. **`exit_condition` + `exit_code_from`.**
   `exit_condition = "any"`. Fit: a plain noun like `health_check` or
   `image_pull_policy`; "condition" says it's a test, not an action.
   Precedent: Tilt's `exitCondition` (`manual`/`ci`), a single noun for
   when a session ends on its own.

5. **One table, `[services.<group>.exit]` with `trigger` and `code_from`.**
   `exit = { trigger = "any", code_from = { container = "api" } }`. Fit:
   grouping as `identity` and `health_check` do, and leaves room
   for a later `--abort-on-container-failure`-style trigger or a timeout
   beside them; costs one level of nesting for two fields. Precedent: Tilt
   puts `exitCondition` and its `ci` settings side by side in one spec;
   Compose models both under `StartOptions` (`OnExit`, `ExitCodeFrom`).

## Not verified

- **Compose's listener ordering.** That `--exit-code-from`'s value overrides
  the abort listener's when both fire is inferred from the code, not probed.
- **Nomad's CLI exit codes.** That no Nomad command exits with a task's code
  was inferred from [tasks.go](https://github.com/hashicorp/nomad/blob/v2.1.1/api/tasks.go) and the alloc runner; the CLI's `job run`
  and `alloc` commands were not read.
- **Kubernetes `restartPolicyRules` defaults and maturity.** The fields were
  read from `v1.37.2` types, behind the `ContainerRestartRules` feature gate;
  their graduation stage was not checked.
- **Testcontainers for Java and other languages.** Only the Go module was
  read.
- **Earthly upstream.** The citation is to a local checkout's docs, not
  re-checked against `earthly/earthly` at that commit.
- **Docker's published reference pages.** Compose flags were read from
  source help strings, not the rendered docs.docker.com pages, which are
  generated from them (inference).
