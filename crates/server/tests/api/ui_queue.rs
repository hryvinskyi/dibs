//! Queue (`/ui/queue`) through the real router: the ready tickets grouped by priority and those that
//! are not ready with their reasons, the cap on a group, the text filter and the narrowings to a
//! claim's kept-out tickets and to an area, a ticket in the inspector with its command — printed as
//! it is copied, quoted for a shell — and every reference a link, the claimed tickets that are not
//! here, an address of no ticket in the queue, and the escaping of what members wrote.

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Person, World};
use crate::ui_support::{Browser, Page, claim, sign_in};

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

/// A project of alice (the viewer), bob and an importer, with its settings.
async fn project() -> (World, Person, Person, Person) {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let bot = world.member(&alice, "bot", Role::Importer).await;
    world.settings(&alice, settings()).await;
    (world, alice, bob, bot)
}

/// The ids of the items an importer lists, in order.
async fn import(world: &World, bot: &Person, items: Value) -> Vec<u64> {
    let imported = world.put(bot, "/v1/items", json!({ "items": items })).await;
    assert_eq!(imported.status, StatusCode::OK, "{}", imported.body);
    imported.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_u64().unwrap())
        .collect()
}

/// The tickets of a project: two of bob's claims, a ticket that touches an area he holds, one that
/// waits for another, and three ready ones; its viewer is alice.
struct Seeded {
    world: World,
    alice: Person,
    bot: Person,
    browser: Browser,
    /// bob's claim, which holds `api:billing`.
    bob_billing: u64,
    /// bob's claim, which holds no area.
    bob_other: u64,
    /// Ready, priority 1, names `api:search`, its tracker says `Todo`.
    search: u64,
    /// Ready, the default priority, names no area, and blocks `blocked`.
    plain: u64,
    /// Ready, priority 3, its title holds markup.
    bold: u64,
    /// Not ready, priority 1: names `api:billing`, which bob holds.
    touches: u64,
    /// Not ready, priority 3: waits for `plain`.
    blocked: u64,
}

impl Seeded {
    async fn new() -> Self {
        let (world, alice, bob, bot) = project().await;
        let ids = import(
            &world,
            &bot,
            json!([
                {"ref": "gh:acme/widgets#10", "title": "Bob billing work"},
                {"ref": "gh:acme/widgets#11", "title": "Bob other work"},
                {"ref": "gh:acme/widgets#20", "title": "Touches billing", "priority": 1,
                 "areas_hint": ["api:billing"]},
                {"ref": "gh:acme/widgets#21", "title": "Search tweaks", "priority": 1,
                 "areas_hint": ["api:search"], "source_status": "Todo"},
                {"ref": "gh:acme/widgets#23", "title": "Plain chore"},
                {"ref": "gh:acme/widgets#24", "title": "Bold <b>move</b>", "priority": 3},
                {"ref": "gh:acme/widgets#25", "title": "Waits on others", "priority": 3}
            ]),
        )
        .await;
        let [
            bob_billing,
            bob_other,
            touches,
            search,
            plain,
            bold,
            blocked,
        ] = ids[..]
        else {
            panic!("seven items were listed: {ids:?}");
        };
        let desk = world.session(&bob, "bob-desk").await;
        claim(&world, &bob, desk, bob_billing, json!(["api:billing"])).await;
        let laptop = world.session(&bob, "bob-laptop").await;
        claim(&world, &bob, laptop, bob_other, json!([])).await;
        let waits = world
            .post(
                &alice,
                &format!("/v1/items/{blocked}/dependencies"),
                json!({"blocked_by": plain}),
            )
            .await;
        assert_eq!(waits.status, StatusCode::OK, "{}", waits.body);
        let browser = sign_in(&world, &alice).await;
        Self {
            world,
            alice,
            bot,
            browser,
            bob_billing,
            bob_other,
            search,
            plain,
            bold,
            touches,
            blocked,
        }
    }

    async fn page(&self, path: &str) -> Page {
        self.browser.get(&self.world, path).await
    }

    /// The tickets that are not claims.
    fn queued(&self) -> [u64; 5] {
        [
            self.search,
            self.plain,
            self.bold,
            self.touches,
            self.blocked,
        ]
    }
}

/// The ids among `all` the list shows a row for, by the address their titles link to.
fn rows(page: &Page, all: &[u64]) -> Vec<u64> {
    all.iter()
        .copied()
        .filter(|id| {
            page.text.contains(&format!(
                r#"<a class="row__t" data-inspect href="/ui/queue/{id}"#
            ))
        })
        .collect()
}

/// The markup of the row of the ticket `id`.
fn row(page: &Page, id: u64) -> &str {
    let start = page
        .text
        .find(&format!(
            r#"<a class="row__t" data-inspect href="/ui/queue/{id}"#
        ))
        .unwrap_or_else(|| panic!("no row of {id} in {}", page.text));
    let end_of_meta = page.text[start..].find(r#"class="row__e""#).unwrap() + start;
    let end = page.text[end_of_meta..].find("</div></div>").unwrap() + end_of_meta;
    &page.text[start..end]
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

#[tokio::test]
async fn the_ready_tickets_are_grouped_by_priority_soonest_first_and_the_waiting_ones_come_last() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/queue").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(r#"<h1 tabindex="-1">Queue</h1>"#),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains(r#"<span class="dim">3 ready · 2 not ready</span>"#),
        "{}",
        page.text
    );
    let head = |text: &str| {
        page.text
            .find(&format!(r#"<div class="grp">{text}"#))
            .unwrap_or_else(|| panic!("no group {text} in {}", page.text))
    };
    let first = head(r#"<span class="pri pri--1">P1</span><b>Ready</b><span class="r">1</span>"#);
    let second = head(r#"<span class="pri pri--2">P2</span><b>Ready</b><span class="r">1</span>"#);
    let third = head(r#"<span class="pri pri--3">P3</span><b>Ready</b><span class="r">1</span>"#);
    let waiting = head(r#"<b>Not ready</b><span class="r">2</span>"#);
    assert!(
        first < second && second < third && third < waiting,
        "{}",
        page.text
    );
    let at = |id: u64| {
        page.text
            .find(&format!(r#"data-inspect href="/ui/queue/{id}""#))
            .unwrap()
    };
    assert!(first < at(seeded.search) && at(seeded.search) < second);
    assert!(second < at(seeded.plain) && at(seeded.plain) < third);
    assert!(third < at(seeded.bold) && at(seeded.bold) < waiting);
    assert!(waiting < at(seeded.touches) && at(seeded.touches) < at(seeded.blocked));
    assert_eq!(rows(&page, &seeded.queued()), seeded.queued());
    assert!(
        page.text
            .contains(r#"data-region="inspector" data-selected="false""#),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(
            "Pick a ticket to see which areas it will touch, who holds them and how to claim it."
        ),
        "{}",
        page.text
    );
    let search = row(&page, seeded.search);
    assert!(
        search.contains(
            r#"title="gh:acme/widgets#21" target="_blank" rel="noopener noreferrer">widgets#21</a>"#
        ),
        "{search}"
    );
    assert!(
        search.contains(
            r#"<a class="area" href="/ui/areas/api:search" title="api:search">search</a>"#
        ),
        "{search}"
    );
    assert!(
        search.contains(r#"<span class="tag">ready</span>"#),
        "{search}"
    );
    let plain = row(&page, seeded.plain);
    assert!(plain.contains("<span>no areas named</span>"), "{plain}");
}

#[tokio::test]
async fn a_ticket_a_live_claim_holds_is_not_in_the_queue() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/queue").await;
    assert_eq!(
        rows(&page, &[seeded.bob_billing, seeded.bob_other]),
        Vec::<u64>::new(),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains(r#"<span class="dim">3 ready · 2 not ready</span>"#),
        "{}",
        page.text
    );
    assert!(!page.text.contains("Bob other work"), "{}", page.text);
}

#[tokio::test]
async fn a_group_shows_its_first_twenty_five_rows_and_offers_all_of_them() {
    let (world, alice, _, bot) = project().await;
    let browser = sign_in(&world, &alice).await;
    let items: Vec<Value> = (100..130)
        .map(|number| {
            json!({"ref": format!("gh:acme/widgets#{number}"), "title": format!("Chore {number}"),
                   "priority": 4})
        })
        .collect();
    let ids = import(&world, &bot, Value::Array(items)).await;
    let page = browser.get(&world, "/ui/queue").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert_eq!(rows(&page, &ids).len(), 25, "{}", page.text);
    assert!(
        page.text
            .contains(r#"<span class="pri pri--4">P4</span><b>Ready</b><span class="r">30</span>"#),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains(r#"<a class="more" href="/ui/queue?all=1">Show all 30</a>"#),
        "{}",
        page.text
    );
    let every = browser.get(&world, "/ui/queue?all=1").await;
    assert_eq!(rows(&every, &ids).len(), 30, "{}", every.text);
    assert!(!every.text.contains("Show all"), "{}", every.text);
    assert!(
        every
            .text
            .contains(r#"<input type="hidden" name="all" value="1">"#),
        "{}",
        every.text
    );
    let narrowed = browser.get(&world, "/ui/queue?q=chore").await;
    assert_eq!(rows(&narrowed, &ids).len(), 30, "{}", narrowed.text);
}

#[tokio::test]
async fn the_text_finds_a_ticket_by_a_word_of_its_title_its_number_or_its_area_and_says_when_none_match()
 {
    let seeded = Seeded::new().await;
    let by_title = seeded.page("/ui/queue?q=tweaks").await;
    assert_eq!(rows(&by_title, &seeded.queued()), [seeded.search]);
    assert!(
        by_title
            .text
            .contains(r#"<span class="dim">1 ready · 0 not ready</span>"#),
        "{}",
        by_title.text
    );
    assert!(
        by_title.text.contains(r#"value="tweaks""#),
        "{}",
        by_title.text
    );
    let by_number = seeded.page("/ui/queue?q=%2325").await;
    assert_eq!(rows(&by_number, &seeded.queued()), [seeded.blocked]);
    let by_area = seeded.page("/ui/queue?q=api%3Abilling").await;
    assert_eq!(rows(&by_area, &seeded.queued()), [seeded.touches]);
    let by_holder = seeded.page("/ui/queue?q=bob").await;
    assert_eq!(rows(&by_holder, &seeded.queued()), [seeded.touches]);
    let none = seeded.page("/ui/queue?q=nothing+like+it").await;
    assert!(
        none.text
            .contains(r#"<p class="note">No ticket matches.</p>"#),
        "{}",
        none.text
    );
    assert!(rows(&none, &seeded.queued()).is_empty());
}

#[tokio::test]
async fn a_ticket_that_touches_an_area_another_member_holds_says_who_and_for_which_ticket() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/queue").await;
    let touches = row(&page, seeded.touches);
    assert!(
        touches.contains(
            r#"<span><a class="area" href="/ui/areas/api:billing" title="api:billing">api:billing</a> is held by <a class="who" href="/ui/claims?who=bob"><span class="av av--s pc-"#
        ),
        "{touches}"
    );
    assert!(
        touches.contains(&format!(
            r#"</a> for <span class="ref"><a href="/ui/claims/{}" title="gh:acme/widgets#10">widgets#10</a>"#,
            seeded.bob_billing
        )),
        "{touches}"
    );
    assert!(!touches.contains("ticket closed"), "{touches}");
    let closed = seeded
        .world
        .put(
            &seeded.bot,
            "/v1/items",
            json!({"items": [{"ref": "gh:acme/widgets#10", "title": "Bob billing work",
                              "closed": true}]}),
        )
        .await;
    assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
    let after = seeded.page("/ui/queue").await;
    let touches = row(&after, seeded.touches);
    assert!(
        touches.contains(r#"<span class="tag tag--bad">ticket closed</span>"#),
        "{touches}"
    );
}

#[tokio::test]
async fn a_ticket_that_waits_for_others_names_them_and_one_with_several_reasons_counts_them() {
    let seeded = Seeded::new().await;
    let page = seeded.page("/ui/queue").await;
    let blocked = row(&page, seeded.blocked);
    assert!(
        blocked.contains(&format!(
            r#"<span>waits for <span class="ref"><a href="/ui/queue/{}" title="gh:acme/widgets#23""#,
            seeded.plain
        )),
        "{blocked}"
    );
    let more = seeded
        .world
        .post(
            &seeded.alice,
            &format!("/v1/items/{}/dependencies", seeded.touches),
            json!({"blocked_by": seeded.search}),
        )
        .await;
    assert_eq!(more.status, StatusCode::OK, "{}", more.body);
    let after = seeded.page("/ui/queue").await;
    assert!(
        row(&after, seeded.touches).contains("<span>2 reasons</span>"),
        "{}",
        after.text
    );
}

#[tokio::test]
async fn the_inspector_shows_the_ticket_its_areas_and_the_command_with_every_reference_a_link() {
    let seeded = Seeded::new().await;
    let page = seeded
        .page(&format!("/ui/queue/{}?q=billing", seeded.touches))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(&format!(
            r#"<div class="row row--queue" data-row aria-current="true"><span class="pri pri--1">P1</span>
<div><a class="row__t" data-inspect href="/ui/queue/{}?q=billing">"#,
            seeded.touches
        )),
        "{}",
        page.text
    );
    let shown = inspector(&page);
    assert!(
        shown.contains(
            r#"data-region="inspector" data-selected="true" aria-label="Ticket gh:acme/widgets#20""#
        ),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span class="tag tag--warn">Not ready</span>"#),
        "{shown}"
    );
    assert!(
        shown.contains(
            r#"<button class="mini" type="button" data-copy-link hidden>Copy link</button>"#
        ),
        "{shown}"
    );
    assert!(
        shown.contains(
            r#"<a class="x" data-inspect href="/ui/queue?q=billing" aria-label="Close">"#
        ),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<h2 data-inspector-heading tabindex="-1">Touches billing</h2>"#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span class="faint">no tracker status</span>"#),
        "{shown}"
    );
    assert!(shown.contains("Why it waits"), "{shown}");
    assert!(
        shown.contains("Areas it will probably touch · 1"),
        "{shown}"
    );
    assert!(
        shown.contains(
            r#"<span class="r" title="held by bob for gh:acme/widgets#10">held by <a class="who" href="/ui/claims?who=bob"><span class="av av--s pc-"#
        ),
        "{shown}"
    );
    assert!(
        shown.contains(&format!(
            r#"for <span class="ref"><a href="/ui/claims/{}""#,
            seeded.bob_billing
        )),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<button type="button" hidden data-copy="dibs claim gh:acme/widgets#20 --area api:billing">Copy</button>"#),
        "{shown}"
    );
    let markup = command_markup(shown);
    assert!(
        markup.contains(&format!(
            r#"dibs claim <a class="lnk" href="/ui/queue/{}">gh:acme/widgets#20</a>"#,
            seeded.touches
        )),
        "{markup}"
    );
    assert!(
        markup.contains(r#" --area <a class="lnk" href="/ui/areas/api:billing">api:billing</a>"#),
        "{markup}"
    );
    let (visible, copied) = command(shown);
    assert_eq!(visible, copied);
    assert_eq!(visible, "dibs claim gh:acme/widgets#20 --area api:billing");
}

/// The markup of the command the inspector prints.
fn command_markup(shown: &str) -> &str {
    let open = r#"<div class="cmd"><span>"#;
    let start = shown
        .find(open)
        .unwrap_or_else(|| panic!("no command in {shown}"))
        + open.len();
    let end = shown[start..].find("</span><button").unwrap() + start;
    &shown[start..end]
}

/// The command the inspector prints as a person reads it, and the text its Copy button copies:
/// both with the markup taken out and the escaped characters read back.
fn command(shown: &str) -> (String, String) {
    let mut visible = String::new();
    let mut in_tag = false;
    for character in command_markup(shown).chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => visible.push(character),
            _ => {}
        }
    }
    let attribute = r#" data-copy=""#;
    let start = shown
        .find(attribute)
        .unwrap_or_else(|| panic!("no copy button in {shown}"))
        + attribute.len();
    let end = shown[start..].find('"').unwrap() + start;
    (unescape(&visible), unescape(&shown[start..end]))
}

/// Text with the characters the page escapes read back.
fn unescape(text: &str) -> String {
    text.replace("&#39;", "'")
        .replace("&#34;", "\"")
        .replace("&#60;", "<")
        .replace("&#62;", ">")
        .replace("&#38;", "&")
}

#[tokio::test]
async fn a_reference_a_shell_would_run_is_quoted_and_the_command_shown_is_the_one_copied() {
    let (world, alice, _, _) = project().await;
    let hostile = world
        .item(&alice, Some("x:$(touch pwned)"), "Hostile reference")
        .await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, &format!("/ui/queue/{hostile}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = inspector(&page);
    let (visible, copied) = command(shown);
    assert_eq!(copied, "dibs claim 'x:$(touch pwned)'");
    assert_eq!(visible, copied);
    assert!(
        shown.contains(r#"data-copy="dibs claim &#39;x:$(touch pwned)&#39;""#),
        "{shown}"
    );
}

#[tokio::test]
async fn a_ticket_without_a_reference_is_claimed_by_its_bare_id_and_still_shown_as_its_number() {
    let (world, alice, _, _) = project().await;
    let local = world.item(&alice, None, "No tracker").await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, &format!("/ui/queue/{local}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = inspector(&page);
    let (visible, copied) = command(shown);
    assert_eq!(copied, format!("dibs claim {local}"));
    assert_eq!(visible, copied);
    assert!(
        shown.contains(&format!(r##"title="#{local}">#{local}</a>"##)),
        "{shown}"
    );
}

#[tokio::test]
async fn the_queue_narrows_to_exactly_the_tickets_a_claim_keeps_out_or_that_will_touch_an_area() {
    let seeded = Seeded::new().await;
    let kept = seeded
        .page(&format!("/ui/queue?kept_by={}", seeded.bob_billing))
        .await;
    assert_eq!(kept.status, StatusCode::OK, "{}", kept.text);
    assert_eq!(rows(&kept, &seeded.queued()), [seeded.touches]);
    assert!(
        kept.text
            .contains(r#"<span class="dim">0 ready · 1 not ready</span>"#),
        "{}",
        kept.text
    );
    assert!(
        kept.text.contains(
            r#"<span class="faint">waiting for the claim on widgets#10</span><a class="mini" href="/ui/queue">every ticket</a>"#
        ),
        "{}",
        kept.text
    );
    assert!(
        kept.text.contains(&format!(
            r#"<input type="hidden" name="kept_by" value="{}">"#,
            seeded.bob_billing
        )),
        "{}",
        kept.text
    );
    assert!(
        kept.text.contains(&format!(
            r#"data-inspect href="/ui/queue/{}?kept_by={}""#,
            seeded.touches, seeded.bob_billing
        )),
        "{}",
        kept.text
    );
    let other = seeded
        .page(&format!("/ui/queue?kept_by={}", seeded.bob_other))
        .await;
    assert!(rows(&other, &seeded.queued()).is_empty(), "{}", other.text);

    let billing = seeded.page("/ui/queue?area=api%3Abilling").await;
    assert_eq!(rows(&billing, &seeded.queued()), [seeded.touches]);
    assert!(
        billing
            .text
            .contains(r#"<input type="hidden" name="area" value="api:billing">"#),
        "{}",
        billing.text
    );
    let search = seeded.page("/ui/queue?area=api:search").await;
    assert_eq!(rows(&search, &seeded.queued()), [seeded.search]);
    assert!(
        search
            .text
            .contains(r#"<span class="dim">1 ready · 0 not ready</span>"#),
        "{}",
        search.text
    );

    let selected = seeded
        .page(&format!(
            "/ui/queue/{}?area=api%3Asearch&q=tweaks",
            seeded.search
        ))
        .await;
    assert_eq!(selected.status, StatusCode::OK, "{}", selected.text);
    let shown = inspector(&selected);
    assert!(
        shown.contains(
            r#"<a class="x" data-inspect href="/ui/queue?q=tweaks&#38;area=api%3Asearch" aria-label="Close">"#
        ),
        "{shown}"
    );

    let unread = seeded.page("/ui/queue?kept_by=abc&area=nowhere").await;
    assert_eq!(unread.status, StatusCode::OK, "{}", unread.text);
    assert_eq!(rows(&unread, &seeded.queued()), seeded.queued());
    assert!(!unread.text.contains("every ticket"), "{}", unread.text);
}

#[tokio::test]
async fn an_area_one_member_holds_for_two_tickets_names_both_in_the_inspector() {
    let (world, alice, bob, bot) = project().await;
    let ids = import(
        &world,
        &bot,
        json!([
            {"ref": "gh:acme/widgets#10", "title": "Bob billing work"},
            {"ref": "gh:acme/widgets#11", "title": "More billing work"},
            {"ref": "gh:acme/widgets#20", "title": "Touches billing", "areas_hint": ["api:billing"]}
        ]),
    )
    .await;
    let [first, second, touches] = ids[..] else {
        panic!("three items were listed: {ids:?}");
    };
    let desk = world.session(&bob, "bob-desk").await;
    claim(&world, &bob, desk, first, json!(["api:billing"])).await;
    let laptop = world.session(&bob, "bob-laptop").await;
    claim(&world, &bob, laptop, second, json!(["api:billing"])).await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, &format!("/ui/queue/{touches}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let shown = inspector(&page);
    assert!(
        shown.contains(
            r#"<span class="r" title="held by bob for gh:acme/widgets#10, bob for gh:acme/widgets#11">held by <a class="who" href="/ui/claims?who=bob">"#
        ),
        "{shown}"
    );
    for claimed in [first, second] {
        assert!(
            shown.contains(&format!(
                r#"<span class="ref"><a href="/ui/claims/{claimed}""#
            )),
            "{shown}"
        );
    }
    let touched = &shown[shown.find("Areas it will probably touch").unwrap()..];
    assert_eq!(
        touched
            .matches(r#"<a class="who" href="/ui/claims?who=bob">"#)
            .count(),
        2,
        "{touched}"
    );
}

#[tokio::test]
async fn a_ready_ticket_shows_its_tracker_status_a_free_area_and_a_ticket_without_areas_says_so() {
    let seeded = Seeded::new().await;
    let search = seeded.page(&format!("/ui/queue/{}", seeded.search)).await;
    let shown = inspector(&search);
    assert!(
        shown.contains(r#"<span class="tag">Ready</span>"#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span class="tag">Todo</span>"#),
        "{shown}"
    );
    assert!(!shown.contains("Why it waits"), "{shown}");
    assert!(
        shown.contains(r#"title="api:search">api:search</a><span class="r">free</span>"#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"data-copy="dibs claim gh:acme/widgets#21 --area api:search""#),
        "{shown}"
    );
    let plain = seeded.page(&format!("/ui/queue/{}", seeded.plain)).await;
    let shown = inspector(&plain);
    assert!(
        shown.contains("Areas it will probably touch · 0"),
        "{shown}"
    );
    assert!(
        shown.contains("The tracker names no area for it."),
        "{shown}"
    );
    assert!(
        shown.contains(r#"data-copy="dibs claim gh:acme/widgets#23""#),
        "{shown}"
    );
}

#[tokio::test]
async fn a_ticket_whose_claim_went_silent_is_ready_and_says_who_held_it() {
    let (world, alice, bob, bot) = project().await;
    let ids = import(
        &world,
        &bot,
        json!([{"ref": "gh:acme/widgets#30", "title": "Left behind"}]),
    )
    .await;
    let desk = world.session(&bob, "bob-desk").await;
    claim(&world, &bob, desk, ids[0], json!([])).await;
    world.fakes.clock.advance(Duration::days(10));
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, &format!("/ui/queue/{}", ids[0])).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let listed = row(&page, ids[0]);
    assert!(
        listed.contains("<span>was held by <a class=\"who\""),
        "{listed}"
    );
    assert!(
        !listed.contains(r#"<span class="tag">ready</span>"#),
        "{listed}"
    );
    let shown = inspector(&page);
    assert!(shown.contains("<dt>Was held by</dt>"), "{shown}");
    assert!(
        shown.contains(r#"<span class="faint">a silent claim, open to a take-over</span>"#),
        "{shown}"
    );
}

#[tokio::test]
async fn an_address_of_no_ticket_in_the_queue_answers_404_with_the_page_and_leads_to_a_claim() {
    let seeded = Seeded::new().await;
    let claimed = seeded
        .page(&format!("/ui/queue/{}", seeded.bob_billing))
        .await;
    assert_eq!(claimed.status, StatusCode::NOT_FOUND, "{}", claimed.text);
    let shown = inspector(&claimed);
    assert!(
        shown.contains("That ticket is not in the queue."),
        "{shown}"
    );
    assert!(
        shown.contains(&format!(
            r#"<a class="lnk" href="/ui/claims/{}">See its claim</a>"#,
            seeded.bob_billing
        )),
        "{shown}"
    );
    assert!(shown.contains(r#"data-selected="true""#), "{shown}");
    assert_eq!(rows(&claimed, &seeded.queued()), seeded.queued());
    for address in ["/ui/queue/9999", "/ui/queue/abc", "/ui/queue/0"] {
        let gone = seeded.page(address).await;
        assert_eq!(gone.status, StatusCode::NOT_FOUND, "{address}");
        let shown = inspector(&gone);
        assert!(
            shown.contains("That ticket is not in the queue."),
            "{address}: {shown}"
        );
        assert!(!shown.contains("See its claim"), "{address}: {shown}");
        assert_eq!(rows(&gone, &seeded.queued()), seeded.queued(), "{address}");
    }
}

#[tokio::test]
async fn what_members_wrote_in_a_title_is_escaped() {
    let seeded = Seeded::new().await;
    let page = seeded.page(&format!("/ui/queue/{}", seeded.bold)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains("Bold &#60;b&#62;move&#60;/b&#62;"),
        "{}",
        page.text
    );
    assert!(!page.text.contains("<b>move"), "{}", page.text);
}
