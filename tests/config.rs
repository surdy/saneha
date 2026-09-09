//! Where a verb finds its server when the environment does not say.
//!
//! The order is the whole of it — environment, then the file, then an error
//! naming the fix — and the first of those is what every other test in this
//! suite, and every deploy script, depends on continuing to win.

use std::path::PathBuf;
use std::process::Command;

mod support;

use support::{stdout_of, TestServer};

/// The path `init --url` writes under a home directory.
fn config_path(home: &std::path::Path) -> PathBuf {
    home.join(".config").join("saneha").join("config.json")
}

/// A command with neither the environment variable nor any inherited
/// `XDG_CONFIG_HOME`, so the only address it can find is the one written
/// under the test's own `HOME`.
fn without_env(server: &TestServer, args: &[&str]) -> Command {
    let mut command = server.command(args);
    command.env_remove("SANEHA_URL");
    command.env_remove("XDG_CONFIG_HOME");
    command
}

#[test]
fn a_verb_finds_the_server_this_machine_was_configured_with() {
    let server = TestServer::start();
    server.run(&["new", "quiet-ledger", "--purpose", "the tour"]);

    let saved = without_env(&server, &["init", "--url", &server.url])
        .output()
        .expect("init");
    assert!(
        saved.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&saved.stderr)
    );
    assert!(
        config_path(server.home()).exists(),
        "the address is written where a later verb will look"
    );

    // The verb that follows has nothing in its environment to go on.
    let listed = without_env(&server, &["list"]).output().expect("list");
    assert!(
        listed.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listing = stdout_of("list", &listed);
    assert!(
        listing.contains("quiet-ledger"),
        "it reached the configured server: {listing:?}"
    );
}

#[test]
fn the_environment_still_beats_the_file() {
    // A prefix on one command is how a one-off against another server is
    // done, and how every test here points a child at an OS-picked port. If
    // the file ever won, both would quietly go somewhere else.
    let server = TestServer::start();
    server.run(&["new", "quiet-ledger", "--purpose", "the tour"]);

    let saved = without_env(&server, &["init", "--url", "http://127.0.0.1:1"])
        .output()
        .expect("init");
    assert!(
        saved.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&saved.stderr)
    );

    // `command` sets SANEHA_URL to the real server; the file says otherwise.
    let listed = server.run(&["list"]);
    assert!(listed.status.success(), "the environment is used");
    assert!(stdout_of("list", &listed).contains("quiet-ledger"));
}

#[test]
fn a_machine_with_neither_says_how_to_set_one() {
    let server = TestServer::start();
    let listed = without_env(&server, &["list"]).output().expect("list");

    assert!(!listed.status.success(), "there is nowhere to go");
    let said = String::from_utf8_lossy(&listed.stderr).to_string();
    assert!(
        said.contains("init --url"),
        "it names the durable fix: {said}"
    );
    assert!(
        said.contains("SANEHA_URL"),
        "and the one-off, which is what a person may want instead: {said}"
    );
}

#[test]
fn an_address_no_verb_could_use_is_refused_before_it_is_saved() {
    // The one place being wrong is durable: a typo in the environment is gone
    // with the shell, a typo here is on the machine until somebody finds it.
    let server = TestServer::start();
    for bad in ["not a url", "http://", "http:///"] {
        let tried = without_env(&server, &["init", "--url", bad])
            .output()
            .expect("init");
        assert!(!tried.status.success(), "{bad:?} is refused");
        assert!(
            !config_path(server.home()).exists(),
            "and nothing is written for {bad:?}"
        );
    }
}

#[test]
fn a_dry_run_says_what_it_would_save_and_saves_nothing() {
    let server = TestServer::start();
    let tried = without_env(&server, &["init", "--url", &server.url, "--dry-run"])
        .output()
        .expect("init");

    let said = stdout_of("init --dry-run", &tried);
    assert!(
        said.contains("would save"),
        "it says what it would do: {said:?}"
    );
    assert!(!config_path(server.home()).exists(), "and writes nothing");
}

#[test]
fn init_without_an_address_leaves_the_one_that_is_there() {
    // `init` is run on its own to bring the pointers up to date. That must not
    // be a way to lose the address the machine already had.
    let server = TestServer::start();
    without_env(&server, &["init", "--url", &server.url])
        .output()
        .expect("init");

    let again = without_env(&server, &["init"])
        .output()
        .expect("init again");
    assert!(
        again.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&again.stderr)
    );

    let listed = without_env(&server, &["list"]).output().expect("list");
    assert!(
        listed.status.success(),
        "the address survived: {:?}",
        String::from_utf8_lossy(&listed.stderr)
    );
}
