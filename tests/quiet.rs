//! Closing quiet channels on the server's own clock, when and only when
//! `saneha serve` was asked to (ADR-0011).
//!
//! Quiet is read the way ADR-0009 reads it — a transcript, and nobody
//! present — so what these hold is the boundary: what is closed, what is
//! left alone, and that nothing at all happens on a server nobody asked.

use std::path::Path;
use std::process::Command;

use saneha::api::{ChannelState, JoinRequest, MessageKind, NewMessage};
use saneha::client::{JoinAnswer, Remote};
use saneha::store::Store;

mod support;

use support::TestServer;

/// The threshold every test here uses, in days.
const AFTER: u32 = 7;

/// This machine as the binary under test sees it.
fn host() -> String {
    saneha::identity::host(saneha::store::HOST.max)
}

fn request(name: &str) -> JoinRequest {
    JoinRequest {
        name: name.to_string(),
        host: host(),
        harness: "claude".to_string(),
        key: None,
        session_id: None,
        pid: None,
        pid_started_at: None,
        cwd: Some("/repos/saneha".to_string()),
        madari_pane: None,
        same_host_session_live: false,
        held_session_id: None,
    }
}

fn channel(remote: &Remote, name: &str) {
    remote.create_channel(Some(name), None).expect("create");
}

fn join(remote: &Remote, channel: &str, name: &str) -> String {
    match remote.join(channel, &request(name)).expect("join") {
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

/// A handoff-shaped transcript: one participant joins, says something and
/// leaves, so the channel is quiet the moment this returns.
fn quiet_channel(remote: &Remote, name: &str) {
    channel(remote, name);
    let who = join(remote, name, "alice");
    send(remote, name, &who, "handoff: where things stand");
    remote.leave(name, &who).expect("leave");
}

/// Makes every message in `name` look `days` old. The sweep measures from the
/// newest message, so this is what "quiet for a week" is made of in a test
/// that cannot wait one.
fn backdate(database: &rusqlite::Connection, name: &str, days: u32) {
    database
        .execute(
            &format!(
                "UPDATE messages
                    SET created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-{days} days')
                  WHERE channel_id = (SELECT id FROM channels WHERE name = ?1)"
            ),
            [name],
        )
        .expect("backdate the transcript");
    database
        .execute(
            &format!(
                "UPDATE channels
                    SET created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-{days} days')
                  WHERE name = ?1"
            ),
            [name],
        )
        .expect("backdate the channel");
}

fn state(remote: &Remote, name: &str) -> ChannelState {
    remote
        .channel_detail(name)
        .expect("look up the channel")
        .channel
        .state
}

fn closes(remote: &Remote, name: &str) -> Vec<String> {
    remote
        .messages_after(name, 0)
        .expect("the transcript")
        .into_iter()
        .filter(|message| message.kind == MessageKind::Close)
        .map(|message| message.body)
        .collect()
}

#[test]
fn the_sweep_closes_what_has_been_quiet_long_enough_and_leaves_the_rest() {
    let server = TestServer::start();
    let remote = server.remote();
    let database = server.database();

    // Quiet for over a week: the one that goes.
    quiet_channel(&remote, "old-otter");
    backdate(&database, "old-otter", AFTER + 1);
    // Quiet, but not for long enough.
    quiet_channel(&remote, "young-otter");
    backdate(&database, "young-otter", AFTER - 1);
    // Old, but somebody is still present — a participant parked on `wait` is
    // exactly this, and it is what makes closing on quiet safe where closing
    // on idle was not.
    channel(&remote, "held-otter");
    let bob = join(&remote, "held-otter", "bob");
    send(&remote, "held-otter", &bob, "still here");
    backdate(&database, "held-otter", AFTER + 1);
    // Old, and joined by nobody: a channel with no transcript is new, not
    // quiet, however long ago it was minted.
    channel(&remote, "new-otter");
    backdate(&database, "new-otter", AFTER + 1);
    // Already closed: nothing to do, and nothing written twice.
    quiet_channel(&remote, "done-otter");
    remote
        .close_channel("done-otter", "surdy@web")
        .expect("close");
    backdate(&database, "done-otter", AFTER + 1);

    let closed = server
        .store()
        .close_quiet_channels(AFTER)
        .expect("close the quiet channels");
    assert_eq!(closed, vec!["old-otter".to_string()]);

    assert_eq!(state(&remote, "old-otter"), ChannelState::Closed);
    assert_eq!(state(&remote, "young-otter"), ChannelState::Open);
    assert_eq!(state(&remote, "held-otter"), ChannelState::Open);
    assert_eq!(state(&remote, "new-otter"), ChannelState::Open);
    assert_eq!(state(&remote, "done-otter"), ChannelState::Closed);

    // The close is the one a `saneha close` writes, saying who and why.
    let said = closes(&remote, "old-otter");
    assert_eq!(said.len(), 1, "one close, not {}: {said:?}", said.len());
    assert_eq!(said[0], "saneha closed the channel, quiet for 7 days");
    assert_eq!(
        closes(&remote, "done-otter").len(),
        1,
        "a closed channel was closed again"
    );
    let detail = remote.channel_detail("old-otter").expect("look up");
    assert!(
        detail.channel.closed_at.is_some(),
        "closed with no closed_at"
    );

    // And it is closed the way anything closed is: no more joins.
    let refused = remote.join("old-otter", &request("carol"));
    assert!(refused.is_err(), "a channel the server closed took a join");

    // Nothing left to do the second time.
    let again = server
        .store()
        .close_quiet_channels(AFTER)
        .expect("sweep again");
    assert!(again.is_empty(), "closed again: {again:?}");
}

#[test]
fn a_server_nobody_asked_closes_nothing_and_says_so() {
    let server = TestServer::start();
    let remote = server.remote();
    quiet_channel(&remote, "old-otter");
    backdate(&server.database(), "old-otter", AFTER * 4);

    let health = remote.health().expect("health");
    assert_eq!(
        health.close_quiet_after_days, None,
        "an unasked server reports a threshold"
    );
    // The start-up sweep has run by now — health answered — and left it.
    assert_eq!(state(&remote, "old-otter"), ChannelState::Open);
}

/// Seeds a database, before any server has opened it, with one channel quiet
/// for over a week and one quiet since a moment ago.
fn seed(path: &Path) {
    let store = Store::open(path).expect("open the database to seed it");
    for name in ["old-otter", "young-otter"] {
        store.create_channel(Some(name), None).expect("create");
        let joined = store.join(name, &request("alice")).expect("join");
        store
            .send(name, &joined.identity, "handoff", &[], &[], None)
            .expect("send");
        store.leave(name, &joined.identity).expect("leave");
    }
    drop(store);
    let database = rusqlite::Connection::open(path).expect("open the database directly");
    backdate(&database, "old-otter", AFTER + 1);
}

#[test]
fn a_server_asked_to_close_quiet_channels_does_so_as_it_starts() {
    let server = TestServer::start_with(
        saneha::server::Options {
            close_quiet_after_days: Some(AFTER),
        },
        seed,
    );
    let remote = server.remote();

    // A threshold crossed while the server was down is acted on as it comes
    // up, not an hour later.
    assert_eq!(state(&remote, "old-otter"), ChannelState::Closed);
    assert_eq!(state(&remote, "young-otter"), ChannelState::Open);
    assert_eq!(
        closes(&remote, "old-otter"),
        vec!["saneha closed the channel, quiet for 7 days".to_string()]
    );

    // And the server says it is one that does this.
    let health = remote.health().expect("health");
    assert_eq!(health.close_quiet_after_days, Some(AFTER));
}

#[test]
fn a_threshold_of_no_days_is_refused_before_anything_is_served() {
    // Zero would close every channel the moment it went quiet, which is the
    // auto-close-on-receipt ADR-0005 refused; the flag will not spell it.
    let database = tempfile::tempdir().expect("temporary directory");
    let output = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--close-quiet-after",
            "0",
            "--db",
        ])
        .arg(database.path().join("saneha.db"))
        .output()
        .expect("run the binary");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains("close-quiet-after"),
        "the refusal did not name the flag: {said}"
    );
}
