//! Inbox (`/ui/inbox`) through the real router.

use axum::http::StatusCode;

use crate::ui_support::{sign_in, team};

#[tokio::test]
async fn inbox_answers_with_its_heading() {
    let (world, alice, _) = team().await;
    let browser = sign_in(&world, &alice).await;
    let page = browser.get(&world, "/ui/inbox").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(r#"<h1 tabindex="-1">Inbox</h1>"#),
        "{}",
        page.text
    );
}
