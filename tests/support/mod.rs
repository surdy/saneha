//! A saneha server for one test: started in this process on a port the OS
//! picks, over a database in a temporary directory the test can also open.

// Each test binary uses a different part of this.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use saneha::client::Remote;
use saneha::store::Store;
use tempfile::TempDir;

/// The environment a harness leaves behind, cleared before every run so a test
/// sees the machine it describes and not the one it is running on.
const HARNESS_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "SANEHA_AS",
];

pub struct TestServer {
    pub url: String,
    database: TempDir,
}

impl TestServer {
    pub fn start() -> TestServer {
        let database = tempfile::tempdir().expect("temporary directory");
        let path = database.path().join("saneha.db");
        let store = Arc::new(Store::open(&path).expect("open the database"));

        let (send_address, receive_address) = mpsc::channel::<SocketAddr>();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind an ephemeral port");
                send_address
                    .send(listener.local_addr().expect("local address"))
                    .expect("hand the address back");
                saneha::server::run(listener, store).await.expect("serve");
            });
        });

        let address = receive_address.recv().expect("the server to bind");
        TestServer {
            url: format!("http://{address}"),
            database,
        }
    }

    pub fn remote(&self) -> Remote {
        Remote::at(&self.url).expect("remote")
    }

    /// The database file the server is writing to, for the tables no verb
    /// reads yet.
    pub fn database_path(&self) -> PathBuf {
        self.database.path().join("saneha.db")
    }

    /// The directory the server keeps attachment files in, beside the
    /// database.
    pub fn attachments_path(&self) -> PathBuf {
        self.database.path().join("attachments")
    }

    /// A second connection to the server's database, for reading the
    /// transcript and for setting up state no verb can set up yet.
    pub fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.database_path()).expect("open the database directly")
    }

    /// A second store over the same database and the same files, for the
    /// housekeeping no verb does yet: the sweep, and removing a channel's
    /// files.
    pub fn store(&self) -> Store {
        Store::open(&self.database_path()).expect("open the store directly")
    }

    /// Blocks until the server is holding at least `waits` wait requests open.
    ///
    /// A test that starts a `saneha wait` as its own process and then does
    /// something to wake it has to know that the wait is really holding first,
    /// or it is not testing the wake at all: a waiter whose first request has
    /// not gone out yet is answered by that first look instead, and one whose
    /// channel was deleted in the meantime fails with the wrong sentence. A
    /// sleep only guesses at this; the server counts it, so this asks.
    ///
    /// Panics rather than returning, because a wait that never arrives is the
    /// test's premise failing and not one of its outcomes.
    pub fn wait_until_holding(&self, waits: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if self.held_waits() >= waits {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the server was still holding {} wait(s) rather than {waits} after 10s",
                self.held_waits()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// How many wait requests the server is holding open, as `/health` reports
    /// it.
    pub fn held_waits(&self) -> usize {
        let (status, body) = self.raw("GET", "/health", b"");
        assert_eq!(status, 200, "health said {status}: {body}");
        serde_json::from_str::<serde_json::Value>(&body)
            .expect("health is JSON")
            .get("held_waits")
            .and_then(serde_json::Value::as_u64)
            .expect("health reports held_waits") as usize
    }

    /// One raw HTTP/1.1 request, for the shapes no subcommand can make: a
    /// query the server cannot parse, a body larger than it will read. Answers
    /// with the status and the response body.
    ///
    /// The writing happens on its own thread because that is the whole point
    /// of the oversize case: the server answers and closes the connection
    /// without reading the rest, and a single-threaded caller would be stuck
    /// writing into it.
    pub fn raw(&self, method: &str, path: &str, body: &[u8]) -> (u16, String) {
        self.raw_with(method, path, &[("content-type", "application/json")], body)
    }

    /// The same, with the headers spelled out: an upload carries its filename
    /// in one, and a request that frames its own body says so in another.
    /// `content-length` is added only when the headers do not settle the
    /// framing themselves.
    pub fn raw_with(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, String) {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let address = self.url.trim_start_matches("http://").to_string();
        let mut stream = TcpStream::connect(&address).expect("connect to the test server");
        let framed = headers.iter().any(|(name, _)| {
            name.eq_ignore_ascii_case("content-length")
                || name.eq_ignore_ascii_case("transfer-encoding")
        });
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\n");
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if !framed {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("Connection: close\r\n\r\n");
        let payload = [head.as_bytes(), body].concat();
        let mut writer = stream.try_clone().expect("clone the socket");
        let writing = thread::spawn(move || {
            let _ = writer.write_all(&payload);
            let _ = writer.flush();
        });

        let mut answer = Vec::new();
        // A reset while the request is still being written is one of the
        // answers this asks about, so what arrived is what is read.
        let _ = stream.read_to_end(&mut answer);
        let _ = writing.join();

        let answer = String::from_utf8_lossy(&answer).into_owned();
        let status = answer
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {answer:?}"));
        let body = answer
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }

    /// The `saneha` binary, pointed at this server, with any harness
    /// environment this test process happens to be running under removed.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_saneha"));
        command.args(args).env("SANEHA_URL", &self.url);
        for key in HARNESS_ENV {
            command.env_remove(key);
        }
        command
    }

    /// Runs the `saneha` binary against this server.
    pub fn run(&self, args: &[&str]) -> std::process::Output {
        self.command(args).output().expect("run the saneha binary")
    }

    /// A shell that runs `script`, with the binary under test in `$SANEHA`.
    ///
    /// This is how a harness runs a command: a shell is spawned for the call
    /// and is gone the moment it returns, so the `saneha` process has a parent
    /// that outlives it by nothing at all. Anything a join records about a
    /// process has to survive that.
    pub fn shell(&self, script: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .env("SANEHA_URL", &self.url)
            .env("SANEHA", env!("CARGO_BIN_EXE_saneha"));
        for key in HARNESS_ENV {
            command.env_remove(key);
        }
        command
    }

    /// Runs the `saneha` binary from `directory`, with `env` on top.
    pub fn run_in(
        &self,
        directory: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> std::process::Output {
        let mut command = self.command(args);
        command.current_dir(directory);
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run the saneha binary")
    }
}

/// Standard output as a trimmed string, with standard error in the panic
/// message when the command failed.
pub fn stdout_of(what: &str, output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
