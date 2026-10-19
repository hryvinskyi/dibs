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

#[test]
fn an_urgent_message_that_asks_is_acked_once_from_the_command_line() {
    let setup = Setup::new();
    let dir = setup.elsewhere();
    let sent = setup
        .dibs(&setup.alice, &dir)
        .args([
            "send",
            "--to",
            "bob",
            "--subject",
            "Staging is down",
            "--importance",
            "urgent",
            "--ack",
            "Do not deploy until I say so.",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&sent), 0, "{}", text(&sent.stderr));
    assert!(
        text(&sent.stdout)
            .contains("It is marked urgent, and each recipient is asked to acknowledge it."),
        "{}",
        text(&sent.stdout)
    );

    let waiting = setup
        .dibs(&setup.bob, &dir)
        .args(["inbox", "--unacked", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&waiting), 0);
    let answer = json(&waiting);
    let message = answer["unacked"][0]["message"]["id"].as_u64().unwrap();
    assert_eq!(answer["unacked"][0]["message"]["importance"], "urgent");
    let listed = setup
        .dibs(&setup.bob, &dir)
        .args(["inbox", "--unacked"])
        .output()
        .unwrap();
    let listed = text(&listed.stdout);
    assert!(
        listed.contains("(from alice, urgent, 1 unread, 1 to ack)"),
        "{listed}"
    );
    assert!(
        listed.contains(&format!("- message {message} from alice in #"))
            && listed.contains(&format!("Staging is down [urgent]; dibs ack {message}")),
        "{listed}"
    );

    let thread = answer["unacked"][0]["thread"]["id"]
        .as_u64()
        .unwrap()
        .to_string();
    let shown = setup
        .dibs(&setup.bob, &dir)
        .args(["thread", &thread])
        .output()
        .unwrap();
    let shown = text(&shown.stdout);
    assert!(
        shown.contains(&format!("message {message}, alice")),
        "{shown}"
    );
    assert!(shown.contains("[urgent, asks for an ack]"), "{shown}");
    assert!(shown.contains("Acknowledgements: bob waiting."), "{shown}");

    let acked = setup
        .dibs(&setup.bob, &dir)
        .args(["ack", &message.to_string()])
        .output()
        .unwrap();
    assert_eq!(status(&acked), 0, "{}", text(&acked.stderr));
    assert!(
        text(&acked.stdout).starts_with(&format!(
            "Acknowledged message {message} in conversation #{thread}"
        )),
        "{}",
        text(&acked.stdout)
    );
    let again = setup
        .dibs(&setup.bob, &dir)
        .args(["ack", &message.to_string(), "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&again), 0, "a repeat is no failure");
    assert_eq!(json(&again)["created"], false);
    assert_eq!(json(&again)["ack"]["message"], message);
    let repeated = setup
        .dibs(&setup.bob, &dir)
        .args(["ack", &message.to_string()])
        .output()
        .unwrap();
    assert!(
        text(&repeated.stdout).starts_with(&format!(
            "You acknowledged message {message} in conversation #{thread} already, at "
        )),
        "{}",
        text(&repeated.stdout)
    );
    let by_author = setup
        .dibs(&setup.alice, &dir)
        .args(["ack", &message.to_string()])
        .output()
        .unwrap();
    assert_eq!(status(&by_author), 1);
    assert!(
        text(&by_author.stderr).contains("Only a member this message was sent to"),
        "{}",
        text(&by_author.stderr)
    );
    let nothing = setup
        .dibs(&setup.bob, &dir)
        .args(["inbox", "--unacked"])
        .output()
        .unwrap();
    assert!(text(&nothing.stdout).contains("No message waits for your acknowledgement."));
    let not_an_id = setup
        .dibs(&setup.bob, &dir)
        .args(["ack", "zero"])
        .output()
        .unwrap();
    assert_eq!(status(&not_an_id), 2, "clap refuses it");
    let unknown = setup
        .dibs(&setup.alice, &dir)
        .args([
            "send",
            "--to",
            "bob",
            "--subject",
            "s",
            "--importance",
            "loud",
            "b",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&unknown), 2);
}
