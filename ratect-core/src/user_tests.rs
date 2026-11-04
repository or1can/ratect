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

const HOME: &str = "/home/me";

/// What [`edit_passwd_home`] should do for one image's file and `User`.
struct PasswdCase {
    name: &'static str,
    passwd: &'static [u8],
    user: &'static str,
    contents: &'static [u8],
    owner: Option<HomeOwner>,
}

fn owner(uid: u32, gid: u32) -> Option<HomeOwner> {
    Some(HomeOwner { uid, gid })
}

const NODE: &[u8] = b"root:x:0:0:root:/root:/bin/ash\nnode:x:1000:1000::/home/node:/bin/sh\n";

#[test]
fn edit_passwd_home_changes_only_the_running_users_home() {
    let cases = [
        PasswdCase {
            name: "no configured user means root",
            passwd: NODE,
            user: "",
            contents: b"root:x:0:0:root:/home/me:/bin/ash\nnode:x:1000:1000::/home/node:/bin/sh\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "a non-root user by name",
            passwd: NODE,
            user: "node",
            contents: b"root:x:0:0:root:/root:/bin/ash\nnode:x:1000:1000::/home/me:/bin/sh\n",
            owner: owner(1000, 1000),
        },
        PasswdCase {
            name: "a non-root user by uid",
            passwd: NODE,
            user: "1000",
            contents: b"root:x:0:0:root:/root:/bin/ash\nnode:x:1000:1000::/home/me:/bin/sh\n",
            owner: owner(1000, 1000),
        },
        PasswdCase {
            name: "name:group keeps the entry's gid for a named group",
            passwd: NODE,
            user: "node:staff",
            contents: b"root:x:0:0:root:/root:/bin/ash\nnode:x:1000:1000::/home/me:/bin/sh\n",
            owner: owner(1000, 1000),
        },
        PasswdCase {
            name: "name:gid owns the home with that gid, and edits no gid field",
            passwd: NODE,
            user: "node:20",
            contents: b"root:x:0:0:root:/root:/bin/ash\nnode:x:1000:1000::/home/me:/bin/sh\n",
            owner: owner(1000, 20),
        },
        PasswdCase {
            name: "uid:gid",
            passwd: NODE,
            user: "1000:20",
            contents: b"root:x:0:0:root:/root:/bin/ash\nnode:x:1000:1000::/home/me:/bin/sh\n",
            owner: owner(1000, 20),
        },
        PasswdCase {
            name: "a duplicate uid edits the first, the one the runtime uses",
            passwd: b"root:x:0:0::/root:/bin/sh\ntoor:x:0:0::/toor:/bin/sh\n",
            user: "0",
            contents: b"root:x:0:0::/home/me:/bin/sh\ntoor:x:0:0::/toor:/bin/sh\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "a name selects its own entry, even a later one for a shared uid",
            passwd: b"root:x:0:0::/root:/bin/sh\ntoor:x:0:0::/toor:/bin/sh\n",
            user: "toor",
            contents: b"root:x:0:0::/root:/bin/sh\ntoor:x:0:0::/home/me:/bin/sh\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "a numeric user is always a uid, never a name",
            passwd: b"1000:x:5:5::/five:/bin/sh\nnode:x:1000:1000::/home/node:/bin/sh\n",
            user: "1000",
            contents: b"1000:x:5:5::/five:/bin/sh\nnode:x:1000:1000::/home/me:/bin/sh\n",
            owner: owner(1000, 1000),
        },
        PasswdCase {
            name: "no trailing newline stays that way after an edit",
            passwd: b"root:x:0:0::/root:/bin/sh",
            user: "",
            contents: b"root:x:0:0::/home/me:/bin/sh",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "a comment holding the uid is not an entry",
            passwd: b"#root:x:0:0::/old:/bin/sh\nroot:x:0:0::/root:/bin/sh\n",
            user: "",
            contents: b"#root:x:0:0::/old:/bin/sh\nroot:x:0:0::/home/me:/bin/sh\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "blank, short and non-numeric lines are preserved, not entries",
            passwd: b"\n+::::::\nbad:x:abc:0::/bad:/bin/sh\nroot:x:0:0::/root:/bin/sh\n",
            user: "",
            contents: b"\n+::::::\nbad:x:abc:0::/bad:/bin/sh\nroot:x:0:0::/home/me:/bin/sh\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "non-UTF-8 lines elsewhere are preserved byte-for-byte",
            passwd: b"caf\xe9:x:1:1:\xff\xfe:/c:/bin/sh\r\nroot:x:0:0::/root:/bin/sh\n\xff\n",
            user: "",
            contents: b"caf\xe9:x:1:1:\xff\xfe:/c:/bin/sh\r\nroot:x:0:0::/home/me:/bin/sh\n\xff\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "a leading space doesn't stop a name selecting its line, as for the runtime",
            passwd: b" app:x:1000:1000::/old:/bin/sh\n",
            user: "app",
            contents: b" app:x:1000:1000::/home/me:/bin/sh\n",
            owner: owner(1000, 1000),
        },
        PasswdCase {
            name: "a name selects a short line, whose missing gid is 0 to the runtime too",
            passwd: b"app:x:1000\nroot:x:0:0::/root:/bin/sh\n",
            user: "app",
            contents: b"app:x:1000:::/home/me\nroot:x:0:0::/root:/bin/sh\n",
            owner: owner(1000, 0),
        },
        PasswdCase {
            name: "a name selects a line with nothing after its own field",
            passwd: b"app",
            user: "app",
            contents: b"app:::::/home/me",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "a name selects a line whose ids aren't numbers, each 0 to the runtime",
            passwd: b"app:x:abc:def::/old:/bin/sh\n",
            user: "app",
            contents: b"app:x:abc:def::/home/me:/bin/sh\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "fields after the home are kept, however many",
            passwd: b"root:x:0:0::/root:/bin/sh:extra\n",
            user: "root",
            contents: b"root:x:0:0::/home/me:/bin/sh:extra\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "uid 0 with an empty file gets a root entry",
            passwd: b"",
            user: "",
            contents: b"root:*:0:0::/home/me:/bin/sh\n",
            owner: owner(0, 0),
        },
        PasswdCase {
            name: "a numeric user with no entry gets one named with its digits, gid 0",
            passwd: NODE,
            user: "4242",
            contents: b"root:x:0:0:root:/root:/bin/ash\nnode:x:1000:1000::/home/node:/bin/sh\n\
                        4242:*:4242:0::/home/me:/bin/sh\n",
            owner: owner(4242, 0),
        },
        PasswdCase {
            name: "a numeric group part sets an appended entry's gid",
            passwd: b"",
            user: "4242:20",
            contents: b"4242:*:4242:20::/home/me:/bin/sh\n",
            owner: owner(4242, 20),
        },
        PasswdCase {
            name: "a named group part isn't resolved, so the entry's gid is 0",
            passwd: b"",
            user: "4242:staff",
            contents: b"4242:*:4242:0::/home/me:/bin/sh\n",
            owner: owner(4242, 0),
        },
        PasswdCase {
            name: "appending to a file with no trailing newline starts a new line",
            passwd: b"root:x:0:0::/root:/bin/sh",
            user: "4242",
            contents: b"root:x:0:0::/root:/bin/sh\n4242:*:4242:0::/home/me:/bin/sh\n",
            owner: owner(4242, 0),
        },
        PasswdCase {
            name: "a named user with no entry appends nothing, for the runtime to report",
            passwd: NODE,
            user: "missing",
            contents: NODE,
            owner: None,
        },
        PasswdCase {
            name: "a named root with no entry is still a name",
            passwd: b"",
            user: "root",
            contents: b"",
            owner: None,
        },
    ];

    for case in cases {
        let edit = edit_passwd_home(case.passwd, case.user, HOME);
        assert_eq!(
            String::from_utf8_lossy(&edit.contents),
            String::from_utf8_lossy(case.contents),
            "{}",
            case.name
        );
        assert_eq!(edit.contents, case.contents, "{}", case.name);
        assert_eq!(edit.home_owner, case.owner, "{}", case.name);
    }
}
