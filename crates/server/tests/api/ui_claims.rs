//! Claims (`/ui/claims`) through the real router: the claims grouped by holder, the filters and
//! their counts, the holder and text filters, a claim in the inspector with every reference a link,
//! the take-over of a silent claim, a wide claim's areas, debt work and the areas a claim waits for,
//! an address of no open claim, and the escaping of what members wrote.

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Person, World};
use crate::ui_support::{Browser, Page, claim, sign_in, team};

/// A project whose repository `api` is `acme/widgets` on GitHub, with the areas `billing` and
/// `search`; a claim goes silent after nine days.
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

/// alice (the viewer), bob and carol with a claim of every kind, and an importer.
struct Seeded {
    world: World,
    alice: Person,
    browser: Browser,
    /// alice's, working on `alice/10-rates`; its title holds markup.
    working: u64,
    /// bob's, in review with `acme/widgets#12`, on `bob/11-refunds`.
    review: u64,
    /// carol's, silent.
    silent: u64,
    /// bob's, held for eight days.
    held: u64,
    /// alice's, on an item its tracker closed.
    finished: u64,
    /// carol's, on `bob/77-other`.
    branch: u64,
}

impl Seeded {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let carol = world.member(&alice, "carol", Role::Member).await;
        let bot = world.member(&alice, "bot", Role::Importer).await;
        world.settings(&alice, settings()).await;

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
            .item(
                &alice,
                Some("gh:acme/widgets#10"),
                "Rates <script>alert(1)</script>",
            )
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
                json!({"items": [
                    {"ref": "gh:acme/widgets#15", "title": "Closed upstream"},
                    {"ref": "gh:acme/widgets#20", "title": "Touches billing",
                     "areas_hint": ["api:billing"]}
                ]}),
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

        let carol_desk = world.session(&carol, "carol-desk").await;
        let branch = world
            .item(&alice, Some("gh:acme/widgets#16"), "Shared checkout")
            .await;
        claim(&world, &carol, carol_desk, branch, json!([])).await;
        beat(&world, &carol, carol_desk, "bob/77-other").await;

        let browser = sign_in(&world, &alice).await;
        Self {
            world,
            alice,
            browser,
            working,
            review,
            silent,
            held,
            finished,
            branch,
        }
    }

    async fn page(&self, path: &str) -> Page {
        self.browser.get(&self.world, path).await
    }

    fn all(&self) -> [u64; 6] {
        [
            self.working,
            self.review,
            self.silent,
            self.held,
            self.finished,
            self.branch,
        ]
    }
}

/// A heartbeat of `session` from a checkout on `branch` of `api`.
async fn beat(world: &World, who: &Person, session: u64, branch: &str) {
    let beat = world
        .post(
            who,
            "/v1/heartbeat",
            json!({"session_id": session, "repo": "api", "branch": branch}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
}

/// The ids of the claims the list shows, by the address their titles link to.
fn rows(page: &Page, all: &[u64]) -> Vec<u64> {
    all.iter()
        .copied()
        .filter(|id| {
            page.text.contains(&format!(
                r#"<a class="row__t" data-inspect href="/ui/claims/{id}"#
            ))
        })
        .collect()
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

/// Asserts that `text` occurs in `markup` and only inside `<a` elements.
fn only_in_links(markup: &str, text: &str) {
    let mut found = 0;
    for (at, _) in markup.match_indices(text) {
        found += 1;
        let before = &markup[..at];
        let opened = before.rfind("<a ").or_else(|| before.rfind("<a>"));
        let closed = before.rfind("</a>");
        assert!(
            opened.is_some_and(|opened| closed.is_none_or(|closed| opened > closed)),
            "{text} outside a link at {at}: {}",
            &markup[at.saturating_sub(200)..(at + 100).min(markup.len())]
        );
    }
    assert!(found > 0, "{text} is not in {markup}");
}

#[tokio::test]
async fn the_claims_are_grouped_by_holder_the_viewer_first_and_their_tickets_lead_to_the_tracker() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/claims").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(r#"<h1 tabindex="-1">Claims</h1>"#),
        "{}",
        page.text
    );
    let head = |handle: &str| {
        page.text
            .find(&format!(
                r#"<div class="grp"><a class="who" href="/ui/claims?who={handle}">"#
            ))
            .unwrap_or_else(|| panic!("no group of {handle} in {}", page.text))
    };
    assert!(head("alice") < head("bob"), "{}", page.text);
    assert!(head("bob") < head("carol"), "{}", page.text);
    assert_eq!(rows(&page, &seeded.all()), seeded.all());
    let mine = &page.text[head("alice")..head("bob")];
    assert!(mine.contains("2 claims · 1 area held"), "{mine}");
    for number in [10, 11, 13, 14, 15, 16] {
        assert!(
            page.text.contains(&format!(
                r#"<a href="https://github.com/acme/widgets/issues/{number}" title="gh:acme/widgets#{number}" target="_blank" rel="noopener noreferrer">widgets#{number}</a>"#
            )),
            "#{number}: {}",
            page.text
        );
    }
    let bobs = &page.text[head("bob")..head("carol")];
    let held = bobs.find(&format!("/ui/claims/{}\"", seeded.held)).unwrap();
    let review = bobs
        .find(&format!("/ui/claims/{}\"", seeded.review))
        .unwrap();
    assert!(held < review, "the longest held first: {bobs}");
    assert!(
        page.text
            .contains(r#"data-region="inspector" data-selected="false""#),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(
            "Pick a claim to see its ticket, pull request, branch, areas and conversations."
        ),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains("Rates &#60;script&#62;alert(1)&#60;/script&#62;"),
        "{}",
        page.text
    );
    assert!(!page.text.contains("<script>alert"), "{}", page.text);
}

#[tokio::test]
async fn each_filter_shows_exactly_its_claims_and_counts_them() {
    let seeded = Seeded::new().await;
    let filters = [
        ("all", "All", seeded.all().to_vec()),
        (
            "working",
            "Working",
            vec![seeded.working, seeded.held, seeded.finished, seeded.branch],
        ),
        ("review", "In review", vec![seeded.review]),
        ("silent", "Silent", vec![seeded.silent]),
        ("held", "Held over a week", vec![seeded.held]),
        ("finished", "Ticket finished", vec![seeded.finished]),
        ("branch", "Other ticket's branch", vec![seeded.branch]),
    ];
    let everything = seeded.page("/ui/claims").await;
    for (code, label, expected) in &filters {
        let href = if *code == "all" {
            "/ui/claims".to_owned()
        } else {
            format!("/ui/claims?filter={code}")
        };
        assert!(
            everything.text.contains(&format!(
                r#"href="{href}"{}>{label}<span class="k">{}</span></a>"#,
                if *code == "all" {
                    r#" aria-current="true""#
                } else {
                    ""
                },
                expected.len()
            )),
            "{code}: {}",
            everything.text
        );
        let page = seeded.page(&href).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.text);
        assert!(
            page.text
                .contains(&format!(r#"<a href="{href}" aria-current="true">{label}"#)),
            "{code}: {}",
            page.text
        );
        let shown: Vec<u64> = seeded
            .all()
            .into_iter()
            .filter(|id| {
                page.text.contains(&format!(r#"data-inspect href="{}""#, {
                    if *code == "all" {
                        format!("/ui/claims/{id}")
                    } else {
                        format!("/ui/claims/{id}?filter={code}")
                    }
                }))
            })
            .collect();
        assert_eq!(&shown, expected, "{code}: {}", page.text);
    }
    let unknown = seeded.page("/ui/claims?filter=nonsense").await;
    assert_eq!(rows(&unknown, &seeded.all()), seeded.all());
}

#[tokio::test]
async fn who_narrows_to_one_holder_and_the_text_finds_a_claim_by_its_title_or_pull_request() {
    let seeded = Seeded::new().await;
    let bobs = seeded.page("/ui/claims?who=bob").await;
    assert_eq!(bobs.status, StatusCode::OK, "{}", bobs.text);
    let shown: Vec<u64> = seeded
        .all()
        .into_iter()
        .filter(|id| {
            bobs.text
                .contains(&format!(r#"data-inspect href="/ui/claims/{id}?who=bob""#))
        })
        .collect();
    assert_eq!(shown, [seeded.review, seeded.held]);
    assert!(
        bobs.text.contains(r#"<span class="faint">· bob</span>"#),
        "{}",
        bobs.text
    );
    assert!(
        bobs.text
            .contains(r#"<a class="mini" href="/ui/claims">everyone</a>"#),
        "{}",
        bobs.text
    );
    assert!(
        bobs.text
            .contains(r#"<input type="hidden" name="who" value="bob">"#),
        "{}",
        bobs.text
    );
    for query in ["refunds", "%2312"] {
        let found = seeded.page(&format!("/ui/claims?q={query}")).await;
        let shown: Vec<u64> = seeded
            .all()
            .into_iter()
            .filter(|id| found.text.contains(&format!("/ui/claims/{id}?q={query}\"")))
            .collect();
        assert_eq!(shown, [seeded.review], "{query}: {}", found.text);
    }
    let none = seeded.page("/ui/claims?q=nothing+like+it").await;
    assert!(
        none.text
            .contains(r#"<p class="note">No claim matches.</p>"#),
        "{}",
        none.text
    );
}

#[tokio::test]
async fn a_claim_in_review_links_its_pull_request_branch_and_the_files_it_shares() {
    let seeded = Seeded::new().await;
    let page = seeded.page(&format!("/ui/claims/{}", seeded.review)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(&format!(
            r#"<div class="row" data-row aria-current="true"><span class="s s--review" aria-hidden="true"></span>
<div><a class="row__t" data-inspect href="/ui/claims/{}">"#,
            seeded.review
        )),
        "{}",
        page.text
    );
    let shown = inspector(&page);
    assert!(
        shown.contains(r#"data-selected="true" aria-label="Claim gh:acme/widgets#11""#),
        "{shown}"
    );
    assert!(shown.contains("In review"), "{shown}");
    assert!(
        shown.contains(&format!(
            r#"<a href="/ui/claims/{}" title="acme/widgets#12">widgets#12</a><a class="ext" href="https://github.com/acme/widgets/pull/12" target="_blank" rel="noopener noreferrer" title="acme/widgets#12 on GitHub" aria-label="acme/widgets#12 on GitHub">↗</a>"#,
            seeded.review
        )),
        "{shown}"
    );
    assert!(
        shown.contains(r#"href="https://github.com/acme/widgets/tree/bob/11-refunds""#),
        "{shown}"
    );
    assert!(shown.contains("Changes the same files as"), "{shown}");
    assert!(
        shown.contains(
            r#"href="https://github.com/acme/widgets/blob/bob/11-refunds/src/billing/a.rs""#
        ),
        "{shown}"
    );
    for name in [
        "widgets#12",
        "acme/widgets#12",
        "bob/11-refunds",
        "src/billing/a.rs",
    ] {
        only_in_links(&page.text, name);
    }
    assert!(
        shown.contains(r#"<button class="mini" type="button" data-copy-link hidden>Copy link</button><a class="x" data-inspect href="/ui/claims" aria-label="Close">×</a>"#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<h2 data-inspector-heading tabindex="-1">Refunds</h2>"#),
        "{shown}"
    );

    let working = seeded.page(&format!("/ui/claims/{}", seeded.working)).await;
    let shown = inspector(&working);
    assert!(
        shown.contains(
            r#"<a class="lnk" href="/ui/queue?q=api%3Abilling">1 ticket will touch it</a>"#
        ),
        "{shown}"
    );
    assert!(
        shown.contains(
            r#"href="https://github.com/acme/widgets/blob/alice/10-rates/src/billing/a.rs""#
        ),
        "{shown}"
    );
}

#[tokio::test]
async fn a_silent_claim_offers_its_take_over_which_returns_to_the_claim() {
    let seeded = Seeded::new().await;
    let address = format!("/ui/claims/{}?filter=silent", seeded.silent);
    let page = seeded.page(&address).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = inspector(&page);
    assert!(shown.contains("No sign of life for 9 d."), "{shown}");
    assert!(
        shown.contains("A silent claim can be taken over with a reason, which carol is sent."),
        "{shown}"
    );
    assert!(
        shown.contains(&format!(
            r#"<form class="reply" method="post" action="/ui/items/{}/takeover"><input type="hidden" name="csrf" value="{}"><input type="hidden" name="back" value="{address}">"#,
            seeded.silent, seeded.browser.token
        )),
        "{shown}"
    );
    let taken = seeded
        .browser
        .post(
            &seeded.world,
            &format!("/ui/items/{}/takeover", seeded.silent),
            &[
                ("csrf", &seeded.browser.token),
                ("reason", "carol is away this week"),
                ("back", &address),
            ],
        )
        .await;
    assert_eq!(taken.status, StatusCode::SEE_OTHER, "{}", taken.text);
    assert_eq!(taken.location(), address);
    let after = seeded.page(&format!("/ui/claims/{}", seeded.silent)).await;
    let shown = inspector(&after);
    assert!(
        shown.contains(r#"<span class="faint">you</span>"#),
        "{shown}"
    );
    assert!(!shown.contains("No sign of life"), "{shown}");
    let status = seeded.world.get(&seeded.alice, "/v1/status").await;
    let holders: Vec<&Value> = status.body["live"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|held| held["item"]["id"] == seeded.silent)
        .collect();
    assert_eq!(holders.len(), 1, "{}", status.body);
}

#[tokio::test]
async fn a_finished_ticket_and_another_tickets_branch_are_called_out() {
    let seeded = Seeded::new().await;
    let finished = seeded
        .page(&format!("/ui/claims/{}", seeded.finished))
        .await;
    let shown = inspector(&finished);
    assert!(
        shown.contains("<b>The ticket is finished.</b> The claim still holds 0 areas."),
        "{shown}"
    );
    let other = seeded.page(&format!("/ui/claims/{}", seeded.branch)).await;
    let shown = inspector(&other);
    assert!(
        shown.contains(r#"This branch is named after ticket <a class="lnk" href="https://github.com/acme/widgets/issues/77" target="_blank" rel="noopener noreferrer">#77</a>."#),
        "{shown}"
    );
    only_in_links(shown, "#77");
    let held = seeded.page(&format!("/ui/claims/{}", seeded.held)).await;
    let shown = inspector(&held);
    assert!(
        shown.contains("<b>Held for 8 days.</b> Its 1 area stays taken while it waits."),
        "{shown}"
    );
}

#[tokio::test]
async fn the_viewers_own_silent_claim_is_not_offered_for_take_over() {
    let (world, alice, _) = team().await;
    let session = world.session(&alice, "laptop").await;
    let item = world.item(&alice, Some("gh:acme/api#3"), "Mine").await;
    claim(&world, &alice, session, item, json!([])).await;
    world.fakes.clock.advance(Duration::hours(2));
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, &format!("/ui/claims/{item}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = inspector(&page);
    assert!(
        shown.contains("<b>No sign of life for 2 h.</b></p>"),
        "{shown}"
    );
    assert!(!shown.contains("/takeover"), "{shown}");
    assert!(!shown.contains("can be taken over"), "{shown}");
}

#[tokio::test]
async fn a_wide_claim_shows_its_reason_and_where_each_of_its_areas_stands() {
    let (world, alice, bob) = team().await;
    let alice_session = world.session(&alice, "laptop").await;
    let bob_session = world.session(&bob, "desk").await;
    let alices = world.item(&alice, None, "Invoices").await;
    let bobs = world.item(&bob, Some("gh:acme/api#9"), "Upgrade").await;
    claim(
        &world,
        &alice,
        alice_session,
        alices,
        json!(["api:billing"]),
    )
    .await;
    let made = world
        .post(
            &bob,
            &format!("/v1/items/{bobs}/claim"),
            json!({"session_id": bob_session,
                   "wide": {"areas": ["api:billing", "api:search"],
                            "reason": "Swap <b>the</b> HTTP client"}}),
        )
        .await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let browser = sign_in(&world, &alice).await;
    let address = format!("/ui/claims/{bobs}");
    let page = browser.get(&world, &address).await;
    let shown = inspector(&page);
    assert!(
        shown.contains("<dt>Wide</dt><dd><span>Swap &#60;b&#62;the&#60;/b&#62; HTTP client</span>"),
        "{shown}"
    );
    assert!(!shown.contains("<b>the</b>"), "{shown}");
    assert!(
        shown.contains(r#"<span class="r">waits for <a class="who" href="/ui/claims?who=alice">"#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<a class="area" href="/ui/areas/api:search" title="api:search">api:search</a><span class="r">free</span>"#),
        "{shown}"
    );
    let message = made.body["announced"][0]["message_id"].as_u64().unwrap();
    let acked = world
        .post(&alice, &format!("/v1/messages/{message}/ack"), json!({}))
        .await;
    assert!(acked.status.is_success(), "{}", acked.body);
    let after = browser.get(&world, &address).await;
    let shown = inspector(&after);
    assert!(shown.contains("<b>alice</b></a> agreed"), "{shown}");
    assert!(!shown.contains("waits for"), "{shown}");
}

#[tokio::test]
async fn an_address_of_no_open_claim_answers_404_with_the_whole_page() {
    let (world, alice, _) = team().await;
    let session = world.session(&alice, "laptop").await;
    let item = world.item(&alice, Some("gh:acme/api#5"), "Let go").await;
    claim(&world, &alice, session, item, json!([])).await;
    let released = world
        .post(&alice, &format!("/v1/items/{item}/release"), json!({}))
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.body);
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, &format!("/ui/claims/{item}")).await;
    assert_eq!(page.status, StatusCode::NOT_FOUND, "{}", page.text);
    assert!(page.text.contains(r#"<aside id="rail""#), "{}", page.text);
    let shown = inspector(&page);
    assert!(shown.contains(r#"data-selected="true""#), "{shown}");
    assert!(
        shown.contains("That claim is not open any more."),
        "{shown}"
    );
    assert!(shown.contains(">api#5</a>"), "{shown}");
    assert!(
        shown.contains(&format!(
            r#"<a class="lnk" href="/ui/queue/{item}">See the queue</a>"#
        )),
        "{shown}"
    );
    for path in ["/ui/claims/999", "/ui/claims/abc", "/ui/claims/0"] {
        let page = browser.get(&world, path).await;
        assert_eq!(page.status, StatusCode::NOT_FOUND, "{path}: {}", page.text);
        let shown = inspector(&page);
        assert!(
            shown.contains("That claim is not open any more."),
            "{path}: {shown}"
        );
        assert!(!shown.contains("See the queue"), "{path}: {shown}");
    }
}

#[tokio::test]
async fn the_conversations_about_a_claim_list_what_members_wrote_and_fold_dibss_notices() {
    let seeded = Seeded::new().await;
    let dave = seeded
        .world
        .member(&seeded.alice, "dave", Role::Member)
        .await;
    let started = seeded
        .world
        .post(
            &dave,
            "/v1/threads",
            json!({"subject": "Rates wait for gh:acme/widgets#11", "to": ["alice"],
                   "item": seeded.working, "label": "web", "body": "see the refunds"}),
        )
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let page = seeded.page(&format!("/ui/claims/{}", seeded.working)).await;
    let shown = inspector(&page);
    assert!(
        shown.contains(&format!(
            r#"<b>dave</b></a><span class="t"><a class="lnk" href="/ui/inbox/{thread}">Rates wait for </a><a class="lnk" href="/ui/claims/{}">gh:acme/widgets#11</a></span><a class="r" href="/ui/inbox/{thread}">"#,
            seeded.review
        )),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span class="tag">dibs</span><a class="t lnk" href="/ui/inbox/"#),
        "{shown}"
    );
    assert!(
        shown.contains(">1 notice from dibs, 1 unread</a>"),
        "{shown}"
    );
    let quiet = seeded.page(&format!("/ui/claims/{}", seeded.branch)).await;
    assert!(
        inspector(&quiet).contains(r#"<li class="faint">Nothing was written about it.</li>"#),
        "{}",
        inspector(&quiet)
    );
}

#[tokio::test]
async fn debt_work_is_marked_and_a_claim_names_the_debt_work_it_waits_for() {
    let (world, alice, bob) = team().await;
    let alice_session = world.session(&alice, "laptop").await;
    let bob_session = world.session(&bob, "desk").await;
    let debt = world
        .item(&alice, Some("gh:acme/api#7"), "Tidy the invoices")
        .await;
    let feature = world
        .item(&bob, Some("gh:acme/api#42"), "Currency on invoices")
        .await;
    let held = world
        .post(
            &alice,
            &format!("/v1/items/{debt}/claim"),
            json!({"session_id": alice_session, "areas": ["api:billing"], "kind": "debt"}),
        )
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.body);
    claim(&world, &bob, bob_session, feature, json!(["api:billing"])).await;
    let browser = sign_in(&world, &bob).await;

    let tidy = browser.get(&world, &format!("/ui/claims/{debt}")).await;
    assert_eq!(tidy.status, StatusCode::OK, "{}", tidy.text);
    assert!(
        inspector(&tidy).contains("<dt>Kind</dt><dd>Debt</dd>"),
        "{}",
        inspector(&tidy)
    );
    let waiting = browser.get(&world, &format!("/ui/claims/{feature}")).await;
    let shown = inspector(&waiting);
    assert!(!shown.contains("<dt>Kind</dt>"), "{shown}");
    assert!(
        shown.contains(r#"<a class="area" href="/ui/areas/api:billing" title="api:billing">api:billing</a><span class="r">waiting for <a class="who" href="/ui/claims?who=alice">"#),
        "{shown}"
    );
    assert!(
        shown.contains("<b>alice</b></a>'s debt work</span>"),
        "{shown}"
    );
}
