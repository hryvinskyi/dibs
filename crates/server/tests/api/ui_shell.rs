//! The frame of the pages through the real router: every screen's address, the rail's counts, its
//! people, slots and worktrees held, the references in a member's text as links, the sign-out form,
//! an inspector address that does not read, and a request without a session.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use time::Duration;

use crate::support::{Person, World, two_repositories};
use crate::ui_support::{Page, claim, fetch, sign_in, team};

/// The markup of the rail's section headed `heading`.
fn section<'a>(page: &'a Page, heading: &str) -> &'a str {
    let start = page
        .text
        .find(&format!(">{heading}</h3>"))
        .unwrap_or_else(|| panic!("no {heading} in {}", page.text));
    let end = page.text[start..].find("</section>").unwrap() + start;
    &page.text[start..end]
}

#[tokio::test]
async fn every_screen_answers_in_the_frame_with_its_own_entry_current() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    let screens = [
        ("/ui/", "now", false),
        ("/ui/claims", "claims", true),
        ("/ui/queue", "queue", true),
        ("/ui/areas", "areas", true),
        ("/ui/inbox", "inbox", true),
        ("/ui/search", "search", false),
    ];
    for (path, name, split) in screens {
        let page = browser.get(&world, path).await;
        assert_eq!(page.status, StatusCode::OK, "{path}: {}", page.text);
        assert!(
            page.text.contains(&format!(r#"<div class="app" data-screen="{name}" data-stream-url="/ui/events/stream" data-session-url="/ui/session" data-login-url="/ui/login" data-after=""#)),
            "{path}: {}",
            page.text
        );
        assert!(
            page.text.contains(
                r#"<aside id="rail" class="rail" data-region="rail" aria-label="Project">"#
            )
        );
        assert!(
            page.text
                .contains(r#"<div class="pane" data-region="list">"#)
        );
        let main = if split {
            r#"<main id="main" class="main split">"#
        } else {
            r#"<main id="main" class="main">"#
        };
        assert!(page.text.contains(main), "{path}: {}", page.text);
        assert_eq!(
            page.text
                .contains(r#"data-region="inspector" data-selected="false""#),
            split,
            "{path}"
        );
        let current = page.text.matches(r#"aria-current="page""#).count();
        if name == "search" {
            assert_eq!(current, 0, "Search is no entry of the navigation");
        } else {
            assert_eq!(current, 1, "{path}");
            assert!(
                page.text
                    .contains(&format!(r#"<a href="{path}" aria-current="page">"#)),
                "{path}: {}",
                page.text
            );
        }
        assert!(page.text.contains(r#"<div class="notices">"#));
        assert!(
            page.text
                .contains(r#"<div class="banner banner--warn" role="status" data-changed hidden>"#)
        );
        assert!(
            page.text.contains(&format!(
                r#"<a class="banner__action" href="{path}">Reload</a>"#
            )),
            "the reload link asks for the page's own address"
        );
        assert!(page.text.contains(
            r#"<div class="banner banner--danger" role="alert" data-session-ended hidden>"#
        ));
        assert!(page.text.contains(r#"<div class="toast-region" data-toast role="status" aria-live="polite" data-copied="Copied" data-copy-failed="Copy failed"></div>"#));
    }
}

/// Posts a message to a thread as `who`.
async fn reply(world: &World, who: &Person, thread: u64, body: &str) {
    let answer = world
        .post(
            who,
            &format!("/v1/threads/{thread}/messages"),
            json!({"label": "desk/mcp", "body": body}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::CREATED, "{}", answer.body);
}

/// Takes a slot for `who`'s session, or joins its queue.
async fn take_slot(world: &World, who: &Person, session: u64, slot: &str) {
    let answer = world
        .post(
            who,
            &format!("/v1/slots/{slot}/take"),
            json!({"session_id": session, "purpose": "ship it", "mode": "session"}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
}

/// Two repositories and two slots, one of them in a group.
fn with_slots() -> Value {
    let mut settings = two_repositories();
    settings["slots"] = json!([
        {"name": "deploy:prod", "max_hold": "30m", "note": "say it in api:billing first"},
        {"name": "db:staging", "max_hold": "1h", "group": "staging"}
    ]);
    settings
}

#[tokio::test]
async fn the_rail_counts_the_claims_the_queue_the_held_areas_and_the_unread_conversations() {
    let (world, alice, bob) = team().await;
    world.settings(&alice, with_slots()).await;
    let laptop = world.session(&bob, "laptop").await;
    let silent = world.item(&bob, Some("gh:acme/api#1"), "Silent item").await;
    claim(&world, &bob, laptop, silent, json!(["api:billing"])).await;
    let alices_laptop = world.session(&alice, "laptop").await;
    take_slot(&world, &alice, alices_laptop, "deploy:prod").await;
    world.fakes.clock.advance(Duration::hours(2));
    let desk = world.session(&bob, "desk").await;
    let checkout = world.item(&bob, Some("gh:acme/web#2"), "Checkout").await;
    claim(&world, &bob, desk, checkout, json!(["web:checkout"])).await;
    let tower = world.session(&alice, "tower").await;
    let search = world.item(&alice, Some("gh:acme/api#3"), "Search").await;
    claim(&world, &alice, tower, search, json!(["api:search"])).await;
    world.item(&alice, None, "First to pick up").await;
    world.item(&alice, None, "Second to pick up").await;

    world
        .thread(&bob, json!(["alice"]), "From bob", "hello")
        .await;
    let asked = world
        .thread(&alice, json!(["bob"]), "From alice", "a question")
        .await;
    reply(&world, &bob, asked, "an answer").await;
    world.get(&alice, "/v1/slots").await;
    let inbox = world.get(&alice, "/v1/inbox?unread=true").await;
    let threads = inbox.body["threads"].as_array().unwrap();
    assert_eq!(
        threads.len(),
        3,
        "dibs told alice her hold expired: {}",
        inbox.body
    );
    assert!(
        threads
            .iter()
            .any(|thread| thread["thread"]["system"] == true),
        "{}",
        inbox.body
    );

    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, "/ui/").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    for entry in [
        r#"<a href="/ui/" aria-current="page">Now</a>"#,
        r#"<a href="/ui/claims">Claims<span class="k">3</span></a>"#,
        r#"<a href="/ui/queue">Queue<span class="k">3</span></a>"#,
        r#"<a href="/ui/areas">Areas<span class="k">2 held</span></a>"#,
        r#"<a href="/ui/inbox">Inbox<span class="k k--hot">2</span><span class="visually-hidden">unread</span></a>"#,
    ] {
        assert!(page.text.contains(entry), "{entry} in {}", page.text);
    }

    let people = section(&page, "People");
    let alices = people
        .find(r#"<span>alice<small>tower</small></span><span class="k">1</span>"#)
        .unwrap_or_else(|| panic!("{people}"));
    let bobs = people
        .find(r#"<span>bob<small>desk</small></span><span class="k">2 · 1 silent</span>"#)
        .unwrap_or_else(|| panic!("{people}"));
    assert!(alices < bobs, "the viewer first: {people}");
    assert!(people.contains(r#"<a href="/ui/claims?who=bob"><span class="av pc-"#));
}

#[tokio::test]
async fn a_grant_request_waiting_for_the_viewer_does_not_raise_the_rails_inbox_count() {
    let (world, alice, bob) = team().await;
    let laptop = world.session(&alice, "laptop").await;
    let desk = world.session(&bob, "desk").await;
    let billing = world.item(&alice, Some("gh:acme/api#1"), "Billing").await;
    let search = world.item(&bob, Some("gh:acme/api#2"), "Search").await;
    claim(&world, &alice, laptop, billing, json!(["api:billing"])).await;
    claim(&world, &bob, desk, search, json!([])).await;
    let asked = world
        .post(
            &bob,
            "/v1/grant-requests",
            json!({"item": search, "area": "api:billing"}),
        )
        .await;
    assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.body);
    let browser = sign_in(&world, &alice).await;
    let entry = |n: usize| {
        let hot = if n > 0 { " k--hot" } else { "" };
        format!(r#"<a href="/ui/inbox">Inbox<span class="k{hot}">{n}</span>"#)
    };
    let waiting = browser.get(&world, "/ui/").await;
    assert!(waiting.text.contains(&entry(0)), "{}", waiting.text);
    let given = world
        .post(
            &alice,
            "/v1/grants",
            json!({"item": search, "area": "api:billing", "to": "bob"}),
        )
        .await;
    assert_eq!(given.status, StatusCode::CREATED, "{}", given.body);
    let decided = browser.get(&world, "/ui/").await;
    assert!(
        decided.text.contains(&entry(1)),
        "once decided it is a message from a person: {}",
        decided.text
    );
}

#[tokio::test]
async fn the_rail_counts_no_unread_conversation_as_none_and_lists_the_viewer_without_claims() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, "/ui/inbox").await;
    assert!(
        page.text.contains(r#"<a href="/ui/inbox" aria-current="page">Inbox<span class="k">0</span><span class="visually-hidden">unread</span></a>"#),
        "{}",
        page.text
    );
    let people = section(&page, "People");
    assert!(
        people.contains(r#"<span>alice</span><span class="k">0</span>"#),
        "{people}"
    );
    assert!(!page.text.contains(">Slots</h3>"), "no slot is declared");
    assert!(section(&page, "Worktrees held").contains(r#"<li class="faint">None right now</li>"#));
}

#[tokio::test]
async fn a_held_slot_shows_its_holder_and_the_query_opens_it() {
    let (world, alice, bob) = team().await;
    world.settings(&alice, with_slots()).await;
    let desk = world.session(&bob, "desk").await;
    take_slot(&world, &bob, desk, "deploy:prod").await;
    let browser = sign_in(&world, &alice).await;

    let closed = browser.get(&world, "/ui/claims").await;
    let slots = section(&closed, "Slots");
    assert!(
        slots.contains(r#"<li class="held"><details id="slot-deploy:prod"><summary><span>deploy:prod</span><span>bob</span></summary><p>say it in <a class="lnk" href="/ui/areas/api:billing">api:billing</a> first<br><span class="faint">Held for at most 30 min</span></p></details></li>"#),
        "{slots}"
    );
    assert!(
        slots.contains(r#"<li><details id="slot-db:staging"><summary><span>db:staging</span><span>free</span></summary><p><span class="faint">Held for at most 1 h · group staging</span></p></details></li>"#),
        "{slots}"
    );

    let open = browser.get(&world, "/ui/claims?slot=deploy:prod").await;
    assert_eq!(open.status, StatusCode::OK);
    let slots = section(&open, "Slots");
    assert!(
        slots.contains(r#"<details id="slot-deploy:prod" open>"#),
        "{slots}"
    );
    assert!(
        slots.contains(r#"<details id="slot-db:staging">"#),
        "{slots}"
    );
    assert!(
        open.text
            .contains(r#"href="/ui/claims?slot=deploy:prod">Reload</a>"#),
        "{}",
        open.text
    );
}

#[tokio::test]
async fn a_worktree_holds_purpose_links_its_references_and_escapes_the_rest() {
    let (world, alice, bob) = team().await;
    let laptop = world.session(&alice, "laptop").await;
    let item = world
        .item(&alice, Some("gh:acme/api#5"), "Invoice totals")
        .await;
    claim(&world, &alice, laptop, item, json!(["api:billing"])).await;
    let taken = world
        .post(
            &alice,
            "/v1/worktrees/take",
            json!({
                "session_id": laptop, "machine": "0123456789abcdef0123456789abcdef",
                "pool": "c0ffee42", "names": ["worktree1"], "agent": "agent-1", "item": item,
                "purpose": "fixing gh:acme/widgets#42 in api:billing, see https://example.com/a <b>x</b>",
            }),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    world.fakes.clock.advance(Duration::minutes(5));

    let browser = sign_in(&world, &bob).await;
    let page = browser.get(&world, "/ui/queue").await;
    let held = section(&page, "Worktrees held");
    assert!(
        held.contains(r#"<li><span><b>worktree1</b> · <a class="who" href="/ui/claims?who=alice"><span class="av av--s pc-"#),
        "{held}"
    );
    assert!(
        held.contains(r#"<b>alice</b></a>'s agent · 5 min</span>"#),
        "{held}"
    );
    assert!(
        held.contains(&format!(
            "<span>fixing \
             <a class=\"lnk\" href=\"https://github.com/acme/widgets/issues/42\" target=\"_blank\" rel=\"noopener noreferrer\">gh:acme/widgets#42</a> \
             in <a class=\"lnk\" href=\"/ui/areas/api:billing\">api:billing</a>, \
             see <a class=\"lnk\" href=\"https://example.com/a\" target=\"_blank\" rel=\"noopener noreferrer\">https://example.com/a</a> \
             &#60;b&#62;x&#60;/b&#62; · for \
             <span class=\"ref\"><a href=\"/ui/claims/{item}\" title=\"gh:acme/api#5\">api#5</a>\
             <a class=\"ext\" href=\"https://github.com/acme/api/issues/5\" target=\"_blank\" rel=\"noopener noreferrer\" \
             title=\"gh:acme/api#5 on the tracker\" aria-label=\"gh:acme/api#5 on the tracker\">↗</a></span></span>"
        )),
        "{held}"
    );
    assert!(!page.text.contains("<b>x</b>"));
    for hidden in ["0123456789abcdef0123456789abcdef", "c0ffee42", "agent-1"] {
        assert!(!page.text.contains(hidden), "{hidden}");
    }
}

#[tokio::test]
async fn the_sign_out_form_carries_the_sessions_form_token_and_ends_the_session() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    let board = browser.get(&world, "/ui/board").await;
    let page = browser.get(&world, "/ui/").await;
    let token = board.form_token();
    assert!(
        page.text.contains(&format!(
            r#"<form class="rail__foot" method="post" action="/ui/logout"><input type="hidden" name="csrf" value="{token}"><span>Signed in as alice</span><button class="btn btn--small" type="submit">Sign out</button></form>"#
        )),
        "{}",
        page.text
    );
    let out = browser
        .post(&world, "/ui/logout", &[("csrf", &page.form_token())])
        .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    assert_eq!(
        browser.get(&world, "/ui/").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn an_inspector_address_that_does_not_read_answers_404_with_the_whole_page() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    for path in [
        "/ui/claims/abc",
        "/ui/claims/0",
        "/ui/queue/-1",
        "/ui/areas/billing",
        "/ui/areas/api:bad%20name",
        "/ui/inbox/x",
    ] {
        let page = browser.get(&world, path).await;
        assert_eq!(page.status, StatusCode::NOT_FOUND, "{path}");
        assert!(
            page.text.contains(r#"data-region="rail""#)
                && page.text.contains(r#"data-region="list""#)
                && page.text.contains(r#"data-region="inspector""#),
            "{path}: {}",
            page.text
        );
    }
}

#[tokio::test]
async fn now_without_a_session_sends_the_browser_to_the_sign_in_page() {
    let (world, _, _) = team().await;
    let page = fetch(&world, Method::GET, "/ui/", None, None).await;
    assert_eq!(page.status, StatusCode::SEE_OTHER);
    assert_eq!(page.location(), "/ui/login");
}
