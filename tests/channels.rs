//! End to end: a server started in this test process on an ephemeral port,
//! exercised through the same HTTP calls the subcommands make, and through the
//! `saneha` binary itself.

use std::process::{Command, Stdio};

mod support;

use support::TestServer;

#[test]
fn creates_channels_with_minted_and_given_names() {
    let server = TestServer::start();
    let remote = server.remote();

    let minted = remote.create_channel(None, None).expect("mint a channel");
    assert!(
        minted.name.contains('-')
            && minted
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'),
        "minted name is not a readable slug: {:?}",
        minted.name
    );
    assert_eq!(minted.state.as_str(), "open");
    assert!(minted.purpose.is_none());
    assert!(!minted.created_at.is_empty());

    let named = remote
        .create_channel(Some("brisk-otter"), Some("coordinating the refactor"))
        .expect("create a named channel");
    assert_eq!(named.name, "brisk-otter");
    assert_eq!(named.purpose.as_deref(), Some("coordinating the refactor"));
    assert_eq!(named.state.as_str(), "open");
}

#[test]
fn a_duplicate_name_fails_clearly() {
    let server = TestServer::start();
    let remote = server.remote();

    remote
        .create_channel(Some("quiet-heron"), None)
        .expect("create the channel");
    let err = remote
        .create_channel(Some("quiet-heron"), None)
        .expect_err("the second create must fail");

    let message = format!("{err:#}");
    assert_eq!(message, "a channel named \"quiet-heron\" already exists");
}

#[test]
fn an_invalid_name_fails_clearly() {
    let server = TestServer::start();
    let remote = server.remote();

    for name in ["Brisk Otter", "brisk_otter", "-leading", "trailing-"] {
        let err = remote
            .create_channel(Some(name), None)
            .expect_err("the name must be rejected");
        let message = format!("{err:#}");
        assert!(
            message.contains("lowercase letters, digits and hyphens"),
            "unhelpful message for {name:?}: {message}"
        );
        assert!(message.contains(name), "{message} does not name {name:?}");
        assert_eq!(message.lines().count(), 1, "{message}");
    }
}

#[test]
fn a_purpose_that_is_not_one_line_fails_clearly() {
    let server = TestServer::start();
    let remote = server.remote();

    let too_long = "p".repeat(100_000);
    for purpose in ["line one\nline two", "line one\r\nline two", &too_long] {
        let err = remote
            .create_channel(Some("brisk-otter"), Some(purpose))
            .expect_err("the purpose must be rejected");
        let message = format!("{err:#}");
        assert!(
            message.contains("one line") && message.contains("256 characters"),
            "unhelpful message: {message}"
        );
        assert_eq!(message.lines().count(), 1, "{message}");
        assert!(message.chars().count() < 400, "{} chars", message.len());
    }

    // Nothing was written on the way to any of those errors.
    assert!(remote.list_channels().expect("list").is_empty());

    let rejected = server.run(&["new", "brisk-otter", "--purpose", "line one\nline two"]);
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("saneha: a purpose is one line"),
        "{stderr}"
    );
}

#[test]
fn a_closed_pipe_ends_the_listing_quietly() {
    let server = TestServer::start();
    let remote = server.remote();

    // More output than a pipe buffer holds, so the write is still going when
    // the reader goes away.
    let purpose = "p".repeat(200);
    for index in 0..400 {
        remote
            .create_channel(Some(&format!("channel-{index}")), Some(&purpose))
            .expect("create");
    }

    for args in [vec!["list"], vec!["list", "--json"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_saneha"))
            .args(&args)
            .env("SANEHA_URL", &server.url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run the saneha binary");
        // Close the read end straight away, the way `saneha list | true` does.
        drop(child.stdout.take());

        let output = child.wait_with_output().expect("wait");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "saneha {args:?} exited with {:?}: {stderr}",
            output.status.code()
        );
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
}

#[test]
fn lists_channels_with_state_and_purpose() {
    let server = TestServer::start();
    let remote = server.remote();

    assert!(remote.list_channels().expect("list").is_empty());

    remote
        .create_channel(Some("brisk-otter"), Some("coordinating the refactor"))
        .expect("create");
    remote
        .create_channel(Some("quiet-heron"), None)
        .expect("create");

    let channels = remote.list_channels().expect("list");
    assert_eq!(channels.len(), 2);

    let otter = channels
        .iter()
        .find(|c| c.name == "brisk-otter")
        .expect("the channel is listed");
    assert_eq!(otter.state.as_str(), "open");
    assert_eq!(otter.purpose.as_deref(), Some("coordinating the refactor"));

    let heron = channels
        .iter()
        .find(|c| c.name == "quiet-heron")
        .expect("the channel is listed");
    assert!(heron.purpose.is_none());
}

#[test]
fn the_binary_creates_and_lists_channels() {
    let server = TestServer::start();

    let minted = server.run(&["new"]);
    assert!(
        minted.status.success(),
        "{}",
        String::from_utf8_lossy(&minted.stderr)
    );
    let minted_name = String::from_utf8_lossy(&minted.stdout).trim().to_string();
    assert!(minted_name.contains('-'), "not a slug: {minted_name:?}");

    let named = server.run(&[
        "new",
        "brisk-otter",
        "--purpose",
        "coordinating the refactor",
    ]);
    assert!(
        named.status.success(),
        "{}",
        String::from_utf8_lossy(&named.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&named.stdout).trim(), "brisk-otter");

    let listed = server.run(&["list"]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let table = String::from_utf8_lossy(&listed.stdout);
    assert!(table.starts_with("NAME"), "{table}");
    assert!(table.contains(&minted_name), "{table}");
    assert!(
        table.contains("brisk-otter")
            && table.contains("open")
            && table.contains("coordinating the refactor"),
        "{table}"
    );

    let json = server.run(&["list", "--json"]);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("--json prints JSON");
    assert_eq!(parsed["channels"].as_array().expect("channels").len(), 2);
}

#[test]
fn the_binary_reports_a_duplicate_on_one_line() {
    let server = TestServer::start();

    assert!(server.run(&["new", "quiet-heron"]).status.success());
    let second = server.run(&["new", "quiet-heron"]);

    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert_eq!(
        stderr.trim(),
        "saneha: a channel named \"quiet-heron\" already exists"
    );
}

#[test]
fn subcommands_need_saneha_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .arg("list")
        .env_remove("SANEHA_URL")
        .output()
        .expect("run the saneha binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("SANEHA_URL is not set"), "{stderr}");
    assert!(stderr.contains("export SANEHA_URL="), "{stderr}");
}

#[test]
fn subcommands_report_an_unreachable_server() {
    // Nothing listens on port 1 of the loopback.
    let output = Command::new(env!("CARGO_BIN_EXE_saneha"))
        .arg("list")
        .env("SANEHA_URL", "http://127.0.0.1:1")
        .output()
        .expect("run the saneha binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("saneha: cannot reach the saneha server at http://127.0.0.1:1"),
        "{stderr}"
    );
}
