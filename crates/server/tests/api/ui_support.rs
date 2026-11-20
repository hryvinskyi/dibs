//! What the tests of the pages share: requests a browser sends, with its cookies and its forms,
//! the answer read as text, signing in through the form, and a project to sign in to.

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use dibs_domain::member::Role;
use dibs_server::ui::{LOGIN_COOKIE, SESSION_COOKIE};

use crate::support::{Person, World, two_repositories};

/// An answer read as text.
pub(crate) struct Page {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) text: String,
}

impl Page {
    /// The value of the cookie this answer sets under `name`, if it sets one.
    pub(crate) fn set_cookie(&self, name: &str) -> Option<String> {
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
    pub(crate) fn set_cookie_line(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(&format!("{name}=")))
            .map(ToOwned::to_owned)
    }

    /// The value of the first hidden form token on the page.
    pub(crate) fn form_token(&self) -> String {
        let start = self
            .text
            .find(r#"name="csrf" value=""#)
            .unwrap_or_else(|| panic!("no form token in {}", self.text))
            + r#"name="csrf" value=""#.len();
        let end = self.text[start..].find('"').unwrap() + start;
        self.text[start..end].to_owned()
    }

    /// Where a redirect goes.
    pub(crate) fn location(&self) -> &str {
        self.headers[header::LOCATION].to_str().unwrap()
    }
}

/// Form-encodes the pairs.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
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
pub(crate) async fn fetch(
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
pub(crate) struct Browser {
    pub(crate) cookie: String,
    pub(crate) token: String,
}

impl Browser {
    pub(crate) async fn get(&self, world: &World, path: &str) -> Page {
        fetch(world, Method::GET, path, Some(&self.cookie), None).await
    }

    pub(crate) async fn post(&self, world: &World, path: &str, pairs: &[(&str, &str)]) -> Page {
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
pub(crate) async fn post_sign_in(world: &World, token: &str) -> Page {
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
pub(crate) async fn sign_in(world: &World, who: &Person) -> Browser {
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
pub(crate) async fn team() -> (World, Person, Person) {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    world.settings(&alice, two_repositories()).await;
    (world, alice, bob)
}

pub(crate) async fn claim(world: &World, who: &Person, session: u64, item: u64, areas: Value) {
    let answer = world
        .post(
            who,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": session, "areas": areas}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
}
