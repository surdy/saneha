//! End to end: a server started in this test process on an ephemeral port,
//! exercised through the same HTTP calls the subcommands make, and through the
//! `saneha` binary itself.

use std::process::{Command, Stdio};

use saneha::api::{Channel, JoinRequest, NewMessage};
use saneha::client::{JoinAnswer, Remote};

mod support;

use support::{stdout_of, TestServer};

#[test]
fn creates_channels_with_minted_and_given_names() {
    let server = TestServer::start();
    let remote = server.remote();

    let minted = remote.create_channel(None, None).expect("mint a channel");
    assert!(
        minted.name.contains('-')
            && minted
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'),
        "minted name is not a readable slug: {:?}",
        minted.name
    );
    assert_eq!(minted.state.as_str(), "open");
    assert!(minted.purpose.is_none());
    assert!(!minted.created_at.is_empty());

    let named = remote
        .create_channel(Some("brisk-otter"), Some("coordinating the refactor"))
        .expect("create a named channel");
    assert_eq!(named.name, "brisk-otter");
    assert_eq!(named.purpose.as_deref(), Some("coordinating the refactor"));
    assert_eq!(named.state.as_str(), "open");
}

#[test]
fn a_duplicate_name_fails_clearly() {
    let server = TestServer::start();
    let remote = server.remote();

    remote
        .create_channel(Some("quiet-heron"), None)
        .expect("create the channel");
    let err = remote
        .create_channel(Some("quiet-heron"), None)
        .expect_err("the second create must fail");

    let message = format!("{err:#}");
    assert_eq!(message, "a channel named \"quiet-heron\" already exists");
}

#[test]
fn an_invalid_name_fails_clearly() {
    let server = TestServer::start();
    let remote = server.remote();

    for name in ["Brisk Otter", "brisk_otter", "-leading", "trailing-"] {
        let err = remote
            .create_channel(Some(name), None)
            .expect_err("the name must be rejected");
        let message = format!("{err:#}");
        assert!(
            message.contains("lowercase letters, digits and hyphens"),
            "unhelpful message for {name:?}: {message}"
        );
        assert!(message.contains(name), "{message} does not name {name:?}");
        assert_eq!(message.lines().count(), 1, "{message}");
    }
}

#[test]
fn a_purpose_that_is_not_one_line_fails_clearly() {
    let server = TestServer::start();
    let remote = server.remote();

    let too_long = "p".repeat(100_000);
    for purpose in ["line one\nline two", "line one\r\nline two", &too_long] {
        let err = remote
            .create_channel(Some("brisk-otter"), Some(purpose))
            .expect_err("the purpose must be rejected");
        let message = format!("{err:#}");
        assert!(
            message.contains("one line") && message.contains("256 characters"),
            "unhelpful message: {message}"
        );
        assert_eq!(message.lines().count(), 1, "{message}");
        assert!(message.chars().count() < 400, "{} chars", message.len());
    }

    // Nothing was written on the way to any of those errors.
    assert!(remote.list_channels(None).expect("list").is_empty());

    let rejected = server.run(&["new", "brisk-otter", "--purpose", "line one\nline two"]);
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("saneha: a purpose is one line"),
        "{stderr}"
    );
}

#[test]
fn a_closed_pipe_ends_the_listing_quietly() {
    let server = TestServer::start();
    let remote = server.remote();

    // More output than a pipe buffer holds, so the write is still going when
    // the reader goes away.
    let purpose = "p".repeat(200);
    for index in 0..400 {
        remote
            .create_channel(Some(&format!("channel-{index}")), Some(&purpose))
            .expect("create");
    }

    for args in [vec!["list"], vec!["list", "--json"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_saneha"))
            .args(&args)
            .env("SANEHA_URL", &server.url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run the saneha binary");
        // Close the read end straight away, the way `saneha list | true` does.
        drop(child.stdout.take());

        let output = child.wait_with_output().expect("wait");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "saneha {args:?} exited with {:?}: {stderr}",
            output.status.code()
        );
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
}

#[test]
fn lists_channels_with_state_and_purpose() {
    let server = TestServer::start();
    let remote = server.remote();

    assert!(remote.list_channels(None).expect("list").is_empty());

    remote
        .create_channel(Some("brisk-otter"), Some("coordinating the refactor"))
        .expect("create");
    remote
        .create_channel(Some("quiet-heron"), None)
        .expect("create");

    let channels = remote.list_channels(None).expect("list");
    assert_eq!(channels.len(), 2);

    let otter = channels
        .iter()
        .find(|c| c.name == "brisk-otter")
        .expect("the channel is listed");
    assert_eq!(otter.state.as_str(), "open");
    assert_eq!(otter.purpose.as_deref(), Some("coordinating the refactor"));

    let heron = channels
        .iter()
        .find(|c| c.name == "quiet-heron")
        .expect("the channel is listed");
    assert!(heron.purpose.is_none());
}

#[test]
fn the_binary_creates_and_lists_channels() {
    let server = TestServer::start();

    let minted = server.run(&["new"]);
    assert!(
        minted.status.success(),
        "{}",
        String::from_utf8_lossy(&minted.stderr)
    );
    let minted_name = String::from_utf8_lossy(&minted.stdout).trim().to_string();
    assert!(minted_name.contains('-'), "not a slug: {minted_name:?}");

    let named = server.run(&[
        "new",
        "brisk-otter",
        "--purpose",
        "coordinating the refactor",
    ]);
    assert!(
        named.status.success(),
        "{}",
        String::from_utf8_lossy(&named.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&named.stdout).trim(), "brisk-otter");

    let listed = server.run(&["list"]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let table = String::from_utf8_lossy(&listed.stdout);
    assert!(table.starts_with("NAME"), "{table}");
    assert!(table.contains(&minted_name), "{table}");
    assert!(
        table.contains("brisk-otter")
            && table.contains("open")
            && table.contains("coordinating the refactor"),
        "{table}"
    );

    let json = server.run(&["list", "--json"]);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("--json prints JSON");
    assert_eq!(parsed["channels"].as_array().expect("channels").len(), 2);
}

#[test]
fn the_binary_reports_a_duplicate_on_one_line() {
    let server = TestServer::start();

    assert!(server.run(&["new", "quiet-heron"]).status.success());
    let second = server.run(&["new", "quiet-heron"]);

    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert_eq!(
        stderr.trim(),
        "saneha: a channel named \"quiet-heron\" already exists"
    );
}

#[test]
fn subcommands_need_saneha_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .arg("list")
        .env_remove("SANEHA_URL")
        .output()
        .expect("run the saneha binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("SANEHA_URL is not set"), "{stderr}");
    assert!(stderr.contains("export SANEHA_URL="), "{stderr}");
}

#[test]
fn subcommands_report_an_unreachable_server() {
    // Nothing listens on port 1 of the loopback.
    let output = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .arg("list")
        .env("SANEHA_URL", "http://127.0.0.1:1")
        .output()
        .expect("run the saneha binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("saneha: cannot reach the saneha server at http://127.0.0.1:1"),
        "{stderr}"
    );
}

/// Joins `name` on this host and returns the identity granted.
fn join(remote: &Remote, channel: &str, name: &str) -> String {
    let request = JoinRequest {
        name: name.to_string(),
        host: saneha::identity::host(saneha::store::HOST.max),
        harness: "claude".to_string(),
        session_id: None,
        pid: None,
        pid_started_at: None,
        cwd: Some("/repos/saneha".to_string()),
        madari_pane: None,
        same_host_session_live: false,
        held_session_id: None,
    };
    match remote.join(channel, &request).expect("join") {
        JoinAnswer::Granted(joined) => joined.identity,
        JoinAnswer::Stale(message) => panic!("{message}"),
    }
}

fn send(remote: &Remote, channel: &str, from: &str, body: &str) {
    remote
        .send_message(
            channel,
            &NewMessage {
                from: from.to_string(),
                body: body.to_string(),
                to: Vec::new(),
                attachments: Vec::new(),
                key: None,
            },
        )
        .expect("send");
}

fn named<'a>(channels: &'a [Channel], name: &str) -> &'a Channel {
    channels
        .iter()
        .find(|c| c.name == name)
        .expect("the channel is listed")
}

/// The newest id of one channel, as a listing reports it.
fn newest(remote: &Remote, name: &str) -> i64 {
    named(&remote.list_channels(None).expect("list"), name).newest_id
}

#[test]
fn a_channel_carries_the_newest_message_id() {
    let server = TestServer::start();
    let remote = server.remote();

    let created = remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    assert_eq!(created.newest_id, 0, "an empty transcript has no newest id");
    assert_eq!(newest(&remote, "brisk-otter"), 0);

    // A join is a system message, so it is message 1: a wait returns on it and
    // a read advances over it, so the newest id has to count it.
    let alice = join(&remote, "brisk-otter", "alice");
    assert_eq!(newest(&remote, "brisk-otter"), 1);

    send(&remote, "brisk-otter", &alice, "the tests pass");
    send(&remote, "brisk-otter", &alice, "and again");
    assert_eq!(newest(&remote, "brisk-otter"), 3);

    // Every shape a channel is serialised in says the same thing.
    assert_eq!(
        remote
            .channel_detail("brisk-otter")
            .expect("detail")
            .channel
            .newest_id,
        3
    );

    // A leave is a system message too, and so is the close.
    remote.leave("brisk-otter", &alice).expect("leave");
    assert_eq!(newest(&remote, "brisk-otter"), 4);

    let closed = remote
        .close_channel("brisk-otter", "surdy@web")
        .expect("close");
    assert_eq!(closed.channel.newest_id, 5);
    assert_eq!(newest(&remote, "brisk-otter"), 5);
}

#[test]
fn asking_as_somebody_carries_their_read_cursor() {
    let server = TestServer::start();
    let remote = server.remote();

    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    remote
        .create_channel(Some("quiet-heron"), None)
        .expect("create");

    let alice = join(&remote, "brisk-otter", "alice");
    send(&remote, "brisk-otter", &alice, "the tests pass");
    send(&remote, "brisk-otter", &alice, "and again");
    remote
        .set_read_cursor("brisk-otter", &alice, 2)
        .expect("advance the cursor");

    // Without `as` a channel carries no cursor at all.
    for channel in remote.list_channels(None).expect("list") {
        assert!(channel.read_cursor.is_none(), "{}", channel.name);
    }

    let listed = remote.list_channels(Some(&alice)).expect("list as alice");
    let otter = named(&listed, "brisk-otter");
    assert_eq!(otter.read_cursor, Some(2));
    assert_eq!(otter.newest_id, 3, "one join and two messages");

    // A channel alice has not joined carries no cursor of hers, and that is
    // not an error: it is what the viewer draws as unjoined.
    assert_eq!(named(&listed, "quiet-heron").read_cursor, None);

    // An identity that is a participant nowhere lists every channel all the
    // same.
    let stranger = format!("nobody@{}", saneha::identity::host(saneha::store::HOST.max));
    let listed = remote
        .list_channels(Some(&stranger))
        .expect("list as a stranger");
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|c| c.read_cursor.is_none()));

    // Asking moved nothing: only a read does that (ADR-0004).
    assert_eq!(
        remote
            .participant("brisk-otter", &alice)
            .expect("look up the participant")
            .expect("the participant is there")
            .read_cursor,
        2
    );
}

#[test]
fn an_as_that_is_not_an_identity_fails_clearly() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    for as_identity in ["alice", "alice%40", "alice@", "@laptop", ""] {
        let (status, body) = server.raw("GET", &format!("/channels?as={as_identity}"), b"");
        assert_eq!(status, 400, "{as_identity:?} answered {status}: {body}");
        assert_eq!(body.lines().count(), 1, "{body}");
        assert!(
            body.contains("identity") || body.contains("lowercase letters"),
            "unhelpful message for {as_identity:?}: {body}"
        );
    }
}

#[test]
fn the_binary_lists_the_newest_id_and_the_unread_count() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    let alice = join(&remote, "brisk-otter", "alice");
    send(&remote, "brisk-otter", &alice, "the tests pass");

    let plain = stdout_of("list", &server.run(&["list"]));
    let header = plain.lines().next().expect("a header");
    assert!(header.contains("NEWEST"), "{plain}");
    assert!(!header.contains("UNREAD"), "{plain}");

    let asked = stdout_of("list --as alice", &server.run(&["list", "--as", "alice"]));
    let lines: Vec<&str> = asked.lines().collect();
    assert!(
        lines[0].contains("NEWEST") && lines[0].contains("UNREAD"),
        "{asked}"
    );
    // The join and the message are both unread, out of a newest id of 2.
    let cells: Vec<&str> = lines[1].split_whitespace().collect();
    assert_eq!(cells, vec!["brisk-otter", "open", "2", "2", "-"], "{asked}");
}

/// A channel to work in, and the identity working in it.
fn channel_and_who(remote: &Remote, name: &str, purpose: Option<&str>) -> String {
    remote.create_channel(Some(name), purpose).expect("create");
    join(remote, name, "surdy")
}

/// The purpose is the one field of a channel that can be changed after it is
/// created — set, corrected, and cleared — and a listing reports the change.
#[test]
fn a_purpose_can_be_set_corrected_and_cleared() {
    let server = TestServer::start();
    let remote = server.remote();
    let who = channel_and_who(&remote, "brisk-otter", Some("coordinating the refactr"));

    let fixed = remote
        .set_purpose("brisk-otter", &who, Some("coordinating the refactor"))
        .expect("correct the purpose");
    assert_eq!(fixed.purpose.as_deref(), Some("coordinating the refactor"));
    assert_eq!(
        named(&remote.list_channels(None).expect("list"), "brisk-otter")
            .purpose
            .as_deref(),
        Some("coordinating the refactor")
    );

    // Blank and absent are the same request, and both clear it.
    let cleared = remote
        .set_purpose("brisk-otter", &who, Some("   "))
        .expect("clear with blank");
    assert!(cleared.purpose.is_none());
    let still = remote
        .set_purpose("brisk-otter", &who, None)
        .expect("clear with nothing");
    assert!(still.purpose.is_none());
}

/// Setting a purpose writes nothing into the transcript: it changes the
/// channel, it is not an event in it.
#[test]
fn setting_a_purpose_leaves_the_transcript_alone() {
    let server = TestServer::start();
    let remote = server.remote();
    let who = channel_and_who(&remote, "brisk-otter", None);
    send(&remote, "brisk-otter", &who, "the first message");
    let before = newest(&remote, "brisk-otter");

    remote
        .set_purpose("brisk-otter", &who, Some("the wake test"))
        .expect("set the purpose");

    assert_eq!(
        newest(&remote, "brisk-otter"),
        before,
        "a purpose change must not allocate a message id"
    );
}

/// A closed channel is the record of a conversation that is over, and what it
/// said it was for is part of that record.
#[test]
fn a_closed_channel_keeps_the_purpose_it_had() {
    let server = TestServer::start();
    let remote = server.remote();
    let who = channel_and_who(&remote, "brisk-otter", Some("the wake test"));
    remote.close_channel("brisk-otter", &who).expect("close");

    let refused = remote
        .set_purpose("brisk-otter", &who, Some("something else"))
        .expect_err("a closed channel refuses a purpose");
    let said = refused.to_string();
    assert!(
        said.contains("closed") && said.contains("purpose"),
        "the refusal should say what was refused and why: {said}"
    );
    assert_eq!(
        remote
            .channel_detail("brisk-otter")
            .expect("detail")
            .channel
            .purpose
            .as_deref(),
        Some("the wake test")
    );
}

/// The same shapes `new` refuses, refused here too: a purpose is one line, and
/// `by` is an identity rather than anything at all.
#[test]
fn a_purpose_is_held_to_one_line_and_a_real_identity() {
    let server = TestServer::start();
    let remote = server.remote();
    let who = channel_and_who(&remote, "brisk-otter", None);

    let two_lines = remote
        .set_purpose("brisk-otter", &who, Some("one line\nand another"))
        .expect_err("a purpose with a line break is refused");
    assert!(
        two_lines.to_string().contains("one line"),
        "the refusal should say a purpose is one line: {two_lines}"
    );

    let too_long = "p".repeat(saneha::store::MAX_PURPOSE + 1);
    remote
        .set_purpose("brisk-otter", &who, Some(&too_long))
        .expect_err("a purpose over the limit is refused");

    remote
        .set_purpose("brisk-otter", "not an identity", Some("anything"))
        .expect_err("`by` has to be an identity");

    // and none of that changed the channel
    assert!(
        named(&remote.list_channels(None).expect("list"), "brisk-otter")
            .purpose
            .is_none()
    );
}
