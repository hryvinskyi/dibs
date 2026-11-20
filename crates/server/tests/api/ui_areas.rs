//! Areas (`/ui/areas`) through the real router: a seat map with a section for each repository, the
//! held areas before the free ones and each seat styled by how its holders leave it, the cap on a
//! repository that declares many areas, the text filter, an area in the inspector with its paths
//! linked to the forge's directory pages, the claims that hold it and the tickets that will touch
//! it, and an address of no declared area.

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Person, World};
use crate::ui_support::{Browser, Page, claim, sign_in};

/// A project whose repository `api` is `acme/widgets` on GitHub, with the areas `billing` (which
/// depends on `search`), `search` and `docs`, and whose repository `web` is on a forge dibs does not
/// know, with the areas `pages` and `reports`; a claim goes silent after nine days.
fn settings() -> Value {
    json!({
        "stale_after": "9d",
        "repos": [
            {
                "id": "api",
                "remote": "https://github.com/acme/widgets",
                "areas": [{"name": "billing", "paths": ["src/billing/**"]},
                          {"name": "search", "paths": ["src/search/*.rs"]},
                          {"name": "docs", "paths": ["**/*.md"]}],
                "area_deps": {"billing": ["api:search"]}
            },
            {
                "id": "web",
                "remote": "https://git.example.com/acme/pages",
                "areas": [{"name": "pages", "paths": ["app/pages/**"]},
                          {"name": "reports", "paths": ["app/reports/**"]}]
            }
        ]
    })
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

/// A project of alice (the viewer), bob and an importer.
struct Seeded {
    world: World,
    browser: Browser,
    /// bob's claim on `api:billing`, held for eight days and still live.
    billing: u64,
    /// alice's claim on `api:search`, taken now; its title holds markup.
    search: u64,
    /// bob's claim on `web:pages`, on an item its tracker closed.
    pages: u64,
}

impl Seeded {
    /// The project with `wanted` tickets that name `api:billing`.
    async fn new(wanted: usize) -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let bot = world.member(&alice, "bot", Role::Importer).await;
        world.settings(&alice, settings()).await;
        let ids = import(
            &world,
            &bot,
            json!([
                {"ref": "gh:acme/widgets#10", "title": "Bob billing work"},
                {"ref": "gh:acme/widgets#11", "title": "Alice <b>search</b> work"},
                {"ref": "gh:acme/widgets#12", "title": "Bob pages work"}
            ]),
        )
        .await;
        let [billing, search, pages] = ids[..] else {
            panic!("three items were listed: {ids:?}");
        };
        let desk = world.session(&bob, "bob-desk").await;
        claim(&world, &bob, desk, billing, json!(["api:billing"])).await;
        world
            .fakes
            .clock
            .advance(Duration::days(8) + Duration::hours(1));
        let laptop = world.session(&alice, "alice-laptop").await;
        claim(&world, &alice, laptop, search, json!(["api:search"])).await;
        let bob_laptop = world.session(&bob, "bob-laptop").await;
        claim(&world, &bob, bob_laptop, pages, json!(["web:pages"])).await;
        let closed = world
            .put(
                &bot,
                "/v1/items",
                json!({"items": [{"ref": "gh:acme/widgets#12", "title": "Bob pages work",
                                  "closed": true}]}),
            )
            .await;
        assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
        let touching: Vec<Value> = (0..wanted)
            .map(|number| {
                json!({"ref": format!("gh:acme/widgets#{}", 30 + number),
                       "title": format!("Billing chore {number}"), "priority": 1,
                       "areas_hint": ["api:billing"]})
            })
            .collect();
        import(&world, &bot, Value::Array(touching)).await;
        let browser = sign_in(&world, &alice).await;
        Self {
            world,
            browser,
            billing,
            search,
            pages,
        }
    }

    async fn page(&self, path: &str) -> Page {
        self.browser.get(&self.world, path).await
    }
}

/// The markup of the seat of the area `full`, from its opening tag to its closing one.
fn seat<'a>(page: &'a Page, full: &str) -> &'a str {
    let at = page
        .text
        .find(&format!(r#"href="/ui/areas/{full}"#))
        .unwrap_or_else(|| panic!("no seat of {full} in {}", page.text));
    let start = page.text[..at].rfind("<a ").unwrap();
    let end = page.text[at..].find("</a>").unwrap() + at + "</a>".len();
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

/// The colour class of a member's avatar on the page.
fn colour_of(page: &Page, handle: &str) -> String {
    let marker =
        format!(r#"<a class="who" href="/ui/claims?who={handle}"><span class="av av--s pc-"#);
    let start = page
        .text
        .find(&marker)
        .unwrap_or_else(|| panic!("no {handle} in {}", page.text))
        + marker.len();
    let digits: String = page.text[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    format!("pc-{digits}")
}

#[tokio::test]
async fn each_repository_has_a_section_with_its_counts_and_the_held_seats_come_first() {
    let seeded = Seeded::new(0).await;
    let page = seeded.page("/ui/areas").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(r#"<h1 tabindex="-1">Areas</h1>"#),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains(r#"<span class="dim">3 of 5 held</span>"#),
        "{}",
        page.text
    );
    let api = page
        .text
        .find(r#"<h2>api<span class="faint">2 held · 1 free</span></h2>"#)
        .unwrap_or_else(|| panic!("no api section in {}", page.text));
    let web = page
        .text
        .find(r#"<h2>web<span class="faint">1 held · 1 free</span></h2>"#)
        .unwrap_or_else(|| panic!("no web section in {}", page.text));
    assert!(api < web, "{}", page.text);
    let at = |full: &str| {
        page.text
            .find(&format!(r#"href="/ui/areas/{full}""#))
            .unwrap()
    };
    assert!(api < at("api:billing") && at("api:billing") < at("api:search"));
    assert!(at("api:search") < at("api:docs") && at("api:docs") < web);
    assert!(web < at("web:pages") && at("web:pages") < at("web:reports"));
    assert!(
        page.text
            .contains(r#"data-region="inspector" data-selected="false""#),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(
            "Pick an area to see who holds it, for which tickets, and which tickets wait for it."
        ),
        "{}",
        page.text
    );
}

#[tokio::test]
async fn a_seat_is_styled_by_who_holds_it_and_how() {
    let seeded = Seeded::new(0).await;
    let page = seeded.page("/ui/areas").await;
    let bob = colour_of(&page, "bob");
    let alice = colour_of(&page, "alice");
    let parked = seat(&page, "api:billing");
    assert!(
        parked.starts_with(&format!(r#"<a class="seat seat--held seat--parked {bob}""#)),
        "{parked}"
    );
    assert!(
        parked.contains(r#"title="api:billing · bob for gh:acme/widgets#10""#),
        "{parked}"
    );
    let working = seat(&page, "api:search");
    assert!(
        working.starts_with(&format!(r#"<a class="seat seat--held {alice}""#)),
        "{working}"
    );
    let finished = seat(&page, "web:pages");
    assert!(
        finished.starts_with(&format!(r#"<a class="seat seat--held seat--fin {bob}""#)),
        "{finished}"
    );
    let free = seat(&page, "api:docs");
    assert!(free.starts_with(r#"<a class="seat" "#), "{free}");
    assert!(free.contains(r#"title="api:docs · free""#), "{free}");
    assert!(free.ends_with(">docs</a>"), "{free}");
    assert!(
        page.text.contains(r#"<span><i class="seat seat--fin"></i>held for a finished ticket</span><span><i class="seat seat--held seat--parked pc-1"></i>held over a week</span><span><i class="seat"></i>free</span>"#),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains(r#"<a class="who" href="/ui/claims?who=alice">"#),
        "{}",
        page.text
    );
}

#[tokio::test]
async fn a_seat_is_the_row_and_the_selected_one_is_current() {
    let seeded = Seeded::new(0).await;
    let page = seeded.page("/ui/areas/api:search").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    for full in [
        "api:billing",
        "api:search",
        "api:docs",
        "web:pages",
        "web:reports",
    ] {
        let markup = seat(&page, full);
        assert!(
            markup.contains(&format!(
                r#" data-row data-inspect href="/ui/areas/{full}""#
            )),
            "{markup}"
        );
        assert_eq!(
            markup.contains(r#"aria-current="true""#),
            full == "api:search",
            "{markup}"
        );
    }
    assert_eq!(page.text.matches("aria-current=\"true\"").count(), 1);
    let narrowed = seeded.page("/ui/areas/api:search?q=search").await;
    assert!(
        seat(&narrowed, "api:search").contains(r#"href="/ui/areas/api:search?q=search""#),
        "{}",
        narrowed.text
    );
}

#[tokio::test]
async fn a_repository_that_declares_many_areas_leaves_its_free_ones_behind_a_link() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let areas: Vec<Value> = (0..85)
        .map(|number| json!({"name": format!("part-{number:02}"), "paths": [format!("src/part-{number:02}/**")]}))
        .collect();
    world
        .settings(
            &alice,
            json!({"repos": [{"id": "api", "remote": "https://github.com/acme/widgets",
                              "areas": areas}]}),
        )
        .await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, "/ui/areas").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert_eq!(page.text.matches("data-row").count(), 0, "{}", page.text);
    assert!(
        page.text
            .contains(r#"<h2>api<span class="faint">0 held · 85 free</span></h2>"#),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains(r#"<a class="seat" href="/ui/areas?all=1">+85 free</a>"#),
        "{}",
        page.text
    );
    let every = browser.get(&world, "/ui/areas?all=1").await;
    assert_eq!(every.text.matches("data-row").count(), 85, "{}", every.text);
    assert!(!every.text.contains("+85 free"), "{}", every.text);
    let narrowed = browser.get(&world, "/ui/areas?q=part-07").await;
    assert_eq!(
        narrowed.text.matches("data-row").count(),
        1,
        "{}",
        narrowed.text
    );
}

#[tokio::test]
async fn a_repository_whose_areas_are_all_held_offers_no_free_ones() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let names: Vec<String> = (0..85).map(|number| format!("part-{number:02}")).collect();
    let areas: Vec<Value> = names
        .iter()
        .map(|name| json!({"name": name, "paths": [format!("src/{name}/**")]}))
        .collect();
    world
        .settings(
            &alice,
            json!({"repos": [{"id": "api", "remote": "https://github.com/acme/widgets",
                              "areas": areas}]}),
        )
        .await;
    let item = world
        .item(&alice, Some("gh:acme/widgets#1"), "Everything")
        .await;
    let session = world.session(&alice, "alice-laptop").await;
    let held: Vec<String> = names.iter().map(|name| format!("api:{name}")).collect();
    claim(&world, &alice, session, item, json!(held)).await;
    let browser = sign_in(&world, &alice).await;

    let page = browser.get(&world, "/ui/areas").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text
            .contains(r#"<h2>api<span class="faint">85 held · 0 free</span></h2>"#),
        "{}",
        page.text
    );
    assert_eq!(page.text.matches("data-row").count(), 85, "{}", page.text);
    assert!(!page.text.contains(" free</a>"), "{}", page.text);

    let narrowed = browser.get(&world, "/ui/areas?q=part-07").await;
    assert_eq!(
        narrowed.text.matches("data-row").count(),
        1,
        "{}",
        narrowed.text
    );
    assert!(!narrowed.text.contains(" free</a>"), "{}", narrowed.text);
}

#[tokio::test]
async fn an_area_held_by_two_claims_names_each_with_its_ticket() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    world.settings(&alice, settings()).await;
    let session = world.session(&alice, "alice-laptop").await;
    let first = world.item(&alice, Some("gh:acme/widgets#1"), "First").await;
    let second = world
        .item(&alice, Some("gh:acme/widgets#2"), "Second")
        .await;
    claim(&world, &alice, session, first, json!(["api:billing"])).await;
    claim(&world, &alice, session, second, json!(["api:billing"])).await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, "/ui/areas").await;
    let held = seat(&page, "api:billing");
    assert!(
        held.contains(
            r#"title="api:billing · alice for gh:acme/widgets#1, alice for gh:acme/widgets#2""#
        ),
        "{held}"
    );
    assert!(held.contains(r#"<span class="k">2</span>"#), "{held}");
}

#[tokio::test]
async fn the_text_leaves_the_areas_whose_name_or_path_it_matches() {
    let seeded = Seeded::new(0).await;
    let by_name = seeded.page("/ui/areas?q=bill").await;
    assert_eq!(
        by_name.text.matches("data-row").count(),
        1,
        "{}",
        by_name.text
    );
    assert!(
        by_name
            .text
            .contains(r#"href="/ui/areas/api:billing?q=bill""#)
    );
    assert!(
        by_name
            .text
            .contains(r#"<h2>api<span class="faint">1 held · 0 free</span></h2>"#),
        "{}",
        by_name.text
    );
    assert!(!by_name.text.contains("<h2>web<"), "{}", by_name.text);
    assert!(by_name.text.contains(r#"value="bill""#), "{}", by_name.text);
    let by_path = seeded.page("/ui/areas?q=app%2Freports").await;
    assert_eq!(
        by_path.text.matches("data-row").count(),
        1,
        "{}",
        by_path.text
    );
    assert!(
        by_path
            .text
            .contains(r#"href="/ui/areas/web:reports?q=app%2Freports""#)
    );
    let none = seeded.page("/ui/areas?q=nothing+like+it").await;
    assert!(
        none.text
            .contains(r#"<p class="note">No area matches.</p>"#),
        "{}",
        none.text
    );
    assert_eq!(none.text.matches("data-row").count(), 0);
}

#[tokio::test]
async fn a_project_that_declares_no_area_says_so() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    world.settings(&alice, json!({})).await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, "/ui/areas").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text
            .contains(r#"<p class="note">The project declares no areas.</p>"#),
        "{}",
        page.text
    );
}

#[tokio::test]
async fn the_inspector_links_each_declared_path_to_the_forges_directory_or_to_the_search() {
    let seeded = Seeded::new(0).await;
    let billing = seeded.page("/ui/areas/api:billing").await;
    assert_eq!(billing.status, StatusCode::OK, "{}", billing.text);
    assert!(
        inspector(&billing).contains(
            r#"<dt>Paths</dt><dd><span class="file"><a href="https://github.com/acme/widgets/tree/HEAD/src/billing" title="api:src/billing" target="_blank" rel="noopener noreferrer">src/billing/**</a></span></dd>"#
        ),
        "{}",
        inspector(&billing)
    );
    let search = seeded.page("/ui/areas/api:search").await;
    assert!(
        inspector(&search).contains(
            r#"<a href="https://github.com/acme/widgets/tree/HEAD/src/search" title="api:src/search" target="_blank" rel="noopener noreferrer">src/search/*.rs</a>"#
        ),
        "{}",
        inspector(&search)
    );
    let docs = seeded.page("/ui/areas/api:docs").await;
    assert!(
        inspector(&docs).contains(
            r#"<a href="https://github.com/acme/widgets/tree/HEAD" title="api:/" target="_blank" rel="noopener noreferrer">**/*.md</a>"#
        ),
        "{}",
        inspector(&docs)
    );
    let pages = seeded.page("/ui/areas/web:pages").await;
    assert!(
        inspector(&pages).contains(
            r#"<a href="/ui/search?q=web%3Aapp%2Fpages" title="web:app/pages">app/pages/**</a>"#
        ),
        "{}",
        inspector(&pages)
    );
}

#[tokio::test]
async fn the_inspector_of_a_held_area_names_its_claims_and_how_they_stand() {
    let seeded = Seeded::new(0).await;
    let billing = seeded.page("/ui/areas/api:billing?q=bill").await;
    let shown = inspector(&billing);
    assert!(
        shown.contains(r#"aria-label="Area api:billing""#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span class="ref">api:billing</span><span class="tag">Held</span>"#),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<button class="mini" type="button" data-copy-link hidden>Copy link</button><a class="x" data-inspect href="/ui/areas?q=bill" aria-label="Close">"#),
        "{shown}"
    );
    assert!(
        shown.contains(
            r#"<h2 data-inspector-heading tabindex="-1"><a class="who" href="/ui/claims?who=bob">"#
        ),
        "{shown}"
    );
    assert!(shown.contains("</a> holds billing</h2>"), "{shown}");
    assert!(
        shown.contains(r#"<dt>Depends on</dt><dd><a class="area" href="/ui/areas/api:search" title="api:search">api:search</a></dd>"#),
        "{shown}"
    );
    assert!(shown.contains("Held for"), "{shown}");
    assert!(shown.contains(r#"title="gh:acme/widgets#10""#), "{shown}");
    assert!(shown.contains("Bob billing work"), "{shown}");
    assert!(
        shown.contains(&format!(
            r#"<a class="mini" href="/ui/claims/{}">See the claim</a>"#,
            seeded.billing
        )),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<span class="tag tag--warn">held over a week</span>"#),
        "{shown}"
    );
    let search = seeded.page("/ui/areas/api:search").await;
    let shown = inspector(&search);
    assert!(
        shown.contains(r#"<span class="r">held just now</span>"#),
        "{shown}"
    );
    assert!(
        shown.contains("Alice &#60;b&#62;search&#60;/b&#62; work"),
        "{shown}"
    );
    assert!(!shown.contains("<b>search</b>"), "{shown}");
    assert!(
        shown.contains(&format!(
            r#"href="/ui/claims/{}">See the claim</a>"#,
            seeded.search
        )),
        "{shown}"
    );
    let pages = seeded.page("/ui/areas/web:pages").await;
    assert!(
        inspector(&pages).contains(r#"<span class="tag tag--bad">ticket closed</span>"#),
        "{}",
        inspector(&pages)
    );
    assert!(
        inspector(&pages).contains(&format!(
            r#"href="/ui/claims/{}">See the claim</a>"#,
            seeded.pages
        )),
        "{}",
        inspector(&pages)
    );
}

#[tokio::test]
async fn the_inspector_of_a_free_area_says_it_is_free_to_claim() {
    let seeded = Seeded::new(0).await;
    let docs = seeded.page("/ui/areas/api:docs").await;
    let shown = inspector(&docs);
    assert!(
        shown.contains(r#"<span class="ref">api:docs</span><span class="tag">Free</span>"#),
        "{shown}"
    );
    assert!(shown.contains("docs is free to claim</h2>"), "{shown}");
    assert!(!shown.contains("Held for"), "{shown}");
    assert!(!shown.contains("Depends on"), "{shown}");
    assert!(
        shown.contains("Tickets that will probably touch it · 0"),
        "{shown}"
    );
    assert!(
        shown.contains(r#"<p class="faint">No open ticket names it.</p>"#),
        "{shown}"
    );
}

#[tokio::test]
async fn the_inspector_lists_six_tickets_that_will_touch_the_area_and_offers_the_rest() {
    let six = Seeded::new(6).await;
    let page = six.page("/ui/areas/api:billing").await;
    let shown = inspector(&page);
    assert!(
        shown.contains("Tickets that will probably touch it · 6"),
        "{shown}"
    );
    assert_eq!(
        shown.matches(r#"<span class="r">P1</span>"#).count(),
        6,
        "{shown}"
    );
    assert!(shown.contains("Billing chore 0"), "{shown}");
    assert!(shown.contains(r#"title="gh:acme/widgets#30""#), "{shown}");
    assert!(!shown.contains("See all"), "{shown}");
    let seven = Seeded::new(7).await;
    let page = seven.page("/ui/areas/api:billing").await;
    let shown = inspector(&page);
    assert!(
        shown.contains("Tickets that will probably touch it · 7"),
        "{shown}"
    );
    assert_eq!(
        shown.matches(r#"<span class="r">P1</span>"#).count(),
        6,
        "{shown}"
    );
    assert!(
        shown.contains(
            r#"<a class="mini" href="/ui/queue?area=api%3Abilling">See all 7 in the queue</a>"#
        ),
        "{shown}"
    );
    let queue = seven.page("/ui/queue?area=api%3Abilling").await;
    let (ready, waiting) = queue
        .text
        .split_once(r#"<span class="dim">"#)
        .and_then(|(_, rest)| rest.split_once("</span>"))
        .and_then(|(counts, _)| counts.split_once(" ready · "))
        .and_then(|(ready, rest)| {
            Some((
                ready.parse::<usize>().ok()?,
                rest.strip_suffix(" not ready")?.parse::<usize>().ok()?,
            ))
        })
        .unwrap_or_default();
    assert_eq!(
        ready + waiting,
        7,
        "the link lists exactly the tickets it counts: {}",
        queue.text
    );
}

#[tokio::test]
async fn an_area_the_project_does_not_declare_answers_not_found_with_the_whole_page() {
    let seeded = Seeded::new(0).await;
    for path in [
        "/ui/areas/api:nope",
        "/ui/areas/nope",
        "/ui/areas/web:billing",
    ] {
        let page = seeded.page(path).await;
        assert_eq!(page.status, StatusCode::NOT_FOUND, "{path}");
        assert!(
            page.text.contains(r#"<h1 tabindex="-1">Areas</h1>"#),
            "{path}: {}",
            page.text
        );
        assert!(
            page.text
                .contains(r#"data-region="inspector" data-selected="true""#),
            "{path}: {}",
            page.text
        );
        assert!(
            page.text.contains("The project declares no such area."),
            "{path}: {}",
            page.text
        );
        assert!(
            page.text.contains(r#"href="/ui/areas/api:billing""#),
            "{path}"
        );
        assert!(!page.text.contains(r#"aria-current="true""#), "{path}");
    }
    let closed = seeded.page("/ui/areas/nope?q=x").await;
    assert!(
        closed
            .text
            .contains(r#"data-inspect href="/ui/areas?q=x" aria-label="Close""#),
        "{}",
        closed.text
    );
}
