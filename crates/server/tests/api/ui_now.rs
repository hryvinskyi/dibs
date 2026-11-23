//! Now (`/ui/`) through the real router: the cards of what waits for a look with their links, the
//! heading and the rail counting them alike, a grant request dibs leaves to the viewer,
//! the claims by age, the viewer's day from their own midnight, and the escaping of what members
//! wrote.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;
use dibs_server::ui::ZONE_COOKIE;

use crate::support::{Person, World, checkout_of};
use crate::ui_support::{Browser, Page, claim, sign_in};

/// A project whose repository `api` is `acme/widgets` on GitHub, with the areas `billing` and
/// `search`, alice as its arbiter; a claim goes silent after nine days.
fn settings() -> Value {
    json!({
        "stale_after": "9d",
        "arbiters": ["alice"],
        "repos": [{
            "id": "api",
            "remote": "https://github.com/acme/widgets",
            "areas": [{"name": "billing", "paths": ["src/billing/**"]},
                      {"name": "search", "paths": ["src/search/**"]}]
        }]
    })
}

/// A project of alice (admin), bob and carol under [`settings`].
async fn project() -> (World, Person, Person, Person) {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let carol = world.member(&alice, "carol", Role::Member).await;
    world.settings(&alice, settings()).await;
    (world, alice, bob, carol)
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

/// The markup of the card labelled `label`.
fn card<'a>(page: &'a Page, label: &str) -> &'a str {
    let start = page
        .text
        .find(&format!(r#"">{label}</span><h2>"#))
        .unwrap_or_else(|| panic!("no {label} card in {}", page.text));
    let end = page.text[start..].find("</article>").unwrap() + start;
    &page.text[start..end]
}

/// How many cards the page shows.
fn cards(page: &Page) -> usize {
    page.text.matches(r#"<article class="need">"#).count()
}

/// The markup of the section headed `id`.
fn section<'a>(page: &'a Page, id: &str) -> &'a str {
    let start = page
        .text
        .find(&format!(r#"aria-labelledby="{id}""#))
        .unwrap_or_else(|| panic!("no section {id} in {}", page.text));
    let end = page.text[start..].find("</section>").unwrap() + start;
    &page.text[start..end]
}

/// The rail's entry for Now, counting `n`, on a screen other than Now.
fn rail_now(n: usize) -> String {
    let hot = if n > 0 { " k--hot" } else { "" };
    format!(
        r#"<a href="/ui/">Now<span class="k{hot}">{n}</span><span class="visually-hidden">waiting</span></a>"#
    )
}

#[tokio::test]
async fn with_nothing_waiting_no_card_shows_and_the_day_is_empty() {
    let (world, alice, _, _) = project().await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, "/ui/").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text
            .contains(r#"<h1 tabindex="-1">Nothing waits for a person</h1>"#),
        "{}",
        page.text
    );
    assert_eq!(cards(&page), 0, "{}", page.text);
    assert!(!page.text.contains(r#"class="needs""#));
    assert!(
        page.text.contains(r#"<a href="/ui/" aria-current="page">Now<span class="k">0</span><span class="visually-hidden">waiting</span></a>"#),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(
            r#"<time datetime="2026-03-02">2026-03-02</time> · 0 claims by 0 people · 0 tickets ready</p>"#
        ),
        "{}",
        page.text
    );
    assert!(section(&page, "now-today").contains(r#"<li class="faint">Nothing yet today.</li>"#));
    assert!(
        !page.text.contains(r#"class="insp""#),
        "Now has no inspector"
    );
}

#[tokio::test]
async fn a_grant_request_dibs_leaves_to_the_viewer_shows_as_the_decide_card() {
    let (world, alice, bob, _) = project().await;
    let laptop = world.session(&alice, "laptop").await;
    let desk = world.session(&bob, "desk").await;
    let billing = world
        .item(&alice, Some("gh:acme/widgets#1"), "Billing")
        .await;
    let search = world.item(&bob, Some("gh:acme/widgets#2"), "Search").await;
    claim(&world, &alice, laptop, billing, json!(["api:billing"])).await;
    claim(&world, &bob, desk, search, json!([])).await;
    let reviewed = world
        .post(
            &alice,
            &format!("/v1/items/{billing}/review"),
            json!({"pr": "acme/widgets#9", "more": false}),
        )
        .await;
    assert_eq!(reviewed.status, StatusCode::OK, "{}", reviewed.body);
    let first = world
        .post(
            &bob,
            "/v1/grant-requests",
            json!({"item": search, "area": "api:billing"}),
        )
        .await;
    let grant = first.body["grant_id"].as_u64().expect("dibs gave a grant");
    let revoked = world
        .call(
            Method::DELETE,
            &format!("/v1/grants/{grant}"),
            Some(&alice.token),
            None,
        )
        .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    let browser = sign_in(&world, &alice).await;
    let before = browser.get(&world, "/ui/").await;
    assert!(
        !before.text.contains(r#"">Decide</span>"#),
        "a request dibs granted is no decision: {}",
        before.text
    );

    let asked = world
        .post(
            &bob,
            "/v1/grant-requests",
            json!({"item": search, "area": "api:billing"}),
        )
        .await;
    assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.body);
    assert_eq!(asked.body["left_to_holder"], true, "{}", asked.body);
    let thread = asked.body["thread_id"].as_u64().unwrap();

    let page = browser.get(&world, "/ui/").await;
    assert_eq!(
        cards(&page),
        2,
        "the decision and the conversation of the request dibs granted: {}",
        page.text
    );
    let decide = card(&page, "Decide");
    assert!(
        decide.contains("<h2>1 grant request is left to you</h2>"),
        "{decide}"
    );
    for link in [
        r#"<a class="who" href="/ui/claims?who=bob">"#.to_owned(),
        format!(r#"<a class="lnk" href="/ui/inbox/{thread}">asks for</a>"#),
        r#"<a class="area" href="/ui/areas/api:billing" title="api:billing">api:billing</a>"#
            .to_owned(),
        format!(r#"<a href="/ui/claims/{search}" title="gh:acme/widgets#2">widgets#2</a>"#),
        r#"<a class="btn" href="/ui/inbox?tab=grants">See them</a>"#.to_owned(),
    ] {
        assert!(decide.contains(&link), "{link} in {decide}");
    }
    assert!(decide.contains("dibs leaves a request to you when a revoke of yours stands"));
    assert!(!decide.contains("your agent"), "{decide}");
    assert!(
        !card(&page, "Answer").contains(&format!(r#"href="/ui/inbox/{thread}""#)),
        "a request left to the viewer is a decision, not an unread conversation"
    );
    assert!(
        page.text
            .contains("<h1 tabindex=\"-1\">2 things wait for a look</h1>")
    );
}

#[tokio::test]
async fn a_request_dibs_decides_itself_shows_no_decide_card_however_long_it_waits() {
    let (world, alice, bob, _) = project().await;
    let laptop = world.session(&alice, "laptop").await;
    let desk = world.session(&bob, "desk").await;
    let billing = world
        .item(&alice, Some("gh:acme/widgets#1"), "Billing")
        .await;
    let search = world.item(&bob, Some("gh:acme/widgets#2"), "Search").await;
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
    assert_eq!(asked.body["left_to_holder"], false, "{}", asked.body);
    let thread = asked.body["thread_id"].as_u64().unwrap();
    let browser = sign_in(&world, &alice).await;

    for waited in [
        Duration::minutes(5),
        Duration::minutes(6),
        Duration::hours(5),
    ] {
        world.fakes.clock.advance(waited);
        let page = browser.get(&world, "/ui/").await;
        assert_eq!(cards(&page), 0, "{}", page.text);
        assert!(!page.text.contains(r#"">Decide</span>"#), "{}", page.text);
    }
    let grants = browser.get(&world, "/ui/inbox?tab=grants").await;
    assert!(
        grants
            .text
            .contains(&format!("/ui/inbox/{thread}?tab=grants")),
        "the Grants tab still lists it: {}",
        grants.text
    );
}

/// A project with one of every card but Decide, seen by alice.
struct Seeded {
    world: World,
    alice: Person,
    /// bob's conversation with alice; its subject holds markup.
    talk: u64,
    /// bob's escalation's conversation.
    escalation: u64,
    /// alice's, working on `alice/10-rates`, changing a file bob's `review` changes.
    working: u64,
    /// bob's, in review.
    review: u64,
    /// carol's, silent.
    silent: u64,
    /// bob's, held for eight days.
    held: u64,
    /// alice's, on an item its tracker closed.
    finished: u64,
    /// carol's on `#5`, on `bob/77-other`.
    branch: u64,
}

impl Seeded {
    async fn new() -> Self {
        let (world, alice, bob, carol) = project().await;
        let bot = world.member(&alice, "bot", Role::Importer).await;

        let carol_laptop = world.session(&carol, "carol-laptop").await;
        let silent = world
            .item(&alice, Some("gh:acme/widgets#13"), "Quiet cleanup")
            .await;
        claim(&world, &carol, carol_laptop, silent, json!([])).await;
        world.fakes.clock.advance(Duration::days(1));
        let bob_desk = world.session(&bob, "bob-desk").await;
        let held = world
            .item(&alice, Some("gh:acme/widgets#14"), "Long refactor")
            .await;
        claim(&world, &bob, bob_desk, held, json!(["api:search"])).await;
        world
            .fakes
            .clock
            .advance(Duration::days(8) + Duration::hours(1));

        let alice_laptop = world.session(&alice, "alice-laptop").await;
        let working = world
            .item(&alice, Some("gh:acme/widgets#10"), "Rates")
            .await;
        claim(
            &world,
            &alice,
            alice_laptop,
            working,
            json!(["api:billing"]),
        )
        .await;
        beat(&world, &alice, alice_laptop, "alice/10-rates").await;
        let bob_laptop = world.session(&bob, "bob-laptop").await;
        let review = world
            .item(&alice, Some("gh:acme/widgets#11"), "Refunds")
            .await;
        claim(&world, &bob, bob_laptop, review, json!([])).await;
        beat(&world, &bob, bob_laptop, "bob/11-refunds").await;

        let carol_desk = world.session(&carol, "carol-desk").await;
        let branch = world
            .item(&alice, Some("gh:acme/widgets#5"), "Shared checkout")
            .await;
        claim(&world, &carol, carol_desk, branch, json!([])).await;
        beat(&world, &carol, carol_desk, "bob/77-other").await;

        let escalated = world
            .post(
                &bob,
                "/v1/escalations",
                json!({"item": held, "reason": "Two agents keep undoing each other"}),
            )
            .await;
        assert_eq!(escalated.status, StatusCode::CREATED, "{}", escalated.body);
        let escalation = escalated.body["escalation"]["thread_id"].as_u64().unwrap();
        let talk = world
            .thread(
                &bob,
                json!(["alice"]),
                "Rates <script>alert(1)</script>",
                "Can we talk about rates?",
            )
            .await;

        for (item, branch) in [(working, "alice/10-rates"), (review, "bob/11-refunds")] {
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
        let reviewed = world
            .post(
                &bob,
                &format!("/v1/items/{review}/review"),
                json!({"pr": "acme/widgets#12"}),
            )
            .await;
        assert_eq!(reviewed.status, StatusCode::OK, "{}", reviewed.body);
        let imported = world
            .put(
                &bot,
                "/v1/items",
                json!({"items": [{"ref": "gh:acme/widgets#15", "title": "Closed upstream"}]}),
            )
            .await;
        assert_eq!(imported.status, StatusCode::OK, "{}", imported.body);
        let finished = imported.body["items"][0]["id"].as_u64().unwrap();
        let alice_desk = world.session(&alice, "alice-desk").await;
        claim(&world, &alice, alice_desk, finished, json!([])).await;
        let closed = world
            .put(
                &bot,
                "/v1/items",
                json!({"items": [{"ref": "gh:acme/widgets#15", "title": "Closed upstream",
                                  "closed": true}]}),
            )
            .await;
        assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
        world.fakes.clock.advance(Duration::minutes(16));

        Self {
            world,
            alice,
            talk,
            escalation,
            working,
            review,
            silent,
            held,
            finished,
            branch,
        }
    }

    async fn page(&self, path: &str) -> Page {
        sign_in(&self.world, &self.alice)
            .await
            .get(&self.world, path)
            .await
    }
}

#[tokio::test]
async fn an_escalation_dibs_raised_for_a_tie_shows_on_the_arbitrate_card_as_raised_by_dibs() {
    let (world, alice, bob, carol) = project().await;
    let bot = world.member(&alice, "bot", Role::Importer).await;
    let bob_desk = world.session(&bob, "bob-desk").await;
    let carol_desk = world.session(&carol, "carol-desk").await;
    let bobs = world.item(&alice, Some("gh:acme/widgets#1"), "Rates").await;
    let carols = world
        .item(&alice, Some("gh:acme/widgets#2"), "Refunds")
        .await;
    claim(&world, &bob, bob_desk, bobs, json!([])).await;
    claim(&world, &carol, carol_desk, carols, json!([])).await;
    for (who, session, item, branch) in [
        (&bob, bob_desk, bobs, "bob/1-rates"),
        (&carol, carol_desk, carols, "carol/2-refunds"),
    ] {
        beat(&world, who, session, branch).await;
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

    let page = sign_in(&world, &alice).await.get(&world, "/ui/").await;
    let arbitrate = card(&page, "Arbitrate");
    assert!(
        arbitrate.contains("<h2>1 disagreement waits for an arbiter</h2>"),
        "the tie went to alice: {arbitrate}"
    );
    assert!(
        arbitrate.contains(r#"<li><span class="tag">raised by dibs</span>"#),
        "{arbitrate}"
    );
    assert!(
        !arbitrate.contains(r#"href="/ui/claims?who=bob""#),
        "bob asked nothing: {arbitrate}"
    );
}

#[tokio::test]
async fn the_overlap_card_shows_which_claim_lands_first_and_why_after_the_sweep_orders_them() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/").await;
    let before = card(&page, "Overlap");
    assert!(
        before.contains(" · Tied: the order rule cannot tell them apart</span>"),
        "both claims were at work when the footprints came in: {before}"
    );
    assert!(!before.contains("lands first"), "{before}");

    let swept = seeded.world.app.tick_overlaps.execute().await.unwrap();
    assert_eq!(swept.reordered, 1);
    let page = seeded.page("/ui/").await;
    let after = card(&page, "Overlap");
    assert!(
        after.contains(" lands first, because it is in review</span>"),
        "{after}"
    );
    assert!(!after.contains("Tied"), "{after}");
    let first = format!(
        r#" · <span class="ref"><a href="/ui/claims/{}""#,
        seeded.review
    );
    assert!(after.contains(&first), "{first} in {after}");
}

#[tokio::test]
async fn each_card_shows_its_title_its_first_lines_links_and_its_foot_link() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);

    let answer = card(&page, "Answer");
    assert!(
        answer.contains("<h2>1 conversation from a person is unread</h2>"),
        "the escalation's conversation is the Arbitrate card's: {answer}"
    );
    assert!(answer.contains(r#"<a class="who" href="/ui/claims?who=bob">"#));
    assert!(
        answer.contains(&format!(
            r#"<a class="lnk" href="/ui/inbox/{}">Rates &#60;script&#62;alert(1)&#60;/script&#62;</a>"#,
            seeded.talk
        )),
        "{answer}"
    );
    assert!(answer.contains(r#"<a class="btn" href="/ui/inbox">Open the inbox</a>"#));

    let arbitrate = card(&page, "Arbitrate");
    assert!(
        arbitrate.contains("<h2>1 disagreement waits for an arbiter</h2>"),
        "{arbitrate}"
    );
    assert!(arbitrate.contains(&format!(r#"href="/ui/inbox/{}""#, seeded.escalation)));
    assert!(arbitrate.contains(r#"<a class="who" href="/ui/claims?who=bob">"#));
    assert!(
        arbitrate.contains(r#"Waiting for <a class="who" href="/ui/claims?who=alice">"#),
        "{arbitrate}"
    );

    let overlap = card(&page, "Overlap");
    assert!(
        overlap.contains("<h2>1 pair of claims touches the same code</h2>"),
        "{overlap}"
    );
    for item in [seeded.working, seeded.review] {
        assert!(
            overlap.contains(&format!(r#"<a href="/ui/claims/{item}""#)),
            "{item} in {overlap}"
        );
    }
    assert!(overlap.contains(r#"<span class="e">1 file</span>"#));

    let release = card(&page, "Release");
    assert!(
        release.contains("<h2>1 claim holds a ticket that is already finished</h2>"),
        "{release}"
    );
    assert!(release.contains(&format!(
        r#"<a href="/ui/claims/{}" title="gh:acme/widgets#15">widgets#15</a>"#,
        seeded.finished
    )));
    assert!(release.contains(r#"<span class="t">Closed upstream</span>"#));
    assert!(release.contains(r#"<a class="btn" href="/ui/claims?filter=finished">See them</a>"#));

    let take_over = card(&page, "Take over");
    assert!(
        take_over.contains("<h2>1 claim went silent 9 d ago</h2>"),
        "{take_over}"
    );
    assert!(take_over.contains(&format!(r#"<a href="/ui/claims/{}""#, seeded.silent)));
    assert!(take_over.contains(r#"<a class="btn" href="/ui/claims?filter=silent">See them</a>"#));

    let narrow = card(&page, "Narrow");
    assert!(
        narrow.contains("<h2>1 claim has been held for more than a week</h2>"),
        "{narrow}"
    );
    assert!(narrow.contains(&format!(r#"<a href="/ui/claims/{}""#, seeded.held)));
    assert!(narrow.contains(r#"<span class="e">8 d</span>"#), "{narrow}");
    assert!(
        narrow.contains(". Together they hold 1 area.</p>"),
        "{narrow}"
    );
    assert!(narrow.contains(r#"<a class="btn" href="/ui/claims?filter=held">See them</a>"#));

    let check = card(&page, "Check");
    assert!(
        check.contains("<h2>1 claim records a branch named after another ticket</h2>"),
        "{check}"
    );
    assert!(check.contains(&format!(r#"<a href="/ui/claims/{}""#, seeded.branch)));
    assert!(
        check.contains(r#"<a href="https://github.com/acme/widgets/tree/bob/77-other" title="bob/77-other" target="_blank" rel="noopener noreferrer">bob/77-other</a>"#),
        "{check}"
    );
    assert!(
        check.contains(r#"names <a class="lnk" href="https://github.com/acme/widgets/issues/77" target="_blank" rel="noopener noreferrer">#77</a>"#),
        "{check}"
    );
    assert!(check.contains(r#"<a class="btn" href="/ui/claims?filter=branch">See them</a>"#));

    let integration = card(&page, "Integration");
    assert!(
        integration.contains(
            r#"<h2>The pull request integration has been silent since <time datetime="2026-03-11T10:00:00Z" data-clock>10:00</time>"#
        ),
        "{integration}"
    );

    assert!(!page.text.contains(r#"">Decide</span>"#));
    let labels = [
        "Answer",
        "Arbitrate",
        "Overlap",
        "Release",
        "Take over",
        "Narrow",
        "Check",
        "Integration",
    ];
    let places: Vec<usize> = labels
        .iter()
        .map(|label| page.text.find(&format!(r#"">{label}</span>"#)).unwrap())
        .collect();
    assert!(places.is_sorted(), "the cards in their order: {places:?}");
}

#[tokio::test]
async fn the_heading_counts_the_cards_and_the_rails_now_entry_shows_the_same_number_everywhere() {
    let seeded = Seeded::new().await;
    let now = seeded.page("/ui/").await;
    assert_eq!(cards(&now), 8, "{}", now.text);
    assert!(
        now.text
            .contains(r#"<h1 tabindex="-1">8 things wait for a look</h1>"#),
        "{}",
        now.text
    );
    assert!(
        now.text.contains(r#"<a href="/ui/" aria-current="page">Now<span class="k k--hot">8</span><span class="visually-hidden">waiting</span></a>"#),
        "{}",
        now.text
    );
    assert!(
        now.text.contains(
            r#"<time datetime="2026-03-11">2026-03-11</time> · 6 claims by 3 people · 1 ticket ready</p>"#
        ),
        "{}",
        now.text
    );
    let claims = seeded.page("/ui/claims").await;
    assert!(claims.text.contains(&rail_now(8)), "{}", claims.text);
}

#[tokio::test]
async fn the_age_grid_places_each_claim_in_its_column_with_a_link_to_it() {
    let (world, alice, bob, _) = project().await;
    let desk = world.session(&bob, "desk").await;
    let old = world.item(&bob, Some("gh:acme/widgets#1"), "Old").await;
    claim(&world, &bob, desk, old, json!([])).await;
    world.fakes.clock.advance(Duration::days(13));
    let mid = world.item(&bob, Some("gh:acme/widgets#2"), "Mid").await;
    claim(&world, &bob, desk, mid, json!([])).await;
    world.fakes.clock.advance(Duration::days(3));
    beat(&world, &bob, desk, "main").await;
    let today = world.item(&bob, Some("gh:acme/widgets#3"), "Today").await;
    claim(&world, &bob, desk, today, json!([])).await;
    let reviewed = world
        .item(&bob, Some("gh:acme/widgets#4"), "Reviewed")
        .await;
    claim(&world, &bob, desk, reviewed, json!([])).await;
    let review = world
        .post(
            &bob,
            &format!("/v1/items/{reviewed}/review"),
            json!({"pr": "acme/widgets#40"}),
        )
        .await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);

    let page = sign_in(&world, &alice).await.get(&world, "/ui/").await;
    let grid = section(&page, "now-ages");
    assert!(
        grid.contains(r#"<span>In review</span><span>Under a day</span><span>1–7 days</span><span class="old">1–2 weeks</span><span class="old">2–3 weeks</span><span class="old">Over 3 weeks</span>"#),
        "{grid}"
    );
    let alices = grid
        .find(r#"<span><a class="who" href="/ui/claims?who=alice">"#)
        .unwrap_or_else(|| panic!("{grid}"));
    let bobs = grid
        .find(r#"<span><a class="who" href="/ui/claims?who=bob">"#)
        .unwrap_or_else(|| panic!("{grid}"));
    assert!(alices < bobs, "the rail's order, the viewer first: {grid}");
    let row = &grid[bobs..];
    let row = &row[..row.find("</div>").unwrap()];
    let cells: Vec<&str> = row.split(r#"<span class="dots"#).skip(1).collect();
    assert_eq!(cells.len(), 6, "{row}");
    for (column, item) in [(0, reviewed), (1, today), (2, mid), (4, old)] {
        assert!(
            cells[column].contains(&format!(r#"href="/ui/claims/{item}""#)),
            "{item} in column {column}: {row}"
        );
    }
    assert!(
        cells[0].contains(r#"<a class="sq sq--review" href"#),
        "{}",
        cells[0]
    );
    assert!(
        cells[2].contains(&format!(
            r#"<a class="sq" href="/ui/claims/{mid}" title="gh:acme/widgets#2 · Mid" aria-label="gh:acme/widgets#2: Mid, working, held 3 d"></a>"#
        )),
        "{}",
        cells[2]
    );
    assert!(cells[4].starts_with(" old\""), "{}", cells[4]);
    for empty in [3, 5] {
        assert!(!cells[empty].contains("<a "), "{}", cells[empty]);
    }
    assert!(grid.contains(r#"<div class="legend">"#));
}

#[tokio::test]
async fn today_lists_the_claims_taken_and_the_items_finished_since_the_day_began() {
    let (world, alice, bob, _) = project().await;
    let desk = world.session(&bob, "desk").await;
    let yesterday = world
        .item(&bob, Some("gh:acme/widgets#1"), "From yesterday")
        .await;
    claim(&world, &bob, desk, yesterday, json!([])).await;
    world.fakes.clock.advance(Duration::days(1));
    beat(&world, &bob, desk, "main").await;
    let taken = world
        .item(&bob, Some("gh:acme/widgets#2"), "Taken today")
        .await;
    claim(&world, &bob, desk, taken, json!([])).await;
    let done = world
        .item(&bob, Some("gh:acme/widgets#3"), "Done today")
        .await;
    claim(&world, &bob, desk, done, json!([])).await;
    world.fakes.clock.advance(Duration::minutes(30));
    let finished = world
        .post(&bob, &format!("/v1/items/{done}/done"), json!({}))
        .await;
    assert_eq!(finished.status, StatusCode::OK, "{}", finished.body);

    let page = sign_in(&world, &alice).await.get(&world, "/ui/").await;
    let today = section(&page, "now-today");
    let finished_at = today
        .find(r#"<time datetime="2026-03-03T09:30:00Z" data-clock>09:30</time><span><a class="who" href="/ui/claims?who=bob">"#)
        .unwrap_or_else(|| panic!("{today}"));
    assert!(
        today[finished_at..].contains(r#"</a> finished <span class="ref">"#),
        "{today}"
    );
    let claimed_at = today
        .find(&format!(
            r#"claimed <span class="ref"><a href="/ui/claims/{taken}""#
        ))
        .unwrap_or_else(|| panic!("{today}"));
    assert!(finished_at < claimed_at, "newest first: {today}");
    assert!(today.contains(r#"<span class="t">Taken today</span>"#));
    assert!(today.contains(r#"<span class="t">Done today</span>"#));
    assert!(!today.contains("From yesterday"), "{today}");
    assert!(!today.contains("Nothing yet today."));
}

#[tokio::test]
async fn today_begins_at_the_viewers_midnight_or_else_at_utcs() {
    let (world, alice, bob, _) = project().await;
    world.fakes.clock.advance(Duration::hours(2));
    let desk = world.session(&bob, "desk").await;
    let done = world
        .item(&bob, Some("gh:acme/widgets#3"), "Done item")
        .await;
    claim(&world, &bob, desk, done, json!([])).await;
    let finished = world
        .post(&bob, &format!("/v1/items/{done}/done"), json!({}))
        .await;
    assert_eq!(finished.status, StatusCode::OK, "{}", finished.body);
    world.fakes.clock.advance(Duration::minutes(1));
    let browser = sign_in(&world, &alice).await;
    let zoned = |minutes: &str| Browser {
        cookie: format!("{}; {ZONE_COOKIE}={minutes}", browser.cookie),
        token: browser.token.clone(),
    };
    let shows = |page: &Page| section(page, "now-today").contains("Done item");
    assert!(shows(&browser.get(&world, "/ui/").await), "UTC's day");
    assert!(
        shows(&zoned("780").get(&world, "/ui/").await),
        "done at 11:00 UTC, the viewer's midnight"
    );
    assert!(
        !shows(&zoned("779").get(&world, "/ui/").await),
        "done a minute before the viewer's midnight"
    );
    assert!(shows(&zoned("-300").get(&world, "/ui/").await));
    for unreadable in ["+60", "841", "abc", ""] {
        assert!(
            shows(&zoned(unreadable).get(&world, "/ui/").await),
            "{unreadable:?} falls back to UTC's day"
        );
    }
}

#[tokio::test]
async fn twelve_requests_dibs_decides_itself_show_no_decide_card_and_no_message() {
    let (world, alice, bob, _) = project().await;
    let mut generous = settings();
    generous["raised_per_hour"] = json!(100);
    world.settings(&alice, generous).await;
    let laptop = world.session(&alice, "laptop").await;
    let desk = world.session(&bob, "desk").await;
    let billing = world
        .item(&alice, Some("gh:acme/widgets#1"), "Billing")
        .await;
    claim(&world, &alice, laptop, billing, json!(["api:billing"])).await;
    let mut asked = Vec::new();
    for n in 2..14 {
        if n == 4 {
            world.fakes.clock.advance(Duration::minutes(11));
        }
        let item = world
            .item(&bob, Some(&format!("gh:acme/widgets#{n}")), "Search")
            .await;
        claim(&world, &bob, desk, item, json!([])).await;
        let request = world
            .post(
                &bob,
                "/v1/grant-requests",
                json!({"item": item, "area": "api:billing"}),
            )
            .await;
        assert_eq!(request.status, StatusCode::CREATED, "{}", request.body);
        asked.push(request.body["thread_id"].as_u64().unwrap());
    }
    let page = sign_in(&world, &alice).await.get(&world, "/ui/").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert_eq!(cards(&page), 0, "no request is a message: {}", page.text);
    assert!(!page.text.contains(r#"">Answer</span>"#), "{}", page.text);
    assert!(!page.text.contains(r#"">Decide</span>"#), "{}", page.text);
    assert_eq!(asked.len(), 12);
    let inbox = sign_in(&world, &alice).await.get(&world, "/ui/inbox").await;
    assert!(inbox.text.contains(&rail_now(0)), "{}", inbox.text);
}

#[tokio::test]
async fn a_message_waiting_for_the_viewers_acknowledgement_stays_on_the_answer_card_until_acknowledged()
 {
    let (world, alice, bob, _) = project().await;
    let started = world
        .post(
            &bob,
            "/v1/threads",
            json!({"subject": "Freeze", "to": ["alice"], "label": "desk/mcp",
                   "body": "No merges today.", "ack_required": true}),
        )
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let message = started.body["message"]["id"].as_u64().unwrap();
    let browser = sign_in(&world, &alice).await;
    let opened = browser.get(&world, &format!("/ui/inbox/{thread}")).await;
    assert_eq!(opened.status, StatusCode::OK, "{}", opened.text);

    let page = browser.get(&world, "/ui/").await;
    assert_eq!(cards(&page), 1, "{}", page.text);
    let answer = card(&page, "Answer");
    assert!(
        answer.contains("<h2>1 conversation waits for your acknowledgement</h2>"),
        "read, and still waiting: {answer}"
    );
    assert!(
        answer.contains(&format!(
            r#"<a class="lnk" href="/ui/inbox/{thread}">Freeze</a>"#
        )),
        "{answer}"
    );
    assert!(
        answer.contains(r#"<span class="tag tag--warn" title="Messages waiting for your acknowledgement">1 to acknowledge</span>"#),
        "{answer}"
    );
    let bobs = sign_in(&world, &bob).await.get(&world, "/ui/").await;
    assert_eq!(cards(&bobs), 0, "its author's never waits: {}", bobs.text);

    let acked = browser
        .post(
            &world,
            &format!("/ui/threads/{thread}/messages/{message}/ack"),
            &[("csrf", &browser.token)],
        )
        .await;
    assert_eq!(acked.status, StatusCode::SEE_OTHER, "{}", acked.text);
    let after = browser.get(&world, "/ui/").await;
    assert_eq!(cards(&after), 0, "{}", after.text);
    assert!(!after.text.contains("to acknowledge"));
}

#[tokio::test]
async fn an_unread_message_asking_for_acknowledgement_is_one_line_of_the_answer_card() {
    let (world, alice, bob, _) = project().await;
    let started = world
        .post(
            &bob,
            "/v1/threads",
            json!({"subject": "Freeze", "to": ["alice"], "label": "desk/mcp",
                   "body": "No merges today.", "ack_required": true}),
        )
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let browser = sign_in(&world, &alice).await;

    let page = browser.get(&world, "/ui/").await;
    assert_eq!(cards(&page), 1, "{}", page.text);
    let answer = card(&page, "Answer");
    assert!(
        answer.contains("<h2>1 conversation from a person is unread</h2>"),
        "{answer}"
    );
    assert_eq!(
        answer
            .matches(&format!(r#"href="/ui/inbox/{thread}""#))
            .count(),
        1,
        "one conversation, one line: {answer}"
    );
    assert!(
        !answer.contains("waits for your acknowledgement")
            && !answer.contains("wait for your acknowledgement"),
        "no acknowledgement sub-list: {answer}"
    );
    assert!(
        answer.contains(r#"<span class="tag tag--warn" title="Messages waiting for your acknowledgement">1 to acknowledge</span>"#),
        "the unread line says it waits: {answer}"
    );
    assert!(answer.contains("reading it is not enough"), "{answer}");
}
