// Copyright 2026 Orican Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Foreground `ratect services up` — [`TaskEngine::bring_up_service`]: a
//! service group's **service instance** brought up, or attached to or
//! replaced where it already is, its members' output streamed, and the instance taken down
//! once the group's exit condition is met or a termination signal arrives
//! (decisions/0026). The process is the instance's client, not its owner:
//! nothing here takes a lock or records anything, and the daemon's labels
//! are the instance ([`crate::service_instances`]).
//!
//! **Bringing up reuses a task run's two stages.** Each container of the
//! instance — the members and whatever they depend on — goes through
//! `ensure_container_created` and `ensure_container_ready` exactly as a
//! task's dependency does, with the instance's labels, its fixed names and
//! the `service` role in place of a task run's. Only a member's exit counts:
//! it gets no dependency-exit watcher, since its exit is what this module
//! waits for.
//!
//! **Who brings it up is decided by the instance's network.** A container's
//! fixed name guards a create on every daemon, but a network's doesn't:
//! Docker 20.10 to 24 create a second network of a name already taken
//! (`docs/task-lifecycle.md`). So the network is created first, and an `up`
//! has won only if, listed straight afterwards, its network is the only one
//! with the instance's labels. One that finds a second removes its own and
//! starts over, and one whose create the daemon refuses as a name in use
//! goes by the instance it then finds — or starts over, when the holder has
//! gone by the time it looks.
//! Two creators can't both see only their own — the later lists after both
//! exist — so exactly one brings the instance up, and the instance is never
//! won piecemeal: nobody else creates any of its containers. A tie, where
//! each sees both, is broken by both starting over after a random pause. The
//! winner's containers join the network by its id, since a loser's twin can
//! make the name ambiguous for a moment.
//!
//! **What an `up` does with an instance it finds depends on its state**,
//! decided in this order. While any container is only created or being
//! removed, another `up` is bringing it up or taking it down: this one
//! waits, with a notice, and looks again. With no network left, it replaces
//! the instance: takes down what it found, then starts over. With a network
//! and no container, it waits. With a member up — running, or paused or
//! restarting, which an exit still lies ahead of — it attaches: it follows
//! the output of each container running at that moment, watches each member
//! that is up, and starts nothing; an exited member is neither started again
//! nor counted towards the exit condition. With no member up, it replaces the
//! instance. So an `up` never repairs an
//! instance from a snapshot another `up` may be changing: a stopped
//! container's network can be removed under it, after which the daemon
//! can't start it again (`research/concurrent-clients-on-one-container-group.md`).
//!
//! **An `up` that won the network or attached takes the instance down** at
//! its end, and only what it won or attached to: the network and containers
//! by their ids, trying for up to `service_instances::TAKE_DOWN_ROUNDS`
//! rounds until none of them is left,
//! tolerating another `up` or `services down` taking them down at the same
//! time
//! ([`service_instances::take_down_until_gone`]). A newer instance another
//! `up` brings up under the same labels meanwhile is left alone. Replacing
//! an instance, an `up` takes down what it found before starting over. One
//! that ends before
//! either leaves alone an instance another `up` may be bringing up — and
//! removes the network its own create made, when a signal arrives with that
//! create still under way, since it can't have won it.

use super::*;
use crate::config::{ExitCodeRule, ExitCondition, ServiceGroup};
use crate::docker::LabelledResource;
use crate::service_instances::{self, Instance, InstanceKey, StopPlan, TakeDownStep};
use futures::stream::{FuturesUnordered, StreamExt};

/// How many times `up` starts over — after a tie for the network, a holder
/// that went away, or an instance it replaced — before giving up. Waiting
/// for an instance another `up` is changing isn't counted.
const ATTEMPTS: usize = 5;

/// How often a waiting `up` looks at an instance again.
const SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// How long after the take-down a nominated member's exit code is still
/// waited for — see `bring_up_service`'s cleanup.
const NOMINATED_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// The network create of an `up`'s current attempt, shared so that the
/// cleanup can still learn its id when a signal ends the attempt mid-create.
type NetworkCreate = futures::future::Shared<
    futures::future::BoxFuture<'static, std::result::Result<String, Arc<anyhow::Error>>>,
>;

/// A service group's name that the project doesn't declare.
#[derive(Debug)]
pub struct UnknownServiceGroup {
    pub name: String,
    pub known: Vec<String>,
}

impl std::fmt::Display for UnknownServiceGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The service group '{}' does not exist.",
            printable(&self.name)
        )?;
        if self.known.is_empty() {
            write!(f, " This project declares no service groups.")
        } else {
            let quoted: Vec<String> = self.known.iter().map(|n| format!("'{n}'")).collect();
            write!(
                f,
                " Its service groups are {}.",
                human_readable_list(&quoted, "and")
            )
        }
    }
}

impl std::error::Error for UnknownServiceGroup {}

/// Whether `condition` is met once `exited` of `members` members have
/// stopped.
fn condition_met(condition: ExitCondition, exited: usize, members: usize) -> bool {
    match condition {
        ExitCondition::AnyStopped => exited >= 1,
        ExitCondition::AllStopped => exited >= members,
    }
}

/// The exit code `rule` selects, from `own` — each member that stopped of
/// its own accord, in the order it did, until the exit condition was met — and `stopped`,
/// what each member that `up` stopped itself afterwards exited with. `None`
/// only for a nominated member whose exit code was never read.
fn select_exit_code(
    rule: &ExitCodeRule,
    own: &[(String, i64)],
    stopped: &HashMap<String, i64>,
) -> Option<i64> {
    match rule {
        ExitCodeRule::FirstNonZero => Some(
            own.iter()
                .map(|(_, code)| *code)
                .find(|code| *code != 0)
                .unwrap_or(0),
        ),
        ExitCodeRule::FirstToExit => own.first().map(|(_, code)| *code),
        ExitCodeRule::Container(nominated) => own
            .iter()
            .find(|(member, _)| member == nominated)
            .map(|(_, code)| *code)
            .or_else(|| stopped.get(nominated).copied()),
    }
}

/// The groups of container names [`service_instances::take_down`] stops in
/// turn: everything nothing else depends on, then what only those depended
/// on, and so on — the reverse of the order they start in.
fn stop_layers(graph: &HashMap<String, Vec<String>>) -> Vec<Vec<String>> {
    let mut remaining: HashSet<&str> = graph.keys().map(String::as_str).collect();
    let mut layers = Vec::new();
    while !remaining.is_empty() {
        let mut layer: Vec<String> = remaining
            .iter()
            .filter(|name| {
                !remaining.iter().any(|other| {
                    graph
                        .get(*other)
                        .is_some_and(|dependencies| dependencies.iter().any(|d| d == *name))
                })
            })
            .map(|name| name.to_string())
            .collect();
        if layer.is_empty() {
            // Unreachable for a graph `build_dependency_graph` proved
            // acyclic; stop the rest together rather than loop.
            layer = remaining.iter().map(|name| name.to_string()).collect();
        }
        layer.sort();
        for name in &layer {
            remaining.remove(name.as_str());
        }
        layers.push(layer);
    }
    layers
}

/// How a group's instance is stopped: each container before what it depends
/// on, each with its own `stop_signal` and `stop_grace_period`.
fn stop_plan(
    graph: &HashMap<String, Vec<String>>,
    containers: &HashMap<String, Container>,
) -> StopPlan {
    StopPlan {
        layers: stop_layers(graph),
        options: containers
            .iter()
            .map(|(name, container)| {
                (
                    name.clone(),
                    (container.stop_signal.clone(), container.stop_grace_period),
                )
            })
            .collect(),
    }
}

/// How `services down` stops `group`'s instance, when `config` still
/// declares the group: as `services up`'s take-down does. `None` for a group
/// it doesn't, or whose containers no longer form a graph — whose instance
/// is then stopped all at once.
pub fn service_stop_plan(config: &crate::config::Config, group: &str) -> Option<StopPlan> {
    let group = config.services.get(group)?;
    let containers: HashMap<String, Container> = config
        .containers
        .iter()
        .map(|(name, container)| (name.clone(), container.clone()))
        .collect();
    let graph = group_graph(&containers, group).ok()?;
    Some(stop_plan(&graph, &containers))
}

/// Every container `group`'s instance has — its members and what they
/// depend on — with what each depends on.
fn group_graph(
    containers: &HashMap<String, Container>,
    group: &ServiceGroup,
) -> Result<HashMap<String, Vec<String>>> {
    let mut graph = HashMap::new();
    for member in &group.containers {
        graph.extend(build_dependency_graph(containers, member, None)?);
    }
    Ok(graph)
}

/// A random pause of 100 to 600 ms, so two `up`s that tied for the network
/// don't tie again.
fn tie_break_pause() -> std::time::Duration {
    let byte = uuid::Uuid::new_v4().as_bytes()[0];
    std::time::Duration::from_millis(100 + u64::from(byte) * 2)
}

/// What one attempt at claiming the instance found.
enum Claim {
    /// This `up` won the instance's network, by this id, and brings it up.
    Won(String),
    /// The instance has a member up: attach to it.
    Attach(Instance),
    /// Another `up` is bringing the instance up or taking it down: look
    /// again shortly.
    Wait,
    /// Start over: a tie for the network, a holder that went away, or an
    /// instance this `up` has just replaced.
    StartOver,
}

/// What an `up` makes of an instance it found, by its containers' states —
/// the module's own documentation says why.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Attach,
    Wait,
    Replace,
}

fn state(resource: &LabelledResource) -> Option<&str> {
    resource.state.as_deref()
}

fn seen(instance: &Instance, members: &HashSet<String>) -> Seen {
    if instance
        .containers
        .iter()
        .any(|container| matches!(state(container), Some("created" | "removing")))
    {
        return Seen::Wait;
    }
    if instance.networks.is_empty() {
        return Seen::Replace;
    }
    if instance.containers.is_empty() {
        return Seen::Wait;
    }
    if instance.containers.iter().any(|container| {
        members.contains(Instance::container_name(container)) && is_up(state(container))
    }) {
        Seen::Attach
    } else {
        Seen::Replace
    }
}

/// Whether a container in `state` hasn't stopped: running, or paused or
/// restarting, which an exit still lies ahead of.
fn is_up(state: Option<&str>) -> bool {
    matches!(state, Some("running" | "paused" | "restarting"))
}

/// What a member's watch yields: its name, and its exit code or why none
/// could be read — including its bring-up failing, for a member this `up`
/// brings up.
type MemberWatch<'w> = futures::future::BoxFuture<'w, (String, Result<i64>)>;

impl<D: ContainerRuntime + Send + Sync + 'static> TaskEngine<D> {
    /// `ratect services up <group>` in the foreground: brings the group's
    /// instance up, attaches to it or replaces it, streams its output, and takes it down when
    /// the group's exit condition is met or on a termination signal. Comes
    /// back as a task run does — `Ok` for exit code 0,
    /// `ContainerExitedNonZero` for the code the group's exit-code rule
    /// selects, `TaskInterrupted` for a signal, and `FailureReported` for a
    /// failure already posted — so the binary's exit code is the run's
    /// (decisions/0016). `start_directory` is the directory of the
    /// configuration file `services up` was given, which the instance is
    /// labelled with.
    pub async fn bring_up_service(&self, group_name: &str, start_directory: &str) -> Result<()> {
        let group = self
            .project
            .config
            .services
            .get(group_name)
            .ok_or_else(|| UnknownServiceGroup {
                name: group_name.to_string(),
                known: self.project.config.services.keys().cloned().collect(),
            })?;
        self.warn_about_unreachable_proxies();
        let project = self.project.config.project_name.clone();

        let mut names: Vec<String> = Vec::new();
        for member in &group.containers {
            for name in container_names_in_task(
                |name| self.project.config.containers.get(name),
                member,
                None,
            ) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        let Evaluated { containers, .. } = self.project.evaluate(
            names.iter().map(String::as_str),
            std::iter::empty::<&str>(),
            &self.config_var_overrides,
            |name| (self.host_env)(name),
        )?;
        let graph = group_graph(&containers, group)?;
        let members: HashSet<String> = group.containers.iter().cloned().collect();

        let key = InstanceKey::default_instance(&project, group_name);
        let owner_labels = crate::labels::OwnershipLabels::for_service_instance(
            &project,
            group_name,
            &key.instance,
            start_directory,
            self.ratect_version.as_deref(),
        );
        let service_names =
            crate::resource_names::ServiceNames::new(&project, group_name, &key.instance);
        let network_name = service_names.network();

        self.event_sink.post(TaskEvent::TaskStarting {
            task: group_name.to_string(),
        });
        self.post_service_graph(&graph, &containers);
        let started_at = std::time::Instant::now();

        let created_containers: CreatedContainers = Mutex::new(HashMap::new());
        let pending_creates: PendingCreates<'_> = Mutex::new(Vec::new());
        let dependency_watchers: Mutex<Vec<tokio::task::JoinHandle<()>>> = Mutex::new(Vec::new());
        let started_containers: Mutex<HashSet<String>> = Mutex::default();
        let created_cells: Mutex<HashMap<String, CreatedCell>> = Mutex::new(HashMap::new());
        let ready_cells: Mutex<HashMap<String, ReadyCell>> = Mutex::new(HashMap::new());
        let image_cells: Mutex<HashMap<String, ReadyCell>> = Mutex::new(HashMap::new());
        let no_proxy_entries: std::collections::BTreeSet<String> = graph.keys().cloned().collect();
        // Whether this `up` has won the instance's network or attached to
        // it, and so takes it down however it ends. One that ends before
        // then leaves alone an instance another `up` may be bringing up.
        let reached = AtomicBool::new(false);
        // The ids of what this `up` won or attached to: its take-down
        // removes these and nothing else, so never a newer instance another
        // `up` brings up under the same labels while this one ends.
        let own_ids: Mutex<HashSet<String>> = Mutex::default();
        let won_network: Mutex<Option<String>> = Mutex::default();
        let network_create: Mutex<Option<NetworkCreate>> = Mutex::new(None);
        // The exit code of a nominated member that had already exited when
        // this `up` attached — the only exit it can't see happen.
        let earlier_exits: Mutex<HashMap<String, i64>> = Mutex::new(HashMap::new());

        // Each member's watch, kept here rather than inside `execution` so
        // that a member still running when the exit condition is met can still say
        // what it exits with once take-down stops it — which a nominated
        // member's exit code needs.
        let watches: Mutex<FuturesUnordered<MemberWatch<'_>>> = Mutex::new(FuturesUnordered::new());
        let execution = async {
            let mut attempt = 0;
            let mut waiting_noticed = false;
            let ids = loop {
                match self
                    .claim_instance(
                        &key,
                        &network_name,
                        &owner_labels,
                        &members,
                        &network_create,
                    )
                    .await?
                {
                    Claim::Won(network_id) => {
                        own_ids.lock().unwrap().insert(network_id.clone());
                        *won_network.lock().unwrap() = Some(network_id.clone());
                        reached.store(true, Ordering::Relaxed);
                        *network_create.lock().unwrap() = None;
                        // By id, not name: on a daemon that takes two
                        // networks of one name, a losing `up`'s twin can
                        // exist for a moment, and the name is then ambiguous.
                        let context = TaskRunContext {
                            graph: &graph,
                            containers: &containers,
                            network: &network_id,
                            created_cells: &created_cells,
                            ready_cells: &ready_cells,
                            image_cells: &image_cells,
                            created_containers: &created_containers,
                            pending_creates: &pending_creates,
                            no_proxy_entries: &no_proxy_entries,
                            customisations: None,
                            owner_labels: &owner_labels,
                            resource_names: ContainerNames::Service(&service_names),
                            dependency_watchers: &dependency_watchers,
                            started_containers: &started_containers,
                            root: None,
                            other_role: crate::labels::ContainerRole::Service,
                            exit_watched_elsewhere: &members,
                        };
                        break self.bring_up_members(&context, group).await?;
                    }
                    Claim::Attach(instance) => {
                        own_ids.lock().unwrap().extend(
                            instance
                                .containers
                                .iter()
                                .chain(&instance.networks)
                                .map(|resource| resource.id.clone()),
                        );
                        reached.store(true, Ordering::Relaxed);
                        let (ids, earlier) = self
                            .attach_instance(&graph, &key, &instance, group, start_directory)
                            .await?;
                        *earlier_exits.lock().unwrap() = earlier;
                        break ids;
                    }
                    Claim::Wait => {
                        if !waiting_noticed {
                            waiting_noticed = true;
                            self.post_notice(format!(
                                "Waiting for service group '{group_name}': another `ratect \
                                 services up` or `services down` is bringing it up or taking it \
                                 down. If none is, end this with Ctrl+C and run `ratect services \
                                 down {group_name}`."
                            ));
                        }
                        tokio::time::sleep(SETTLE_POLL).await;
                        continue;
                    }
                    Claim::StartOver => {}
                }
                attempt += 1;
                if attempt >= ATTEMPTS {
                    anyhow::bail!(
                        "Could not bring up service group '{group_name}': its instance kept \
                         changing under {ATTEMPTS} attempts to reach it."
                    );
                }
                tokio::time::sleep(tie_break_pause()).await;
            };
            for (member, id) in &ids {
                let member = member.clone();
                let id = id.clone();
                let docker = Arc::clone(&self.docker);
                watches
                    .lock()
                    .unwrap()
                    .push(futures::FutureExt::boxed(async move {
                        let exit = docker.wait_for_container_exit(&id).await;
                        (member, exit)
                    }));
            }
            self.watch_members(group, ids.len(), &watches).await
        };

        let (outcome, interrupts_before_cleanup) = tokio::select! {
            biased;
            outcome = execution => (outcome, self.interrupt.count()),
            () = self.interrupt.interrupted() => (
                Err(anyhow::Error::new(crate::interrupt::TaskInterrupted::new(
                    self.interrupt.last_signal(),
                ))),
                self.interrupt.count(),
            ),
        };
        self.event_sink.post(TaskEvent::RunEnded {
            task: group_name.to_string(),
        });
        for watcher in dependency_watchers.into_inner().unwrap() {
            watcher.abort();
        }
        let interrupted_by = outcome
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<crate::interrupt::TaskInterrupted>())
            .map(|interrupted| interrupted.signal);

        // A failure is reported before take-down says a word, as a task
        // run's is (ratect#334).
        let outcome = match outcome {
            Err(error)
                if interrupted_by.is_none() && !crate::exit_code::already_reported(&error) =>
            {
                self.event_sink.post(TaskEvent::Error {
                    message: format!("{error:?}").into(),
                });
                Err(FailureReported(error).into())
            }
            outcome => outcome,
        };
        let cause = match &outcome {
            _ if interrupted_by.is_some() => None,
            Ok(own) => match select_exit_code(&group.exit_code_from, own, &HashMap::new()) {
                Some(0) | None => None,
                Some(exit_code) => Some(CleanupCause::Exited {
                    task: group_name.to_string(),
                    exit_code,
                }),
            },
            Err(error) => Some(failed_step(error).unwrap_or_else(|| CleanupCause::Failed {
                task: group_name.to_string(),
            })),
        };

        let reached = reached.load(Ordering::Relaxed);
        if let Some(signal) = interrupted_by.filter(|_| !reached) {
            self.post_notice(format!(
                "{}; not bringing up service group '{group_name}'.",
                signal.ended_run()
            ));
        }
        if let Some(signal) = interrupted_by.filter(|_| reached) {
            // A task run's notice, for the same reason (decisions/0017):
            // the one line saying why the group is being taken down.
            self.post_notice(format!(
                "{}; taking service group '{group_name}' down. {} to force-remove what's \
                 left.",
                signal.ended_run(),
                signal.send_again(),
            ));
        }
        let escalate_at = interrupts_before_cleanup + 1;
        // A network this `up` was creating when the signal came: it can't
        // have won it, and nothing else would remove it.
        if let Some(create) = network_create.into_inner().unwrap().filter(|_| !reached) {
            if let Some(Ok(id)) = self
                .until_interrupted(self.interrupt.wait_for(escalate_at), create)
                .await
            {
                if let Err(error) = self.docker.remove_network(&id).await {
                    tracing::debug!(
                        "could not remove the network a cancelled create made: {error:#}"
                    );
                }
            }
        }
        let build_cancels = self.start_cancelling_abandoned_builds().await;
        let pending = pending_creates.into_inner().unwrap();
        let _ = self
            .until_interrupted(
                self.interrupt.wait_for(escalate_at),
                futures::future::join_all(pending.iter().map(|(_, create)| create.clone())),
            )
            .await;
        // A create cut short there may still make its container after the
        // take-down has looked, as a task run's may (`run_task`'s cleanup),
        // and that container would hold every later `up` waiting.
        let mut unanswered: Vec<String> = {
            let known = created_containers.lock().unwrap();
            pending
                .into_iter()
                .filter(|(name, create)| create.peek().is_none() && !known.contains_key(name))
                .map(|(name, _)| name)
                .collect()
        };
        unanswered.sort();
        unanswered.dedup();
        if !unanswered.is_empty() {
            let (creates, they) = if unanswered.len() == 1 {
                ("create", "it")
            } else {
                ("creates", "they")
            };
            self.event_sink.post(TaskEvent::Warning {
                message: format!(
                    "Stopped waiting for the {creates} of {} to answer, so {they} may exist; \
                     `ratect services down {group_name}` removes {they}.",
                    unanswered.join(", "),
                )
                .to_text(),
            });
        }
        if reached {
            self.event_sink.post(TaskEvent::CleanupStarting { cause });
        }
        let mut own_ids = own_ids.into_inner().unwrap();
        own_ids.extend(created_containers.lock().unwrap().values().cloned());
        let plan = stop_plan(&graph, &containers);
        let mut watches = watches.into_inner().unwrap();
        let earlier_exits = earlier_exits.into_inner().unwrap();
        let nominated = match &group.exit_code_from {
            ExitCodeRule::Container(nominated)
                if !earlier_exits.contains_key(nominated)
                    && outcome
                        .as_ref()
                        .is_ok_and(|own| !own.iter().any(|(member, _)| member == nominated)) =>
            {
                Some(nominated.as_str())
            }
            _ => None,
        };
        let (taken_down_tx, taken_down) = tokio::sync::oneshot::channel::<()>();
        let take_down = async {
            let failures = if reached {
                self.take_down_service(
                    &key,
                    &own_ids,
                    won_network.into_inner().unwrap().as_deref(),
                    &plan,
                    escalate_at,
                )
                .await
            } else {
                Vec::new()
            };
            let _ = taken_down_tx.send(());
            failures
        };
        // A nominated member still running when the exit condition is met says what it
        // exits with once the take-down stops it. Waited for until it does,
        // and no longer than `NOMINATED_EXIT_GRACE` after the take-down has
        // finished — a stop that failed leaves it running — or a further
        // signal.
        let nominated_exit = async {
            let Some(nominated) = nominated else {
                return HashMap::new();
            };
            let wait = async {
                while let Some((member, exit)) = watches.next().await {
                    if member == nominated {
                        return exit.ok().map(|code| HashMap::from([(member, code)]));
                    }
                }
                None
            };
            let deadline = async {
                let _ = taken_down.await;
                tokio::time::sleep(NOMINATED_EXIT_GRACE).await;
            };
            tokio::select! {
                stopped = wait => stopped.unwrap_or_default(),
                () = deadline => HashMap::new(),
                () = self.interrupt.wait_for(escalate_at) => HashMap::new(),
            }
        };
        let (cleanup_failures, mut stopped) =
            futures::future::join(take_down, nominated_exit).await;
        stopped.extend(earlier_exits);
        self.report_build_cancels(self.interrupt.wait_for(escalate_at), build_cancels)
            .await;
        if reached {
            self.event_sink.post(TaskEvent::CleanupFinished);
        }
        if interrupted_by.is_some() && reached && cleanup_failures.is_empty() {
            self.post_notice(format!("Service group '{group_name}' is down."));
        }

        let outcome = outcome.and_then(|own| {
            select_exit_code(&group.exit_code_from, &own, &stopped).with_context(|| {
                let ExitCodeRule::Container(nominated) = &group.exit_code_from else {
                    unreachable!("only a nominated member's exit code can go unread")
                };
                format!(
                    "Service group '{group_name}' takes its exit code from container \
                     '{nominated}', but Docker reported none for it before the service group \
                     was down."
                )
            })
        });
        // A take-down that left something behind fails a run that was
        // otherwise fine, as a task run's cleanup does; a run that had
        // already failed keeps its own error (decisions/0016).
        let outcome = if cleanup_failures.is_empty() {
            outcome
        } else {
            let failures: Vec<String> = cleanup_failures
                .iter()
                .map(|error| format!("{error:#}"))
                .collect();
            let message = format!(
                "Taking down service group '{group_name}' left something behind: {}. Run \
                 `ratect services down {group_name}` to try again.",
                failures.join("; ")
            );
            self.event_sink.post(TaskEvent::Error {
                message: message.clone().into(),
            });
            outcome.and_then(|_| Err(FailureReported(anyhow::anyhow!(message)).into()))
        };
        let duration = started_at.elapsed();
        match outcome {
            Ok(exit_code) => {
                self.event_sink.post(TaskEvent::TaskFinished {
                    task: group_name.to_string(),
                    exit_code,
                    duration,
                });
                if exit_code == 0 {
                    Ok(())
                } else {
                    Err(crate::docker::ContainerExitedNonZero { exit_code }.into())
                }
            }
            Err(error) => {
                if interrupted_by.is_none() {
                    self.event_sink.post(TaskEvent::TaskFailed {
                        task: group_name.to_string(),
                        duration,
                    });
                }
                Err(error)
            }
        }
    }

    /// What every container of the instance is, for the progress display —
    /// a task run's `TaskGraphResolved`, with no task container.
    fn post_service_graph(
        &self,
        graph: &HashMap<String, Vec<String>>,
        containers: &HashMap<String, Container>,
    ) {
        let infos = graph
            .keys()
            .filter(|name| is_narrated(containers, name))
            .map(|name| {
                let config = containers.get(name);
                crate::ui::TaskContainerInfo {
                    name: name.clone(),
                    image: config.and_then(|c| c.image.clone()),
                    build_tag: config
                        .and_then(|c| c.build_directory.as_ref())
                        .map(|_| format!("{}-{}", self.project.config.project_name, name)),
                    dependencies: config
                        .map(|c| declared_dependencies(c, None))
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|dependency| is_narrated(containers, dependency))
                        .collect(),
                    is_task_container: false,
                    has_setup_commands: config.is_some_and(|c| {
                        c.setup_commands
                            .as_ref()
                            .is_some_and(|commands| !commands.is_empty())
                    }),
                }
            })
            .collect();
        self.event_sink
            .post(TaskEvent::TaskGraphResolved { containers: infos });
    }

    /// One attempt at claiming the instance — see the module's own
    /// documentation for how, and why. The network create is put in
    /// `network_create` while it is under way, and left there once it has
    /// made a network this `up` hasn't yet won or removed.
    async fn claim_instance(
        &self,
        key: &InstanceKey,
        network_name: &str,
        owner_labels: &crate::labels::OwnershipLabels,
        members: &HashSet<String>,
        network_create: &Mutex<Option<NetworkCreate>>,
    ) -> Result<Claim> {
        let group_name = &key.group;
        if let Some(instance) = service_instances::find_one(self.docker.as_ref(), key).await? {
            return self.claim_found(key, instance, members).await;
        }
        let create: NetworkCreate = {
            let docker = Arc::clone(&self.docker);
            let name = network_name.to_string();
            let labels = owner_labels.for_network();
            // Spawned, so that a signal ending this attempt doesn't drop a
            // create the daemon may still carry out.
            let handle = tokio::spawn(async move { docker.create_network(&name, &labels).await });
            futures::FutureExt::shared(futures::FutureExt::boxed(async move {
                match handle.await {
                    Ok(created) => created.map_err(Arc::new),
                    Err(error) => Err(Arc::new(anyhow::Error::new(error))),
                }
            }))
        };
        *network_create.lock().unwrap() = Some(create.clone());
        match create.await {
            Ok(id) => {
                let networks = service_instances::find_one(self.docker.as_ref(), key)
                    .await?
                    .map(|instance| instance.networks)
                    .unwrap_or_default();
                if networks.len() == 1 && networks[0].id == id {
                    return Ok(Claim::Won(id));
                }
                // A second network of the same name, on a daemon that takes
                // one: only an `up` that saw its own alone has won.
                self.docker.remove_network(&id).await?;
                *network_create.lock().unwrap() = None;
                Ok(Claim::StartOver)
            }
            Err(error) => {
                // With the slot emptied this is the only copy, so the error
                // comes back whole, its causes and type included.
                *network_create.lock().unwrap() = None;
                let error =
                    Arc::try_unwrap(error).unwrap_or_else(|shared| anyhow::anyhow!("{shared:#}"));
                if !error.is::<crate::docker::NameInUse>() {
                    return Err(error);
                }
                match service_instances::find_one(self.docker.as_ref(), key).await? {
                    Some(instance) if !instance.networks.is_empty() => {
                        self.claim_found(key, instance, members).await
                    }
                    // The holder went between its create and this look, as
                    // an `up` whose bring-up failed takes its network with it.
                    _ if !self.docker.network_exists(network_name).await? => Ok(Claim::StartOver),
                    _ => anyhow::bail!(
                        "Could not bring up service group '{group_name}': the network name \
                         '{network_name}' is in use by a network that is not this service \
                         group's instance. Ratect neither attaches to it nor removes it."
                    ),
                }
            }
        }
    }

    /// What to do with an instance this `up` found: attach, wait, or replace
    /// it — taking down what it found, then starting over.
    async fn claim_found(
        &self,
        key: &InstanceKey,
        instance: Instance,
        members: &HashSet<String>,
    ) -> Result<Claim> {
        match seen(&instance, members) {
            Seen::Attach => Ok(Claim::Attach(instance)),
            Seen::Wait => Ok(Claim::Wait),
            Seen::Replace => {
                let group_name = &key.group;
                self.post_notice(format!(
                    "Replacing service group '{group_name}': none of its members is up, or its \
                     network is gone."
                ));
                let failures = service_instances::take_down_until_gone(
                    self.docker.as_ref(),
                    &instance,
                    &service_stop_plan(&self.project.config, group_name).unwrap_or_default(),
                    |_| {},
                )
                .await;
                if let Some(first) = failures.into_iter().next() {
                    return Err(
                        first.context(format!("Could not replace service group '{group_name}'"))
                    );
                }
                Ok(Claim::StartOver)
            }
        }
    }

    /// Brings every container of the instance up, as a task run brings up
    /// its dependencies, once this `up` has won its network — but creates
    /// every one before it starts any, unlike a task run, so that another
    /// `up` that finds the instance meanwhile sees a container only created
    /// and waits, rather than attaching to an instance half there.
    async fn bring_up_members<'p>(
        &'p self,
        context: &TaskRunContext<'_, 'p>,
        group: &ServiceGroup,
    ) -> Result<HashMap<String, String>> {
        let mut container_names: Vec<&String> = context.graph.keys().collect();
        container_names.sort();
        futures::future::try_join_all(
            container_names
                .iter()
                .map(|name| self.ensure_container_created(name, context)),
        )
        .await?;
        let ids = futures::future::try_join_all(
            group
                .containers
                .iter()
                .map(|member| self.ensure_container_ready(member, context)),
        )
        .await?;
        Ok(group.containers.iter().cloned().zip(ids).collect())
    }

    /// Attaches to an instance with a member up: follows the output of each
    /// of its containers running at that moment, and returns the id of each member
    /// that hasn't stopped, to watch, and the exit code of a nominated member
    /// that had already, which no watch would see. Starts nothing.
    async fn attach_instance(
        &self,
        graph: &HashMap<String, Vec<String>>,
        key: &InstanceKey,
        instance: &Instance,
        group: &ServiceGroup,
        start_directory: &str,
    ) -> Result<(HashMap<String, String>, HashMap<String, i64>)> {
        let group_name = &key.group;
        let directories = instance.start_directories();
        if directories
            .iter()
            .any(|directory| *directory != start_directory)
        {
            let elsewhere: Vec<&str> = directories
                .into_iter()
                .filter(|directory| *directory != start_directory)
                .collect();
            self.event_sink.post(TaskEvent::Warning {
                message: format!(
                    "Attaching to service group '{group_name}', which was brought up from {}, \
                     not {start_directory}: its containers' bind mounts point there.",
                    elsewhere.join(" and ")
                )
                .to_text(),
            });
        }
        // Also how a foreground `up` that was killed outright, and so said
        // nothing, is found out: the next one says the group was still up.
        self.post_notice(format!(
            "Service group '{group_name}' is already up; attaching to it. Ending this \
             takes it down."
        ));
        let mut names: Vec<&String> = graph.keys().collect();
        names.sort();
        let mut ids = HashMap::new();
        for name in names {
            let Some(container) = instance.container(name) else {
                continue;
            };
            // Output is followed only from a running container: Docker's
            // attach refuses a paused or restarting one.
            if state(container) == Some("running") {
                self.docker.follow_container(&container.id, name).await?;
            }
            if group.containers.contains(name) && is_up(state(container)) {
                ids.insert(name.clone(), container.id.clone());
            }
        }
        let mut earlier = HashMap::new();
        if let ExitCodeRule::Container(nominated) = &group.exit_code_from {
            if let Some(container) = instance
                .container(nominated)
                .filter(|container| matches!(state(container), Some("exited" | "dead")))
            {
                // Stopped, so the wait answers at once with the code it left.
                let code = self
                    .docker
                    .wait_for_container_exit(&container.id)
                    .await
                    .with_context(|| {
                        format!("Could not read the exit code of container '{nominated}'")
                    })?;
                earlier.insert(nominated.clone(), code);
            }
        }
        Ok((ids, earlier))
    }

    /// Waits for the `watched` members this `up` watches to stop until the
    /// group's exit condition is met, returning each one that did, in the order
    /// it did.
    async fn watch_members(
        &self,
        group: &ServiceGroup,
        watched: usize,
        watches: &Mutex<FuturesUnordered<MemberWatch<'_>>>,
    ) -> Result<Vec<(String, i64)>> {
        let mut own = Vec::new();
        loop {
            // Taken out for each wait and put back, so the lock is never
            // held across an `.await`.
            let mut pending = std::mem::take(&mut *watches.lock().unwrap());
            let next = pending.next().await;
            *watches.lock().unwrap() = pending;
            let Some((member, exit)) = next else {
                return Ok(own);
            };
            let exit_code =
                exit.with_context(|| format!("Could not wait for container '{member}' to stop"))?;
            self.event_sink.post(TaskEvent::ServiceMemberExited {
                container: member.clone(),
                exit_code,
            });
            own.push((member, exit_code));
            if condition_met(group.exit_condition, own.len(), watched) {
                return Ok(own);
            }
        }
    }

    /// Takes down what of the instance this `up` won or attached to,
    /// `own_ids` — every container of it, while the network it won is there,
    /// since nothing else creates one — whoever else is taking it down at the
    /// same time, and never a newer instance brought up meanwhile
    /// ([`service_instances::take_down_until_gone`]). A further
    /// signal escalates to removing what is left of it by force, as a task
    /// run's cleanup does (ratect#249). Returns what couldn't be removed.
    async fn take_down_service(
        &self,
        key: &InstanceKey,
        own_ids: &HashSet<String>,
        won_network: Option<&str>,
        plan: &StopPlan,
        escalate_at: usize,
    ) -> Vec<anyhow::Error> {
        let instance = match service_instances::find_one(self.docker.as_ref(), key).await {
            Ok(found) => match found.and_then(|found| {
                // While the network this `up` won is there, only this `up`
                // can have created any container of the instance — one whose
                // create it stopped waiting for included.
                let ours = won_network
                    .is_some_and(|won| found.networks.iter().any(|network| network.id == won));
                if ours {
                    let mut own_ids = own_ids.clone();
                    own_ids.extend(found.containers.iter().map(|c| c.id.clone()));
                    found.retaining(&own_ids)
                } else {
                    found.retaining(own_ids)
                }
            }) {
                Some(instance) => instance,
                None => return Vec::new(),
            },
            Err(error) => return vec![error],
        };
        // Each step posted once, whichever of the graceful and the forced
        // pass reports it.
        let reported: Mutex<HashSet<TakeDownStep>> = Mutex::default();
        let report = |step: TakeDownStep| {
            if !reported.lock().unwrap().insert(step.clone()) {
                return;
            }
            self.event_sink.post(match step {
                TakeDownStep::ContainerStopped(container) => {
                    TaskEvent::ContainerStopped { container }
                }
                TakeDownStep::ContainerRemoved(container) => {
                    TaskEvent::ContainerRemoved { container }
                }
                TakeDownStep::RemovingNetwork(_) => TaskEvent::RemovingServiceGroupNetwork,
                TakeDownStep::NetworkRemoved(_) => TaskEvent::ServiceGroupNetworkRemoved,
            })
        };
        let graceful =
            service_instances::take_down_until_gone(self.docker.as_ref(), &instance, plan, report);
        match self
            .until_interrupted(self.interrupt.wait_for(escalate_at), graceful)
            .await
        {
            Some(failures) => failures,
            None => {
                let mut failures = Vec::new();
                let remaining = match service_instances::find_remaining(
                    self.docker.as_ref(),
                    &instance,
                )
                .await
                {
                    Ok(Some(instance)) => instance,
                    Ok(None) => return failures,
                    Err(error) => return vec![error],
                };
                for container in &remaining.containers {
                    match self.docker.remove_container_forcibly(&container.id).await {
                        Ok(()) => report(TakeDownStep::ContainerRemoved(
                            Instance::container_name(container).to_string(),
                        )),
                        Err(error) => failures.push(error),
                    }
                }
                for network in &remaining.networks {
                    report(TakeDownStep::RemovingNetwork(network.name.clone()));
                    match self.docker.remove_network(&network.id).await {
                        Ok(()) => report(TakeDownStep::NetworkRemoved(network.name.clone())),
                        Err(error) => failures.push(error),
                    }
                }
                failures
            }
        }
    }
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;
