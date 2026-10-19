//! The browser pages through the real router: signing in and out, the form tokens, the security
//! headers, escaping, the event stream over the session cookie, sessions that end, cookies the
//! server did not set, the static bundle, the board's columns, the take-over form, and a thread's
//! importance and acknowledgements.

use std::time::Duration as StdDuration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;
use dibs_server::ui::{
    BUNDLE_HASH, CONTENT_SECURITY_POLICY, LOGIN_COOKIE, SESSION_COOKIE, ZONE_COOKIE,
};

use crate::support::{Person, World, two_repositories};

/// An answer read as text.
struct Page {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
}

impl Page {
    /// The value of the cookie this answer sets under `name`, if it sets one.
    fn set_cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| {
                let (pair, _) = value.split_once(';').unwrap_or((value, ""));
                let (key, value) = pair.split_once('=')?;
                (key == name).then(|| value.to_owned())
            })
    }

    /// The whole `Set-Cookie` line of the cookie `name`.
    fn set_cookie_line(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(&format!("{name}=")))
            .map(ToOwned::to_owned)
    }

    /// The value of the first hidden form token on the page.
    fn form_token(&self) -> String {
        let start = self
            .text
            .find(r#"name="csrf" value=""#)
            .unwrap_or_else(|| panic!("no form token in {}", self.text))
            + r#"name="csrf" value=""#.len();
        let end = self.text[start..].find('"').unwrap() + start;
        self.text[start..end].to_owned()
    }

    /// Where a redirect goes.
    fn location(&self) -> &str {
        self.headers[header::LOCATION].to_str().unwrap()
    }

    /// The markup of the board column labelled `id`.
    fn column(&self, id: &str) -> &str {
        let start = self
            .text
            .find(&format!(r#"aria-labelledby="{id}""#))
            .unwrap();
        let end = self.text[start..].find("</section>").unwrap() + start;
        &self.text[start..end]
    }
}

/// Form-encodes the pairs.
fn form(pairs: &[(&str, &str)]) -> String {
    let encode = |text: &str| {
        text.bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    char::from(byte).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect::<String>()
    };
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Sends a request with these cookies (`name=value; …`) and, when given, a form body.
async fn fetch(
    world: &World,
    method: Method,
    path: &str,
    cookie: Option<&str>,
    body: Option<String>,
) -> Page {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = world.open(request).await;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Page {
        status,
        headers,
        text: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

/// A browser signed in as someone: its cookie and the form token its pages carry.
struct Browser {
    cookie: String,
    token: String,
}

impl Browser {
    async fn get(&self, world: &World, path: &str) -> Page {
        fetch(world, Method::GET, path, Some(&self.cookie), None).await
    }

    async fn post(&self, world: &World, path: &str, pairs: &[(&str, &str)]) -> Page {
        fetch(
            world,
            Method::POST,
            path,
            Some(&self.cookie),
            Some(form(pairs)),
        )
        .await
    }
}

/// Renders the sign-in form and posts `token` with the form's cookie; answers the post's answer.
async fn post_sign_in(world: &World, token: &str) -> Page {
    let shown = fetch(world, Method::GET, "/ui/login", None, None).await;
    assert_eq!(shown.status, StatusCode::OK);
    let nonce = shown.set_cookie(LOGIN_COOKIE).unwrap();
    assert_eq!(shown.form_token(), nonce);
    fetch(
        world,
        Method::POST,
        "/ui/login",
        Some(&format!("{LOGIN_COOKIE}={nonce}")),
        Some(form(&[("csrf", &nonce), ("token", token)])),
    )
    .await
}

/// Signs `who` in through the form.
async fn sign_in(world: &World, who: &Person) -> Browser {
    let signed = post_sign_in(world, &format!("  {}\n", who.token)).await;
    assert_eq!(signed.status, StatusCode::SEE_OTHER, "{}", signed.text);
    assert_eq!(signed.location(), "/ui/");
    let secret = signed.set_cookie(SESSION_COOKIE).unwrap();
    let cookie = format!("{SESSION_COOKIE}={secret}");
    let status = fetch(world, Method::GET, "/ui/", Some(&cookie), None).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    Browser {
        token: status.form_token(),
        cookie,
    }
}

/// A project with alice (admin) and bob, its settings declaring two repositories.
async fn team() -> (World, Person, Person) {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    world.settings(&alice, two_repositories()).await;
    (world, alice, bob)
}

async fn claim(world: &World, who: &Person, session: u64, item: u64, areas: Value) {
    let answer = world
        .post(
            who,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": session, "areas": areas}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
}

#[tokio::test]
async fn a_good_token_opens_a_session_whose_cookie_carries_every_flag() {
    let (world, alice, _) = team().await;
    let signed = post_sign_in(&world, &alice.token).await;
    assert_eq!(signed.status, StatusCode::SEE_OTHER);
    let line = signed.set_cookie_line(SESSION_COOKIE).unwrap();
    for flag in [
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/ui",
        "Max-Age=43200",
    ] {
        assert!(line.contains(flag), "{line}");
    }
    let secret = signed.set_cookie(SESSION_COOKIE).unwrap();
    assert!(secret.starts_with("dibs_") && secret != alice.token);
    assert_eq!(signed.set_cookie(LOGIN_COOKIE).as_deref(), Some(""));
    let page = fetch(
        &world,
        Method::GET,
        "/ui/",
        Some(&format!("{SESSION_COOKIE}={secret}")),
        None,
    )
    .await;
    assert!(
        page.text
            .contains("Signed in as <span class=\"account__who\">alice</span>")
    );
    let renewed = page.set_cookie_line(SESSION_COOKIE).unwrap();
    assert!(renewed.contains("Max-Age=43200"), "{renewed}");
}

#[tokio::test]
async fn a_bad_or_an_integrations_token_shows_the_form_again_with_one_sentence() {
    let (world, alice, _) = team().await;
    let bad = post_sign_in(&world, "dibs_not-a-token-anyone-was-ever-given-xyz").await;
    assert_eq!(bad.status, StatusCode::OK);
    assert!(
        bad.text.contains("That token wasn't recognised"),
        "{}",
        bad.text
    );
    assert_eq!(bad.text.matches("banner--danger").count(), 1);
    assert!(bad.set_cookie(SESSION_COOKIE).is_none());
    let mirror = world.member(&alice, "mirror", Role::Importer).await;
    let integration = post_sign_in(&world, &mirror.token).await;
    assert!(integration.text.contains("belongs to an integration"));
    assert!(integration.set_cookie(SESSION_COOKIE).is_none());
}

#[tokio::test]
async fn a_sign_in_without_the_forms_own_cookie_is_refused() {
    let (world, alice, _) = team().await;
    let shown = fetch(&world, Method::GET, "/ui/login", None, None).await;
    let nonce = shown.form_token();
    let forged = fetch(
        &world,
        Method::POST,
        "/ui/login",
        None,
        Some(form(&[("csrf", &nonce), ("token", &alice.token)])),
    )
    .await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert!(forged.text.contains("This sign-in form expired"));
    assert!(forged.set_cookie(SESSION_COOKIE).is_none());
    let mismatched = fetch(
        &world,
        Method::POST,
        "/ui/login",
        Some(&format!("{LOGIN_COOKIE}={nonce}")),
        Some(form(&[("csrf", "something-else"), ("token", &alice.token)])),
    )
    .await;
    assert_eq!(mismatched.status, StatusCode::FORBIDDEN);
    assert!(mismatched.set_cookie(SESSION_COOKIE).is_none());
}

#[tokio::test]
async fn every_page_answer_carries_the_security_headers() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    let answers = [
        fetch(&world, Method::GET, "/ui/login", None, None).await,
        browser.get(&world, "/ui/").await,
        browser.get(&world, "/ui/board").await,
        browser.get(&world, "/ui/threads").await,
        fetch(
            &world,
            Method::GET,
            "/ui/",
            Some("dibs_ui=dibs_gone-gone-gone-gone-gone-gone-gone"),
            None,
        )
        .await,
        browser
            .get(&world, &format!("/ui/static/{BUNDLE_HASH}/main.js"))
            .await,
    ];
    for page in answers {
        assert_eq!(
            page.headers[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        );
        assert_eq!(
            page.headers[header::CONTENT_SECURITY_POLICY],
            CONTENT_SECURITY_POLICY
        );
        assert_eq!(page.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(page.headers[header::REFERRER_POLICY], "no-referrer");
        assert!(!page.text.contains("<script>"), "an inline script");
        assert!(!page.text.contains(" style="), "an inline style");
        assert!(!page.text.contains(" onclick="), "an inline handler");
    }
}

#[tokio::test]
async fn a_post_without_the_sessions_form_token_changes_nothing() {
    let (world, alice, bob) = team().await;
    let thread = world
        .thread(&alice, json!(["bob"]), "Billing", "first")
        .await;
    let browser = sign_in(&world, &alice).await;
    let path = format!("/ui/threads/{thread}/messages");
    for token in [None, Some("0".repeat(64)), Some(String::new())] {
        let mut pairs = vec![("body", "hello")];
        if let Some(token) = token.as_deref() {
            pairs.push(("csrf", token));
        }
        let refused = browser.post(&world, &path, &pairs).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{token:?}");
        assert!(refused.text.contains("This form expired"));
    }
    let read = world.get(&alice, &format!("/v1/threads/{thread}")).await;
    assert_eq!(read.body["messages"].as_array().unwrap().len(), 1);
    let other = sign_in(&world, &bob).await;
    let foreign = browser
        .post(&world, "/ui/logout", &[("csrf", &other.token)])
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    assert_eq!(browser.get(&world, "/ui/").await.status, StatusCode::OK);
    let item = world.item(&bob, None, "Billing fix").await;
    let take = browser
        .post(
            &world,
            &format!("/ui/items/{item}/takeover"),
            &[("reason", "gone"), ("csrf", "x")],
        )
        .await;
    assert_eq!(take.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn signing_out_ends_the_session() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    let out = browser
        .post(&world, "/ui/logout", &[("csrf", &browser.token)])
        .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    assert_eq!(out.location(), "/ui/login");
    assert_eq!(out.set_cookie(SESSION_COOKIE).as_deref(), Some(""));
    let after = browser.get(&world, "/ui/").await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert!(after.text.contains("Your session ended"));
    let again = browser
        .post(&world, "/ui/logout", &[("csrf", &browser.token)])
        .await;
    assert_eq!(again.location(), "/ui/login");
}

#[tokio::test]
async fn a_thread_shows_a_script_as_text_and_a_reply_is_signed_from_the_web() {
    let (world, alice, bob) = team().await;
    let body = "<script>alert('x')</script>\nsecond line & more";
    let thread = world
        .thread(&bob, json!(["alice"]), "<b>Subject</b>", body)
        .await;
    let browser = sign_in(&world, &alice).await;
    let threads = browser.get(&world, "/ui/threads").await;
    assert!(threads.text.contains("thread-row--unread"));
    assert!(threads.text.contains("&#60;b&#62;Subject&#60;/b&#62;"));
    assert!(threads.text.contains("1 unread"));
    let page = browser.get(&world, &format!("/ui/threads/{thread}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.text.contains("<script>alert"));
    assert!(
        page.text.contains(
            "<p class=\"msg__body\">&#60;script&#62;alert(&#39;x&#39;)&#60;/script&#62;\nsecond line &#38; more</p>"
        ),
        "{}",
        page.text
    );
    assert!(page.text.contains(&format!("data-thread-id=\"{thread}\"")));
    let read = browser.get(&world, "/ui/threads").await;
    assert!(!read.text.contains("thread-row--unread"));
    let reply = browser
        .post(
            &world,
            &format!("/ui/threads/{thread}/messages"),
            &[("csrf", &browser.token), ("body", "on it\nтак")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text);
    assert!(
        reply
            .location()
            .starts_with(&format!("/ui/threads/{thread}#m-"))
    );
    let api = world.get(&alice, &format!("/v1/threads/{thread}")).await;
    let last = api.body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["label"], "web");
    assert_eq!(last["author"], "alice");
    assert_eq!(last["body"], "on it\nтак");
    let blank = browser
        .post(
            &world,
            &format!("/ui/threads/{thread}/messages"),
            &[("csrf", &browser.token), ("body", "  \n ")],
        )
        .await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(blank.text.contains("Write a message before sending."));
    let missing = browser.get(&world, "/ui/threads/999999").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert!(missing.text.contains("There is no such page"));
}

#[tokio::test]
async fn a_thread_shows_importance_and_acks_and_a_recipient_acks_through_the_form() {
    let (world, alice, bob) = team().await;
    let started = world
        .post(
            &alice,
            "/v1/threads",
            json!({"subject": "Staging is down", "to": ["bob"], "label": "laptop/mcp",
                   "body": "Do not deploy.", "importance": "urgent", "ack_required": true}),
        )
        .await;
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let message = started.body["message"]["id"].as_u64().unwrap();
    let status = || {
        let path = format!("/v1/messages/{message}/status");
        let (world, alice) = (&world, &alice);
        async move { world.get(alice, &path).await.body["recipients"][0]["acked_at"].clone() }
    };
    let browser = sign_in(&world, &bob).await;
    let list = browser.get(&world, "/ui/threads").await;
    assert!(list.text.contains("· 1 needs your ack"), "{}", list.text);
    assert!(list.text.contains("badge__icon--stop"), "{}", list.text);
    assert!(list.text.contains("Needs your ack"), "{}", list.text);

    let page = browser.get(&world, &format!("/ui/threads/{thread}")).await;
    assert!(page.text.contains("Urgent</span>"), "{}", page.text);
    assert!(page.text.contains("Acknowledgement requested"));
    assert!(
        page.text
            .contains(r#"<span class="ack__name">bob (you)</span> waiting"#),
        "{}",
        page.text
    );
    let action = format!(r#"action="/ui/threads/{thread}/messages/{message}/ack""#);
    assert!(page.text.contains(&action), "{}", page.text);
    let path = format!("/ui/threads/{thread}/messages/{message}/ack");
    for pairs in [vec![], vec![("csrf", "0".repeat(64))]] {
        let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let refused = browser.post(&world, &path, &pairs).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert!(refused.text.contains("This form expired"));
    }
    assert_eq!(status().await, Value::Null, "nothing was recorded");

    world.fakes.clock.advance(Duration::minutes(2));
    let acked = browser
        .post(&world, &path, &[("csrf", &browser.token)])
        .await;
    assert_eq!(acked.status, StatusCode::SEE_OTHER, "{}", acked.text);
    assert_eq!(
        acked.location(),
        format!("/ui/threads/{thread}#m-{message}")
    );
    assert_eq!(status().await, "2026-03-02T09:02:00Z");
    let after = browser.get(&world, &format!("/ui/threads/{thread}")).await;
    assert!(
        after
            .text
            .contains(r#"<span class="ack__name">bob (you)</span> acked"#),
        "{}",
        after.text
    );
    assert!(!after.text.contains(&action), "no button once acknowledged");
    let twice = browser
        .post(&world, &path, &[("csrf", &browser.token)])
        .await;
    assert_eq!(
        twice.status,
        StatusCode::SEE_OTHER,
        "a second click shows the thread"
    );
    assert_eq!(
        twice.location(),
        format!("/ui/threads/{thread}#m-{message}")
    );
    assert_eq!(
        status().await,
        "2026-03-02T09:02:00Z",
        "the first acknowledgement stands"
    );
    let listed = browser.get(&world, "/ui/threads").await;
    assert!(!listed.text.contains("Needs your ack"));

    let author = sign_in(&world, &alice).await;
    let authors = author.get(&world, &format!("/ui/threads/{thread}")).await;
    assert!(
        authors
            .text
            .contains(r#"<span class="ack__name">bob</span> acked"#),
        "{}",
        authors.text
    );
    assert!(
        !authors.text.contains(&action),
        "its author has nothing to ack"
    );
    assert_eq!(
        author
            .post(&world, &path, &[("csrf", &author.token)])
            .await
            .status,
        StatusCode::FORBIDDEN,
        "the author is not a recipient"
    );
    let asked = author
        .post(
            &world,
            &format!("/ui/threads/{thread}/messages"),
            &[
                ("csrf", &author.token),
                ("body", "Deploy is back."),
                ("require_ack", "1"),
            ],
        )
        .await;
    assert_eq!(asked.status, StatusCode::SEE_OTHER, "{}", asked.text);
    let api = world.get(&alice, &format!("/v1/threads/{thread}")).await;
    let last = api.body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["ack_required"], true);
    assert_eq!(last["importance"], "normal");
    let blank = author
        .post(
            &world,
            &format!("/ui/threads/{thread}/messages"),
            &[("csrf", &author.token), ("body", " "), ("require_ack", "1")],
        )
        .await;
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        blank
            .text
            .contains(r#"name="require_ack" value="1" checked"#),
        "the draft keeps its choice"
    );
}

#[tokio::test]
async fn the_stream_follows_a_signed_in_browser_and_refuses_one_without_a_session() {
    let (world, alice, bob) = team().await;
    world
        .thread(&bob, json!(["alice"]), "Billing", "hello")
        .await;
    let browser = sign_in(&world, &alice).await;
    let request = Request::builder()
        .uri("/ui/events/stream?after=0")
        .header(header::COOKIE, &browser.cookie)
        .body(Body::empty())
        .unwrap();
    let response = world.open(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut body = response.into_body();
    let mut text = String::new();
    while !text.contains("\"kind\":\"thread_started\"") {
        let frame = tokio::time::timeout(StdDuration::from_secs(5), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        text.push_str(std::str::from_utf8(&frame.into_data().unwrap()).unwrap());
    }
    assert!(text.starts_with("id: "), "{text}");
    for cookie in [
        None,
        Some("dibs_ui=dibs_never-issued-never-issued-never-issued"),
    ] {
        let refused = fetch(&world, Method::GET, "/ui/events/stream", cookie, None).await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{cookie:?}");
        let probe = fetch(&world, Method::GET, "/ui/session", cookie, None).await;
        assert_eq!(probe.status, StatusCode::UNAUTHORIZED);
    }
    let bearer = world.get(&alice, "/ui/events/stream").await;
    assert_eq!(bearer.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        browser.get(&world, "/ui/session").await.status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_revoked_token_ends_the_browser_sessions_it_opened() {
    let (world, alice, bob) = team().await;
    let browser = sign_in(&world, &bob).await;
    let prefix = &bob.token["dibs_".len().."dibs_".len() + 8];
    let revoked = world
        .delete(&alice, &format!("/v1/admin/tokens/{prefix}"))
        .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    let page = browser.get(&world, "/ui/board").await;
    assert_eq!(page.status, StatusCode::UNAUTHORIZED);
    assert!(page.text.contains("Your session ended — sign in again"));
    assert_eq!(page.set_cookie(SESSION_COOKIE).as_deref(), Some(""));
    let stream = browser.get(&world, "/ui/events/stream").await;
    assert_eq!(stream.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_expired_session_shows_the_session_ended_page() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    world.fakes.clock.advance(Duration::hours(11));
    assert_eq!(browser.get(&world, "/ui/").await.status, StatusCode::OK);
    world.fakes.clock.advance(Duration::hours(11));
    assert_eq!(browser.get(&world, "/ui/").await.status, StatusCode::OK);
    world.fakes.clock.advance(Duration::hours(13));
    let ended = browser.get(&world, "/ui/").await;
    assert_eq!(ended.status, StatusCode::UNAUTHORIZED);
    assert!(ended.text.contains("Your session ended"));
}

#[tokio::test]
async fn a_cookie_the_server_did_not_set_signs_nobody_in() {
    let (world, alice, _) = team().await;
    let token_as_cookie = format!("{SESSION_COOKIE}={}", alice.token);
    let page = fetch(&world, Method::GET, "/ui/", Some(&token_as_cookie), None).await;
    assert_eq!(page.status, StatusCode::UNAUTHORIZED);
    assert!(!page.text.contains("alice"));
    let stream = fetch(
        &world,
        Method::GET,
        "/ui/events/stream",
        Some(&token_as_cookie),
        None,
    )
    .await;
    assert_eq!(stream.status, StatusCode::UNAUTHORIZED);
    for cookie in ["session=abc; dibs_ui_x=1", "garbage"] {
        let other = fetch(&world, Method::GET, "/ui/", Some(cookie), None).await;
        assert_eq!(other.status, StatusCode::SEE_OTHER, "{cookie}");
        assert_eq!(other.location(), "/ui/login");
    }
    let bearer = world.get(&alice, "/ui/").await;
    assert_eq!(bearer.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn the_bundle_is_served_under_its_directory_and_its_imports_resolve_there() {
    let world = World::new().await;
    let login = fetch(&world, Method::GET, "/ui/login", None, None).await;
    let stylesheet = format!("/ui/static/{BUNDLE_HASH}/dibs.css");
    assert!(login.text.contains(&format!("href=\"{stylesheet}\"")));
    let css = fetch(&world, Method::GET, &stylesheet, None, None).await;
    assert_eq!(css.status, StatusCode::OK);
    assert_eq!(css.headers[header::CONTENT_TYPE], "text/css; charset=utf-8");
    let mut pending = vec!["main.js".to_owned()];
    let mut served = Vec::new();
    while let Some(name) = pending.pop() {
        if served.contains(&name) {
            continue;
        }
        let path = format!("/ui/static/{BUNDLE_HASH}/{name}");
        let module = fetch(&world, Method::GET, &path, None, None).await;
        assert_eq!(module.status, StatusCode::OK, "{path}");
        assert_eq!(
            module.headers[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            module.headers[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        for line in module.text.lines() {
            if let Some((_, rest)) = line.split_once("from './") {
                pending.push(rest.split('\'').next().unwrap().to_owned());
            }
        }
        served.push(name);
    }
    assert!(served.len() > 2, "{served:?}");
    let elsewhere = fetch(
        &world,
        Method::GET,
        "/ui/static/0000000000000000/main.js",
        None,
        None,
    )
    .await;
    assert_eq!(elsewhere.status, StatusCode::NOT_FOUND);
    let unknown = fetch(
        &world,
        Method::GET,
        &format!("/ui/static/{BUNDLE_HASH}/missing.js"),
        None,
        None,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

/// A board with one item in each column: open, claimed live by bob, claimed by alice and gone
/// stale, done by bob today.
async fn board() -> (World, Person, Person) {
    let (world, alice, bob) = team().await;
    let alice_session = world.session(&alice, "laptop").await;
    world
        .item(&alice, Some("gh:acme/api#1"), "Ready <item>")
        .await;
    let stale = world
        .item(&alice, Some("gh:acme/api#2"), "Stale item")
        .await;
    let live = world.item(&alice, Some("gh:acme/web#3"), "Live item").await;
    let done = world.item(&alice, None, "Done item").await;
    claim(&world, &alice, alice_session, stale, json!(["api:billing"])).await;
    world.fakes.clock.advance(Duration::hours(2));
    let bob_session = world.session(&bob, "desk").await;
    claim(&world, &bob, bob_session, live, json!(["web:checkout"])).await;
    claim(&world, &bob, bob_session, done, json!([])).await;
    let completed = world
        .post(&bob, &format!("/v1/items/{done}/done"), json!({}))
        .await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.body);
    (world, alice, bob)
}

#[tokio::test]
async fn the_board_puts_each_item_in_its_column_and_filters_them() {
    let (world, _alice, bob) = board().await;
    let browser = sign_in(&world, &bob).await;
    let page = browser.get(&world, "/ui/board").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let ready = page.column("c-ready");
    assert!(
        ready.contains("gh:acme/api#1") && ready.contains("Ready &#60;item&#62;"),
        "{ready}"
    );
    assert!(
        !ready.contains("Live item"),
        "a live claim takes an item off the ready list"
    );
    assert!(
        ready.contains("Stale item")
            && ready.contains("Stale claim by <span class=\"member\">alice</span>"),
        "a stale claim leaves its item ready, marked: {ready}"
    );
    assert!(
        ready.contains("href=\"https://github.com/acme/api/issues/1\" rel=\"noopener noreferrer\""),
        "{ready}"
    );
    let progress = page.column("c-prog");
    assert!(
        progress.contains("Live item") && progress.contains("web:checkout"),
        "{progress}"
    );
    assert!(!progress.contains("Stale item"));
    let stale = page.column("c-stale");
    assert!(
        stale.contains("Stale item") && stale.contains("alice"),
        "{stale}"
    );
    assert!(stale.contains("Take over claim"));
    let done = page.column("c-done");
    assert!(done.contains("Done item") && done.contains("bob"), "{done}");
    assert!(!page.text.contains("<option selected>"));
    assert!(page.text.contains("<option>api:billing</option>"));
    assert!(page.text.contains("<option>bob</option>"));
    let mine = browser.get(&world, "/ui/board?member=bob").await;
    assert!(mine.text.contains("<option selected>bob</option>"));
    assert!(mine.column("c-prog").contains("Live item"));
    assert!(!mine.column("c-stale").contains("Stale item"));
    assert!(mine.column("c-done").contains("Done item"));
    assert!(
        !mine.column("c-ready").contains("Ready &#60;item"),
        "nobody holds a ready item"
    );
    let billing = browser.get(&world, "/ui/board?area=api%3Abilling").await;
    assert!(billing.column("c-stale").contains("Stale item"));
    assert!(!billing.column("c-prog").contains("Live item"));
    assert!(!billing.column("c-done").contains("Done item"));
    assert!(
        billing
            .text
            .contains("href=\"/ui/board?area=api%3Abilling\"")
    );
    world.fakes.clock.advance(Duration::hours(11));
    let tonight = browser.get(&world, "/ui/board").await;
    assert!(tonight.column("c-done").contains("Done item"));
    world.fakes.clock.advance(Duration::hours(3));
    let tomorrow = browser.get(&world, "/ui/board").await;
    assert!(!tomorrow.column("c-done").contains("Done item"));
}

#[tokio::test]
async fn the_ready_column_shows_the_imported_ready_list_with_its_reasons() {
    let (world, alice, bob) = team().await;
    let bot = world.member(&alice, "board", Role::Importer).await;
    let imported = world
        .put(
            &bot,
            "/v1/items",
            json!({"source": "board", "items": [
                {"ref": "gh:acme/api#1", "title": "Soon <first>", "priority": 0, "areas_hint": ["search"]},
                {"ref": "gh:acme/api#2", "title": "Waits for the rate table",
                 "blocked_by": ["gh:acme/api#3"]},
                {"ref": "gh:acme/api#3", "title": "Rate table", "priority": 3},
                {"ref": "gh:acme/api#4", "title": "Touches billing", "areas_hint": ["api:billing"]},
                {"ref": "gh:acme/web#5", "title": "Closed upstream while held"}
            ]}),
        )
        .await;
    assert_eq!(imported.status, StatusCode::OK, "{}", imported.body);
    let held = world
        .item(&alice, Some("gh:acme/api#9"), "Billing rewrite")
        .await;
    let closing = imported.body["items"][4]["id"].as_u64().unwrap();
    let session = world.session(&alice, "laptop").await;
    claim(&world, &alice, session, held, json!(["api:billing"])).await;
    claim(&world, &alice, session, closing, json!([])).await;
    let closed = world
        .put(
            &bot,
            "/v1/items",
            json!({"items": [{"ref": "gh:acme/web#5", "title": "Closed upstream while held", "closed": true}]}),
        )
        .await;
    assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);

    let browser = sign_in(&world, &bob).await;
    let page = browser.get(&world, "/ui/board").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let ready = page.column("c-ready");
    let first = ready.find("Soon &#60;first&#62;").unwrap();
    let later = ready.find("Rate table").unwrap();
    assert!(first < later, "soonest first: {ready}");
    assert!(ready.contains(">P0<") && ready.contains(">P3<"), "{ready}");
    assert!(
        ready.contains("<span class=\"chip\">api:search</span>"),
        "{ready}"
    );
    assert!(
        ready.contains(r#"<a class="ref" href="https://github.com/acme/api/issues/1" rel="noopener noreferrer">gh:acme/api#1</a>"#),
        "{ready}"
    );
    let waiting = &ready[ready.find("Not ready yet").unwrap()..];
    assert!(
        waiting
            .contains("Blocked by <a class=\"ref\" href=\"https://github.com/acme/api/issues/3\""),
        "{waiting}"
    );
    assert!(
        waiting.contains("api:billing is held by alice for <a class=\"ref\" href=\"https://github.com/acme/api/issues/9\""),
        "{waiting}"
    );
    assert!(
        !ready.contains("Billing rewrite"),
        "an item in progress is not in Ready"
    );
    assert!(
        !ready.contains("Closed upstream while held"),
        "a closed item is not ready"
    );
    let progress = page.column("c-prog");
    assert!(
        progress.contains("Closed upstream while held")
            && progress.contains("Closed upstream</span>"),
        "{progress}"
    );
    let search = browser.get(&world, "/ui/board?area=api%3Asearch").await;
    let filtered = search.column("c-ready");
    assert!(
        filtered.contains("Soon &#60;first") && !filtered.contains("Rate table"),
        "{filtered}"
    );
}

#[tokio::test]
async fn the_take_over_form_takes_a_stale_claim_and_tells_its_holder() {
    let (world, alice, bob) = board().await;
    let browser = sign_in(&world, &bob).await;
    let status = world.get(&bob, "/v1/status").await;
    let stale = status.body["stale"][0]["item"]["id"].as_u64().unwrap();
    let missing = browser
        .post(
            &world,
            &format!("/ui/items/{stale}/takeover"),
            &[("csrf", &browser.token), ("reason", " "), ("back", "board")],
        )
        .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(missing.text.contains("Write a reason"));
    let taken = browser
        .post(
            &world,
            &format!("/ui/items/{stale}/takeover"),
            &[
                ("csrf", &browser.token),
                ("reason", "no heartbeat for two hours"),
                ("back", "board"),
            ],
        )
        .await;
    assert_eq!(taken.status, StatusCode::SEE_OTHER, "{}", taken.text);
    assert_eq!(taken.location(), "/ui/board");
    let item = world.get(&bob, &format!("/v1/items/{stale}")).await;
    assert_eq!(item.body["claim"]["holder"], "bob", "{}", item.body);
    assert_eq!(item.body["claim"]["areas"], json!(["api:billing"]));
    let told = world.get(&alice, "/v1/inbox").await;
    assert!(
        told.body.to_string().contains("Taken over"),
        "{}",
        told.body
    );
    let again = browser
        .post(
            &world,
            &format!("/ui/items/{stale}/takeover"),
            &[
                ("csrf", &browser.token),
                ("reason", "again"),
                ("back", "status"),
            ],
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.text);
    assert!(again.text.contains("href=\"/ui/\""));
}

#[tokio::test]
async fn the_status_page_lists_claims_and_holds_or_says_there_are_none() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let browser = sign_in(&world, &alice).await;
    let empty = browser.get(&world, "/ui/").await;
    assert!(empty.text.contains("Nothing claimed"));
    assert!(empty.text.contains("No live claims"));
    assert!(empty.text.contains("Heartbeat within the last 1 d"));
    let (world, _alice, bob) = board().await;
    let browser = sign_in(&world, &bob).await;
    let page = browser.get(&world, "/ui/").await;
    assert!(
        page.text.contains("1 live claim · 1 stale"),
        "{}",
        page.text
    );
    assert!(page.text.contains("Heartbeat within the last 1 h"));
    assert!(
        page.text
            .contains("<span class=\"chip\">web:checkout</span>")
    );
    assert!(page.text.contains("data-status-view"));
    assert!(page.text.contains("data-changed hidden"));
}

/// Reads frames of a stream's body until one holds `needle`; answers everything read.
async fn read_until(body: &mut Body, needle: &str) -> String {
    let mut text = String::new();
    while !text.contains(needle) {
        let frame = tokio::time::timeout(StdDuration::from_secs(5), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        text.push_str(std::str::from_utf8(&frame.into_data().unwrap()).unwrap());
    }
    text
}

#[tokio::test]
async fn signing_out_ends_the_browsers_open_stream_at_its_next_read() {
    let (world, alice, bob) = team().await;
    let browser = sign_in(&world, &alice).await;
    let request = Request::builder()
        .uri("/ui/events/stream")
        .header(header::COOKIE, &browser.cookie)
        .body(Body::empty())
        .unwrap();
    let response = world.open(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    world
        .thread(&bob, json!(["alice"]), "Billing", "hello")
        .await;
    read_until(&mut body, "\"kind\":\"thread_started\"").await;
    let out = browser
        .post(&world, "/ui/logout", &[("csrf", &browser.token)])
        .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    world
        .thread(&bob, json!(["alice"]), "Search", "again")
        .await;
    let ended = tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            match body.frame().await {
                None => return true,
                Some(Ok(frame)) => {
                    let data = frame.into_data().unwrap();
                    assert!(
                        !std::str::from_utf8(&data)
                            .unwrap()
                            .contains("thread_started"),
                        "an event after signing out"
                    );
                }
                Some(Err(_)) => return true,
            }
        }
    })
    .await
    .unwrap();
    assert!(ended);
}

#[tokio::test]
async fn done_today_counts_from_the_viewers_midnight_or_else_from_utcs() {
    let (world, _alice, bob) = board().await;
    world.fakes.clock.advance(Duration::minutes(1));
    let browser = sign_in(&world, &bob).await;
    let utc = browser.get(&world, "/ui/board").await;
    assert!(utc.column("c-done").contains("since 00:00 UTC"));
    assert!(utc.column("c-done").contains("Done item"));
    let zoned = |minutes: &str| Browser {
        cookie: format!("{}; {ZONE_COOKIE}={minutes}", browser.cookie),
        token: browser.token.clone(),
    };
    let at_midnight = zoned("780").get(&world, "/ui/board").await;
    assert!(!at_midnight.column("c-done").contains("since 00:00 UTC"));
    assert!(
        at_midnight.column("c-done").contains("Done item"),
        "done at 11:00 UTC, the viewer's midnight"
    );
    let midnight_after_it = zoned("779").get(&world, "/ui/board").await;
    assert!(
        !midnight_after_it.column("c-done").contains("Done item"),
        "done a minute before the viewer's midnight"
    );
    let west = zoned("-300").get(&world, "/ui/board").await;
    assert!(west.column("c-done").contains("Done item"));
    for unreadable in ["+60", "841", "abc", ""] {
        let fallback = zoned(unreadable).get(&world, "/ui/board").await;
        assert!(
            fallback.column("c-done").contains("since 00:00 UTC"),
            "{unreadable:?}"
        );
        assert!(fallback.column("c-done").contains("Done item"));
    }
}
