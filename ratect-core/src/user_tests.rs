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

use super::*;

fn current_user_fixture() -> CurrentUser {
    CurrentUser {
        uid: 1000,
        gid: 1000,
        username: "ratect".to_string(),
        groupname: "ratect".to_string(),
    }
}

fn root_user_fixture() -> CurrentUser {
    CurrentUser {
        uid: 0,
        gid: 0,
        username: "root".to_string(),
        groupname: "root".to_string(),
    }
}

#[test]
fn generate_passwd_file_for_a_normal_user_includes_both_root_and_that_user() {
    let passwd = generate_passwd_file(&current_user_fixture(), "/home/ratect");
    assert_eq!(
        passwd,
        "root:x:0:0:root:/root:/bin/sh\nratect:x:1000:1000:ratect:/home/ratect:/bin/sh"
    );
}

#[test]
fn generate_passwd_file_for_uid_zero_has_a_single_root_entry_using_the_configured_home() {
    let passwd = generate_passwd_file(&root_user_fixture(), "/home/ratect");
    assert_eq!(passwd, "root:x:0:0:root:/home/ratect:/bin/sh");
}

#[test]
fn generate_shadow_file_for_a_normal_user_includes_both_root_and_that_user() {
    let shadow = generate_shadow_file(&current_user_fixture());
    assert_eq!(
        shadow,
        "root:*:19500:0:99999:7:::\nratect:*:19500:0:99999:7:::"
    );
}

#[test]
fn generate_shadow_file_for_uid_zero_has_a_single_root_entry() {
    let shadow = generate_shadow_file(&root_user_fixture());
    assert_eq!(shadow, "root:*:19500:0:99999:7:::");
}

#[test]
fn generate_group_file_for_a_normal_group_includes_both_root_and_that_group() {
    let group = generate_group_file(&current_user_fixture());
    assert_eq!(group, "root:x:0:root\nratect:x:1000:ratect");
}

#[test]
fn generate_group_file_for_gid_zero_has_a_single_root_entry() {
    let group = generate_group_file(&root_user_fixture());
    assert_eq!(group, "root:x:0:root");
}
