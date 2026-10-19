//! `dibs login` and `dibs whoami`, and which token a checkout signs in with.

use dibs_domain::member::Role;

use crate::support::{API_REMOTE, Setup, json, make_checkout, status, text};

#[test]
fn login_stores_the_token_for_its_project_and_whoami_reads_it_back() {
    let setup = Setup::new();
    let file = setup
        .home(&setup.bob)
        .join("config")
        .join("credentials.toml");
    let stored = std::fs::read_to_string(&file).unwrap();
    assert!(stored.contains("project = \"acme\""), "{stored}");
    assert!(stored.contains(&setup.server.url()), "{stored}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let out = setup
        .dibs(&setup.bob, &setup.elsewhere())
        .args(["whoami", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    let answer = json(&out);
    assert_eq!(answer["member"]["handle"], "bob");
    assert_eq!(answer["project"], "acme");
    assert_eq!(answer["server"], setup.server.url());

    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let out = setup.dibs(&setup.bob, &api).arg("whoami").output().unwrap();
    assert_eq!(status(&out), 0);
    let line = text(&out.stdout);
    assert!(
        line.contains("bob") && line.contains("repository api"),
        "{line}"
    );
}

#[test]
fn a_refused_token_is_not_stored() {
    let setup = Setup::new();
    let out = setup.login(&setup.bob, "dibs_this_token_signs_in_nobody_at_all");
    assert_eq!(status(&out), 1);
    assert!(text(&out.stderr).contains("refused this token"));
    let empty = setup.login(&setup.bob, "  ");
    assert_eq!(status(&empty), 2);
}

#[test]
fn a_token_of_another_project_is_refused_in_this_checkout() {
    let setup = Setup::new();
    let other = setup.server.project("globex", "gus").unwrap();
    let dir = setup.root.path().join("acme-api");
    make_checkout(&dir, &setup.server.url(), "acme", Some("api"), API_REMOTE);

    let out = setup
        .dibs(&setup.bob, &dir)
        .args(["login"])
        .write_stdin(other.token.clone())
        .output()
        .unwrap();
    assert_eq!(status(&out), 2);
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("'acme'") && stderr.contains("'globex'"),
        "{stderr}"
    );

    let out = setup
        .dibs(&setup.bob, &dir)
        .env("DIBS_TOKEN", &other.token)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(status(&out), 2);
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("'acme'") && stderr.contains("'globex'"),
        "{stderr}"
    );
}

#[test]
fn one_person_in_two_projects_on_one_server_keeps_both_tokens() {
    let setup = Setup::new();
    let globex_admin = setup.server.project("globex", "gus").unwrap();
    let bob_in_globex = setup
        .server
        .member(&globex_admin, "bob", Role::Member)
        .unwrap();
    let globex = setup.root.path().join("globex");
    make_checkout(&globex, &setup.server.url(), "globex", None, API_REMOTE);
    let out = setup
        .dibs(&setup.bob, &globex)
        .arg("login")
        .write_stdin(bob_in_globex.token.clone())
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);

    let acme = setup.checkout("api", Some("api"), API_REMOTE);
    for (dir, project) in [(&acme, "acme"), (&globex, "globex")] {
        let out = setup
            .dibs(&setup.bob, dir)
            .args(["whoami", "--json"])
            .output()
            .unwrap();
        assert_eq!(status(&out), 0);
        assert_eq!(json(&out)["project"], project);
    }
    let outside = setup
        .dibs(&setup.bob, &setup.elsewhere())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(status(&outside), 2);
}

#[test]
fn a_project_with_no_stored_token_says_how_to_sign_in() {
    let setup = Setup::new();
    let dir = setup.root.path().join("unknown");
    make_checkout(&dir, &setup.server.url(), "initech", None, API_REMOTE);
    let out = setup.dibs(&setup.bob, &dir).arg("status").output().unwrap();
    assert_eq!(status(&out), 2);
    assert!(text(&out.stderr).contains("dibs login"));
}
