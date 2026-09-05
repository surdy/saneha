//! The viewer: the page the server serves at its root, and the hold on the
//! transcript route that lets that page follow a channel live.
//!
//! The page itself is HTML in the binary and is tested for what a server can
//! be held to — that it is served, that it is served under a channel's own
//! path, and that it asks nothing of the internet. What it draws is a matter
//! for a browser, and is verified by hand.

use std::time::{Duration, Instant};

use saneha::api::{JoinRequest, NewMessage};
use saneha::client::{JoinAnswer, Remote};

mod support;

use support::TestServer;

/// How long a test waits for a held request that should already have been
/// answered. Waking on a send is a round trip over the loopback interface.
const PATIENCE: Duration = Duration::from_secs(5);

fn host() -> String {
    saneha::identity::host(saneha::store::HOST.max)
}

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

/// One GET, with the status and the body, and no status turned into an error:
/// a `204` and a `404` are both answers this asks about.
fn get(url: &str) -> (u16, String) {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(90)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent.get(url).call().expect("the server to answer");
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().expect("read the body");
    (status, body)
}

/// The same, with the header a test wants to look at.
fn get_with_header(url: &str, header: &str) -> (u16, String, String) {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent.get(url).call().expect("the server to answer");
    let status = response.status().as_u16();
    let value = response
        .headers()
        .get(header)
        .map(|value| value.to_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    let body = response.body_mut().read_to_string().expect("read the body");
    (status, value, body)
}

#[test]
fn the_root_serves_the_viewer_as_html() {
    let server = TestServer::start();

    let (status, content_type, body) = get_with_header(&server.url, "content-type");
    assert_eq!(status, 200, "the root did not answer: {body}");
    assert!(
        content_type.starts_with("text/html"),
        "the viewer was not served as HTML: {content_type:?}"
    );
    assert!(
        body.contains("<title>saneha</title>"),
        "the root served something that is not the viewer"
    );
    assert!(
        body.contains("<!doctype html>"),
        "the root served something that is not a page"
    );
}

/// A channel is a link somebody can send. The page under that path is the same
/// page: it reads the channel out of its own URL, and nothing about it is
/// rendered on the server.
#[test]
fn a_channel_path_serves_the_same_page() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    let (_, root) = get(&server.url);
    let (status, page) = get(&format!("{}/c/brisk-otter", server.url));
    assert_eq!(status, 200, "a channel path did not serve the page: {page}");
    assert_eq!(page, root, "a channel path served a different page");

    // A name nobody has used is the same page too: what is or is not there is
    // the page's own question to ask, and it says so on screen.
    let (status, page) = get(&format!("{}/c/nothing-here", server.url));
    assert_eq!(status, 200);
    assert_eq!(page, root);
}

/// The server is reachable from a LAN and a tailnet and may have no route out
/// at all, so the page has to be the whole of itself: no script, no
/// stylesheet, no font, no image from anywhere else.
#[test]
fn the_viewer_asks_nothing_of_the_internet() {
    let server = TestServer::start();
    let (_, page) = get(&server.url);

    for scheme in ["http://", "https://"] {
        assert!(
            !page.contains(scheme),
            "the viewer names {scheme} somewhere; it must fetch nothing from the internet"
        );
    }

    // Every `src` and `href` is same-origin: an absolute path, a fragment, or
    // the one inline data URI the favicon is.
    for attribute in ["src=\"", "href=\""] {
        let mut rest = page.as_str();
        while let Some(at) = rest.find(attribute) {
            rest = &rest[at + attribute.len()..];
            let end = rest.find('"').expect("an unterminated attribute value");
            let value = &rest[..end];
            assert!(
                value.starts_with('/') || value.starts_with('#') || value.starts_with("data:"),
                "the viewer points {attribute}{value:?} somewhere other than this server"
            );
            rest = &rest[end..];
        }
    }
}

/// A poll with nothing to wait for is the fetch it always was.
#[test]
fn without_a_hold_the_transcript_answers_at_once() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    let started = Instant::now();
    let (status, body) = get(&format!(
        "{}/channels/brisk-otter/messages?after=0&limit=500",
        server.url
    ));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, r#"{"messages":[]}"#);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a fetch with no hold waited"
    );
}

/// A hold is only a hold when there is nothing to say: a page that already has
/// something after `after` comes back with it.
#[test]
fn a_hold_answers_at_once_when_the_transcript_is_ahead() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    join(&remote, "brisk-otter", "alpha");

    let started = Instant::now();
    let (status, body) = get(&format!(
        "{}/channels/brisk-otter/messages?after=0&hold=30",
        server.url
    ));
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("joined"),
        "the hold did not return the join that was already there: {body}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a hold with something to say waited anyway"
    );
}

/// The point of the whole thing: a message lands and the held request answers
/// with it, rather than the page asking again a second later.
#[test]
fn a_hold_ends_when_a_message_lands() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let alpha = join(&remote, "brisk-otter", "alpha");
    let after = remote
        .messages("brisk-otter", 0, 500)
        .expect("the transcript so far")
        .last()
        .expect("the join")
        .id;

    let url = format!(
        "{}/channels/brisk-otter/messages?after={after}&hold=30",
        server.url
    );
    std::thread::scope(|scope| {
        let held = scope.spawn(move || {
            let (status, body) = get(&url);
            (Instant::now(), status, body)
        });

        // The poll is really being held, rather than still on its way: the
        // server counts what it holds, so this is asked rather than slept out.
        // Otherwise a send that landed first would be answered by the opening
        // look and the wake would go untested.
        server.wait_until_holding(1);
        let sent_at = Instant::now();
        remote
            .send_message(
                "brisk-otter",
                &NewMessage {
                    from: alpha.clone(),
                    body: "the build is green".to_string(),
                    to: Vec::new(),
                    attachments: Vec::new(),
                },
            )
            .expect("send");

        let (answered_at, status, body) = held.join().expect("the held request");
        assert_eq!(status, 200, "{body}");
        assert!(
            body.contains("the build is green"),
            "the hold did not answer with the message that landed: {body}"
        );
        let woken = answered_at.saturating_duration_since(sent_at);
        assert!(
            woken < Duration::from_secs(1),
            "the hold took {woken:?} to answer a send that had already committed"
        );
    });
}

/// A close is what a viewer most needs to hear: it writes a system message, so
/// the same hold brings it back and the page stops asking.
#[test]
fn a_close_ends_a_hold() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let after = 0;

    let url = format!(
        "{}/channels/brisk-otter/messages?after={after}&hold=30",
        server.url
    );
    std::thread::scope(|scope| {
        let held = scope.spawn(move || get(&url));
        server.wait_until_holding(1);
        remote
            .close_channel("brisk-otter", "surdy@web")
            .expect("close");

        let (status, body) = held.join().expect("the held request");
        assert_eq!(status, 200, "{body}");
        assert!(
            body.contains("closed"),
            "the hold did not answer with the close: {body}"
        );
    });
}

/// A hold that hears nothing ends as a `204`, and the page asks again. This is
/// what keeps one held request short enough to survive a reverse proxy while a
/// tab follows a quiet channel for hours.
#[test]
fn a_hold_that_hears_nothing_answers_204() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    let started = Instant::now();
    let (status, body) = get(&format!(
        "{}/channels/brisk-otter/messages?after=0&hold=1",
        server.url
    ));
    let waited = started.elapsed();
    assert_eq!(status, 204, "a hold that heard nothing did not end as 204");
    assert!(body.is_empty(), "a 204 carried a body: {body:?}");
    assert!(
        waited >= Duration::from_millis(800) && waited < PATIENCE,
        "a one second hold ended after {waited:?}"
    );
}

/// A viewer following a channel is a request the server is holding open, and
/// `held_waits` counts it alongside the per-participant waits: what that number
/// answers is how much the server has in flight on purpose, and a tab left open
/// on a quiet channel is part of it.
#[test]
fn a_held_poll_is_counted_and_then_is_not() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    assert_eq!(server.held_waits(), 0, "the server started out holding one");

    let url = format!(
        "{}/channels/brisk-otter/messages?after=0&hold=1",
        server.url
    );
    std::thread::scope(|scope| {
        let held = scope.spawn(move || get(&url));
        server.wait_until_holding(1);
        let (status, _) = held.join().expect("the held poll");
        assert_eq!(status, 204);
    });

    // The guard comes off with the request, however it ended.
    let deadline = Instant::now() + PATIENCE;
    while server.held_waits() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        server.held_waits(),
        0,
        "a poll that ended was still counted as held"
    );
}

/// A channel that is not there is a `404` before anything is held or
/// registered, so a name in a URL that nobody has ever used leaves nothing
/// behind on the server.
#[test]
fn a_hold_on_a_channel_that_is_not_there_is_refused() {
    let server = TestServer::start();

    let started = Instant::now();
    let (status, body) = get(&format!(
        "{}/channels/nothing-here/messages?after=0&hold=30",
        server.url
    ));
    assert_eq!(status, 404, "{body}");
    assert!(
        body.contains("nothing-here"),
        "the refusal did not name the channel: {body}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a channel that is not there was held open anyway"
    );
}
