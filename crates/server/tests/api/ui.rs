//! The browser pages through the real router: signing in and out, the form tokens, the security
//! headers, escaping, the event stream over the session cookie, sessions that end, cookies the
//! server did not set, the static bundle, the take-over form, a reply and an acknowledgement, the
//! notice of a refused form inside the frame, and the old addresses that lead to the screens.

use std::time::Duration as StdDuration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;
use dibs_server::ui::{BUNDLE_HASH, CONTENT_SECURITY_POLICY, LOGIN_COOKIE, SESSION_COOKIE};

use crate::support::{Person, World};
use crate::ui_support::{Browser, claim, fetch, form, post_sign_in, sign_in, team};

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
    assert!(page.text.contains("Signed in as alice"), "{}", page.text);
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

/// What a signed-in browser's stream sends from the start of the log until it has sent `until`.
async fn browser_stream(world: &World, browser: &Browser, until: &str) -> String {
    let request = Request::builder()
        .uri("/ui/events/stream?after=0")
        .header(header::COOKIE, &browser.cookie)
        .body(Body::empty())
        .unwrap();
    let response = world.open(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut text = String::new();
    while !text.contains(until) {
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
async fn a_browsers_stream_carries_a_members_report_to_them_and_to_no_other_member() {
    let (world, alice, bob) = team().await;
    let carol = world.member(&alice, "carol", Role::Member).await;
    let session = world.session(&bob, "laptop").await;
    world
        .post(
            &bob,
            "/v1/heartbeat",
            json!({"session_id": session, "machine": "0123456789abcdef0123456789abcdef",
                   "reports": [{"day": "2026-03-02", "counts": {"needs_slot": 1}}]}),
        )
        .await;
    world.item(&alice, None, "The last event").await;
    let bobs = browser_stream(&world, &sign_in(&world, &bob).await, "The last event").await;
    assert!(bobs.contains("\"kind\":\"hook_reported\""), "{bobs}");
    let carols = browser_stream(&world, &sign_in(&world, &carol).await, "The last event").await;
    assert!(!carols.contains("hook_reported"), "{carols}");
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
    let page = browser.get(&world, "/ui/claims").await;
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
    for (path, label) in [
        ("/ui/", "Now"),
        ("/ui/claims", "Claims"),
        ("/ui/queue", "Queue"),
        ("/ui/areas", "Areas"),
        ("/ui/inbox", "Inbox"),
    ] {
        assert!(
            ended
                .text
                .contains(&format!(r#"<a href="{path}">{label}</a>"#)),
            "{label}: {}",
            ended.text
        );
    }
    for retired in ["/ui/board", "/ui/threads", "Status", "Board", "Threads"] {
        assert!(!ended.text.contains(retired), "{retired}: {}", ended.text);
    }
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

#[tokio::test]
async fn the_bundle_serves_its_fonts_for_a_year() {
    let world = World::new().await;
    let path = format!("/ui/static/{BUNDLE_HASH}/IBMPlexSans-Regular-Latin1.woff2");
    let request = Request::builder().uri(path).body(Body::empty()).unwrap();
    let response = world.open(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "font/woff2");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.starts_with(b"wOF2"));
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
async fn a_page_whose_cursor_the_retention_passed_learns_where_to_follow_from() {
    let (world, alice, _bob) = team().await;
    let browser = sign_in(&world, &alice).await;
    let first = world.get(&alice, "/v1/events?limit=1").await;
    let early = first.body["next"].as_u64().unwrap();
    world.item(&alice, None, "Old work").await;
    assert_eq!(
        browser
            .get(&world, &format!("/ui/session?after={early}"))
            .await
            .status,
        StatusCode::NO_CONTENT,
        "nothing was removed yet"
    );
    world.fakes.clock.advance(Duration::days(91));
    world.app.prune.execute().await.unwrap();
    let fresh = sign_in(&world, &alice).await;
    let behind = fresh
        .get(&world, &format!("/ui/session?after={early}"))
        .await;
    assert_eq!(behind.status, StatusCode::GONE, "{}", behind.text);
    let body: Value = serde_json::from_str(&behind.text).unwrap();
    assert_eq!(body["error"]["code"], "cursor_expired");
    let latest = body["error"]["details"]["latest"].as_u64().unwrap();
    assert!(latest > early, "{body}");
    assert_eq!(
        fresh
            .get(&world, &format!("/ui/session?after={latest}"))
            .await
            .status,
        StatusCode::NO_CONTENT,
        "from the newest event the page follows on"
    );
    assert_eq!(
        fresh.get(&world, "/ui/session?after=x").await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

/// The first inline event handler attribute (` on…="`) of a page, if it has one.
fn inline_handler(text: &str) -> Option<&str> {
    text.match_indices(" on").find_map(|(at, _)| {
        let rest = &text[at + 1..];
        let name = rest
            .find(|c: char| !c.is_ascii_lowercase())
            .map(|end| &rest[..end])?;
        (name.len() > 2 && rest[name.len()..].starts_with("=\"")).then_some(name)
    })
}

#[tokio::test]
async fn every_page_answer_carries_the_security_headers() {
    let (world, alice, bob, silent) = silent_claim().await;
    let thread = world
        .thread(&bob, json!(["alice"]), "Billing", "first")
        .await;
    let browser = sign_in(&world, &alice).await;
    let mut answers = vec![
        fetch(&world, Method::GET, "/ui/login", None, None).await,
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
        browser.get(&world, "/ui/board").await,
        browser
            .post(
                &world,
                &format!("/ui/items/{silent}/takeover"),
                &[("csrf", &browser.token), ("reason", " ")],
            )
            .await,
    ];
    for path in [
        "/ui/".to_owned(),
        "/ui/claims".to_owned(),
        format!("/ui/claims/{silent}"),
        "/ui/queue".to_owned(),
        "/ui/areas".to_owned(),
        "/ui/inbox".to_owned(),
        format!("/ui/inbox/{thread}"),
        "/ui/search?q=x".to_owned(),
    ] {
        let page = browser.get(&world, &path).await;
        assert_eq!(page.status, StatusCode::OK, "{path}: {}", page.text);
        answers.push(page);
    }
    for page in answers {
        assert_eq!(
            page.headers[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
        );
        assert_eq!(
            page.headers[header::CONTENT_SECURITY_POLICY],
            CONTENT_SECURITY_POLICY
        );
        assert_eq!(page.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(page.headers[header::REFERRER_POLICY], "no-referrer");
        assert!(!page.text.contains("<script>"), "an inline script");
        assert!(!page.text.contains(" style="), "an inline style");
        assert_eq!(inline_handler(&page.text), None, "an inline handler");
    }
}

#[test]
fn an_inline_handler_is_found_and_words_starting_with_on_are_not() {
    assert_eq!(inline_handler(r#"<a onclick="x()">"#), Some("onclick"));
    assert_eq!(inline_handler(r#"<p>one="two" or on="x" or one</p>"#), None);
}

#[tokio::test]
async fn a_reply_from_inbox_is_signed_from_the_web_and_one_to_no_conversation_is_refused_in_its_frame()
 {
    let (world, alice, bob) = team().await;
    let thread = world
        .thread(&bob, json!(["alice"]), "Billing", "first")
        .await;
    let browser = sign_in(&world, &alice).await;
    let path = format!("/ui/threads/{thread}/messages");
    let reply = browser
        .post(
            &world,
            &path,
            &[("csrf", &browser.token), ("body", "on it\nтак")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text);
    assert!(
        reply
            .location()
            .starts_with(&format!("/ui/inbox/{thread}#m-")),
        "{}",
        reply.location()
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

    let nowhere = browser
        .post(
            &world,
            "/ui/threads/999999/messages",
            &[("csrf", &browser.token), ("body", "hello")],
        )
        .await;
    assert_eq!(nowhere.status, StatusCode::NOT_FOUND, "{}", nowhere.text);
    assert!(
        nowhere.text.contains(r#"<aside id="rail""#),
        "{}",
        nowhere.text
    );
    assert!(
        nowhere
            .text
            .contains(r#"<a href="/ui/inbox" aria-current="page">Inbox"#),
        "{}",
        nowhere.text
    );
    assert!(nowhere.text.contains(r#"<h1 tabindex="-1">Not found</h1>"#));
    assert!(nowhere.text.contains("There is no such page"));
    assert!(
        nowhere
            .text
            .contains(r#"<a class="banner__action" href="/ui/inbox">Go back</a>"#),
        "{}",
        nowhere.text
    );
}

#[tokio::test]
async fn a_conversation_shows_importance_and_acks_and_a_recipient_acks_through_the_form() {
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
    let address = format!("/ui/inbox/{thread}");
    let browser = sign_in(&world, &bob).await;
    let page = browser.get(&world, &address).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text
            .contains(&format!(r#"<li class="msg msg--urgent" id="m-{message}">"#)),
        "{}",
        page.text
    );
    assert!(
        page.text
            .contains(r#"<span class="tag tag--bad">urgent</span>"#)
    );
    assert!(page.text.contains("Acknowledgement requested"));
    assert!(
        page.text.contains(
            r#"<li class="tag tag--warn" data-ack-member="bob"><span aria-hidden="true">○</span> bob (you) waiting</li>"#
        ),
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
        assert!(refused.text.contains(r#"<aside id="rail""#));
    }
    assert_eq!(status().await, Value::Null, "nothing was recorded");

    world.fakes.clock.advance(Duration::minutes(2));
    let acked = browser
        .post(&world, &path, &[("csrf", &browser.token)])
        .await;
    assert_eq!(acked.status, StatusCode::SEE_OTHER, "{}", acked.text);
    assert_eq!(acked.location(), format!("/ui/inbox/{thread}#m-{message}"));
    assert_eq!(status().await, "2026-03-02T09:02:00Z");
    let after = browser.get(&world, &address).await;
    assert!(
        after.text.contains(
            r#"<li class="tag" data-ack-member="bob"><span aria-hidden="true">✓</span> bob (you) acked <time datetime="2026-03-02T09:02:00Z" data-clock>"#
        ),
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
        "a second click shows the conversation"
    );
    assert_eq!(twice.location(), format!("/ui/inbox/{thread}#m-{message}"));
    assert_eq!(
        status().await,
        "2026-03-02T09:02:00Z",
        "the first acknowledgement stands"
    );

    let author = sign_in(&world, &alice).await;
    let authors = author.get(&world, &address).await;
    assert!(
        authors.text.contains(
            r#"<li class="tag" data-ack-member="bob"><span aria-hidden="true">✓</span> bob acked"#
        ),
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
}

/// A project of alice and bob in which alice's claim on `gh:acme/api#2`, with `api:billing`, went
/// silent two hours ago; answers that item too.
async fn silent_claim() -> (World, Person, Person, u64) {
    let (world, alice, bob) = team().await;
    let session = world.session(&alice, "laptop").await;
    let item = world
        .item(&alice, Some("gh:acme/api#2"), "Stale item")
        .await;
    claim(&world, &alice, session, item, json!(["api:billing"])).await;
    world.fakes.clock.advance(Duration::hours(2));
    (world, alice, bob, item)
}

#[tokio::test]
async fn the_take_over_form_takes_a_silent_claim_and_a_refusal_shows_in_the_frame_of_claims() {
    let (world, alice, bob, silent) = silent_claim().await;
    let browser = sign_in(&world, &bob).await;
    let path = format!("/ui/items/{silent}/takeover");
    let back = format!("/ui/claims/{silent}?filter=silent");
    let missing = browser
        .post(
            &world,
            &path,
            &[("csrf", &browser.token), ("reason", " "), ("back", &back)],
        )
        .await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(missing.text.contains("Write a reason"), "{}", missing.text);
    assert!(
        missing.text.contains(&format!(
            r#"<a class="banner__action" href="/ui/claims/{silent}?filter=silent">Go back</a>"#
        )),
        "{}",
        missing.text
    );
    assert!(
        missing.text.contains(r#"data-changed-kinds="""#),
        "the browser's address is the form's, so no change refreshes the notice: {}",
        missing.text
    );
    let claims = browser.get(&world, &back).await;
    assert!(
        claims.text.contains(r#"data-changed-kinds="claimed "#),
        "a screen at its own address follows the changes: {}",
        claims.text
    );
    let taken = browser
        .post(
            &world,
            &path,
            &[
                ("csrf", &browser.token),
                ("reason", "no heartbeat for two hours"),
                ("back", &back),
            ],
        )
        .await;
    assert_eq!(taken.status, StatusCode::SEE_OTHER, "{}", taken.text);
    assert_eq!(taken.location(), back);
    let item = world.get(&bob, &format!("/v1/items/{silent}")).await;
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
            &path,
            &[("csrf", &browser.token), ("reason", "again")],
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.text);
    for part in [
        r#"<aside id="rail" class="rail" data-region="rail" aria-label="Project">"#,
        r#"<a href="/ui/claims" aria-current="page">Claims"#,
        r#"<div class="pane" data-region="list"><div class="bar-top"><h1 tabindex="-1">Not done</h1></div>"#,
        r#"<a class="banner__action" href="/ui/claims">Go back</a>"#,
        "This claim is yours; claim the item again to renew it.",
    ] {
        assert!(again.text.contains(part), "{part}: {}", again.text);
    }
}

#[tokio::test]
async fn a_take_over_returns_to_the_path_its_form_names_and_never_to_another_site() {
    let (world, alice, bob, silent) = silent_claim().await;
    let browser = sign_in(&world, &bob).await;
    let page = browser
        .get(&world, &format!("/ui/claims/{silent}?filter=silent"))
        .await;
    assert!(
        page.text.contains(&format!(
            r#"<input type="hidden" name="back" value="/ui/claims/{silent}?filter=silent">"#
        )),
        "{}",
        page.text
    );
    let path = format!("/ui/items/{silent}/takeover");
    for foreign in [
        "https://example.com/ui/claims",
        "//example.com/",
        "/\\example.com",
    ] {
        let missing = browser
            .post(
                &world,
                &path,
                &[("csrf", &browser.token), ("reason", " "), ("back", foreign)],
            )
            .await;
        assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            missing
                .text
                .contains(r#"<a class="banner__action" href="/ui/claims">Go back</a>"#),
            "{foreign}: {}",
            missing.text
        );
        assert!(!missing.text.contains("example.com"), "{foreign}");
    }
    let taken = browser
        .post(
            &world,
            &path,
            &[
                ("csrf", &browser.token),
                ("reason", "taking it"),
                ("back", "https://example.com/"),
            ],
        )
        .await;
    assert_eq!(taken.status, StatusCode::SEE_OTHER, "{}", taken.text);
    assert_eq!(
        taken.location(),
        "/ui/claims",
        "a foreign address falls back to Claims"
    );
    let session = world.session(&alice, "laptop").await;
    let other = world
        .item(&alice, Some("gh:acme/api#7"), "Another stale item")
        .await;
    claim(&world, &alice, session, other, json!(["api:search"])).await;
    world.fakes.clock.advance(Duration::hours(2));
    let kept = browser
        .post(
            &world,
            &format!("/ui/items/{other}/takeover"),
            &[
                ("csrf", &browser.token),
                ("reason", "no heartbeat for a day"),
                ("back", "/ui/claims?filter=silent&who=alice"),
            ],
        )
        .await;
    assert_eq!(kept.status, StatusCode::SEE_OTHER, "{}", kept.text);
    assert_eq!(kept.location(), "/ui/claims?filter=silent&who=alice");
}

#[tokio::test]
async fn the_old_addresses_lead_to_the_screens_that_took_their_place() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    for (old, new) in [
        ("/ui/board", "/ui/claims"),
        ("/ui/board?member=bob", "/ui/claims?who=bob"),
        (
            "/ui/board?area=api%3Abilling&member=b%26o",
            "/ui/claims?who=b%26o",
        ),
        ("/ui/board?member=a&member=b", "/ui/claims"),
        ("/ui/threads", "/ui/inbox"),
        ("/ui/threads/7", "/ui/inbox/7"),
        ("/ui/threads/a%0D%0Ab", "/ui/inbox/a%0D%0Ab"),
        (
            "/ui/threads/%2F%2Fexample.com",
            "/ui/inbox/%2F%2Fexample.com",
        ),
        ("/ui/threads/%2E%2E", "/ui/inbox"),
    ] {
        let answer = browser.get(&world, old).await;
        assert_eq!(answer.status, StatusCode::PERMANENT_REDIRECT, "{old}");
        assert_eq!(answer.location(), new, "{old}");
        let anonymous = fetch(&world, Method::GET, old, None, None).await;
        assert_eq!(anonymous.location(), new, "{old} without a session");
    }
    let gone = browser.get(&world, "/ui/inbox/a%0D%0Ab").await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.text);
    assert!(gone.text.contains(r#"<aside id="rail""#), "{}", gone.text);
    assert_eq!(
        browser.get(&world, "/ui/board/fragment").await.status,
        StatusCode::NOT_FOUND
    );
}
