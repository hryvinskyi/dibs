//! Inbox (`/ui/inbox`) through the real router: the tabs and their counts, the messages from
//! people, the grant requests waiting for the viewer, dibs's notices folded onto their item, the text
//! filter, a conversation in the inspector with what its live view needs, a message dibs wrote,
//! where a reply and an acknowledgement land, and a conversation the viewer cannot read.

use axum::http::StatusCode;
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World, checkout_of};
use crate::ui_support::{Browser, Page, claim, sign_in, team};

/// A project whose repository `api` is `acme/widgets` on GitHub, with the areas `billing` and
/// `search`.
fn settings() -> Value {
    json!({
        "stale_after": "9d",
        "repos": [{
            "id": "api",
            "remote": "https://github.com/acme/widgets",
            "areas": [{"name": "billing", "paths": ["src/billing/**"]},
                      {"name": "search", "paths": ["src/search/**"]}]
        }]
    })
}

/// alice (the viewer) with a conversation of every kind.
struct Seeded {
    world: World,
    alice: Person,
    bob: Person,
    carol: Person,
    browser: Browser,
    /// alice's claim on `gh:acme/widgets#40`, holding `api:billing`.
    billing: u64,
    /// bob's claim on `gh:acme/widgets#41`.
    search: u64,
    /// bob's conversation with alice about the billing item, unread by her.
    from_bob: u64,
    /// alice's conversation with bob, which bob answered.
    from_alice: u64,
    /// bob's request for a grant of `api:billing` for his item.
    grant: u64,
}

/// A heartbeat of `session` from a checkout on `branch` of `api`.
async fn beat(world: &World, who: &Person, session: u64, branch: &str) {
    let beat = world
        .post(
            who,
            "/v1/heartbeat",
            json!({"session_id": session, "checkout": checkout_of(session), "repo": "api",
                   "branch": branch}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
}

/// Starts a conversation as `who` with `body` and answers its id.
async fn start(world: &World, who: &Person, body: Value) -> u64 {
    let started = world.post(who, "/v1/threads", body).await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    started.body["thread"]["id"].as_u64().unwrap()
}

impl Seeded {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let carol = world.member(&alice, "carol", Role::Member).await;
        let bot = world.member(&alice, "bot", Role::Importer).await;
        world.settings(&alice, settings()).await;

        let alice_laptop = world.session(&alice, "alice-laptop").await;
        let billing = world
            .item(&alice, Some("gh:acme/widgets#40"), "Billing rewrite")
            .await;
        claim(
            &world,
            &alice,
            alice_laptop,
            billing,
            json!(["api:billing"]),
        )
        .await;
        beat(&world, &alice, alice_laptop, "alice/40-billing").await;
        let bob_desk = world.session(&bob, "bob-desk").await;
        let search = world
            .item(&alice, Some("gh:acme/widgets#41"), "Search tweak")
            .await;
        claim(&world, &bob, bob_desk, search, json!([])).await;
        beat(&world, &bob, bob_desk, "bob/41-search").await;
        let carol_desk = world.session(&carol, "carol-desk").await;
        let cache = world
            .item(&alice, Some("gh:acme/widgets#43"), "Cache warmup")
            .await;
        claim(&world, &carol, carol_desk, cache, json!([])).await;
        beat(&world, &carol, carol_desk, "carol/43-cache").await;
        for (item, branch) in [
            (billing, "alice/40-billing"),
            (search, "bob/41-search"),
            (cache, "carol/43-cache"),
        ] {
            let stored = world
                .put(
                    &bot,
                    &format!("/v1/items/{item}/footprint"),
                    json!({"repo": "api", "branch": branch, "base": "main",
                           "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd1234",
                           "paths": ["src/billing/a.rs"]}),
                )
                .await;
            assert_eq!(stored.body["stored"], true, "{}", stored.body);
        }

        let from_bob = start(
            &world,
            &bob,
            json!({"subject": "Refund rounding", "to": ["alice"], "item": billing,
                   "label": "web", "body": "Which rounding mode?"}),
        )
        .await;
        let from_alice = world
            .thread(&alice, json!(["bob"]), "Deploy window", "When can we ship?")
            .await;
        let answered = world
            .post(
                &bob,
                &format!("/v1/threads/{from_alice}/messages"),
                json!({"label": "desk/mcp", "body": "Thursday"}),
            )
            .await;
        assert_eq!(answered.status, StatusCode::CREATED, "{}", answered.body);
        let asked = world
            .post(
                &bob,
                "/v1/grant-requests",
                json!({"item": search, "area": "api:billing"}),
            )
            .await;
        assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.body);
        let grant = asked.body["thread_id"].as_u64().unwrap();

        let browser = sign_in(&world, &alice).await;
        Self {
            world,
            alice,
            bob,
            carol,
            browser,
            billing,
            search,
            from_bob,
            from_alice,
            grant,
        }
    }

    async fn page(&self, path: &str) -> Page {
        self.browser.get(&self.world, path).await
    }

    /// The conversations dibs started that alice may read.
    async fn notices(&self) -> Vec<u64> {
        let listed = self.world.get(&self.alice, "/v1/threads").await;
        listed.body["threads"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|summary| summary["thread"]["system"] == true)
            .map(|summary| summary["thread"]["id"].as_u64().unwrap())
            .collect()
    }
}

/// The markup of the inspector.
fn inspector(page: &Page) -> &str {
    let start = page
        .text
        .find(r#"data-region="inspector""#)
        .unwrap_or_else(|| panic!("no inspector in {}", page.text));
    let end = page.text[start..].find("</aside>").unwrap() + start;
    &page.text[start..end]
}

/// The markup of the list.
fn list(page: &Page) -> &str {
    let start = page.text.find(r#"data-region="list""#).unwrap();
    let end = page.text[start..]
        .find(r#"data-region="inspector""#)
        .unwrap()
        + start;
    &page.text[start..end]
}

/// Whether the list holds a row that opens `href`.
fn lists(page: &Page, href: &str) -> bool {
    list(page).contains(&format!(r#"data-inspect href="{href}""#))
}

#[tokio::test]
async fn people_lists_a_members_conversation_marked_unread_and_no_notice_from_dibs() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/inbox").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(r#"<h1 tabindex="-1">Inbox</h1>"#),
        "{}",
        page.text
    );
    let shown = list(&page);
    assert!(
        shown.contains(&format!(
            r#"<div class="row" data-row><span class="s s--review" aria-hidden="true"></span>
<div><a class="row__t" data-inspect href="/ui/inbox/{}">Refund rounding</a>"#,
            seeded.from_bob
        )),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span>to <a class="lnk" href="/ui/claims?who=alice">alice</a></span>"#),
        "{shown}"
    );
    assert!(
        shown.contains(&format!(
            r#"<a href="/ui/claims/{}" title="gh:acme/widgets#40">widgets#40</a>"#,
            seeded.billing
        )),
        "{shown}"
    );
    assert!(shown.contains("<span>1 message</span>"), "{shown}");
    assert!(lists(&page, &format!("/ui/inbox/{}", seeded.from_alice)));
    assert!(!lists(&page, &format!("/ui/inbox/{}", seeded.grant)));
    for notice in seeded.notices().await {
        assert!(!lists(&page, &format!("/ui/inbox/{notice}")), "{notice}");
    }
    assert!(
        page.text
            .contains(r#"data-region="inspector" data-selected="false""#),
        "{}",
        page.text
    );
    assert!(page.text.contains(
        "Pick a conversation. Messages from people come first; what dibs reports is folded onto each claim."
    ));
}

#[tokio::test]
async fn the_tabs_count_unread_people_waiting_grants_and_unread_notices() {
    let seeded = Seeded::new().await;
    assert_eq!(seeded.notices().await.len(), 2, "two overlaps reach alice");
    let page = seeded.page("/ui/inbox").await;
    for tab in [
        r#"<a href="/ui/inbox" aria-current="true">People<span class="k">2</span></a>"#,
        r#"<a href="/ui/inbox?tab=grants">Grants<span class="k">1</span></a>"#,
        r#"<a href="/ui/inbox?tab=dibs">From dibs<span class="k">2</span></a>"#,
        r#"<a href="/ui/inbox" aria-current="page">Inbox<span class="k k--hot">2</span>"#,
    ] {
        assert!(page.text.contains(tab), "{tab} in {}", page.text);
    }
}

#[tokio::test]
async fn opening_a_conversation_takes_it_off_the_rails_count_on_that_very_answer() {
    let seeded = Seeded::new().await;
    let page = seeded.page(&format!("/ui/inbox/{}", seeded.from_bob)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(
            r#"<a href="/ui/inbox" aria-current="page">Inbox<span class="k k--hot">1</span>"#
        ),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(
            r#"<a href="/ui/inbox" aria-current="true">People<span class="k">1</span></a>"#
        ),
        "{}",
        page.text
    );
    let unread = seeded
        .world
        .get(&seeded.alice, "/v1/inbox?unread=true")
        .await;
    assert!(
        unread.body["threads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|summary| summary["thread"]["id"] != seeded.from_bob),
        "{}",
        unread.body
    );
}

#[tokio::test]
async fn grants_lists_a_waiting_request_with_both_tickets_and_opens_its_conversation() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/inbox?tab=grants").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = list(&page);
    let href = format!("/ui/inbox/{}?tab=grants", seeded.grant);
    assert!(lists(&page, &href), "{shown}");
    assert!(
        shown.contains(r#"<b>bob</b></a><span>asks for <a class="area" href="/ui/areas/api:billing" title="api:billing">api:billing</a> for "#),
        "{shown}"
    );
    assert!(
        shown.contains(&format!(
            r#"<a href="/ui/claims/{}" title="gh:acme/widgets#41">widgets#41</a>"#,
            seeded.search
        )),
        "{shown}"
    );
    assert!(
        shown.contains(&format!(
            r#" · your claim <span class="ref"><a href="/ui/claims/{}" title="gh:acme/widgets#40">widgets#40</a>"#,
            seeded.billing
        )),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<input type="hidden" name="tab" value="grants">"#),
        "{shown}"
    );
    let opened = seeded.page(&href).await;
    assert_eq!(opened.status, StatusCode::OK, "{}", opened.text);
    assert!(
        inspector(&opened).contains(&format!(r#"data-thread-id="{}""#, seeded.grant)),
        "{}",
        inspector(&opened)
    );
    let pasted = seeded.page(&format!("/ui/inbox/{}", seeded.grant)).await;
    assert!(
        pasted
            .text
            .contains(r#"<a href="/ui/inbox?tab=grants" aria-current="true">Grants"#),
        "a conversation opened without a tab opens on its own: {}",
        pasted.text
    );
    assert!(
        inspector(&pasted).contains(
            r#"<a class="x" data-inspect href="/ui/inbox?tab=grants" aria-label="Close">"#
        ),
        "{}",
        inspector(&pasted)
    );
}

#[tokio::test]
async fn from_dibs_folds_the_notices_about_one_claimed_item_into_one_row() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/inbox?tab=dibs").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = list(&page);
    let title = format!(
        r#"<a class="row__t" href="/ui/claims/{}">Billing rewrite</a>"#,
        seeded.billing
    );
    assert_eq!(shown.matches(&title).count(), 1, "{shown}");
    assert!(
        shown.contains(r#"<span class="s s--working" aria-hidden="true"></span>"#),
        "{shown}"
    );
    assert!(
        shown.contains("<span>2 notices · 2 unread</span>"),
        "{shown}"
    );
    let notices = seeded.notices().await;
    assert_eq!(
        notices
            .iter()
            .filter(|notice| lists(&page, &format!("/ui/inbox/{notice}?tab=dibs")))
            .count(),
        1,
        "the newest notice opens from the row: {shown}"
    );
    assert!(
        shown.contains(r#"<p class="note">A notice from dibs also shows on its claim.</p>"#),
        "{shown}"
    );
    assert!(!lists(
        &page,
        &format!("/ui/inbox/{}?tab=dibs", seeded.from_bob)
    ));
}

#[tokio::test]
async fn the_text_narrows_the_list_by_a_subject_word_and_by_a_tickets_number() {
    let seeded = Seeded::new().await;
    for query in ["deploy", "%2340"] {
        let page = seeded.page(&format!("/ui/inbox?q={query}")).await;
        let shown: Vec<u64> = [seeded.from_bob, seeded.from_alice]
            .into_iter()
            .filter(|id| lists(&page, &format!("/ui/inbox/{id}?q={query}")))
            .collect();
        let expected = if query == "deploy" {
            seeded.from_alice
        } else {
            seeded.from_bob
        };
        assert_eq!(shown, [expected], "{query}: {}", page.text);
    }
    let none = seeded.page("/ui/inbox?q=nothing+like+it").await;
    assert!(
        none.text.contains(r#"<p class="note">Nothing here.</p>"#),
        "{}",
        none.text
    );
}

#[tokio::test]
async fn the_inspector_links_a_message_carries_what_its_live_view_needs_and_marks_it_read() {
    let seeded = Seeded::new().await;
    let thread = seeded
        .world
        .thread(
            &seeded.bob,
            json!(["alice"]),
            "Totals",
            "see gh:acme/widgets#42 <script>",
        )
        .await;
    let page = seeded.page(&format!("/ui/inbox/{thread}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = inspector(&page);
    assert!(
        shown.contains(r#"data-region="inspector" data-selected="true" data-live="own" aria-label="Conversation""#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<p class="msg__body">see <a class="lnk" href="https://github.com/acme/widgets/issues/42" target="_blank" rel="noopener noreferrer">gh:acme/widgets#42</a> &#60;script&#62;</p>"#),
        "{shown}"
    );
    assert!(!page.text.contains("<script>"), "{}", page.text);
    let after = page
        .text
        .split(r#"data-after=""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();
    for attribute in [
        r#"data-thread-view"#.to_owned(),
        format!(r#"data-thread-id="{thread}""#),
        r#"data-me="alice""#.to_owned(),
        r#"data-stream-url="/ui/events/stream""#.to_owned(),
        r#"data-session-url="/ui/session""#.to_owned(),
        format!(r#"data-after="{after}""#),
        r#"data-new-message-label="1 new message""#.to_owned(),
        r#"data-new-messages-label="{count} new messages""#.to_owned(),
        format!(r#"data-ack-action="/ui/threads/{thread}/messages/{{message}}/ack""#),
        r#"data-sender="bob""#.to_owned(),
        r#"data-system-author="from dibs""#.to_owned(),
    ] {
        assert!(shown.contains(&attribute), "{attribute} in {shown}");
    }
    for part in [
        "data-messages",
        "<template data-message-template>",
        "<template data-ack-done-template>",
        "data-live-pill hidden",
        "data-live-pill-text",
        "data-conn",
        r#"data-field="author""#,
        r#"data-field="session""#,
        r#"data-field="body""#,
        r#"data-field="time""#,
        r#"data-field="name""#,
        r#"data-field="you""#,
        r#"data-importance="high""#,
        r#"data-importance="urgent""#,
        "data-ack hidden",
        "data-ack-list",
        "data-ack-form",
    ] {
        assert!(shown.contains(part), "{part} in {shown}");
    }
    assert!(
        shown.contains(&format!(
            r#"<form class="reply" method="post" action="/ui/threads/{thread}/messages" aria-labelledby="reply-h">
  <h3 id="reply-h" class="label">Reply</h3>
  <input type="hidden" name="csrf" value="{}">"#,
            seeded.browser.token
        )),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<h2 data-inspector-heading tabindex="-1">Totals</h2>"#),
        "{shown}"
    );
    assert!(
        shown.contains("<dt>From</dt><dd><a class=\"who\" href=\"/ui/claims?who=bob\">"),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span class="faint">no ticket</span>"#),
        "{shown}"
    );
    let unread = seeded
        .world
        .get(&seeded.alice, "/v1/inbox?unread=true")
        .await;
    assert!(
        unread.body["threads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|summary| summary["thread"]["id"] != thread),
        "{}",
        unread.body
    );
}

#[tokio::test]
async fn a_reply_and_an_acknowledgement_land_on_their_message_in_the_inbox() {
    let seeded = Seeded::new().await;
    let browser = &seeded.browser;
    let path = format!("/ui/threads/{}/messages", seeded.from_bob);
    let reply = browser
        .post(
            &seeded.world,
            &path,
            &[("csrf", &browser.token), ("body", "Half up")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text);
    let read = seeded
        .world
        .get(&seeded.alice, &format!("/v1/threads/{}", seeded.from_bob))
        .await;
    let posted = read.body["messages"].as_array().unwrap().last().unwrap()["id"]
        .as_u64()
        .unwrap();
    assert_eq!(
        reply.location(),
        format!("/ui/inbox/{}#m-{posted}", seeded.from_bob)
    );
    let page = seeded.page(&format!("/ui/inbox/{}", seeded.from_bob)).await;
    assert!(
        inspector(&page).contains(&format!(r#"id="m-{posted}""#)),
        "{}",
        inspector(&page)
    );

    let blank = browser
        .post(
            &seeded.world,
            &path,
            &[
                ("csrf", &browser.token),
                ("body", "  \n "),
                ("require_ack", "1"),
            ],
        )
        .await;
    assert_eq!(
        blank.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        blank.text
    );
    assert!(
        blank.text.contains(r#"<h1 tabindex="-1">Inbox</h1>"#),
        "{}",
        blank.text
    );
    assert!(
        blank.text.contains(r#"data-changed-kinds="""#),
        "the browser's address is the reply's, so no change refreshes the page: {}",
        blank.text
    );
    let shown = inspector(&blank);
    assert!(
        shown.contains(&format!(r#"data-thread-id="{}""#, seeded.from_bob)),
        "{shown}"
    );
    assert!(shown.contains("Write a message before sending."), "{shown}");
    assert!(shown.contains(r#"aria-invalid="true""#), "{shown}");
    assert!(
        shown.contains(r#"name="require_ack" value="1" checked"#),
        "{shown}"
    );
    assert!(
        blank.text.contains(&format!(
            r#"href="/ui/inbox/{}">Reload</a>"#,
            seeded.from_bob
        )),
        "the page's own address is the conversation's: {}",
        blank.text
    );

    let started = seeded
        .world
        .post(
            &seeded.bob,
            "/v1/threads",
            json!({"subject": "Freeze", "to": ["alice"], "label": "desk/mcp",
                   "body": "No merges today.", "ack_required": true}),
        )
        .await;
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let message = started.body["message"]["id"].as_u64().unwrap();
    let ack = format!("/ui/threads/{thread}/messages/{message}/ack");
    let page = seeded.page(&format!("/ui/inbox/{thread}")).await;
    assert!(
        inspector(&page).contains(&format!(r#"action="{ack}" data-ack-form>"#)),
        "{}",
        inspector(&page)
    );
    let acked = browser
        .post(&seeded.world, &ack, &[("csrf", &browser.token)])
        .await;
    assert_eq!(acked.status, StatusCode::SEE_OTHER, "{}", acked.text);
    assert_eq!(acked.location(), format!("/ui/inbox/{thread}#m-{message}"));
}

#[tokio::test]
async fn a_conversation_between_two_other_members_answers_404_with_the_whole_page() {
    let seeded = Seeded::new().await;
    let theirs = seeded
        .world
        .thread(&seeded.carol, json!(["bob"]), "Between us", "hello")
        .await;
    for path in [format!("/ui/inbox/{theirs}"), "/ui/inbox/999999".to_owned()] {
        let page = seeded.page(&path).await;
        assert_eq!(page.status, StatusCode::NOT_FOUND, "{path}: {}", page.text);
        assert!(page.text.contains(r#"<aside id="rail""#), "{path}");
        let shown = inspector(&page);
        assert!(
            shown.contains("That conversation is not one you can read."),
            "{path}: {shown}"
        );
        assert!(!page.text.contains("Between us"), "{path}");
        assert!(
            lists(&page, &format!("/ui/inbox/{}", seeded.from_bob)),
            "{path}"
        );
    }
}

#[tokio::test]
async fn an_inbox_page_holds_one_session_ended_notice_with_a_conversation_open_or_not() {
    let seeded = Seeded::new().await;
    for path in [
        "/ui/inbox".to_owned(),
        format!("/ui/inbox/{}", seeded.from_bob),
    ] {
        let page = seeded.page(&path).await;
        assert_eq!(page.status, StatusCode::OK, "{path}: {}", page.text);
        assert_eq!(
            page.text.matches("data-session-ended").count(),
            1,
            "{path}: {}",
            page.text
        );
        assert!(
            page.text.contains(
                r#"<div class="banner banner--danger" role="alert" data-session-ended hidden>"#
            ),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_message_from_dibs_reads_from_dibs_and_an_ended_announcement_offers_no_ack() {
    let (world, alice, bob) = team().await;
    let alice_session = world.session(&alice, "laptop").await;
    let bob_session = world.session(&bob, "desk").await;
    let held = world.item(&alice, Some("gh:acme/api#1"), "Invoices").await;
    claim(&world, &alice, alice_session, held, json!(["api:billing"])).await;
    let wide = world
        .item(&bob, Some("gh:acme/api#2"), "Upgrade the HTTP client")
        .await;
    let made = world
        .post(
            &bob,
            &format!("/v1/items/{wide}/claim"),
            json!({"session_id": bob_session,
                   "wide": {"areas": ["api:billing"], "reason": "One client everywhere"}}),
        )
        .await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let thread = made.body["announced"][0]["thread_id"].as_u64().unwrap();
    let message = made.body["announced"][0]["message_id"].as_u64().unwrap();
    let action = format!(r#"action="/ui/threads/{thread}/messages/{message}/ack""#);
    let address = format!("/ui/inbox/{thread}");

    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, &address).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = inspector(&page);
    assert!(
        shown.contains(&format!(
            r#"<li class="msg msg--system msg--high" id="m-{message}"><span class="av" aria-hidden="true"></span><div class="msg__in">
  <div class="msg__head"><b>from dibs</b><span class="tag tag--warn">high</span><time"#
        )),
        "from dibs, with no session: {shown}"
    );
    assert!(shown.contains(r#"data-sender="bob""#), "{shown}");
    assert!(shown.contains(r#"data-system-author="from dibs""#));
    assert!(shown.contains(&action), "the holder may acknowledge it");

    let released = world
        .post(&bob, &format!("/v1/items/{wide}/release"), json!({}))
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.body);
    let after = browser.get(&world, &address).await;
    let shown = inspector(&after);
    assert!(
        !shown.contains(&action),
        "the wide claim ended, so its announcement asks for nothing: {shown}"
    );
    assert!(shown.contains("msg--system"));

    let starter = sign_in(&world, &bob).await;
    let his = starter.get(&world, &address).await;
    let shown = inspector(&his);
    assert!(
        !shown.contains(&action),
        "dibs wrote it for bob, who is not asked"
    );
    assert!(
        !shown.contains("msg--mine"),
        "what dibs wrote is no member's own"
    );
}

/// The row of the list that opens `href`.
fn row<'a>(page: &'a Page, href: &str) -> &'a str {
    list(page)
        .split(r#"<div class="row" data-row"#)
        .find(|row| row.contains(&format!(r#"data-inspect href="{href}""#)))
        .unwrap_or_else(|| panic!("no row opening {href} in {}", list(page)))
}

#[tokio::test]
async fn twelve_waiting_grant_requests_are_all_grants_and_none_counts_as_a_message_from_people() {
    let seeded = Seeded::new().await;
    let world = &seeded.world;
    let bob_laptop = world.session(&seeded.bob, "bob-laptop").await;
    let mut asked = vec![seeded.grant];
    for n in 50..61 {
        world.fakes.clock.advance(time::Duration::minutes(7));
        let item = world
            .item(&seeded.alice, Some(&format!("gh:acme/widgets#{n}")), "More")
            .await;
        claim(world, &seeded.bob, bob_laptop, item, json!([])).await;
        let request = world
            .post(
                &seeded.bob,
                "/v1/grant-requests",
                json!({"item": item, "area": "api:billing"}),
            )
            .await;
        assert_eq!(request.status, StatusCode::CREATED, "{}", request.body);
        asked.push(request.body["thread_id"].as_u64().unwrap());
    }
    let agents = world.get(&seeded.alice, "/v1/inbox").await;
    assert_eq!(
        agents.body["grant_asks"].as_array().map(Vec::len),
        Some(10),
        "an agent is handed the newest ten: {}",
        agents.body
    );

    let people = seeded.page("/ui/inbox").await;
    for count in [
        r#"<a href="/ui/inbox" aria-current="true">People<span class="k">2</span></a>"#,
        r#"<a href="/ui/inbox" aria-current="page">Inbox<span class="k k--hot">2</span>"#,
    ] {
        assert!(people.text.contains(count), "{count} in {}", people.text);
    }
    for thread in &asked {
        assert!(
            !lists(&people, &format!("/ui/inbox/{thread}")),
            "request {thread} is not a message from people"
        );
    }
    let opened = seeded.page(&format!("/ui/inbox/{}", asked[0])).await;
    assert!(
        opened
            .text
            .contains(r#"<a href="/ui/inbox?tab=grants" aria-current="true">Grants"#),
        "the oldest request opens on Grants: {}",
        opened.text
    );

    let grants = seeded.page("/ui/inbox?tab=grants").await;
    assert!(
        grants
            .text
            .contains(r#"aria-current="true">Grants<span class="k">12</span></a>"#),
        "{}",
        grants.text
    );
    for thread in &asked {
        assert!(
            lists(&grants, &format!("/ui/inbox/{thread}?tab=grants")),
            "request {thread} in {}",
            list(&grants)
        );
    }
}

#[tokio::test]
async fn a_message_waiting_for_the_viewers_acknowledgement_keeps_its_mark_once_read_until_acknowledged()
 {
    let seeded = Seeded::new().await;
    let started = seeded
        .world
        .post(
            &seeded.bob,
            "/v1/threads",
            json!({"subject": "Freeze", "to": ["alice"], "label": "desk/mcp",
                   "body": "No merges today.", "ack_required": true}),
        )
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let message = started.body["message"]["id"].as_u64().unwrap();
    let href = format!("/ui/inbox/{thread}");
    let mark = r#"<span class="tag tag--warn" title="Messages waiting for your acknowledgement">1 to acknowledge</span>"#;

    let unread = seeded.page("/ui/inbox").await;
    assert!(
        row(&unread, &href).contains(mark),
        "{}",
        row(&unread, &href)
    );

    let opened = seeded.page(&href).await;
    let shown = inspector(&opened);
    assert!(
        shown.contains(
            r#"<dt>Acknowledge</dt><dd><span class="tag tag--warn">1 message waits for yours</span></dd>"#
        ),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span aria-hidden="true">○</span> alice (you) waiting"#),
        "the message itself says it waits: {shown}"
    );
    let read = seeded.page("/ui/inbox").await;
    let shown = row(&read, &href);
    assert!(
        shown.starts_with(r#"><span aria-hidden="true"></span>"#),
        "read: {shown}"
    );
    assert!(shown.contains(mark), "still waiting once read: {shown}");

    let browser = &seeded.browser;
    let acked = browser
        .post(
            &seeded.world,
            &format!("/ui/threads/{thread}/messages/{message}/ack"),
            &[("csrf", &browser.token)],
        )
        .await;
    assert_eq!(acked.status, StatusCode::SEE_OTHER, "{}", acked.text);
    let after = seeded.page("/ui/inbox").await;
    assert!(
        !row(&after, &href).contains("to acknowledge"),
        "{}",
        row(&after, &href)
    );
    assert!(!inspector(&seeded.page(&href).await).contains("<dt>Acknowledge</dt>"));

    let bobs = sign_in(&seeded.world, &seeded.bob).await;
    let written = bobs.get(&seeded.world, "/ui/inbox").await;
    assert!(
        !list(&written).contains("to acknowledge"),
        "a conversation its author wrote never waits on itself: {}",
        list(&written)
    );
}
