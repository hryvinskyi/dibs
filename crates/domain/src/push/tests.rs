use super::*;
use crate::slot::split_commands;
use crate::testing::settings;

/// `api` protects `main` for alice; `web` protects `main` for bob.
fn project() -> ProjectSettings {
    settings(
        r#"{"repos":[
            {"id":"api","remote":"example.com/acme/api",
             "protected_refs":[{"ref":"refs/heads/main","pushers":["alice"]}]},
            {"id":"web","remote":"example.com/acme/web",
             "protected_refs":[{"ref":"refs/heads/main","pushers":["bob"]}]}]}"#,
    )
}

/// A checkout of `api` on `branch` (`None`: a detached head), with `origin`, a fork, the `web`
/// repository as a remote, an upstream for `main` and `feature`, a `gh:` shorthand, and `extra`.
fn facts(branch: Option<&str>, extra: &[(&str, &str)]) -> GitFacts {
    let mut config: Vec<(String, String)> = [
        ("remote.origin.url", "git@example.com:acme/api.git"),
        ("remote.fork.url", "git@example.com:bob/api.git"),
        ("remote.web.url", "https://example.com/acme/web.git"),
        ("branch.main.remote", "origin"),
        ("branch.main.merge", "refs/heads/main"),
        ("branch.feature.remote", "origin"),
        ("branch.feature.merge", "refs/heads/feature"),
        ("url.git@example.com:.insteadof", "gh:"),
    ]
    .iter()
    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
    .collect();
    config.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    GitFacts {
        branch: branch.map(str::to_owned),
        config,
    }
}

fn handle(text: &str) -> Handle {
    Handle::parse(text).unwrap()
}

/// What `who` pushing `line` from `facts` needs: `None` when nothing refuses it, else the
/// refused branch and whether the push named it.
fn judged(line: &str, who: &str, facts: Option<&GitFacts>) -> Judged {
    let settings = project();
    let pusher = handle(who);
    let here = |dir: &str| dir == "." || dir == "./";
    let protection = PushProtection {
        pusher: &pusher,
        settings: &settings,
        facts,
        here: &here,
    };
    let commands = split_commands(line);
    let commands: Vec<&SimpleCommand> = commands.iter().collect();
    match protection.refusal(&git_pushes(&commands)) {
        Some(CommandVerdict::NeedsPermission {
            target, certain, ..
        }) => Some((target.to_string(), certain)),
        Some(other) => panic!("{line}: {other:?}"),
        None => None,
    }
}

/// The branch a push is refused for, and whether the push named it; `None` when it passes.
type Judged = Option<(String, bool)>;

const API_MAIN: &str = "api:refs/heads/main";
const WEB_MAIN: &str = "web:refs/heads/main";

#[test]
fn every_spelling_of_a_push_to_a_protected_branch_is_refused_and_a_branch_of_ones_own_is_not() {
    let on_main = facts(Some("main"), &[]);
    let on_feature = facts(Some("feature"), &[]);
    let detached = facts(None, &[]);
    let refused = |branch: &str| Some((branch.to_owned(), true));
    let unsure = |branch: &str| Some((branch.to_owned(), false));
    let cases: Vec<(&str, &GitFacts, Judged)> = vec![
        ("git push", &on_main, refused(API_MAIN)),
        ("git push", &on_feature, None),
        ("git push origin", &on_main, refused(API_MAIN)),
        ("git push origin", &on_feature, None),
        ("git push origin HEAD", &on_main, refused(API_MAIN)),
        ("git push origin HEAD", &on_feature, None),
        ("git push origin @", &on_main, refused(API_MAIN)),
        ("git push origin HEAD", &detached, unsure(API_MAIN)),
        ("git push origin HEAD:main", &on_feature, refused(API_MAIN)),
        (
            "git push origin HEAD:refs/heads/main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("git push --all origin", &on_feature, unsure(API_MAIN)),
        ("git push --branches origin", &on_feature, unsure(API_MAIN)),
        ("git push --mirror origin", &on_feature, unsure(API_MAIN)),
        (
            "git push --prune origin feature",
            &on_feature,
            unsure(API_MAIN),
        ),
        (
            "git push origin 'refs/heads/*:refs/heads/*'",
            &on_feature,
            refused(API_MAIN),
        ),
        (
            "git push origin 'refs/heads/feat*:refs/heads/feat*'",
            &on_feature,
            None,
        ),
        ("git push origin ':'", &on_feature, unsure(API_MAIN)),
        ("git -C . push origin main", &on_feature, refused(API_MAIN)),
        (
            "git -C ../elsewhere push origin feature",
            &on_feature,
            unsure(API_MAIN),
        ),
        (
            "git --git-dir=other/.git push origin feature",
            &on_feature,
            unsure(API_MAIN),
        ),
        (
            "git push --repo=origin main",
            &on_feature,
            refused(API_MAIN),
        ),
        (
            "git push --repo origin main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("git push origin :main", &on_feature, refused(API_MAIN)),
        (
            "git push origin --delete main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("git push -d origin main", &on_feature, refused(API_MAIN)),
        ("git push origin +main", &on_feature, refused(API_MAIN)),
        (
            "git push origin refs/heads/main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("git push origin main:main", &on_feature, refused(API_MAIN)),
        (
            "git push origin feature:main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("git push origin main:", &on_feature, unsure(API_MAIN)),
        ("git push -u origin feature", &on_feature, None),
        (
            "git push --force-with-lease origin feature",
            &on_feature,
            None,
        ),
        ("git push origin v1.0:refs/tags/v1.0", &on_feature, None),
        ("git push --tags", &on_feature, None),
        (
            "git push --tags origin main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("git push fork main", &on_main, None),
        (
            "git push https://example.com/acme/api.git main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("git push gh:acme/api main", &on_feature, refused(API_MAIN)),
        ("git push gh:bob/api main", &on_feature, None),
        ("git push web main", &on_feature, None),
        ("git push web feature", &on_feature, None),
        (
            "cd api && git fetch && git push origin main",
            &on_feature,
            refused(API_MAIN),
        ),
        ("echo git push origin main", &on_main, None),
        ("git status", &on_main, None),
    ];
    for (line, facts, expected) in cases {
        assert_eq!(judged(line, "bob", Some(facts)), expected, "{line}");
    }
}

#[test]
fn each_repository_is_judged_by_its_own_pushers() {
    let on_feature = facts(Some("feature"), &[]);
    assert_eq!(
        judged("git push origin main", "alice", Some(&on_feature)),
        None
    );
    assert_eq!(
        judged("git push --all origin", "alice", Some(&on_feature)),
        None
    );
    assert_eq!(
        judged("git push web main", "alice", Some(&on_feature)),
        Some((WEB_MAIN.to_owned(), true))
    );
    assert_eq!(judged("git push web main", "bob", Some(&on_feature)), None);
}

#[test]
fn what_git_pushes_by_default_comes_from_the_checkouts_settings() {
    let matching = facts(Some("feature"), &[("push.default", "matching")]);
    assert_eq!(
        judged("git push", "bob", Some(&matching)),
        Some((API_MAIN.to_owned(), false))
    );
    let nothing = facts(Some("main"), &[("push.default", "nothing")]);
    assert_eq!(judged("git push", "bob", Some(&nothing)), None);
    let configured = facts(Some("feature"), &[("remote.origin.push", "refs/heads/*")]);
    assert_eq!(
        judged("git push", "bob", Some(&configured)),
        Some((API_MAIN.to_owned(), false))
    );
    let upstream_main = facts(
        Some("topic"),
        &[
            ("branch.topic.remote", "origin"),
            ("branch.topic.merge", "refs/heads/main"),
            ("push.default", "upstream"),
        ],
    );
    assert_eq!(
        judged("git push", "bob", Some(&upstream_main)),
        Some((API_MAIN.to_owned(), true)),
        "a branch whose upstream is main pushes to main"
    );
    let to_fork = facts(Some("main"), &[("branch.main.pushremote", "fork")]);
    assert_eq!(judged("git push", "bob", Some(&to_fork)), None);
    let default_fork = facts(Some("main"), &[("remote.pushdefault", "fork")]);
    assert_eq!(judged("git push", "bob", Some(&default_fork)), None);
    let detached = facts(None, &[]);
    assert_eq!(
        judged("git push", "bob", Some(&detached)),
        None,
        "git pushes nothing from a detached head by default"
    );
}

#[test]
fn a_push_is_refused_for_a_non_pusher_when_the_checkouts_git_cannot_be_read() {
    assert_eq!(
        judged("git push origin feature", "bob", None),
        Some((API_MAIN.to_owned(), false))
    );
    assert_eq!(
        judged("git status", "bob", None),
        None,
        "only a push is judged"
    );
}

#[test]
fn a_project_with_one_repository_has_every_remote_for_it() {
    let single = settings(r#"{"protected_refs":[{"ref":"refs/heads/main","pushers":["alice"]}]}"#);
    let pusher = handle("bob");
    let here = |_: &str| true;
    let facts = facts(Some("feature"), &[]);
    let protection = PushProtection {
        pusher: &pusher,
        settings: &single,
        facts: Some(&facts),
        here: &here,
    };
    let commands = split_commands("git push fork main");
    let commands: Vec<&SimpleCommand> = commands.iter().collect();
    assert!(matches!(
        protection.refusal(&git_pushes(&commands)),
        Some(CommandVerdict::NeedsPermission { certain: true, .. })
    ));
    assert!(protection.applies());
}

#[test]
fn the_pre_push_refs_are_judged_against_the_repository_of_the_remote_pushed_to() {
    let settings = project();
    let pusher = handle("bob");
    let here = |_: &str| true;
    let facts = facts(Some("feature"), &[]);
    let protection = PushProtection {
        pusher: &pusher,
        settings: &settings,
        facts: Some(&facts),
        here: &here,
    };
    let refs = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    let main = refs(&["refs/heads/main"]);
    let api = settings.repo(&crate::repo::RepoId::parse("api").unwrap());
    let named = |verdict: Option<CommandVerdict>| match verdict {
        Some(CommandVerdict::NeedsPermission { target, .. }) => Some(target.to_string()),
        _ => None,
    };
    assert_eq!(
        named(protection.refs_refusal(Some("origin"), Some("/srv/mirror/api.git"), &main, api)),
        Some(API_MAIN.to_owned()),
        "a named remote is the repository of its fetch URL, wherever it pushes"
    );
    assert_eq!(
        named(protection.refs_refusal(Some("origin"), None, &refs(&["refs/heads/feature"]), api)),
        None
    );
    assert_eq!(
        named(protection.refs_refusal(Some("origin"), None, &refs(&["refs/tags/v1"]), api)),
        None
    );
    assert_eq!(
        named(protection.refs_refusal(Some("fork"), None, &main, api)),
        None
    );
    assert_eq!(
        named(protection.refs_refusal(
            Some("https://example.com/acme/web.git"),
            Some("https://example.com/acme/web.git"),
            &main,
            api
        )),
        None,
        "a push to a URL is the repository of that URL, where bob may push"
    );
    assert_eq!(
        named(protection.refs_refusal(
            Some("ssh://git@example.com/acme/api.git"),
            Some("ssh://git@example.com/acme/api.git"),
            &main,
            None
        )),
        Some(API_MAIN.to_owned())
    );
    assert_eq!(
        named(protection.refs_refusal(None, None, &main, api)),
        Some(API_MAIN.to_owned()),
        "a hook that passes no remote is judged as the checkout's own repository"
    );
    let without_git = PushProtection {
        facts: None,
        ..protection
    };
    assert_eq!(
        named(without_git.refs_refusal(
            Some("origin"),
            Some("git@example.com:acme/api.git"),
            &main,
            api
        )),
        Some(API_MAIN.to_owned())
    );
}

/// `api` protects the tags `v*` for carol and `main` for alice, the tags declared first.
fn tagged() -> ProjectSettings {
    settings(
        r#"{"repos":[
            {"id":"api","remote":"example.com/acme/api",
             "protected_refs":[{"ref":"refs/tags/v*","pushers":["carol"]},
                               {"ref":"refs/heads/main","pushers":["alice"]}]},
            {"id":"web","remote":"example.com/acme/web"}]}"#,
    )
}

/// The protected ref the refs git names for a push to `remote` are refused for, as `who`.
fn pre_push(
    settings: &ProjectSettings,
    who: &str,
    remote: Option<&str>,
    names: &[&str],
    fallback: bool,
) -> Judged {
    let pusher = handle(who);
    let here = |_: &str| true;
    let facts = facts(Some("feature"), &[]);
    let protection = PushProtection {
        pusher: &pusher,
        settings,
        facts: Some(&facts),
        here: &here,
    };
    let refs: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    let governing = settings
        .repo(&crate::repo::RepoId::parse("api").unwrap())
        .filter(|_| fallback);
    match protection.refs_refusal(remote, None, &refs, governing) {
        Some(CommandVerdict::NeedsPermission {
            target, certain, ..
        }) => Some((target.to_string(), certain)),
        Some(other) => panic!("{names:?}: {other:?}"),
        None => None,
    }
}

const API_TAGS: &str = "api:refs/tags/v*";

#[test]
fn a_tag_git_pushes_or_deletes_is_refused_for_anyone_its_protection_does_not_list() {
    let settings = tagged();
    let release = ["refs/tags/v2.4.0"];
    assert_eq!(
        pre_push(&settings, "bob", Some("origin"), &release, true),
        Some((API_TAGS.to_owned(), true)),
        "a push of a release tag by someone who is not its pusher"
    );
    assert_eq!(
        pre_push(&settings, "carol", Some("origin"), &release, true),
        None,
        "its pusher pushes it"
    );
    assert_eq!(
        pre_push(&settings, "alice", Some("origin"), &release, true),
        Some((API_TAGS.to_owned(), true)),
        "pushing a protected branch lets nobody push a protected tag"
    );
    assert_eq!(
        pre_push(
            &settings,
            "bob",
            Some("origin"),
            &["refs/tags/nightly"],
            true
        ),
        None,
        "a tag the pattern does not cover is free"
    );
    assert_eq!(
        pre_push(&settings, "bob", Some("fork"), &release, true),
        None,
        "a fork protects nothing"
    );
    assert_eq!(
        pre_push(
            &settings,
            "bob",
            Some("origin"),
            &["refs/heads/feature", "refs/tags/v2.4.0"],
            true
        ),
        Some((API_TAGS.to_owned(), true)),
        "one protected tag among the refs refuses the push"
    );
    assert_eq!(
        pre_push(
            &settings,
            "carol",
            Some("origin"),
            &["refs/heads/main"],
            true
        ),
        Some((API_MAIN.to_owned(), true)),
        "the tags' pusher may not push the branch"
    );
}

#[test]
fn a_tag_pushed_where_the_repository_cannot_be_told_is_refused_when_some_protection_covers_it() {
    let tags_only = settings(
        r#"{"repos":[
            {"id":"api","remote":"example.com/acme/api",
             "protected_refs":[{"ref":"refs/tags/v*","pushers":["carol"]}]}]}"#,
    );
    assert_eq!(
        pre_push(&tags_only, "bob", None, &["refs/tags/v1"], false),
        Some((API_TAGS.to_owned(), false))
    );
    assert_eq!(
        pre_push(&tags_only, "bob", None, &["refs/tags/nightly"], false),
        None
    );
    assert_eq!(
        pre_push(&tags_only, "bob", None, &["refs/heads/main"], false),
        None,
        "protected tags alone protect no branch"
    );
    assert_eq!(
        pre_push(&tags_only, "carol", None, &["refs/tags/v1"], false),
        None
    );
}

/// What `who` pushing `line` needs under `settings`, from a checkout of `api` on `feature` with
/// `extra` git settings.
fn line_judged(
    settings: &ProjectSettings,
    line: &str,
    who: &str,
    extra: &[(&str, &str)],
) -> Judged {
    let facts = facts(Some("feature"), extra);
    let pusher = handle(who);
    let here = |dir: &str| dir == "." || dir == "./";
    let protection = PushProtection {
        pusher: &pusher,
        settings,
        facts: Some(&facts),
        here: &here,
    };
    let commands = split_commands(line);
    let commands: Vec<&SimpleCommand> = commands.iter().collect();
    match protection.refusal(&git_pushes(&commands)) {
        Some(CommandVerdict::NeedsPermission {
            target, certain, ..
        }) => Some((target.to_string(), certain)),
        Some(other) => panic!("{line}: {other:?}"),
        None => None,
    }
}

#[test]
fn a_command_line_that_names_or_may_push_a_protected_tag_is_refused_for_anyone_not_listed() {
    let settings = tagged();
    let named = Some((API_TAGS.to_owned(), true));
    let unsure = Some((API_TAGS.to_owned(), false));
    for (line, expected) in [
        ("git push origin v2.4.0", &named),
        ("git push origin refs/tags/v2.4.0", &named),
        ("git push origin v2.4.0:refs/tags/v2.4.0", &named),
        ("git push origin :refs/tags/v2.3.0", &named),
        ("git push origin --delete v2.3.0", &named),
        ("git push origin -d refs/tags/v2.3.0", &named),
        ("git push origin 'refs/tags/*:refs/tags/*'", &named),
        ("git push origin 'refs/tags/v2*:refs/tags/v2*'", &named),
        ("git push --tags", &unsure),
        ("git push origin --tags", &unsure),
        ("git push origin feature --tags", &unsure),
        ("git push --follow-tags origin feature", &unsure),
    ] {
        assert_eq!(
            &line_judged(&settings, line, "bob", &[]),
            expected,
            "{line}"
        );
    }
    for line in [
        "git push origin nightly",
        "git push origin refs/tags/nightly",
        "git push origin feature",
        "git push origin 'refs/tags/release-*:refs/tags/release-*'",
        "git push fork v2.4.0",
        "git push fork --tags",
    ] {
        assert_eq!(line_judged(&settings, line, "bob", &[]), None, "{line}");
    }
    assert_eq!(
        line_judged(&settings, "git push", "bob", &[("push.followtags", "true")]),
        unsure,
        "push.followTags makes a bare push carry tags"
    );
    assert_eq!(
        line_judged(
            &settings,
            "git push origin",
            "bob",
            &[("remote.origin.push", "refs/heads/feature")]
        ),
        Some((API_MAIN.to_owned(), false)),
        "a push setting may push anything; the branch is named first"
    );
    for line in [
        "git push origin v2.4.0",
        "git push origin :refs/tags/v2.3.0",
        "git push --tags",
        "git push origin feature --follow-tags",
    ] {
        assert_eq!(
            line_judged(&settings, line, "carol", &[]),
            None,
            "the pusher: {line}"
        );
    }
    assert_eq!(
        line_judged(&settings, "git push --all origin", "carol", &[]),
        Some((API_MAIN.to_owned(), false)),
        "every branch pushed is refused for the branch, not the tags declared before it"
    );
    assert_eq!(
        line_judged(&settings, "git push --mirror origin", "carol", &[]),
        Some((API_MAIN.to_owned(), false))
    );

    let tags_only = settings_of_tags_only();
    assert_eq!(
        line_judged(&tags_only, "git push --mirror origin", "bob", &[]),
        Some(("default:refs/tags/v*".to_owned(), false)),
        "a mirror pushes every tag"
    );
    assert_eq!(
        line_judged(&tags_only, "git push --all origin", "bob", &[]),
        None
    );
    assert_eq!(
        line_judged(&tags_only, "git push origin main", "bob", &[]),
        None
    );
    let pusher = handle("bob");
    let here = |_: &str| false;
    let unreadable = PushProtection {
        pusher: &pusher,
        settings: &tags_only,
        facts: None,
        here: &here,
    };
    let commands = split_commands("git push origin feature");
    let commands: Vec<&SimpleCommand> = commands.iter().collect();
    assert!(
        matches!(
            unreadable.refusal(&git_pushes(&commands)),
            Some(CommandVerdict::NeedsPermission { certain: false, .. })
        ),
        "a push whose git settings cannot be read may push a tag, so it is refused"
    );
}

#[test]
fn an_exact_tag_rule_covers_that_tag_alone() {
    let exact = settings(
        r#"{"repos":[
            {"id":"api","remote":"example.com/acme/api",
             "protected_refs":[{"ref":"refs/tags/stable","pushers":["carol"]}]}]}"#,
    );
    assert_eq!(
        line_judged(&exact, "git push origin stable", "bob", &[]),
        Some(("api:refs/tags/stable".to_owned(), true))
    );
    assert_eq!(
        line_judged(
            &exact,
            "git push origin 'refs/tags/st*:refs/tags/st*'",
            "bob",
            &[]
        ),
        Some(("api:refs/tags/stable".to_owned(), true))
    );
    assert_eq!(
        line_judged(&exact, "git push origin stable2", "bob", &[]),
        None
    );
    assert_eq!(
        line_judged(&exact, "git push origin stable", "carol", &[]),
        None
    );
}

fn settings_of_tags_only() -> ProjectSettings {
    settings(r#"{"protected_refs":[{"ref":"refs/tags/v*","pushers":["carol"]}]}"#)
}
