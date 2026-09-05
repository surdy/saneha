//! Sending and reading: who a message is addressed to, what a read returns,
//! and where a read cursor ends up.

use std::io::Write;
use std::process::Stdio;

use saneha::api::{JoinRequest, Message, NewMessage, DEFAULT_MESSAGE_LIMIT};
use saneha::client::{JoinAnswer, Remote};

mod support;

use support::{stdout_of, TestServer};

/// This machine as the binary under test sees it, which is the host half of
/// every identity a subcommand claims here.
fn host() -> String {
    saneha::identity::host(saneha::store::HOST.max)
}

/// A channel with nobody in it yet.
fn channel(remote: &Remote, name: &str) {
    remote.create_channel(Some(name), None).expect("create");
}

/// Joins `name` on this host and returns the identity granted.
fn join(remote: &Remote, channel: &str, name: &str) -> String {
    join_on(remote, channel, name, &host())
}

/// Joins `name@host`, on a host this test names rather than the one it runs
/// on, which is how two participants come to share a name.
fn join_on(remote: &Remote, channel: &str, name: &str, host: &str) -> String {
    let request = JoinRequest {
        name: name.to_string(),
        host: host.to_string(),
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

/// A message from `from`, with no `--to`: everything it is addressed to comes
/// out of the body.
fn send(remote: &Remote, channel: &str, from: &str, body: &str) -> Message {
    send_to(remote, channel, from, body, &[]).expect("send")
}

fn send_to(
    remote: &Remote,
    channel: &str,
    from: &str,
    body: &str,
    to: &[&str],
) -> anyhow::Result<Message> {
    remote.send_message(
        channel,
        &NewMessage {
            from: from.to_string(),
            body: body.to_string(),
            to: to.iter().map(|value| (*value).to_string()).collect(),
            attachments: Vec::new(),
        },
    )
}

/// A participant's read cursor as the server has it.
fn read_cursor(remote: &Remote, channel: &str, identity: &str) -> i64 {
    remote
        .participant(channel, identity)
        .expect("look up the participant")
        .expect("the participant is there")
        .read_cursor
}

#[test]
fn a_mention_addresses_the_participant_it_names() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");

    // At the start of the body.
    let message = send(&remote, "brisk-otter", &alice, "@bob look at this");
    assert_eq!(message.recipients, vec![bob.clone()]);

    // After whitespace, and ending at punctuation a name cannot hold.
    let message = send(&remote, "brisk-otter", &alice, "thanks @bob, that worked.");
    assert_eq!(message.recipients, vec![bob.clone()]);

    // In full.
    let message = send(&remote, "brisk-otter", &alice, &format!("@{bob} in full"));
    assert_eq!(message.recipients, vec![bob.clone()]);

    // Email-like text is text, and so is a mention with something other than
    // whitespace in front of it.
    let message = send(&remote, "brisk-otter", &alice, "write to bob@example.com");
    assert!(message.recipients.is_empty(), "{:?}", message.recipients);
    let message = send(&remote, "brisk-otter", &alice, "see(@bob)");
    assert!(message.recipients.is_empty(), "{:?}", message.recipients);
}

#[test]
fn no_recipients_is_a_broadcast() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    join(&remote, "brisk-otter", "bob");

    let message = send(&remote, "brisk-otter", &alice, "morning, all");
    assert!(message.recipients.is_empty(), "{:?}", message.recipients);
    assert_eq!(message.from.as_deref(), Some(alice.as_str()));
    assert_eq!(message.kind.as_str(), "message");
    assert_eq!(message.channel, "brisk-otter");
    assert!(message.id > 0);
    assert!(message.created_at.ends_with('Z'), "{}", message.created_at);
}

#[test]
fn everyone_addresses_everyone_in_the_channel() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    let carol = join_on(&remote, "brisk-otter", "carol", "quadhost");

    // An away participant is still addressed: messages to it accumulate as
    // unread until it comes back.
    server
        .database()
        .execute(
            "UPDATE participants SET away = 1 WHERE identity = ?1",
            [&carol],
        )
        .expect("mark carol away");

    let message = send(&remote, "brisk-otter", &alice, "@all please look");
    assert_eq!(message.recipients, vec![alice, bob, carol]);
}

#[test]
fn to_adds_recipients_the_body_did_not_mention() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    let carol = join_on(&remote, "brisk-otter", "carol", "quadhost");

    let message =
        send_to(&remote, "brisk-otter", &alice, "a quiet word", &["bob"]).expect("send with --to");
    assert_eq!(message.recipients, vec![bob.clone()]);

    // A full identity, and one the body already mentions, which is not two
    // recipients.
    let message = send_to(
        &remote,
        "brisk-otter",
        &alice,
        "@bob and carol",
        &[&carol, "bob"],
    )
    .expect("send with --to");
    assert_eq!(message.recipients, vec![bob, carol]);
}

#[test]
fn an_unknown_mention_is_refused_with_the_participant_list_and_writes_nothing() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    let before = remote.messages("brisk-otter", 0, 100).expect("read").len();

    for body in ["@dave hello", "@alice@nowhere hello"] {
        let err = send_to(&remote, "brisk-otter", &alice, body, &[]).expect_err("no such name");
        let message = format!("{err:#}");
        assert!(message.contains("brisk-otter"), "{message}");
        assert!(
            message.contains(&alice) && message.contains(&bob),
            "the participants are not listed: {message}"
        );
        assert_eq!(message.lines().count(), 1, "{message}");
    }

    // A --to that names nobody is refused the same way.
    let err = send_to(&remote, "brisk-otter", &alice, "hello", &["dave"]).expect_err("no dave");
    assert!(format!("{err:#}").contains(&bob), "{err:#}");

    // Nothing was written on the way to any of those.
    let after = remote.messages("brisk-otter", 0, 100).expect("read");
    assert_eq!(after.len(), before);
}

#[test]
fn an_ambiguous_short_form_is_refused_with_the_candidates() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let here = join(&remote, "brisk-otter", "bob");
    let there = join_on(&remote, "brisk-otter", "bob", "quadhost");

    let err = send_to(&remote, "brisk-otter", &alice, "@bob look", &[]).expect_err("two bobs");
    let message = format!("{err:#}");
    assert!(message.contains("ambiguous"), "{message}");
    assert!(
        message.contains(&here) && message.contains(&there),
        "the candidates are not named: {message}"
    );
    assert_eq!(message.lines().count(), 1, "{message}");

    // The full form is never ambiguous.
    let sent = send(&remote, "brisk-otter", &alice, &format!("@{there} look"));
    assert_eq!(sent.recipients, vec![there]);
}

#[test]
fn a_body_over_the_cap_is_refused() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let just_fits = "a".repeat(saneha::api::MAX_BODY);
    send(&remote, "brisk-otter", &alice, &just_fits);

    let too_long = "a".repeat(saneha::api::MAX_BODY + 1);
    let err = send_to(&remote, "brisk-otter", &alice, &too_long, &[]).expect_err("too long");
    let message = format!("{err:#}");
    assert!(message.contains("65536 bytes"), "{message}");
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(message.chars().count() < 200, "{} chars", message.len());

    // And a body that says nothing is not a message.
    let err = send_to(&remote, "brisk-otter", &alice, "   ", &[]).expect_err("empty");
    assert_eq!(format!("{err:#}"), "a message needs a body");
}

#[test]
fn a_closed_channel_takes_no_messages_but_can_still_be_read() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    send(&remote, "brisk-otter", &alice, "before the close");

    // `saneha close` is a later ticket, so the state is set directly.
    server
        .database()
        .execute(
            "UPDATE channels SET state = 'closed' WHERE name = 'brisk-otter'",
            [],
        )
        .expect("close the channel");

    let err = send_to(&remote, "brisk-otter", &alice, "after the close", &[])
        .expect_err("a closed channel takes no messages");
    let message = format!("{err:#}");
    assert_eq!(
        message,
        "the channel \"brisk-otter\" is closed, so nothing more can be sent to it"
    );

    // Reading it, and finishing reading it, still work.
    let messages = remote.messages_after("brisk-otter", 0).expect("read");
    assert_eq!(messages.len(), 2);
    let me = remote
        .set_read_cursor("brisk-otter", &alice, 2)
        .expect("advance the cursor in a closed channel");
    assert_eq!(me.read_cursor, 2);
}

#[test]
fn only_a_participant_can_send() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    let err = send_to(&remote, "brisk-otter", "ghost@nowhere", "hello", &[])
        .expect_err("a stranger cannot send");
    let message = format!("{err:#}");
    assert!(
        message.contains("join it first: saneha join brisk-otter"),
        "{message}"
    );
    assert_eq!(message.lines().count(), 1, "{message}");
}

#[test]
fn a_read_returns_the_unread_and_advances_the_cursor() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    send(&remote, "brisk-otter", &alice, "@bob first");
    send(&remote, "brisk-otter", &alice, "second");

    let unread = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--as", "bob"]),
    );
    // The system messages are part of the transcript, so they are read too.
    assert!(unread.contains(&format!("{alice} joined")), "{unread}");
    assert!(unread.contains(&format!("{bob} joined")), "{unread}");
    assert!(
        unread.contains("first") && unread.contains("second"),
        "{unread}"
    );
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), 4);

    // The second read has nothing to say, and says nothing.
    let again = server.run(&["read", "brisk-otter", "--as", "bob"]);
    assert!(again.status.success(), "{:?}", again.status.code());
    assert_eq!(String::from_utf8_lossy(&again.stdout), "");
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), 4);

    // Alice has read nothing, so she still has all of it waiting.
    let hers = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--as", "alice"]),
    );
    assert!(hers.contains("first"), "{hers}");
    assert_eq!(read_cursor(&remote, "brisk-otter", &alice), 4);
}

#[test]
fn all_and_since_read_without_advancing() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    send(&remote, "brisk-otter", &alice, "first");
    send(&remote, "brisk-otter", &alice, "second");

    let whole = stdout_of(
        "saneha read --all",
        &server.run(&["read", "brisk-otter", "--all", "--as", "bob"]),
    );
    assert!(
        whole.contains("first") && whole.contains("second"),
        "{whole}"
    );
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), 0);

    let since = stdout_of(
        "saneha read --since",
        &server.run(&["read", "brisk-otter", "--since", "3", "--as", "bob"]),
    );
    assert!(!since.contains("first"), "{since}");
    assert!(since.contains("second"), "{since}");
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), 0);

    // A plain read still has everything waiting, because neither of those
    // moved anything.
    let unread = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--as", "bob"]),
    );
    assert!(unread.contains("first"), "{unread}");
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), 4);
}

#[test]
fn a_read_cursor_never_moves_backwards() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    send(&remote, "brisk-otter", &alice, "first");
    send(&remote, "brisk-otter", &alice, "second");

    let me = remote
        .set_read_cursor("brisk-otter", &alice, 3)
        .expect("advance");
    assert_eq!(me.read_cursor, 3);

    // Behind where it already is: ignored, not an error, so two readers of one
    // identity cannot make each other re-read.
    for behind in [1, 0, -5] {
        let me = remote
            .set_read_cursor("brisk-otter", &alice, behind)
            .expect("a cursor behind the recorded one is ignored");
        assert_eq!(me.read_cursor, 3, "moved backwards to {behind}");
    }

    // Past the end of the transcript: held at the last message, so whatever
    // arrives next is still unread.
    let me = remote
        .set_read_cursor("brisk-otter", &alice, 9_000)
        .expect("advance past the end");
    assert_eq!(me.read_cursor, 3);
}

#[test]
fn a_backlog_longer_than_one_page_is_read_whole() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");

    // One join message each, and then more messages than one page holds.
    let written = DEFAULT_MESSAGE_LIMIT + 5;
    for index in 0..written {
        send(&remote, "brisk-otter", &alice, &format!("message {index}"));
    }
    let last = (written + 2) as i64;

    // One page stops at the limit, and asking for more than the server allows
    // does not get more than the server allows.
    let page = remote.messages("brisk-otter", 0, 10).expect("one page");
    assert_eq!(page.len(), 10);
    let page = remote
        .messages("brisk-otter", 0, 100_000)
        .expect("a page larger than the cap");
    assert_eq!(
        page.len(),
        saneha::api::MAX_MESSAGE_LIMIT.min(last as usize)
    );

    // The client keeps asking until it has the lot.
    let all = remote.messages_after("brisk-otter", 0).expect("read");
    assert_eq!(all.len(), last as usize);
    assert_eq!(all.last().expect("the last message").id, last);
    assert_eq!(all.first().expect("the first message").id, 1);

    let read = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--as", "bob"]),
    );
    assert!(
        read.contains("message 0"),
        "the start of the backlog is missing"
    );
    assert!(
        read.contains(&format!("message {}", written - 1)),
        "the end of the backlog is missing"
    );
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), last);
}

#[test]
fn the_binary_sends_and_reads() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");

    // Only the id on standard output, so ID=$(saneha send ...) is safe.
    let id = stdout_of(
        "saneha send",
        &server.run(&[
            "send",
            "brisk-otter",
            "@bob",
            "have",
            "a",
            "look",
            "--as",
            "alice",
        ]),
    );
    assert_eq!(id, "3");

    let sent = stdout_of(
        "saneha send --json",
        &server.run(&[
            "send",
            "brisk-otter",
            "a quiet word",
            "--to",
            "bob",
            "--as",
            "alice",
            "--json",
        ]),
    );
    let sent: serde_json::Value = serde_json::from_str(&sent).expect("--json prints JSON");
    assert_eq!(sent["id"], 4);
    assert_eq!(sent["channel"], "brisk-otter");
    assert_eq!(sent["from"], alice.as_str());
    assert_eq!(sent["recipients"][0], bob.as_str());
    assert!(sent["created_at"].is_string());

    let read = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--as", "bob"]),
    );
    let lines: Vec<&str> = read.lines().collect();
    // A system message is one line, with a dot where the sender would be.
    assert!(lines[0].starts_with("#1  "), "{:?}", lines[0]);
    assert!(
        lines[0].contains("  ·  ") && lines[0].ends_with(&format!("{alice} joined")),
        "{:?}",
        lines[0]
    );
    // A message names who wrote it and who it is for, and sets its body under
    // that. Both bobs would have to be spelled out; there is only one, so the
    // short name is enough.
    let header = lines
        .iter()
        .find(|line| line.starts_with("#3  "))
        .expect("the message is there");
    assert!(header.contains(&alice), "{header}");
    assert!(header.ends_with("→ bob"), "{header}");
    assert!(read.contains("\n    @bob have a look\n"), "{read}");

    // A broadcast says so.
    assert!(
        server
            .run(&["send", "brisk-otter", "morning", "--as", "alice"])
            .status
            .success(),
        "the broadcast failed"
    );
    let read = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--as", "bob"]),
    );
    assert!(read.contains("→ everyone"), "{read}");

    let json = stdout_of(
        "saneha read --json",
        &server.run(&["read", "brisk-otter", "--all", "--as", "bob", "--json"]),
    );
    let messages: Vec<serde_json::Value> = serde_json::from_str(&json).expect("an array");
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[0]["kind"], "join");
    assert_eq!(messages[2]["kind"], "message");
}

#[test]
fn a_recipient_is_named_in_full_when_two_participants_share_a_name() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let here = join(&remote, "brisk-otter", "bob");
    let there = join_on(&remote, "brisk-otter", "bob", "quadhost");

    send(&remote, "brisk-otter", &alice, &format!("@{there} look"));
    let read = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--all", "--as", "alice"]),
    );
    assert!(read.contains(&format!("→ {there}")), "{read}");
    assert!(!read.contains("→ bob\n"), "{read}");
    assert!(!here.is_empty());
}

#[test]
fn a_body_can_come_from_standard_input() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");

    let mut command = server.command(&["send", "brisk-otter", "-", "--as", "alice"]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the saneha binary");
    child
        .stdin
        .as_mut()
        .expect("standard input")
        .write_all(b"@bob a first line\n\n- and a list\n")
        .expect("write the body");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait");
    let id: i64 = stdout_of("saneha send -", &output).parse().expect("an id");

    let written = remote
        .messages("brisk-otter", id - 1, 1)
        .expect("read it back")
        .pop()
        .expect("the message is there");
    // The lines are kept, and the newline a heredoc ends with is not.
    assert_eq!(written.body, "@bob a first line\n\n- and a list");
    assert_eq!(written.from.as_deref(), Some(alice.as_str()));
    assert_eq!(written.recipients, vec![bob]);

    // A multi-line body is set out under its header, blank line and all.
    let read = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--all", "--as", "alice"]),
    );
    assert!(
        read.contains("    @bob a first line\n\n    - and a list"),
        "{read}"
    );
}

#[test]
fn a_read_that_never_reached_the_reader_marks_nothing_as_read() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");

    // More than a pipe buffer holds, so the write is still going when the
    // reader goes away and cannot have finished before it did.
    let line = "s".repeat(400);
    for index in 0..300 {
        send(&remote, "brisk-otter", &alice, &format!("{index} {line}"));
    }

    let mut child = server
        .command(&["read", "brisk-otter", "--as", "bob"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the saneha binary");
    // Close the read end straight away, the way `saneha read ch | head -1`
    // does once head has what it wanted.
    drop(child.stdout.take());
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "a reader walking away is not a failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Nothing arrived, so nothing was read: the messages are still waiting.
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), 0);
    let read = stdout_of(
        "saneha read",
        &server.run(&["read", "brisk-otter", "--as", "bob"]),
    );
    assert!(read.contains("0 "), "the first message is missing");
    assert!(read.contains("299 "), "the last message is missing");
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), 302);
}

#[test]
fn code_in_a_body_addresses_nobody() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");

    // The kind of message one coding agent sends another: a fenced block whose
    // code is full of at-signs, and a shell line with a package scope in it.
    let body = "@bob here is the change\n\
                ```python\n\
                @dataclass\n\
                class Thing: ...\n\
                ```\n\
                then run `npm i @types/node`";
    let message = send(&remote, "brisk-otter", &alice, body);
    assert_eq!(message.recipients, vec![bob.clone()]);

    // And with nobody named outside the code, it is a broadcast rather than a
    // refusal.
    let only_code = "```\n@dataclass\n```";
    let message = send(&remote, "brisk-otter", &alice, only_code);
    assert!(message.recipients.is_empty(), "{:?}", message.recipients);

    // A name written the way a person types it still finds them.
    let message = send(&remote, "brisk-otter", &alice, "@Bob please look");
    assert_eq!(message.recipients, vec![bob]);
}

#[test]
fn a_body_too_large_to_send_is_refused_before_it_is_sent() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    // Pointed at nothing: if the subcommand asked the server, this would fail
    // with "cannot reach" instead.
    let mut command = server.command(&["send", "brisk-otter", "-", "--as", "alice"]);
    command.env("SANEHA_URL", "http://127.0.0.1:1");
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the saneha binary");
    child
        .stdin
        .as_mut()
        .expect("standard input")
        .write_all("a".repeat(saneha::api::MAX_BODY + 1).as_bytes())
        .expect("write the body");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("65536 bytes"), "{stderr}");
    assert!(stderr.contains("attachment"), "{stderr}");
    assert_eq!(
        remote.messages("brisk-otter", 0, 10).expect("read").len(),
        1
    );
}

#[test]
fn a_request_the_server_cannot_read_is_refused_in_sanehas_own_words() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    // A query that is not a message id.
    let (status, body) = server.raw("GET", "/channels/brisk-otter/messages?after=abc", b"");
    assert_eq!(status, 400);
    let error: serde_json::Value = serde_json::from_str(&body).expect("the saneha error shape");
    let said = error["error"].as_str().expect("an error sentence");
    assert!(
        said.starts_with("saneha could not read that request"),
        "{said}"
    );
    assert_eq!(said.lines().count(), 1, "{said}");

    // A body this server will not read at all, which is what stands between a
    // huge paste and the message cap.
    let huge = format!(
        "{{\"from\":\"{alice}\",\"body\":\"{}\"}}",
        "a".repeat(saneha::api::MAX_BODY * 10)
    );
    let (status, body) = server.raw("POST", "/channels/brisk-otter/messages", huge.as_bytes());
    assert_eq!(status, 413);
    let error: serde_json::Value = serde_json::from_str(&body).expect("the saneha error shape");
    let said = error["error"].as_str().expect("an error sentence");
    assert!(said.contains("65536 bytes"), "{said}");
    assert!(said.contains("attachment"), "{said}");

    // A body that is not JSON at all, on a route that takes JSON.
    let (status, body) = server.raw("POST", "/channels", b"not json");
    assert_eq!(status, 400);
    let error: serde_json::Value = serde_json::from_str(&body).expect("the saneha error shape");
    assert!(
        error["error"]
            .as_str()
            .expect("an error sentence")
            .starts_with("saneha could not read that request"),
        "{error}"
    );

    // Nothing was written on the way to any of that.
    assert_eq!(
        remote.messages("brisk-otter", 0, 10).expect("read").len(),
        1
    );
}

#[test]
fn reading_a_channel_this_identity_has_not_joined_says_to_join_it() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    let refused = server.run(&["read", "brisk-otter", "--as", "stranger"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains("join it first: saneha join brisk-otter"),
        "{stderr}"
    );

    // And a channel that is not there is not reported as one this identity
    // merely has not joined.
    let missing = server.run(&["read", "quiet-heron", "--as", "alice"]);
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("there is no channel named"), "{stderr}");
}
