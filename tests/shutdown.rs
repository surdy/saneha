//! Graceful shutdown: the `saneha` binary, started as a server and then sent
//! the signal a supervisor would send, stops on its own and exits 0.
//!
//! Signals are a unix idea, so is this test binary.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long a stop may take before the test calls it stalled. A graceful
/// shutdown with no connections open takes milliseconds, and one with a wait
/// held open should take no longer, because the signal ends the waits. In
/// production the patience is the Quadlet unit's `StopTimeout=25`, and past
/// that podman's SIGKILL.
const PATIENCE: Duration = Duration::from_secs(5);

#[test]
fn stops_gracefully_on_sigterm() {
    stops_gracefully_on("TERM");
}

#[test]
fn stops_gracefully_on_sigint() {
    stops_gracefully_on("INT");
}

/// Starts a server, waits until it is answering, sends it `signal`, and
/// insists that it stopped by itself and said so.
fn stops_gracefully_on(signal: &str) {
    let database = tempfile::tempdir().expect("temporary directory");
    let mut server = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .args(["serve", "--bind", "127.0.0.1:0", "--db"])
        .arg(database.path().join("saneha.db"))
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the server");

    let mut lines = BufReader::new(server.stdout.take().expect("the server's standard output"))
        .lines()
        .map(|line| line.expect("read a line the server printed"));

    let address = address_from(&lines.next().expect("the line saying where it is serving"));
    // A served request is the proof that the accept loop is running, and so
    // that the shutdown future has been polled and the handlers are installed.
    // Signalling before that would find the default disposition instead.
    let health = get(&address, "/health");
    assert!(
        health.contains("200 OK") && health.contains("\"status\":\"ok\""),
        "the server did not answer /health: {health}"
    );

    let sent = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(server.id().to_string())
        .status()
        .expect("run kill");
    assert!(sent.success(), "kill -{signal} failed");

    let status = wait_for(&mut server, signal);
    assert_eq!(
        status.code(),
        Some(0),
        "the server did not exit 0 on SIG{signal}: {status}"
    );
    // The rest of what it printed: the other startup lines, and last of all
    // the line naming the signal it stopped on.
    let said: Vec<String> = lines.collect();
    assert_eq!(
        said.last().map(String::as_str),
        Some(format!("saneha is stopping on SIG{signal}").as_str()),
        "the server did not say which signal stopped it: {said:?}"
    );
}

/// A held `saneha wait` must not turn a stop into a stall.
///
/// A graceful shutdown waits for every request in flight, and a wait is in
/// flight on purpose for as long as an hour. Without the server ending its own
/// held waits, this stop would wait out the unit's stop timeout and end in
/// SIGKILL, which is exactly the stall the SIGTERM handling above removed.
#[test]
fn a_held_wait_does_not_stall_the_stop() {
    let database = tempfile::tempdir().expect("temporary directory");
    let mut server = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .args(["serve", "--bind", "127.0.0.1:0", "--db"])
        .arg(database.path().join("saneha.db"))
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the server");

    let mut lines = BufReader::new(server.stdout.take().expect("the server's standard output"))
        .lines()
        .map(|line| line.expect("read a line the server printed"));
    let address = address_from(&lines.next().expect("the line saying where it is serving"));
    let url = format!("http://{address}");

    let saneha = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_saneha"))
            .args(args)
            .env("SANEHA_URL", &url)
            .env_remove("SANEHA_AS")
            .output()
            .expect("run the saneha binary");
        assert!(
            output.status.success(),
            "saneha {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    saneha(&["new", "brisk-otter"]);
    saneha(&["join", "brisk-otter", "--as", "alice"]);
    // Reading the join messages leaves alice with nothing unread, so the wait
    // that follows really does block.
    saneha(&["read", "brisk-otter", "--as", "alice"]);

    // A timeout far longer than the moment the signal arrives, so what ends
    // this wait is the server rather than its own clock — and deliberately
    // under the client's thirty-second retry budget, which is the only reason
    // this test ends promptly. A waiter told its server is stopping goes on
    // asking for that budget so it survives a redeploy; with a longer
    // `--timeout` this would sit here for the whole of it before exiting 1.
    let mut waiter = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .args(["wait", "brisk-otter", "--as", "alice", "--timeout", "5"])
        .env("SANEHA_URL", &url)
        .env_remove("SANEHA_AS")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start saneha wait");

    // Long enough that the request is being held rather than still being made.
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        waiter.try_wait().expect("ask about the wait").is_none(),
        "the wait ended before the server was signalled"
    );

    let sent = Command::new("kill")
        .arg("-TERM")
        .arg(server.id().to_string())
        .status()
        .expect("run kill");
    assert!(sent.success(), "kill -TERM failed");

    // Within PATIENCE, not within TimeoutStopSec: the held wait was ended by
    // the server rather than waited out by it.
    let status = wait_for(&mut server, "TERM");
    assert_eq!(
        status.code(),
        Some(0),
        "the server did not exit 0 with a wait held open: {status}"
    );

    // The waiter is told the server is stopping, asks again while it is not
    // there, and gives up as an error rather than hanging or claiming a
    // timeout it never reached.
    let ended = ends_within(
        &mut waiter,
        PATIENCE * 3,
        "SIGTERM to the server it was waiting on",
    );
    assert_eq!(
        ended.code(),
        Some(1),
        "the wait did not end as an error when its server went away: {ended}"
    );
}

/// `127.0.0.1:<port>` out of the line the server prints when it starts.
fn address_from(line: &str) -> String {
    line.rsplit_once("http://")
        .unwrap_or_else(|| panic!("no address in the server's first line: {line}"))
        .1
        .trim()
        .to_string()
}

/// One request over a connection of its own, response and all.
fn get(address: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(address).expect("connect to the server");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("set a read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .expect("send the request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read the response");
    response
}

/// Waits out `PATIENCE` for the server to exit, killing it rather than leaving
/// it behind when it will not.
fn wait_for(server: &mut Child, signal: &str) -> ExitStatus {
    ends_within(server, PATIENCE, &format!("SIG{signal}"))
}

/// Waits out `patience` for a process to exit, killing it rather than leaving
/// it behind when it will not. `after` finishes the sentence it is failed
/// with: what it was that should have ended it.
fn ends_within(child: &mut Child, patience: Duration, after: &str) -> ExitStatus {
    let deadline = Instant::now() + patience;
    loop {
        match child.try_wait().expect("ask whether it exited") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("still running {patience:?} after {after}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
