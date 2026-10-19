use crate::support::{Place, Server, body_of, init, request, run, token_of};

#[test]
fn a_project_is_created_once_and_its_printed_token_works_against_the_api() {
    let place = Place::new();
    let token = init(&place.database());

    let again = run(
        &place.database(),
        &[
            "admin",
            "init",
            "--project",
            "widgets",
            "--admin",
            "bob",
            "--name",
            "Bob",
        ],
        &[],
    );
    assert_eq!(again.code, Some(1), "{again:?}");
    assert!(
        again.stderr.contains("a project 'widgets' already exists"),
        "{again:?}"
    );
    assert_eq!(
        again.stdout, "",
        "no token is printed for a refused project"
    );

    let server = Server::spawn(&place.database(), &[]);
    let (status, answer) = request(server.address, "GET", "/v1/me", &token, None);
    assert_eq!(status, 200, "{answer}");
    let me = body_of(&answer);
    assert_eq!(me["member"]["handle"], "alice", "{answer}");
    assert_eq!(me["member"]["role"], "admin", "{answer}");
    assert_eq!(me["project"]["slug"], "widgets", "{answer}");
}

#[test]
fn members_are_added_and_tokens_rotated_while_the_server_runs() {
    let place = Place::new();
    let alice = init(&place.database());
    let server = Server::spawn(&place.database(), &[]);

    let added = run(
        &place.database(),
        &[
            "admin",
            "member",
            "add",
            "--project",
            "widgets",
            "carol",
            "--name",
            "Carol",
        ],
        &[],
    );
    assert_eq!(added.code, Some(0), "{added:?}");
    let carol = token_of(&added);
    let (status, answer) = request(server.address, "GET", "/v1/me", &carol, None);
    assert_eq!(status, 200, "{answer}");
    assert_eq!(body_of(&answer)["member"]["role"], "member", "{answer}");

    let rotated = run(
        &place.database(),
        &["admin", "token", "rotate", "widgets", "alice"],
        &[],
    );
    assert_eq!(rotated.code, Some(0), "{rotated:?}");
    let new_alice = token_of(&rotated);
    let (old, _) = request(server.address, "GET", "/v1/me", &alice, None);
    assert_eq!(old, 401);
    let (new, answer) = request(server.address, "GET", "/v1/me", &new_alice, None);
    assert_eq!(new, 200, "{answer}");
    assert_eq!(body_of(&answer)["member"]["handle"], "alice", "{answer}");
}

#[test]
fn an_unknown_project_or_member_or_a_taken_handle_is_refused() {
    let place = Place::new();
    init(&place.database());
    let cases: &[(&[&str], &str)] = &[
        (
            &[
                "admin",
                "member",
                "add",
                "--project",
                "gadgets",
                "carol",
                "--name",
                "Carol",
            ],
            "there is no project 'gadgets'",
        ),
        (
            &[
                "admin",
                "member",
                "add",
                "--project",
                "widgets",
                "alice",
                "--name",
                "Alice",
            ],
            "the handle 'alice' is taken in this project",
        ),
        (
            &["admin", "token", "rotate", "widgets", "nobody"],
            "there is no member 'nobody'",
        ),
    ];
    for (args, sentence) in cases {
        let ran = run(&place.database(), args, &[]);
        assert_eq!(ran.code, Some(1), "{args:?}: {ran:?}");
        assert!(ran.stderr.contains(sentence), "{args:?}: {ran:?}");
        assert_eq!(ran.stdout, "", "{args:?}");
    }
}

#[test]
fn only_creating_a_project_creates_the_database_file() {
    let place = Place::new();
    for args in [
        &["admin", "token", "rotate", "widgets", "alice"][..],
        &["migrate"][..],
    ] {
        let ran = run(&place.database(), args, &[]);
        assert_eq!(ran.code, Some(1), "{args:?}: {ran:?}");
        assert!(ran.stderr.contains("there is no database at"), "{ran:?}");
    }
    assert!(!place.database().exists());
}

#[test]
fn a_migration_brings_the_schema_up_to_date_offline_only() {
    let place = Place::new();
    init(&place.database());
    let migrated = run(&place.database(), &["migrate"], &[]);
    assert_eq!(migrated.code, Some(0), "{migrated:?}");
    assert!(migrated.stdout.contains("is up to date"), "{migrated:?}");

    let _server = Server::spawn(&place.database(), &[]);
    let refused = run(&place.database(), &["migrate"], &[]);
    assert_eq!(refused.code, Some(1), "{refused:?}");
    assert!(refused.stderr.contains("holds"), "{refused:?}");
}

#[test]
fn a_second_server_on_one_database_refuses_to_start() {
    let place = Place::new();
    let _first = Server::spawn(&place.database(), &[]);
    let second = run(&place.database(), &["serve"], &[]);
    assert_eq!(second.code, Some(1), "{second:?}");
    assert!(
        second
            .stderr
            .contains("another server, a restore or a migration holds the database"),
        "{second:?}"
    );
}

#[test]
fn snapshot_commands_need_a_sensible_snapshot_configuration() {
    let place = Place::new();
    init(&place.database());
    let unset = run(&place.database(), &["admin", "snapshot"], &[]);
    assert_eq!(unset.code, Some(2), "{unset:?}");
    assert!(
        unset.stderr.contains("DIBS_SNAPSHOT_DIR is not set"),
        "{unset:?}"
    );
    let every = run(
        &place.database(),
        &["admin", "snapshot"],
        &[
            ("DIBS_SNAPSHOT_DIR", &place.snapshots()),
            ("DIBS_SNAPSHOT_EVERY", std::path::Path::new("0h")),
        ],
    );
    assert_eq!(every.code, Some(2), "{every:?}");
    assert!(
        every.stderr.contains("DIBS_SNAPSHOT_EVERY is not valid"),
        "{every:?}"
    );
}
