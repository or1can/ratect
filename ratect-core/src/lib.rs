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

//! Ratect's engine, shared by the `ratect` and `ratect-compat` binaries.
//!
//! Each module's own `//!` carries its notes — what it does, the gotchas, and
//! the decisions that are easy to undo by accident (see
//! [decisions/0006](https://github.com/or1can/ratect/blob/main/decisions/0006-code-and-documentation-locality.md)).
//! Read these with:
//!
//! ```text
//! cargo doc --open -p ratect-core --all-features --document-private-items
//! ```
//!
//! `--document-private-items` is not optional here: this is an internal
//! library whose audience is contributors, so its doc comments link freely to
//! private items — which is what the allow below permits. `dockerignore`
//! deliberately keeps the strict default, since it is meant to be publishable
//! on its own.
#![allow(rustdoc::private_intra_doc_links)]

pub mod cache;
pub mod config;
pub mod diagnostics;
pub mod docker;
pub mod engine;
pub mod exit_code;
pub mod expressions;
pub mod git_include;
// Deliberately un-documented here: an outer doc comment on a `mod` item is
// resolved in *this* module's scope, so every intra-doc link in the module's
// own `//!` header would silently break. See the header itself.
pub(crate) mod include_trust;
pub mod interrupt;
pub mod labels;
pub mod proxy;
pub mod resources;
#[cfg(feature = "schema")]
pub mod schema;
pub mod ssh_agent;
pub mod ui;
pub mod user;
