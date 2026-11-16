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

//! The rules on their own; `engine_tests.rs` drives the whole of
//! `bring_up_service` against its fake runtime.

use super::*;

fn exits(pairs: &[(&str, i64)]) -> Vec<(String, i64)> {
    pairs
        .iter()
        .map(|(member, code)| (member.to_string(), *code))
        .collect()
}

#[test]
fn each_exit_condition_is_met_when_it_says() {
    assert!(!condition_met(ExitCondition::AnyStopped, 0, 2));
    assert!(condition_met(ExitCondition::AnyStopped, 1, 2));
    assert!(!condition_met(ExitCondition::AllStopped, 1, 2));
    assert!(condition_met(ExitCondition::AllStopped, 2, 2));
}

#[test]
fn each_exit_code_rule_selects_its_own_exit_code() {
    let own = exits(&[("api", 0), ("db", 3), ("cache", 4)]);
    let stopped = HashMap::from([("worker".to_string(), 143)]);
    let cases = [
        (ExitCodeRule::FirstNonZero, Some(3)),
        (ExitCodeRule::FirstToExit, Some(0)),
        (ExitCodeRule::Container("cache".to_string()), Some(4)),
        (ExitCodeRule::Container("worker".to_string()), Some(143)),
        (ExitCodeRule::Container("absent".to_string()), None),
    ];
    for (rule, expected) in cases {
        assert_eq!(
            select_exit_code(&rule, &own, &stopped),
            expected,
            "{rule:?}"
        );
    }
    assert_eq!(
        select_exit_code(&ExitCodeRule::FirstNonZero, &exits(&[("api", 0)]), &stopped),
        Some(0),
        "a member the take-down stopped is not counted"
    );
}

#[test]
fn containers_are_stopped_before_what_they_depend_on() {
    let graph = HashMap::from([
        ("api".to_string(), vec!["db".to_string()]),
        (
            "worker".to_string(),
            vec!["db".to_string(), "queue".to_string()],
        ),
        ("db".to_string(), vec![]),
        ("queue".to_string(), vec![]),
    ]);

    assert_eq!(
        stop_layers(&graph),
        [vec!["api", "worker"], vec!["db", "queue"]]
    );
}

/// An instance of `stack` in the given containers' states, with a network
/// unless `network` is false.
fn instance(states: &[(&str, &str)], network: bool) -> Instance {
    let resource = |name: &str, state: Option<&str>| LabelledResource {
        id: format!("{name}-id"),
        name: name.to_string(),
        labels: HashMap::from([(crate::labels::CONTAINER.to_string(), name.to_string())]),
        created: Some(0),
        state: state.map(str::to_string),
    };
    Instance {
        key: InstanceKey::default_instance("demo", "stack"),
        containers: states
            .iter()
            .map(|(name, state)| resource(name, Some(state)))
            .collect(),
        networks: if network {
            vec![resource("demo..stack..default", None)]
        } else {
            Vec::new()
        },
    }
}

#[test]
fn an_up_waits_attaches_or_replaces_by_what_it_finds() {
    let members: HashSet<String> = ["api", "db"].map(String::from).into();
    let cases = [
        (
            &[("api", "created"), ("db", "created")][..],
            true,
            Seen::Wait,
        ),
        // Created with the network gone: the `up` creating them is still at
        // it, and fails on its own.
        (
            &[("api", "created"), ("db", "running")][..],
            false,
            Seen::Wait,
        ),
        (
            &[("api", "removing"), ("db", "running")][..],
            true,
            Seen::Wait,
        ),
        (&[][..], true, Seen::Wait),
        (
            &[("api", "running"), ("db", "exited")][..],
            true,
            Seen::Attach,
        ),
        (
            &[("api", "paused"), ("db", "exited")][..],
            true,
            Seen::Attach,
        ),
        (
            &[("api", "running"), ("db", "running")][..],
            false,
            Seen::Replace,
        ),
        (
            &[("api", "exited"), ("db", "dead")][..],
            true,
            Seen::Replace,
        ),
        // A dependency running is no member running.
        (
            &[("api", "exited"), ("cache", "running")][..],
            true,
            Seen::Replace,
        ),
    ];
    for (states, network, expected) in cases {
        assert_eq!(
            seen(&instance(states, network), &members),
            expected,
            "{states:?}, network {network}"
        );
    }
}
