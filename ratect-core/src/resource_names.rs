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

//! The Docker names one task execution's containers and network are created
//! under — Batect's `DockerResourceNameGenerator`, ported (ratect#299):
//! `<project>-<container>-<suffix>` for a container and
//! `<project>-network-<suffix>` for the task network, where the suffix is six
//! random `a-z0-9` characters drawn once per task execution, so everything
//! one execution creates shares it.
//!
//! A name is for a person reading `docker ps`, never for Ratect to read back:
//! which project, task, run or container a resource belongs to is what the
//! `eu.orican.ratect.*` labels say ([`crate::labels`],
//! [decisions/0002](../../decisions/0002-runtime-ownership-labels.md)), and
//! nothing parses a name to recover any of it. That is also why the suffix is
//! not the run id — six characters is Batect's format, and the `run` label
//! keeps the full one. A collision between two executions' suffixes is
//! accepted as Batect accepts it, not retried: Docker refuses the second
//! execution, at a point that depends on the daemon's version
//! (`docs/task-lifecycle.md`'s Cross-task isolation, ratect#690).
//!
//! Neither the project name nor a container name is truncated or checked
//! here: Batect's `limitTo` truncation has no production caller for these
//! names, and both are already image-name components, checked at load
//! ([decisions/0018](../../decisions/0018-built-images-named-for-people-found-by-labels.md)),
//! which Docker takes in a container or network name as they are.

/// The characters a suffix is drawn from — Batect's `generateId`'s
/// `('a'..'z') + ('0'..'9')`.
const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Six characters drawn uniformly from `a-z0-9`. The randomness is a v4
/// `Uuid`'s, already a dependency for the run id, rather than a new RNG
/// crate. Bytes 6 and 8 carry its fixed version and variant bits, so only
/// the other fourteen are used, and a byte at or above the largest multiple
/// of 36 is skipped, so every character is equally likely.
fn generate_suffix() -> String {
    const LIMIT: u8 = (u8::MAX / 36) * 36;
    let mut suffix = String::with_capacity(6);
    while suffix.len() < 6 {
        let bytes = uuid::Uuid::new_v4().into_bytes();
        for (index, byte) in bytes.into_iter().enumerate() {
            if index != 6 && index != 8 && byte < LIMIT && suffix.len() < 6 {
                suffix.push(ALPHABET[usize::from(byte % 36)] as char);
            }
        }
    }
    suffix
}

/// One task execution's Docker names: built once, in `run_task_internal`,
/// alongside its `RunLabels`, so every name it gives shares one suffix.
#[derive(Debug, Clone)]
pub struct ResourceNames {
    project: String,
    suffix: String,
}

impl ResourceNames {
    /// `project` is `Config::project_name`, used as written.
    pub fn new(project: &str) -> Self {
        Self {
            project: project.to_string(),
            suffix: generate_suffix(),
        }
    }

    /// `<project>-<container>-<suffix>`, `container` being the configured
    /// container's own key.
    pub fn container(&self, container: &str) -> String {
        self.name(container)
    }

    /// `<project>-network-<suffix>` — only used when Ratect creates the task
    /// network; `--use-network`'s is used under its own name.
    pub fn network(&self) -> String {
        self.name("network")
    }

    /// Batect's `generateNameFor`: `<project>-<part>-<suffix>`.
    fn name(&self, part: &str) -> String {
        format!("{}-{part}-{}", self.project, self.suffix)
    }
}

#[cfg(test)]
#[path = "resource_names_tests.rs"]
mod tests;
