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
const HARNESS_ENV: &[&str] = &["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "SANEHA_AS"];

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

    /// A second connection to the server's database, for reading the
    /// transcript and for setting up state no verb can set up yet.
    pub fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.database_path()).expect("open the database directly")
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
