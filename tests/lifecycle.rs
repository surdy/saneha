//! Leave, close and delete: what each one writes, what it changes, what it
//! refuses afterwards, and what happens to a wait being held while it lands.
//!
//! The verbs run as real processes wherever the exit code is half the answer,
//! because only a process has one.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use saneha::api::{ChannelState, JoinRequest, Message, MessageKind, NewMessage};
use saneha::client::{JoinAnswer, Remote};

mod support;

use support::{stdout_of, TestServer};

/// How long a test waits for a wait that should already have ended.
const PATIENCE: Duration = Duration::from_secs(5);

/// Long enough for a wake to have reached a wait that is already holding. A
/// wait still waiting after this is one that decided not to be woken, rather
/// than one that had not started listening yet — that part is not guessed at,
/// it is asked of the server with `wait_until_holding`.
const SETTLE: Duration = Duration::from_millis(600);

/// What "promptly" means for a waiter that was woken by a verb rather than by
/// its own hold running out.
///
/// It is measured from before the verb runs, so it covers starting that
/// process as well as the wake itself; the hold it is being told apart from is
/// a whole minute, so a couple of seconds is both far under that and enough
/// slack for a loaded machine.
const PROMPTLY: Duration = Duration::from_secs(3);

/// This machine as the binary under test sees it, which is the host half of
/// every identity a subcommand claims here.
fn host() -> String {
    saneha::identity::host(saneha::store::HOST.max)
}

fn channel(remote: &Remote, name: &str) {
    remote.create_channel(Some(name), None).expect("create");
}

/// Joins `name` on this host and returns the identity granted.
fn join(remote: &Remote, channel: &str, name: &str) -> String {
    let request = JoinRequest {
        name: name.to_string(),
        host: host(),
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

fn send(remote: &Remote, channel: &str, from: &str, body: &str) -> Message {
    remote
        .send_message(
            channel,
            &NewMessage {
                from: from.to_string(),
                body: body.to_string(),
                to: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .expect("send")
}

/// Moves a participant's read cursor to the end of the transcript, which is
/// where `saneha read` would leave it. Every join writes a system message, so
/// without this a fresh participant always has something unread and no wait
/// would ever block.
fn catch_up(remote: &Remote, channel: &str, identity: &str) {
    let last = remote
        .messages_after(channel, 0)
        .expect("the transcript")
        .last()
        .map(|message| message.id)
        .unwrap_or_default();
    if last > 0 {
        remote
            .set_read_cursor(channel, identity, last)
            .expect("move the read cursor");
    }
}

fn transcript(remote: &Remote, channel: &str) -> Vec<Message> {
    remote.messages_after(channel, 0).expect("the transcript")
}

fn participant(remote: &Remote, channel: &str, identity: &str) -> saneha::api::Participant {
    remote
        .participant(channel, identity)
        .expect("look up the participant")
        .expect("the participant is there")
}

/// A subcommand running as its own process.
struct Running {
    child: Child,
}

impl Running {
    fn start(server: &TestServer, args: &[&str]) -> Running {
        let child = server
            .command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the subcommand");
        Running { child }
    }

    /// Whether it is still going, asked without waiting for it.
    fn still_waiting(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("ask whether it ended")
            .is_none()
    }

    /// Waits out [`PATIENCE`] for it to end, and answers with its exit code
    /// and what it printed. How long it took is measured from the verb that
    /// should have ended it, not from here: what a test is asking is whether
    /// the wake arrived, and the process was already running when it did.
    fn finish(mut self) -> Ended {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if !self.still_waiting() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("it was still running {PATIENCE:?} after it should have ended");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let mut printed = String::new();
        self.child
            .stdout
            .take()
            .expect("standard output")
            .read_to_string(&mut printed)
            .expect("read what it printed");
        let mut said = String::new();
        self.child
            .stderr
            .take()
            .expect("standard error")
            .read_to_string(&mut said)
            .expect("read what it said");
        let status = self.child.wait().expect("reap it");

        Ended {
            code: status.code().unwrap_or_else(|| panic!("killed: {status}")),
            printed,
            said,
        }
    }
}

/// How a run ended.
struct Ended {
    code: i32,
    printed: String,
    said: String,
}

impl Ended {
    #[track_caller]
    fn exited(&self, code: i32) -> &Ended {
        assert_eq!(
            self.code, code,
            "it exited {} rather than {code}; it printed {:?} and said {:?}",
            self.code, self.printed, self.said
        );
        self
    }
}

// ---------------------------------------------------------------- leave

#[test]
fn a_leave_writes_a_system_message_and_marks_the_participant_away() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let bob = join(&remote, "brisk-otter", "bob");

    let printed = stdout_of(
        "leave",
        &server.run(&["leave", "brisk-otter", "--as", "bob"]),
    );
    assert_eq!(printed, format!("{bob} left brisk-otter"));

    let last = transcript(&remote, "brisk-otter")
        .pop()
        .expect("a transcript");
    assert_eq!(last.kind, MessageKind::Leave);
    // A system message is written by the server about a participant, never by
    // one: that is what the schema's CHECK holds.
    assert_eq!(last.from, None);
    assert_eq!(last.about.as_deref(), Some(bob.as_str()));
    assert_eq!(last.body, format!("{bob} left"));

    let bob_now = participant(&remote, "brisk-otter", &bob);
    assert!(bob_now.away, "the leave did not mark the participant away");
    assert!(
        bob_now.left_at.is_some(),
        "the leave recorded no time it happened"
    );
}

#[test]
fn leaving_twice_does_nothing_the_second_time() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let bob = join(&remote, "brisk-otter", "bob");

    server.run(&["leave", "brisk-otter", "--as", "bob"]);
    let first = transcript(&remote, "brisk-otter");
    let left_at = participant(&remote, "brisk-otter", &bob).left_at;

    let output = server.run(&["leave", "brisk-otter", "--as", "bob"]);
    let printed = stdout_of("the second leave", &output);
    assert!(
        printed.contains("already away"),
        "the second leave did not say it had already happened: {printed:?}"
    );

    let second = transcript(&remote, "brisk-otter");
    assert_eq!(
        first.len(),
        second.len(),
        "the second leave wrote a second system message"
    );
    assert_eq!(
        participant(&remote, "brisk-otter", &bob).left_at,
        left_at,
        "the second leave moved the time of the first"
    );
}

#[test]
fn a_join_after_a_leave_resumes_and_clears_away() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    send(&remote, "brisk-otter", &alice, "before you left");
    catch_up(&remote, "brisk-otter", &bob);
    let cursor = participant(&remote, "brisk-otter", &bob).read_cursor;

    server.run(&["leave", "brisk-otter", "--as", "bob"]);
    // Messages go on arriving for an away participant, which is the whole
    // point of leaving rather than being removed.
    send(&remote, "brisk-otter", &alice, "@bob while you were away");

    let rejoined = join(&remote, "brisk-otter", "bob");
    assert_eq!(rejoined, bob, "the resume was granted another identity");

    let bob_now = participant(&remote, "brisk-otter", &bob);
    assert!(!bob_now.away, "the join did not clear away");
    assert_eq!(bob_now.left_at, None, "the join did not clear left_at");
    assert_eq!(
        bob_now.read_cursor, cursor,
        "the resume moved the read cursor"
    );
}

#[test]
fn an_away_participant_can_still_be_mentioned() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    server.run(&["leave", "brisk-otter", "--as", "bob"]);

    let message = send(&remote, "brisk-otter", &alice, "@bob are you there");
    assert_eq!(message.recipients, vec![bob]);
}

#[test]
fn leaving_a_channel_this_identity_has_not_joined_is_refused() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    let ended = Running::start(&server, &["leave", "brisk-otter", "--as", "stranger"]).finish();
    ended.exited(1);
    assert!(
        ended.said.contains("saneha join brisk-otter"),
        "the refusal did not say how to join: {:?}",
        ended.said
    );
}

#[test]
fn a_leave_wakes_a_waiter() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Running::start(&server, &["wait", "brisk-otter", "--as", "bob"]);
    server.wait_until_holding(1);
    assert!(waiter.still_waiting(), "the wait ended before the leave");

    let left = Instant::now();
    server.run(&["leave", "brisk-otter", "--as", "alice"]);
    let ended = waiter.finish();

    ended.exited(0);
    assert!(
        ended.printed.contains("left"),
        "the wait did not print the leave that woke it: {:?}",
        ended.printed
    );
    assert!(
        left.elapsed() < PROMPTLY,
        "the wait took {:?} to notice a leave",
        left.elapsed()
    );
}

#[test]
fn a_leave_does_not_wake_a_mentions_waiter() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Running::start(
        &server,
        &["wait", "brisk-otter", "--as", "bob", "--mentions"],
    );
    server.wait_until_holding(1);

    server.run(&["leave", "brisk-otter", "--as", "alice"]);
    std::thread::sleep(SETTLE);
    assert!(
        waiter.still_waiting(),
        "a leave woke a wait that asked only for what is addressed to it"
    );

    // And what it did ask for still reaches it, so the wait was asleep rather
    // than broken. Alice left, so she rejoins to be able to say anything.
    let alice = {
        let resumed = join(&remote, "brisk-otter", "alice");
        assert_eq!(resumed, alice);
        alice
    };
    send(&remote, "brisk-otter", &alice, "@bob now you");
    waiter.finish().exited(0);
}

// ---------------------------------------------------------------- close

#[test]
fn a_close_writes_a_system_message_and_closes_the_channel() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    let printed = stdout_of(
        "close",
        &server.run(&["close", "brisk-otter", "--as", "alice"]),
    );
    assert_eq!(printed, "brisk-otter is now closed");

    let last = transcript(&remote, "brisk-otter")
        .pop()
        .expect("a transcript");
    assert_eq!(last.kind, MessageKind::Close);
    // A close is about the channel, so it names nobody in either participant
    // column; who closed it is in the body.
    assert_eq!(last.from, None);
    assert_eq!(last.about, None);
    assert_eq!(last.body, format!("alice@{} closed the channel", host()));

    assert_eq!(
        remote
            .channel_detail("brisk-otter")
            .expect("the channel")
            .channel
            .state,
        ChannelState::Closed
    );
}

#[test]
fn closing_a_closed_channel_changes_nothing() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");
    server.run(&["close", "brisk-otter", "--as", "alice"]);
    let first = transcript(&remote, "brisk-otter");

    let output = server.run(&["close", "brisk-otter", "--as", "alice"]);
    let printed = stdout_of("the second close", &output);
    assert_eq!(printed, "brisk-otter was already closed");
    assert_eq!(
        transcript(&remote, "brisk-otter").len(),
        first.len(),
        "the second close wrote a second system message"
    );
}

#[test]
fn somebody_who_never_joined_may_close() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    // The viewer closes as `surdy@web`, which has joined nothing anywhere.
    let closed = remote
        .close_channel("brisk-otter", "surdy@web")
        .expect("a close by a person");
    assert!(closed.closed);
    assert_eq!(closed.channel.state, ChannelState::Closed);

    let last = transcript(&remote, "brisk-otter")
        .pop()
        .expect("a transcript");
    assert_eq!(last.body, "surdy@web closed the channel");
}

#[test]
fn a_close_by_something_that_is_not_an_identity_is_refused() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");

    let refused = remote
        .close_channel("brisk-otter", "not an identity")
        .expect_err("a close by nobody in particular");
    assert!(
        refused.to_string().contains("name@host"),
        "the refusal did not say what an identity is: {refused}"
    );
    assert_eq!(
        remote
            .channel_detail("brisk-otter")
            .expect("the channel")
            .channel
            .state,
        ChannelState::Open,
        "a refused close closed the channel anyway"
    );
}

#[test]
fn a_closed_channel_refuses_sends_and_joins_and_still_reads() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    send(&remote, "brisk-otter", &alice, "one last thing");
    server.run(&["close", "brisk-otter", "--as", "alice"]);

    let ended = Running::start(&server, &["send", "brisk-otter", "nope", "--as", "alice"]).finish();
    ended.exited(1);
    assert!(
        ended.said.contains("closed"),
        "the send was refused without saying why: {:?}",
        ended.said
    );

    let ended = Running::start(&server, &["join", "brisk-otter", "--as", "carol"]).finish();
    ended.exited(1);
    assert!(
        ended.said.contains("closed"),
        "the join was refused without saying why: {:?}",
        ended.said
    );

    // Reading is what a closed channel is still for.
    let printed = stdout_of(
        "read --all",
        &server.run(&["read", "brisk-otter", "--all", "--as", "alice"]),
    );
    assert!(
        printed.contains("one last thing") && printed.contains("closed the channel"),
        "a closed channel did not read to the end: {printed:?}"
    );
}

#[test]
fn a_close_ends_a_held_wait_with_the_closed_code() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Running::start(&server, &["wait", "brisk-otter", "--as", "bob"]);
    server.wait_until_holding(1);
    assert!(waiter.still_waiting(), "the wait ended before the close");

    let closed = Instant::now();
    server.run(&["close", "brisk-otter", "--as", "alice"]);
    let ended = waiter.finish();

    ended.exited(4);
    assert!(
        closed.elapsed() < PROMPTLY,
        "the wait took {:?} to notice a close",
        closed.elapsed()
    );
}

#[test]
fn a_mentions_wait_is_woken_by_a_close() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Running::start(
        &server,
        &["wait", "brisk-otter", "--as", "bob", "--mentions"],
    );
    server.wait_until_holding(1);
    assert!(waiter.still_waiting(), "the wait ended before the close");

    server.run(&["close", "brisk-otter", "--as", "alice"]);
    waiter.finish().exited(4);
}

#[test]
fn list_shows_the_closed_state() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    channel(&remote, "keen-heron");
    join(&remote, "brisk-otter", "alice");
    server.run(&["close", "brisk-otter", "--as", "alice"]);

    let printed = stdout_of("list", &server.run(&["list"]));
    let row = |name: &str| {
        printed
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("no row for {name} in {printed:?}"))
    };
    assert!(row("brisk-otter").contains("closed"));
    assert!(row("keen-heron").contains("open"));
}

// ---------------------------------------------------------------- delete

#[test]
fn a_delete_without_yes_says_what_would_go_and_removes_nothing() {
    let server = TestServer::start();
    let remote = server.remote();
    let files = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    join(&remote, "brisk-otter", "bob");
    send(&remote, "brisk-otter", &alice, "the tests pass");

    let path = files.path().join("handoff.md");
    std::fs::write(&path, b"0123456789").expect("write the file to attach");
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    remote
        .send_message(
            "brisk-otter",
            &NewMessage {
                from: alice.clone(),
                body: "the handoff".to_string(),
                to: Vec::new(),
                attachments: vec![attachment.id.clone()],
            },
        )
        .expect("send with an attachment");

    let ended = Running::start(&server, &["delete", "brisk-otter"]).finish();
    ended.exited(1);
    assert!(
        ended.printed.contains("2 participants")
            && ended.printed.contains("4 messages")
            && ended.printed.contains("1 attachment (10 bytes)"),
        "the dry run did not say what would go: {:?}",
        ended.printed
    );
    assert!(
        ended.said.contains("--yes"),
        "the refusal did not say how to go ahead: {:?}",
        ended.said
    );

    // And it removed nothing.
    assert_eq!(transcript(&remote, "brisk-otter").len(), 4);
    assert!(remote.channel_detail("brisk-otter").is_ok());
}

#[test]
fn a_delete_removes_the_rows_and_the_files() {
    let server = TestServer::start();
    let remote = server.remote();
    let files = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    channel(&remote, "keen-heron");
    let alice = join(&remote, "brisk-otter", "alice");
    join(&remote, "keen-heron", "alice");

    let path = files.path().join("handoff.md");
    std::fs::write(&path, b"the handoff itself").expect("write the file to attach");
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    remote
        .send_message(
            "brisk-otter",
            &NewMessage {
                from: alice.clone(),
                body: "the handoff".to_string(),
                to: Vec::new(),
                attachments: vec![attachment.id.clone()],
            },
        )
        .expect("send with an attachment");

    let channel_id: i64 = server
        .database()
        .query_row(
            "SELECT id FROM channels WHERE name = 'brisk-otter'",
            [],
            |row| row.get(0),
        )
        .expect("the channel row id");
    let directory: PathBuf = server.attachments_path().join(channel_id.to_string());
    assert!(
        directory.join(&attachment.id).exists(),
        "the attachment was never stored"
    );

    let printed = stdout_of("delete", &server.run(&["delete", "brisk-otter", "--yes"]));
    assert!(
        printed.contains("deleted brisk-otter")
            && printed.contains("1 participant")
            && printed.contains("1 attachment"),
        "the delete did not say what went: {printed:?}"
    );

    // The row, and by the cascade everything keyed to it.
    let count = |table: &str| -> i64 {
        server
            .database()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE channel_id = ?1"),
                [channel_id],
                |row| row.get(0),
            )
            .expect("count the rows")
    };
    assert_eq!(count("participants"), 0);
    assert_eq!(count("messages"), 0);
    assert_eq!(count("attachments"), 0);
    assert_eq!(count("recipients"), 0);

    // And the files that row id named.
    assert!(
        !directory.exists(),
        "the deleted channel's files are still there"
    );

    // The other channel is untouched: a delete takes one channel and no more.
    assert!(remote.channel_detail("keen-heron").is_ok());
    assert_eq!(transcript(&remote, "keen-heron").len(), 1);
}

#[test]
fn a_delete_ends_a_held_wait() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Running::start(&server, &["wait", "brisk-otter", "--as", "bob"]);
    server.wait_until_holding(1);
    assert!(waiter.still_waiting(), "the wait ended before the delete");

    let deleted = Instant::now();
    server.run(&["delete", "brisk-otter", "--yes"]);
    let ended = waiter.finish();

    ended.exited(1);
    assert!(
        ended.said.contains("no longer exists"),
        "the wait did not say the channel had gone: {:?}",
        ended.said
    );
    assert!(
        ended.printed.is_empty(),
        "a wait on a deleted channel printed {:?}",
        ended.printed
    );
    assert!(
        deleted.elapsed() < PROMPTLY,
        "the wait took {:?} to notice a delete",
        deleted.elapsed()
    );
}

#[test]
fn deleting_a_channel_that_is_not_there_says_so() {
    let server = TestServer::start();

    let ended = Running::start(&server, &["delete", "keen-heron", "--yes"]).finish();
    ended.exited(1);
    assert!(
        ended.said.contains("keen-heron"),
        "the error did not name the channel: {:?}",
        ended.said
    );

    // The dry run is the same answer, from the same absence.
    let ended = Running::start(&server, &["delete", "keen-heron"]).finish();
    ended.exited(1);
    assert!(ended.said.contains("keen-heron"), "{:?}", ended.said);
}

#[test]
fn a_closed_channel_can_still_be_deleted() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    send(&remote, "brisk-otter", &alice, "the tests pass");
    server.run(&["close", "brisk-otter", "--as", "alice"]);

    let printed = stdout_of("delete", &server.run(&["delete", "brisk-otter", "--yes"]));
    assert!(printed.starts_with("deleted brisk-otter"), "{printed:?}");
    assert!(
        remote.channel_detail("brisk-otter").is_err(),
        "the closed channel survived its own delete"
    );
    assert!(
        remote.list_channels().expect("list").is_empty(),
        "the deleted channel is still listed"
    );
}

#[test]
fn a_delete_needs_the_confirmation_in_the_url() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");

    let (status, body) = server.raw("DELETE", "/channels/brisk-otter", b"");
    assert_eq!(status, 400, "an unconfirmed delete: {body}");
    assert!(body.contains("--yes"), "{body}");
    assert!(
        remote.channel_detail("brisk-otter").is_ok(),
        "an unconfirmed delete removed the channel anyway"
    );

    let (status, body) = server.raw("DELETE", "/channels/brisk-otter?confirm=true", b"");
    assert_eq!(status, 200, "a confirmed delete: {body}");
    assert!(remote.channel_detail("brisk-otter").is_err());
}
