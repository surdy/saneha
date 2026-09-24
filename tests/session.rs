//! A session is found by its harness session, not by the name it would derive
//! (ADR-0010).
//!
//! Every test here is a Claude Code session running commands the way the
//! harness does — a fresh shell per command, the session id and the harness's
//! own pid in the environment — because the defect this holds shut only
//! appears across commands: a `join` granted one name, and every verb after it
//! derived another.

use std::path::Path;
use std::process::{Command, Output};

mod support;

use support::{stdout_of, TestServer};

fn host() -> String {
    saneha::identity::host(saneha::store::HOST.max)
}

/// A process standing in for a harness session that is still running, which
/// this test can end when it likes.
struct Session {
    id: &'static str,
    child: std::process::Child,
}

impl Session {
    fn start(id: &'static str) -> Session {
        let child = Command::new("sleep")
            .arg("120")
            .spawn()
            .expect("start a process to stand in for a harness session");
        Session { id, child }
    }

    fn end(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// `saneha <args>` as this session runs it: through a shell that is gone
    /// as soon as it returns, under Claude Code's markers.
    fn run(&self, server: &TestServer, directory: &Path, args: &str) -> Output {
        server
            .shell(&format!("\"$SANEHA\" {args}"))
            .current_dir(directory)
            .env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_SESSION_ID", self.id)
            .env("CLAUDE_PID", self.child.id().to_string())
            .output()
            .expect("run the saneha binary through a shell")
    }

    /// The name the server grants this session when another live one holds
    /// the name it asked for.
    fn suffixed(&self, name: &str) -> String {
        let started =
            saneha::identity::process_start(self.child.id()).expect("the process is running");
        let (minute, _) = saneha::identity::start_clock(&started).expect("a clock");
        format!("{name}-{minute}@{}", host())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.end();
    }
}

/// A repository-less directory named `notes-method`, so the derived name is
/// `notes-method`.
fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let parent = tempfile::tempdir().expect("temporary directory");
    let directory = parent.path().join("notes-method");
    std::fs::create_dir(&directory).expect("create the directory");
    (parent, directory)
}

fn cursor_of(server: &TestServer, channel: &str, identity: &str) -> i64 {
    server
        .remote()
        .list_participants(channel)
        .expect("participants")
        .into_iter()
        .find(|participant| participant.identity == identity)
        .unwrap_or_else(|| panic!("{identity} is not in {channel}"))
        .read_cursor
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `fond-ivory`, 2026-09-15: the session picking up a handoff was granted a
/// second name, then read, sent and left with no `--as`. Every one of those
/// acted as the session that had handed over. Now each finds its own.
#[test]
fn a_bare_verb_after_a_join_is_the_session_that_joined() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("fond-ivory"), None)
        .expect("create");
    let (_parent, directory) = project();
    let handing = Session::start("session-handing");
    let picking = Session::start("session-picking");
    let first = format!("notes-method@{}", host());

    let joined = handing.run(&server, &directory, "join fond-ivory");
    assert_eq!(stdout_of("the first join", &joined), first);
    let sent = handing.run(&server, &directory, "send fond-ivory the handoff");
    stdout_of("the handoff", &sent);

    // The first session is still running, so the second is granted a name of
    // its own.
    let second = picking.suffixed("notes-method");
    let joined = picking.run(&server, &directory, "join fond-ivory");
    assert_eq!(stdout_of("the second join", &joined), second);

    let handing_cursor = cursor_of(&server, "fond-ivory", &first);

    // No --as, which is what an agent without the skill runs.
    let read = picking.run(&server, &directory, "read fond-ivory");
    assert!(
        stdout_of("the bare read", &read).contains("the handoff"),
        "the read should print the handoff"
    );
    assert!(
        cursor_of(&server, "fond-ivory", &second) > 0,
        "the reader's own cursor moved"
    );
    assert_eq!(
        cursor_of(&server, "fond-ivory", &first),
        handing_cursor,
        "the other session's cursor must not move"
    );

    let sent = picking.run(&server, &directory, "send fond-ivory taken --json");
    let message: serde_json::Value =
        serde_json::from_str(&stdout_of("the bare send", &sent)).expect("JSON");
    assert_eq!(message["from"], serde_json::json!(second));

    let left = picking.run(&server, &directory, "leave fond-ivory");
    assert_eq!(
        stdout_of("the bare leave", &left),
        format!("{second} left fond-ivory")
    );
    let participants = server
        .remote()
        .list_participants("fond-ivory")
        .expect("participants");
    let away = |identity: &str| {
        participants
            .iter()
            .find(|participant| participant.identity == identity)
            .expect("a participant")
            .away
    };
    assert!(away(&second), "the leave was the picking-up session's");
    assert!(!away(&first), "and not the session that handed over");

    // --as still says who, over the session.
    let named = picking.run(
        &server,
        &directory,
        "send fond-ivory named --as notes-method --json",
    );
    let message: serde_json::Value =
        serde_json::from_str(&stdout_of("the named send", &named)).expect("JSON");
    assert_eq!(message["from"], serde_json::json!(first));
}

/// A session that never joined does not borrow the participant its derived
/// name belongs to, whether that session is still running or has finished.
#[test]
fn a_session_that_has_not_joined_is_refused_rather_than_taken_for_another() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let (_parent, directory) = project();
    let mut holder = Session::start("session-holder");
    let stranger = Session::start("session-stranger");
    let holder_identity = format!("notes-method@{}", host());

    stdout_of(
        "the holder's join",
        &holder.run(&server, &directory, "join brisk-otter"),
    );
    stdout_of(
        "the holder's send",
        &holder.run(&server, &directory, "send brisk-otter hello"),
    );

    let refused = |what: &str, args: &str| {
        let output = stranger.run(&server, &directory, args);
        assert!(!output.status.success(), "{what} should be refused");
        let stderr = stderr_of(&output);
        assert!(
            stderr.contains("this session has not joined")
                && stderr.contains(&holder_identity)
                && stderr.contains("saneha join brisk-otter"),
            "{what}: {stderr}"
        );
    };

    for finished in [false, true] {
        if finished {
            holder.end();
        }
        let before = cursor_of(&server, "brisk-otter", &holder_identity);
        refused("read", "read brisk-otter");
        refused("send", "send brisk-otter not-mine");
        refused("wait", "wait brisk-otter --timeout 1");
        refused("leave", "leave brisk-otter");
        assert_eq!(
            cursor_of(&server, "brisk-otter", &holder_identity),
            before,
            "nothing moved the holder's cursor (finished: {finished})"
        );
    }

    let transcript = server
        .remote()
        .messages_after("brisk-otter", 0)
        .expect("transcript");
    assert!(
        !transcript
            .iter()
            .any(|message| message.body.contains("not-mine")),
        "the refused send wrote nothing"
    );
}

/// A session joining again joins as the name it was granted, not as the one it
/// would derive: otherwise every rejoin of a suffixed session is a third name.
#[test]
fn a_session_joining_again_is_the_participant_it_already_is() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let (_parent, directory) = project();
    let one = Session::start("session-one");
    let two = Session::start("session-two");

    stdout_of("one", &one.run(&server, &directory, "join brisk-otter"));
    let granted = stdout_of("two", &two.run(&server, &directory, "join brisk-otter"));
    assert_eq!(granted, two.suffixed("notes-method"));

    let again = two.run(&server, &directory, "join brisk-otter --json");
    let again: serde_json::Value =
        serde_json::from_str(&stdout_of("two again", &again)).expect("JSON");
    assert_eq!(again["identity"], serde_json::json!(granted));
    assert_eq!(again["resumed"], serde_json::json!(true));
    assert_eq!(
        server
            .remote()
            .list_participants("brisk-otter")
            .expect("participants")
            .len(),
        2
    );
}

/// A join still resumes a name a finished session left, since that is the only
/// way back in for a harness with no session id; but it says so, and says at
/// which cursor, because what that session read is no longer unread.
#[test]
fn a_join_that_inherits_another_session_s_cursor_says_so() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let (_parent, directory) = project();
    let mut before = Session::start("session-before");
    let after = Session::start("session-after");

    stdout_of("join", &before.run(&server, &directory, "join brisk-otter"));
    stdout_of(
        "send",
        &before.run(&server, &directory, "send brisk-otter the-doc"),
    );
    stdout_of("read", &before.run(&server, &directory, "read brisk-otter"));
    before.end();

    let joined = after.run(&server, &directory, "join brisk-otter");
    let identity = stdout_of("the join", &joined);
    assert_eq!(identity, format!("notes-method@{}", host()));
    let stderr = stderr_of(&joined);
    let cursor = cursor_of(&server, "brisk-otter", &identity);
    assert!(cursor > 0, "the cursor was inherited");
    assert!(
        stderr.contains(&format!("resumed {identity}"))
            && stderr.contains(&format!("read cursor {cursor}"))
            && stderr.contains("--all"),
        "{stderr}"
    );

    // Its own session coming back is not news, and says nothing.
    let again = after.run(&server, &directory, "join brisk-otter");
    stdout_of("the second join", &again);
    assert!(
        !stderr_of(&again).contains("resumed"),
        "{}",
        stderr_of(&again)
    );
}

/// The derived name no longer carries the harness, so a participant another
/// harness joined under it is somebody else, and is kept apart even when it
/// is not running.
#[test]
fn a_derived_name_another_harness_holds_is_not_resumed() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let (_parent, directory) = project();
    let session = Session::start("session-claude");

    let codex = server.run_in(
        &directory,
        &["join", "brisk-otter", "--harness", "codex"],
        &[],
    );
    let codex = stdout_of("the codex join", &codex);
    assert_eq!(codex, format!("notes-method@{}", host()));

    let claude = session.run(&server, &directory, "join brisk-otter");
    let granted = stdout_of("the claude join", &claude);
    assert_ne!(granted, codex, "a second participant, not a resume");
    assert!(granted.starts_with("notes-method-"), "{granted}");
    let stderr = stderr_of(&claude);
    assert!(
        stderr.contains("was joined under codex, not claude"),
        "{stderr}"
    );
}
