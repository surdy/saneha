//! Waiting: what wakes a participant, what does not, how a wait ends, and the
//! exit code it ends with.
//!
//! Every test here runs the real `saneha wait` as its own process against a
//! server in this one, because the exit code is half of what the verb answers
//! and only a process has one.

use std::io::Read;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use saneha::api::{JoinRequest, Message, NewMessage};
use saneha::client::{JoinAnswer, Remote};

mod support;

use support::TestServer;

/// How long a test waits for a `wait` that should already have ended. Waking
/// on a send is a round trip over the loopback interface; five seconds is
/// slack for a loaded machine, and still far under the minute a held request
/// would otherwise sit there for.
const PATIENCE: Duration = Duration::from_secs(5);

/// Long enough for a wake to have happened if one was going to. A wait that is
/// still waiting after this is one that decided not to be woken.
const SETTLE: Duration = Duration::from_millis(600);

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
        cwd: "/repos/saneha".to_string(),
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

fn read_cursor(remote: &Remote, channel: &str, identity: &str) -> i64 {
    remote
        .participant(channel, identity)
        .expect("look up the participant")
        .expect("the participant is there")
        .read_cursor
}

/// Closes a channel, as a person would from the viewer. The close is what
/// wakes the waiters, so these tests use the verb rather than the state it
/// leaves behind.
fn close(remote: &Remote, channel: &str) {
    remote
        .close_channel(channel, "surdy@web")
        .expect("close the channel");
}

/// A `saneha wait` running as its own process, and when it started.
struct Waiter {
    child: Child,
    started: Instant,
}

impl Waiter {
    fn start(server: &TestServer, args: &[&str]) -> Waiter {
        let child = server
            .command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start saneha wait");
        Waiter {
            child,
            started: Instant::now(),
        }
    }

    /// Whether it is still holding on, asked without waiting for it.
    fn still_waiting(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("ask whether the wait ended")
            .is_none()
    }

    /// Waits out [`PATIENCE`] for it to end, and answers with its exit code,
    /// what it printed, and how long it took.
    fn finish(mut self) -> Ended {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if !self.still_waiting() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("the wait was still running {PATIENCE:?} after it should have ended");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let took = self.started.elapsed();
        let mut printed = String::new();
        self.child
            .stdout
            .take()
            .expect("the wait's standard output")
            .read_to_string(&mut printed)
            .expect("read what the wait printed");
        let mut said = String::new();
        self.child
            .stderr
            .take()
            .expect("the wait's standard error")
            .read_to_string(&mut said)
            .expect("read what the wait said");
        let status = self.child.wait().expect("reap the wait");

        Ended {
            code: status.code().unwrap_or_else(|| panic!("killed: {status}")),
            printed,
            said,
            took,
        }
    }
}

/// How a wait ended.
struct Ended {
    code: i32,
    printed: String,
    said: String,
    took: Duration,
}

impl Ended {
    #[track_caller]
    fn exited(&self, code: i32) -> &Ended {
        assert_eq!(
            self.code, code,
            "the wait exited {} rather than {code}; it printed {:?} and said {:?}",
            self.code, self.printed, self.said
        );
        self
    }
}

#[test]
fn something_already_unread_comes_back_at_once() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    join(&remote, "brisk-otter", "bob");
    send(&remote, "brisk-otter", &alice, "the tests pass");

    // Nothing catches bob up, so the joins and the message are all unread.
    let ended = Waiter::start(&server, &["wait", "brisk-otter", "--as", "bob"]).finish();
    ended.exited(0);
    assert!(
        ended.printed.contains("the tests pass"),
        "the wait did not print the message: {:?}",
        ended.printed
    );
    // Most of this is starting a process; what it is not is the minute a held
    // request would have sat there for.
    assert!(
        ended.took < Duration::from_secs(3),
        "a wait with something unread took {:?}",
        ended.took
    );
}

#[test]
fn a_send_wakes_a_waiting_participant() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Waiter::start(&server, &["wait", "brisk-otter", "--as", "bob"]);
    // Far enough in that the wait is holding rather than still starting.
    std::thread::sleep(SETTLE);
    assert!(
        waiter.still_waiting(),
        "the wait ended before anything came"
    );

    let sent = Instant::now();
    send(&remote, "brisk-otter", &alice, "look at this");
    let ended = waiter.finish();

    ended.exited(0);
    assert!(
        ended.printed.contains("look at this"),
        "the wait did not print what woke it: {:?}",
        ended.printed
    );
    // The hold is a minute; anything near a second is the notifier and not
    // the hold running out.
    assert!(
        sent.elapsed() < Duration::from_secs(2),
        "the wait took {:?} to notice a send",
        sent.elapsed()
    );
}

#[test]
fn mentions_sleeps_through_what_is_addressed_to_somebody_else() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    join(&remote, "brisk-otter", "carol");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Waiter::start(
        &server,
        &["wait", "brisk-otter", "--as", "bob", "--mentions"],
    );
    std::thread::sleep(SETTLE);

    send(&remote, "brisk-otter", &alice, "@carol only for you");
    std::thread::sleep(SETTLE);
    assert!(
        waiter.still_waiting(),
        "a message addressed to somebody else woke the wait"
    );

    send(&remote, "brisk-otter", &alice, "@bob now you");
    let ended = waiter.finish();

    ended.exited(0);
    assert!(
        ended.printed.contains("now you"),
        "the wait did not print what woke it: {:?}",
        ended.printed
    );
    assert!(
        !ended.printed.contains("only for you"),
        "the wait printed a message addressed to somebody else: {:?}",
        ended.printed
    );
}

/// The skill loop is wait, read, reply, wait. If the reply woke the wait that
/// follows it, an agent would spin instead of idling.
#[test]
fn nobody_is_woken_by_their_own_message() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mine = send(&remote, "brisk-otter", &bob, "@alice on it");
    // A sender that had read everything stays caught up, so its own message is
    // not even unread: `read` will not echo it back either.
    assert_eq!(read_cursor(&remote, "brisk-otter", &bob), mine.id);

    let ended = Waiter::start(
        &server,
        &["wait", "brisk-otter", "--as", "bob", "--timeout", "2"],
    )
    .finish();
    ended.exited(3);
    assert!(
        ended.printed.is_empty(),
        "a participant was woken by its own message: {:?}",
        ended.printed
    );
}

/// Not a wake, but not hidden either: a sender that was already behind keeps
/// its own message in the backlog, and what a wait prints stays byte for byte
/// what `read` would print.
#[test]
fn an_own_message_is_printed_with_the_backlog_it_sits_in() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    // Alice speaks first, so bob is behind; bob's own message after that does
    // not move a cursor that was not at the end.
    send(&remote, "brisk-otter", &alice, "from alice");
    let before = read_cursor(&remote, "brisk-otter", &bob);
    send(&remote, "brisk-otter", &bob, "from bob");
    assert_eq!(
        read_cursor(&remote, "brisk-otter", &bob),
        before,
        "a send moved the cursor of a sender that was behind"
    );

    let ended = Waiter::start(&server, &["wait", "brisk-otter", "--as", "bob"]).finish();
    ended.exited(0);
    assert!(
        ended.printed.contains("from alice") && ended.printed.contains("from bob"),
        "the backlog was not printed whole: {:?}",
        ended.printed
    );
    assert_eq!(
        read_cursor(&remote, "brisk-otter", &bob),
        before,
        "the wait moved the read cursor"
    );
}

#[test]
fn mentions_wakes_on_a_broadcast() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let mut waiter = Waiter::start(
        &server,
        &["wait", "brisk-otter", "--as", "bob", "--mentions"],
    );
    std::thread::sleep(SETTLE);
    assert!(
        waiter.still_waiting(),
        "the wait ended before anything came"
    );

    // No recipients at all, which is everyone.
    send(&remote, "brisk-otter", &alice, "morning, all");
    let ended = waiter.finish();

    ended.exited(0);
    assert!(
        ended.printed.contains("morning, all"),
        "a broadcast did not wake a wait for mentions: {:?}",
        ended.printed
    );
}

#[test]
fn a_timeout_with_nothing_to_say_exits_three_and_prints_nothing() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    let ended = Waiter::start(
        &server,
        &["wait", "brisk-otter", "--as", "bob", "--timeout", "1"],
    )
    .finish();

    ended.exited(3);
    assert!(
        ended.printed.is_empty(),
        "a wait that timed out printed {:?}",
        ended.printed
    );
}

#[test]
fn waiting_never_moves_the_read_cursor() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);
    let before = read_cursor(&remote, "brisk-otter", &bob);

    let waiter = Waiter::start(&server, &["wait", "brisk-otter", "--as", "bob"]);
    std::thread::sleep(SETTLE);
    let message = send(&remote, "brisk-otter", &alice, "still unread after this");
    waiter.finish().exited(0);

    assert_eq!(
        read_cursor(&remote, "brisk-otter", &bob),
        before,
        "the wait moved the read cursor"
    );
    // And the message it printed is still there to be read.
    let unread = remote
        .messages_after("brisk-otter", before)
        .expect("the transcript");
    assert_eq!(unread.last().map(|last| last.id), Some(message.id));
}

#[test]
fn a_hold_that_elapses_is_re_issued_until_the_timeout() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    // A one-second hold, so several of them come and go before the message
    // does and the caller has to ask again each time.
    let mut waiter = Waiter::start(
        &server,
        &[
            "wait",
            "brisk-otter",
            "--as",
            "bob",
            "--timeout",
            "20",
            "--hold",
            "1",
        ],
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        waiter.still_waiting(),
        "the wait gave up when a hold elapsed instead of asking again"
    );

    send(&remote, "brisk-otter", &alice, "after three holds");
    let ended = waiter.finish();

    ended.exited(0);
    assert!(
        ended.printed.contains("after three holds"),
        "the re-issued wait did not print what woke it: {:?}",
        ended.printed
    );
}

#[test]
fn a_closed_channel_ends_the_wait_with_four() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let bob = join(&remote, "brisk-otter", "bob");
    let last = send(&remote, "brisk-otter", &alice, "one last thing");
    catch_up(&remote, "brisk-otter", &bob);
    close(&remote, "brisk-otter");

    // Bob was caught up before the close, so the only thing left for him is
    // the close itself, which is a system message like any other.
    let ended = Waiter::start(&server, &["wait", "brisk-otter", "--as", "bob"]).finish();
    ended.exited(4);
    assert!(
        ended.printed.contains("closed the channel"),
        "a wait on a closed channel did not print the close: {:?}",
        ended.printed
    );
    assert!(
        !ended.printed.contains("one last thing"),
        "a wait printed what this participant had already read: {:?}",
        ended.printed
    );

    // What had arrived is printed before the four: a closed channel is the end
    // of a transcript, not a reason to drop the end of it. Alice never caught
    // up, so her own message is still unread by her.
    let ended = Waiter::start(&server, &["wait", "brisk-otter", "--as", "alice"]).finish();
    ended.exited(4);
    assert!(
        ended.printed.contains(&format!("#{}", last.id)),
        "a closed channel dropped what was still unread: {:?}",
        ended.printed
    );
}

#[test]
fn closing_a_channel_ends_a_wait_already_in_progress() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    let bob = join(&remote, "brisk-otter", "bob");
    catch_up(&remote, "brisk-otter", &bob);

    // The first process a test starts is slow to reach its first request, and
    // this one has to be holding before the close lands, so that cost is paid
    // here rather than inside SETTLE.
    server.run(&["list"]);
    let mut waiter = Waiter::start(&server, &["wait", "brisk-otter", "--as", "bob"]);
    std::thread::sleep(SETTLE);
    assert!(waiter.still_waiting(), "the wait ended before the close");

    // The close wakes it: no re-issue, no waiting out the hold.
    let closed = Instant::now();
    close(&remote, "brisk-otter");
    let ended = waiter.finish();
    ended.exited(4);
    assert!(
        closed.elapsed() < Duration::from_secs(1),
        "the wait took {:?} to notice a close",
        closed.elapsed()
    );
}

#[test]
fn an_unknown_channel_is_an_error() {
    let server = TestServer::start();
    let ended = Waiter::start(&server, &["wait", "keen-heron", "--as", "bob"]).finish();
    ended.exited(1);
    assert!(
        ended.said.contains("keen-heron"),
        "the error did not name the channel: {:?}",
        ended.said
    );
}

#[test]
fn waiting_on_a_channel_this_identity_has_not_joined_is_an_error() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    let ended = Waiter::start(&server, &["wait", "brisk-otter", "--as", "stranger"]).finish();
    ended.exited(1);
    assert!(
        ended.said.contains("saneha join brisk-otter"),
        "the error did not say how to join: {:?}",
        ended.said
    );
}
