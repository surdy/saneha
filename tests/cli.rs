//! The command line itself: which flags a verb accepts, rather than what the
//! verb then does.
//!
//! The identity flags are the whole subject. A rehearsal of the wake loop
//! (issue #32) found `--as` rejected by `participants`, `fetch`, `new` and
//! `list` while the skill told an agent to pass it to every verb, so a skill
//! that had been granted a suffixed name could not ask who else was in the
//! channel. They are global options now, and this is what keeps them global.

use std::process::Output;

mod support;

use support::TestServer;

/// What clap exits with when it could not parse the command line. Every case
/// here is about not seeing this.
const USAGE_ERROR: i32 = 2;

/// Every verb that talks to a server, with arguments that are enough to parse.
/// Most of them fail against the server — the channel is not there, or this
/// caller has not joined it — and that is fine: the question is only whether
/// the command line was understood.
const CLIENT_VERBS: &[&[&str]] = &[
    &["new", "flag-check"],
    &["list"],
    &["join", "ghost-channel"],
    &["participants", "ghost-channel"],
    &["send", "ghost-channel", "hello"],
    &["read", "ghost-channel"],
    &["wait", "ghost-channel", "--timeout", "1"],
    &["fetch", "ghost-channel", "0123456789abcdef0123456789abcdef"],
    &["leave", "ghost-channel"],
    &["close", "ghost-channel"],
    &["delete", "ghost-channel"],
    &["skill"],
];

fn complaint(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn every_client_verb_accepts_the_identity_flags() {
    let server = TestServer::start();

    for verb in CLIENT_VERBS {
        let mut args = verb.to_vec();
        args.extend_from_slice(&["--as", "reviewer", "--harness", "claude"]);
        let output = server.run(&args);

        // The verb may still fail — a channel named ghost-channel is not
        // there — but it must fail on its own terms and not on the flags.
        assert_ne!(
            output.status.code(),
            Some(USAGE_ERROR),
            "{args:?} was a usage error: {}",
            complaint(&output)
        );
        assert!(
            !complaint(&output).contains("unexpected argument"),
            "{args:?}: {}",
            complaint(&output)
        );
    }
}

/// Global means either side of the verb, which is what makes
/// `SANEHA_AS=<name> saneha …` and `saneha --as <name> …` interchangeable in a
/// skill's instructions.
#[test]
fn the_identity_flags_are_taken_before_the_verb_as_well_as_after() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create the channel");

    let before = server.run(&["--as", "reviewer", "join", "brisk-otter"]);
    assert_eq!(
        support::stdout_of("join with --as before the verb", &before),
        format!(
            "reviewer@{}",
            saneha::identity::host(saneha::store::HOST.max)
        )
    );

    // And the environment variable says the same thing to a verb that never
    // had a flag of its own.
    let listed = server.run_in(
        std::path::Path::new("."),
        &["participants", "brisk-otter"],
        &[("SANEHA_AS", "reviewer")],
    );
    assert!(
        support::stdout_of("participants", &listed).contains("reviewer"),
        "{}",
        String::from_utf8_lossy(&listed.stdout)
    );
}

/// The control: a flag that really is unknown still exits 2, so the assertions
/// above are worth something.
#[test]
fn an_unknown_flag_is_still_a_usage_error() {
    let server = TestServer::start();
    let output = server.run(&["participants", "brisk-otter", "--nonesuch"]);
    assert_eq!(output.status.code(), Some(USAGE_ERROR));
    assert!(
        complaint(&output).contains("unexpected argument"),
        "{}",
        complaint(&output)
    );
}
