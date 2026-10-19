//! A message that asks to be acknowledged, sent, acknowledged and seen acknowledged across the
//! built binaries: the sender's `dibs send --ack`, the recipient's brief and `dibs ack`, and the
//! sender's view of the thread.

use crate::harness::Team;

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["src/billing/**"]
"#;

#[test]
fn a_message_sent_with_an_ack_is_acked_by_its_recipient_and_its_sender_sees_it() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, "git@example.com:acme/widgets.git");
    let (code, sent) = team.alice.json(
        &work,
        &[
            "send",
            "--to",
            "bob",
            "--subject",
            "Staging is down",
            "--importance",
            "urgent",
            "--ack",
            "Do not deploy until I say so.",
        ],
    );
    assert_eq!(code, Some(0), "{sent}");
    let thread = sent["thread"]["id"].as_u64().unwrap().to_string();
    let message = sent["message"]["id"].as_u64().unwrap();
    assert_eq!(sent["message"]["importance"], "urgent");
    assert_eq!(sent["message"]["ack_required"], true);

    let (_, waiting) = team.bob.json(&work, &["inbox", "--unacked"]);
    assert_eq!(waiting["unacked"][0]["message"]["id"], message, "{waiting}");
    assert_eq!(waiting["unacked"][0]["message"]["author"], "alice");

    let (_, before) = team.alice.json(&work, &["thread", &thread]);
    assert_eq!(
        before["messages"][0]["acks"][0]["member"], "bob",
        "{before}"
    );
    assert!(before["messages"][0]["acks"][0]["acked_at"].is_null());

    let acked = team.bob.ok(&work, &["ack", &message.to_string()]);
    assert!(
        acked.starts_with(&format!("Acknowledged message {message}")),
        "{acked}"
    );
    let (code, again) = team.bob.json(&work, &["ack", &message.to_string()]);
    assert_eq!(code, Some(0), "a repeat is no failure: {again}");
    assert_eq!(again["created"], false);

    let (_, after) = team.alice.json(&work, &["thread", &thread]);
    assert!(
        after["messages"][0]["acks"][0]["acked_at"].is_string(),
        "{after}"
    );
    let shown = team.alice.ok(&work, &["thread", &thread]);
    assert!(shown.contains("Acknowledgements: bob acked at "), "{shown}");
    let (_, left) = team.bob.json(&work, &["inbox", "--unacked"]);
    assert_eq!(left["unacked"], serde_json::json!([]), "{left}");
}
