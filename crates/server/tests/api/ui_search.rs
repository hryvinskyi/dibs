//! Search (`/ui/search`) and the palette's answer (`/ui/search?…&palette=1`) through the real
//! router: what a query finds, of which kind, in which order, where each hit leads here and
//! outside, the caps, the hint and the empty answer, the escaping of what members wrote, and the
//! palette every page carries.

use axum::http::{StatusCode, header};
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World};
use crate::ui_support::{Browser, Page, claim, sign_in};

/// A project whose repository `api` is `acme/widgets` on GitHub, with the areas `billing` and
/// `search`, and a slot.
fn settings() -> Value {
    json!({
        "repos": [{
            "id": "api",
            "remote": "https://github.com/acme/widgets",
            "areas": [{"name": "billing", "paths": ["src/billing/**"]},
                      {"name": "search", "paths": ["src/search/**"]}]
        }],
        "slots": [{"name": "deploy:prod", "max_hold": "30m", "note": "say it in the channel"}]
    })
}

/// alice (the viewer) and bob; bob's claim in review with `acme/widgets#12`, whose base a merge of
/// `acme/widgets#30` moved; the ready `#13`; seven chores waiting in the queue; a conversation
/// whose subject holds markup.
struct Seeded {
    world: World,
    browser: Browser,
    /// bob's claim in review, on `gh:acme/widgets#11`.
    review: u64,
    /// The ready item `gh:acme/widgets#13`.
    ready: u64,
    /// bob's conversation with alice.
    thread: u64,
}

impl Seeded {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let bot = world.member(&alice, "bot", Role::Importer).await;
        world.settings(&alice, settings()).await;

        let desk = world.session(&bob, "bob-desk").await;
        let review = world
            .item(
                &alice,
                Some("gh:acme/widgets#11"),
                "Refunds reach the ledger",
            )
            .await;
        claim(&world, &bob, desk, review, json!(["api:billing"])).await;
        beat(&world, &bob, desk, "bob/11-refunds").await;
        let stored = world
            .put(
                &bot,
                &format!("/v1/items/{review}/footprint"),
                json!({"repo": "api", "branch": "bob/11-refunds", "base": "main",
                       "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd1234",
                       "paths": ["src/billing/a.rs"]}),
            )
            .await;
        assert_eq!(stored.body["stored"], true, "{}", stored.body);
        let reviewed = world
            .post(
                &bob,
                &format!("/v1/items/{review}/review"),
                json!({"pr": "acme/widgets#12"}),
            )
            .await;
        assert_eq!(reviewed.status, StatusCode::OK, "{}", reviewed.body);
        let merged = world
            .post(
                &bot,
                "/v1/prs/merged",
                json!({"pr": "acme/widgets#30", "base": "main",
                       "merged_at": "2099-01-01T00:00:00Z", "files": ["src/billing/a.rs"]}),
            )
            .await;
        assert_eq!(merged.status, StatusCode::OK, "{}", merged.body);

        let ready = world
            .item(&alice, Some("gh:acme/widgets#13"), "Ledger rates")
            .await;
        for number in 21..=27 {
            world
                .item(
                    &alice,
                    Some(&format!("gh:acme/widgets#{number}")),
                    &format!("Chore {number}"),
                )
                .await;
        }
        let thread = start(
            &world,
            &bob,
            json!({"subject": "Ledger <b>totals</b>", "to": ["alice"], "label": "web",
                   "body": "Which rounding mode?"}),
        )
        .await;

        let browser = sign_in(&world, &alice).await;
        Self {
            world,
            browser,
            review,
            ready,
            thread,
        }
    }

    async fn page(&self, path: &str) -> Page {
        let page = self.browser.get(&self.world, path).await;
        assert_eq!(page.status, StatusCode::OK, "{path}: {}", page.text);
        page
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

/// Starts a conversation as `who` with `body` and answers its id.
async fn start(world: &World, who: &Person, body: Value) -> u64 {
    let started = world.post(who, "/v1/threads", body).await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    started.body["thread"]["id"].as_u64().unwrap()
}

/// The results region of a page.
fn results(page: &Page) -> &str {
    let start = page
        .text
        .find(r#"data-region="results""#)
        .unwrap_or_else(|| panic!("no results in {}", page.text));
    &page.text[start..]
}

/// The headings of the kinds listed, in order.
fn kinds(page: &Page) -> Vec<String> {
    results(page)
        .split(r#"class="pal__grp label""#)
        .skip(1)
        .filter_map(|rest| {
            let start = rest.find('>')? + 1;
            let end = rest[start..].find('<')? + start;
            Some(rest[start..end].to_owned())
        })
        .collect()
}

/// The addresses the hits lead to here, in order.
fn heres(page: &Page) -> Vec<String> {
    results(page)
        .split(r#"<a href=""#)
        .skip(1)
        .filter_map(|rest| {
            let (href, tail) = rest.split_once('"')?;
            tail.starts_with(" data-hit-here>").then(|| href.to_owned())
        })
        .collect()
}

/// The titles of the hits, in order.
fn titles(page: &Page) -> Vec<String> {
    results(page)
        .split(r#"<div class="t">"#)
        .skip(1)
        .filter_map(|rest| rest.split_once("</div>").map(|(title, _)| title.to_owned()))
        .collect()
}

#[tokio::test]
async fn a_number_lists_the_pull_request_of_that_number_first_leading_to_its_claim_and_its_forge() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/search?q=12").await;
    assert_eq!(titles(&page)[0], "PR acme/widgets#12", "{}", page.text);
    assert_eq!(kinds(&page)[0], "Pull requests", "{}", page.text);
    assert_eq!(
        heres(&page)[0],
        format!("/ui/claims/{}", seeded.review),
        "{}",
        page.text
    );
    let results = results(&page);
    assert!(
        results.contains(r#"<a class="ext" href="https://github.com/acme/widgets/pull/12" target="_blank" rel="noopener noreferrer" data-hit-out aria-label="PR acme/widgets#12 on GitHub">↗</a>"#),
        "{results}"
    );
}

#[tokio::test]
async fn words_of_a_title_find_the_claim_and_a_hash_number_the_ready_ticket_first() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/search?q=refunds").await;
    assert_eq!(kinds(&page)[0], "Claims", "{}", page.text);
    assert_eq!(
        titles(&page)[0],
        "gh:acme/widgets#11  Refunds reach the ledger"
    );
    assert_eq!(heres(&page)[0], format!("/ui/claims/{}", seeded.review));
    assert!(
        results(&page).contains(r#"<div class="u">bob · in review · held "#),
        "{}",
        page.text
    );

    let page = seeded.page("/ui/search?q=%2313").await;
    assert_eq!(kinds(&page)[0], "Tickets in the queue", "{}", page.text);
    assert_eq!(titles(&page)[0], "gh:acme/widgets#13  Ledger rates");
    assert_eq!(heres(&page)[0], format!("/ui/queue/{}", seeded.ready));
    assert!(
        results(&page).contains(r#"<div class="u">ready · P"#),
        "{}",
        page.text
    );
}

#[tokio::test]
async fn an_area_a_person_a_slot_a_branch_and_a_merged_pull_request_are_found_by_name() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/search?q=billing").await;
    assert!(kinds(&page).contains(&"Areas".to_owned()), "{}", page.text);
    assert!(titles(&page).contains(&"api:billing".to_owned()));
    assert!(heres(&page).contains(&"/ui/areas/api:billing".to_owned()));
    assert!(
        results(&page).contains(r#"<div class="u">held by bob for gh:acme/widgets#11</div>"#),
        "{}",
        page.text
    );

    let page = seeded.page("/ui/search?q=bob").await;
    let at = titles(&page)
        .iter()
        .position(|title| title == "bob")
        .unwrap();
    assert_eq!(heres(&page)[at], "/ui/claims?who=bob", "{}", page.text);

    let page = seeded.page("/ui/search?q=deploy").await;
    assert_eq!(kinds(&page), ["Slots"], "{}", page.text);
    assert_eq!(titles(&page), ["deploy:prod"]);
    assert_eq!(heres(&page), ["/ui/?slot=deploy%3Aprod"]);

    let page = seeded.page("/ui/search?q=11-refunds").await;
    let at = kinds(&page)
        .iter()
        .position(|kind| kind == "Branches")
        .unwrap();
    assert_eq!(titles(&page).len(), kinds(&page).len(), "{}", page.text);
    assert_eq!(titles(&page)[at], "bob/11-refunds");
    assert_eq!(heres(&page)[at], "/ui/claims?q=bob%2F11-refunds");
    assert!(
        results(&page).contains(r#"href="https://github.com/acme/widgets/tree/bob/11-refunds" target="_blank" rel="noopener noreferrer" data-hit-out aria-label="bob/11-refunds on GitHub""#),
        "{}",
        page.text
    );

    let page = seeded.page("/ui/search?q=merged").await;
    assert_eq!(titles(&page), ["PR acme/widgets#30"], "{}", page.text);
    assert_eq!(heres(&page), ["/ui/claims?q=acme%2Fwidgets%2330"]);
    assert!(
        results(&page).contains(r#"<div class="u">merged · moved the base under 1 claim</div>"#),
        "{}",
        page.text
    );
}

#[tokio::test]
async fn the_kinds_come_in_their_fixed_order() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/search?q=ledger").await;
    assert_eq!(
        kinds(&page),
        [
            "Claims",
            "Pull requests",
            "Tickets in the queue",
            "Conversations"
        ],
        "{}",
        page.text
    );
    assert!(heres(&page).contains(&format!("/ui/inbox/{}", seeded.thread)));
}

#[tokio::test]
async fn the_page_lists_every_hit_of_a_kind_and_the_palette_five_and_how_many_more() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/search?q=chore").await;
    assert_eq!(titles(&page).len(), 7, "{}", page.text);
    assert!(!page.text.contains(" more</p>"), "{}", page.text);
    assert!(page.text.contains(r#"data-region="rail""#));

    let palette = seeded.page("/ui/search?q=chore&palette=1").await;
    assert_eq!(
        palette.headers[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
    assert_eq!(palette.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(titles(&palette).len(), 5, "{}", palette.text);
    assert!(
        palette.text.contains(r#"<p class="pal__more">2 more</p>"#),
        "{}",
        palette.text
    );
    assert!(!palette.text.contains(r#"data-region="rail""#));
    assert!(!palette.text.contains("<dialog"));
    assert!(palette.text.contains(r#"<div role="group" aria-labelledby="hits-queue"><div class="pal__grp label" id="hits-queue">Tickets in the queue</div>"#), "{}", palette.text);
    for n in 0..5 {
        assert!(
            palette.text.contains(&format!(
                r#"<div class="pal__row" data-hit role="option" id="hit-{n}" aria-selected="false">"#
            )),
            "hit-{n}: {}",
            palette.text
        );
    }
    assert!(!palette.text.contains(r#"id="hit-5""#));
    assert!(!page.text.contains(r#"role="option" id="hit-"#));
}

#[tokio::test]
async fn an_empty_query_shows_the_hint_and_a_query_without_a_hit_says_so() {
    let seeded = Seeded::new().await;
    for path in ["/ui/search", "/ui/search?q=+", "/ui/search?palette=1"] {
        let page = seeded.page(path).await;
        assert!(
            results(&page).contains(r#"<div class="pal__hint"><span>A ticket or pull request number: <span class="mono">42</span>, <span class="mono">#17</span></span>"#),
            "{path}: {}",
            page.text
        );
        assert!(titles(&page).is_empty(), "{path}");
    }
    for path in ["/ui/search?q=zebra", "/ui/search?q=zebra&palette=1"] {
        let page = seeded.page(path).await;
        assert!(
            results(&page)
                .contains(r#"<div class="pal__hint">Nothing in this project matches.</div>"#),
            "{path}: {}",
            page.text
        );
    }
}

#[tokio::test]
async fn the_page_has_its_heading_and_keeps_the_query_in_its_form_with_nothing_current() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/search?q=a%22b").await;
    assert!(page.text.contains(r#"<h1 tabindex="-1">Search</h1>"#));
    assert!(
        page.text.contains(r#"<input class="search" type="search" name="q" value="a&#34;b" aria-label="Search everything">"#),
        "{}",
        page.text
    );
    assert!(
        !page.text.contains(r#"aria-current="page""#),
        "{}",
        page.text
    );
}

#[tokio::test]
async fn what_a_member_wrote_is_escaped() {
    let seeded = Seeded::new().await;
    for path in ["/ui/search?q=totals", "/ui/search?q=totals&palette=1"] {
        let page = seeded.page(path).await;
        let results = results(&page);
        assert!(!results.contains("<b>totals</b>"), "{path}: {results}");
        assert!(
            results.contains("Ledger &#60;b&#62;totals&#60;/b&#62;"),
            "{path}: {results}"
        );
    }
}

#[tokio::test]
async fn the_palette_answers_a_browser_without_a_session_as_a_page_does() {
    let seeded = Seeded::new().await;
    let anonymous = crate::ui_support::fetch(
        &seeded.world,
        axum::http::Method::GET,
        "/ui/search?q=chore&palette=1",
        None,
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::SEE_OTHER);
    assert_eq!(anonymous.location(), "/ui/login");
}

#[tokio::test]
async fn every_page_carries_the_palette() {
    let seeded = Seeded::new().await;
    for path in [
        "/ui/",
        "/ui/claims",
        "/ui/queue",
        "/ui/areas",
        "/ui/inbox",
        "/ui/search",
    ] {
        let page = seeded.page(path).await;
        let at = page
            .text
            .find(r#"<dialog class="pal" data-palette aria-label="Search everything">"#)
            .unwrap_or_else(|| panic!("{path}: {}", page.text));
        assert!(
            at > page.text.find(r#"<main id="main""#).unwrap(),
            "{path}: after the app"
        );
        let dialog = &page.text[at..];
        assert!(dialog.contains(r#"<form method="get" action="/ui/search" role="search"><input data-palette-input name="q" type="search" role="combobox" aria-expanded="true" aria-controls="palette-results" aria-autocomplete="list" autocomplete="off""#), "{path}");
        assert!(dialog.contains(r#"<div id="palette-results" class="pal__list" role="listbox" aria-label="Results" data-palette-results><div class="pal__hint">"#), "{path}");
        assert!(
            dialog.contains(
                r#"<kbd data-kbd-mac="⌘↵" data-kbd-other="Ctrl ↵">⌘↵</kbd> open outside"#
            ),
            "{path}"
        );
        assert!(
            page.text
                .contains(r#"<kbd data-kbd-mac="⌘K" data-kbd-other="Ctrl K">⌘K</kbd>"#),
            "{path}"
        );
    }
}
