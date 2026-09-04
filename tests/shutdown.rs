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
/// shutdown with no connections open takes milliseconds; the container's own
/// patience is ten seconds.
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
    let deadline = Instant::now() + PATIENCE;
    loop {
        match server.try_wait().expect("ask whether the server exited") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = server.kill();
                let _ = server.wait();
                panic!("the server was still running {PATIENCE:?} after SIG{signal}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}
