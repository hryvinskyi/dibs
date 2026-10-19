//! Conversations through the command line.

use crate::support::{Setup, json, status, text};

#[test]
fn a_message_is_sent_found_in_the_inbox_read_and_marked() {
    let setup = Setup::new();
    let dir = setup.elsewhere();
    let sent = setup
        .dibs(&setup.alice, &dir)
        .args([
            "send",
            "--to",
            "bob",
            "--subject",
            "Billing rates",
            "-",
            "--json",
        ])
        .write_stdin("Line one\nLine two — ünïcode\n")
        .output()
        .unwrap();
    assert_eq!(status(&sent), 0);
    let thread = json(&sent)["thread"]["id"].as_u64().unwrap().to_string();

    let inbox = setup
        .dibs(&setup.bob, &dir)
        .args(["inbox", "--unread"])
        .output()
        .unwrap();
    assert_eq!(status(&inbox), 0);
    let listed = text(&inbox.stdout);
    assert!(
        listed.contains(&format!("#{thread} Billing rates (from alice, 1 unread)")),
        "{listed}"
    );

    let shown = setup
        .dibs(&setup.bob, &dir)
        .args(["thread", &thread])
        .output()
        .unwrap();
    assert_eq!(status(&shown), 0);
    assert!(text(&shown.stdout).contains("Line two — ünïcode"));

    let read = setup
        .dibs(&setup.bob, &dir)
        .args(["read", &thread])
        .output()
        .unwrap();
    assert_eq!(status(&read), 0);
    assert!(text(&read.stdout).contains("1 new"));
    let empty = setup
        .dibs(&setup.bob, &dir)
        .args(["inbox", "--unread"])
        .output()
        .unwrap();
    assert!(text(&empty.stdout).contains("No conversations are waiting for you."));

    let everyone = setup
        .dibs(&setup.bob, &dir)
        .args(["send", "--project", "--subject", "Deploy", "noon today"])
        .output()
        .unwrap();
    assert_eq!(status(&everyone), 0);
    let missing = setup
        .dibs(&setup.bob, &dir)
        .args(["thread", "999"])
        .output()
        .unwrap();
    assert_eq!(status(&missing), 1);
}
