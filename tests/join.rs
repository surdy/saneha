//! Joining a channel: the identity that is worked out, the identity that is
//! granted, and what the transcript says about it.

use std::path::Path;
use std::process::Command;

use saneha::api::JoinRequest;
use saneha::client::JoinAnswer;

mod support;

use support::{stdout_of, TestServer};

/// This machine as the binary under test sees it. Every granted identity ends
/// in it, so the tests name it once.
fn host() -> String {
    saneha::identity::host(saneha::store::HOST.max)
}

/// A directory that is not a git repository, named `name`.
fn plain_directory(name: &str) -> tempfile::TempDir {
    let parent = tempfile::tempdir().expect("temporary directory");
    std::fs::create_dir(parent.path().join(name)).expect("create the directory");
    parent
}

/// A directory as the process running in it reports it: macOS hands out
/// temporary directories behind a symlink, and a working directory is recorded
/// as the process sees it.
fn real(directory: &Path) -> String {
    directory
        .canonicalize()
        .expect("canonicalize")
        .to_string_lossy()
        .into_owned()
}

/// `saneha join` as JSON, so a test can read every field of the answer.
fn join_json(
    server: &TestServer,
    directory: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> serde_json::Value {
    let mut all = vec!["join"];
    all.extend_from_slice(args);
    all.push("--json");
    let output = server.run_in(directory, &all, env);
    let stdout = stdout_of(&format!("saneha join {args:?}"), &output);
    serde_json::from_str(&stdout).expect("--json prints JSON")
}

/// What the join flow sends when it has already worked out who it is.
fn request(name: &str, harness: &str) -> JoinRequest {
    JoinRequest {
        name: name.to_string(),
        host: host(),
        harness: harness.to_string(),
        session_id: None,
        pid: None,
        pid_started_at: None,
        cwd: Some("/repos/saneha".to_string()),
        madari_pane: None,
        same_host_session_live: false,
        held_session_id: None,
    }
}

/// A process this test owns and can end when it likes, standing in for a
/// harness session that is still running.
struct Session {
    child: std::process::Child,
}

impl Session {
    /// Starts one, and returns it with the pid a harness would publish.
    fn start() -> Session {
        let child = Command::new("sleep")
            .arg("120")
            .spawn()
            .expect("start a process to stand in for a harness session");
        Session { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Ends it and reaps it, so the pid really is gone.
    fn end(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.end();
    }
}

/// A join made the way a harness makes one: through a shell that is gone as
/// soon as the command returns, with the harness publishing its own pid.
fn join_as_harness(
    server: &TestServer,
    directory: &Path,
    args: &str,
    session: &str,
    pid: u32,
) -> serde_json::Value {
    let output = server
        .shell(&format!("\"$SANEHA\" join {args} --json"))
        .current_dir(directory)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("CLAUDE_PID", pid.to_string())
        .output()
        .expect("run the saneha binary through a shell");
    let stdout = stdout_of(&format!("saneha join {args} as {session}"), &output);
    serde_json::from_str(&stdout).expect("--json prints JSON")
}

#[test]
fn joining_an_unknown_channel_says_so() {
    let server = TestServer::start();

    let err = server
        .remote()
        .join("nobody-here", &request("agent", "claude"))
        .expect_err("an unknown channel must be refused");
    let message = format!("{err:#}");
    assert_eq!(
        message,
        "there is no channel named \"nobody-here\"; create it with: saneha new nobody-here"
    );

    let output = server.run(&["join", "nobody-here"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("saneha: there is no channel named \"nobody-here\""),
        "{stderr}"
    );
}

#[test]
fn a_closed_channel_refuses_a_join() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    // Nothing closes a channel yet; `saneha close` will, and a join has to
    // already refuse one.
    server
        .database()
        .execute(
            "UPDATE channels SET state = 'closed' WHERE name = 'brisk-otter'",
            [],
        )
        .expect("close the channel");

    let err = server
        .remote()
        .join("brisk-otter", &request("agent", "claude"))
        .expect_err("a closed channel must be refused");
    assert_eq!(
        format!("{err:#}"),
        "the channel \"brisk-otter\" is closed, so nobody can join it"
    );
}

#[test]
fn an_identity_is_the_directory_the_harness_and_the_host() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    let joined = join_json(
        &server,
        &directory,
        &["brisk-otter"],
        &[("CLAUDECODE", "1")],
    );
    assert_eq!(
        joined["identity"],
        serde_json::json!(format!("notes-method-claude@{}", host()))
    );
    assert_eq!(joined["resumed"], serde_json::json!(false));
    assert_eq!(joined["suffixed"], serde_json::json!(false));
    assert_eq!(
        joined["participant"]["harness"],
        serde_json::json!("claude")
    );
    assert_eq!(joined["participant"]["host"], serde_json::json!(host()));
    assert_eq!(joined["participant"]["read_cursor"], serde_json::json!(0));
    assert_eq!(joined["participant"]["away"], serde_json::json!(false));
    assert_eq!(
        joined["participant"]["cwd"],
        serde_json::json!(real(&directory))
    );
    // Claude Code was not asked for its own pid here, so the fallback stands:
    // the process that started the command, which is this test.
    assert_eq!(
        joined["participant"]["pid"],
        serde_json::json!(std::process::id())
    );

    // With no harness marker the identity still forms, and the CLI says how to
    // do better.
    let output = server.run_in(&directory, &["join", "brisk-otter"], &[]);
    let identity = stdout_of("saneha join", &output);
    assert_eq!(identity, format!("notes-method-unknown@{}", host()));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no harness was recognised"), "{stderr}");
    assert!(stderr.contains("--harness"), "{stderr}");
    assert!(stderr.contains("SANEHA_AS"), "{stderr}");
}

#[test]
fn an_identity_inside_a_repository_uses_the_repository_name() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    let parent = plain_directory("my-repo");
    let repository = parent.path().join("my-repo");
    let initialised = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repository)
        .status()
        .expect("run git");
    assert!(initialised.success(), "git init failed");
    let nested = repository.join("src").join("deep");
    std::fs::create_dir_all(&nested).expect("create a nested directory");

    // Run from deep inside the repository: the name comes from the top of the
    // repository, not from the directory the command happened to be in.
    let output = server.run_in(&nested, &["join", "brisk-otter"], &[("CLAUDECODE", "1")]);
    assert_eq!(
        stdout_of("saneha join", &output),
        format!("my-repo-claude@{}", host())
    );
}

#[test]
fn every_worktree_of_a_repository_derives_the_repository_name() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    let parent = plain_directory("my-repo");
    let repository = parent.path().join("my-repo");
    let git = |args: &[&str], directory: &Path| {
        let status = Command::new("git")
            .args(args)
            .current_dir(directory)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--quiet"], &repository);
    git(
        &[
            "-c",
            "user.name=saneha",
            "-c",
            "user.email=saneha@example.com",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "first",
        ],
        &repository,
    );
    let checkout = parent.path().join("my-repo-review");
    git(
        &[
            "worktree",
            "add",
            "--quiet",
            checkout.to_str().expect("path"),
            "-b",
            "review",
        ],
        &repository,
    );

    // The name says which project is talking. Where that project is checked
    // out is a placement detail, and two live checkouts on one host are told
    // apart by the suffix, not by the directory they sit in.
    let output = server.run_in(&checkout, &["join", "brisk-otter"], &[("CLAUDECODE", "1")]);
    assert_eq!(
        stdout_of("saneha join from a second checkout", &output),
        format!("my-repo-claude@{}", host())
    );
}

#[test]
fn a_name_can_be_given_outright() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    let with_flag = server.run_in(
        &directory,
        &["join", "brisk-otter", "--as", "reviewer"],
        &[("CLAUDECODE", "1")],
    );
    assert_eq!(
        stdout_of("saneha join --as", &with_flag),
        format!("reviewer@{}", host())
    );

    let with_env = server.run_in(
        &directory,
        &["join", "brisk-otter"],
        &[("SANEHA_AS", "planner")],
    );
    assert_eq!(
        stdout_of("saneha join with SANEHA_AS", &with_env),
        format!("planner@{}", host())
    );

    // --as wins over the environment.
    let both = server.run_in(
        &directory,
        &["join", "brisk-otter", "--as", "reviewer"],
        &[("SANEHA_AS", "planner")],
    );
    assert_eq!(
        stdout_of("saneha join --as with SANEHA_AS", &both),
        format!("reviewer@{}", host())
    );
}

#[test]
fn the_harness_can_be_overridden() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    // The marker says claude; --harness says otherwise and is believed.
    let joined = join_json(
        &server,
        &directory,
        &["brisk-otter", "--harness", "codex"],
        &[
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "session-one"),
        ],
    );
    assert_eq!(
        joined["identity"],
        serde_json::json!(format!("notes-method-codex@{}", host()))
    );
    assert_eq!(joined["participant"]["harness"], serde_json::json!("codex"));
    // A harness this build does not know publishes no session id, so none is
    // recorded even though Claude Code's is in the environment.
    assert_eq!(joined["participant"]["session_id"], serde_json::Value::Null);

    // A harness recognised by name reads its session id even when it was named
    // rather than detected.
    let joined = join_json(
        &server,
        &directory,
        &["brisk-otter", "--as", "reviewer", "--harness", "claude"],
        &[("CLAUDE_CODE_SESSION_ID", "session-one")],
    );
    assert_eq!(
        joined["participant"]["session_id"],
        serde_json::json!("session-one")
    );

    let rejected = server.run_in(
        &directory,
        &["join", "brisk-otter", "--harness", "Claude Code"],
        &[],
    );
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains("harness ids use lowercase letters, digits and hyphens"),
        "{stderr}"
    );
}

#[test]
fn rejoining_resumes_the_participant_and_keeps_its_read_cursor() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    let first = join_json(
        &server,
        &directory,
        &["brisk-otter", "--as", "reviewer", "--harness", "codex"],
        &[],
    );
    assert_eq!(first["resumed"], serde_json::json!(false));
    assert_eq!(first["participant"]["session_id"], serde_json::Value::Null);

    // Reading is a later ticket, so the cursor is moved, and the participant
    // is marked away, the only way there is today.
    let identity = format!("reviewer@{}", host());
    let moved = server
        .database()
        .execute(
            "UPDATE participants SET read_cursor = 7, away = 1 WHERE identity = ?1",
            [&identity],
        )
        .expect("move the read cursor");
    assert_eq!(moved, 1);

    let second = join_json(
        &server,
        &directory,
        &["brisk-otter", "--as", "reviewer", "--harness", "claude"],
        &[("CLAUDE_CODE_SESSION_ID", "session-two")],
    );
    assert_eq!(second["identity"], serde_json::json!(identity));
    assert_eq!(second["resumed"], serde_json::json!(true));
    assert_eq!(second["suffixed"], serde_json::json!(false));
    // The cursor and the name are what a resume is for.
    assert_eq!(second["participant"]["read_cursor"], serde_json::json!(7));
    assert_eq!(second["participant"]["name"], serde_json::json!("reviewer"));
    // Everything about the session that is joining is refreshed.
    assert_eq!(
        second["participant"]["session_id"],
        serde_json::json!("session-two")
    );
    assert_eq!(
        second["participant"]["harness"],
        serde_json::json!("claude")
    );
    assert_eq!(second["participant"]["away"], serde_json::json!(false));
    let joined_at = |value: &serde_json::Value| {
        value["participant"]["joined_at"]
            .as_str()
            .expect("joined_at")
            .to_string()
    };
    assert!(joined_at(&second) >= joined_at(&first), "{second:?}");

    // A resume is not a second participant.
    assert_eq!(
        server
            .remote()
            .list_participants("brisk-otter")
            .expect("list")
            .len(),
        1
    );
}

#[test]
fn a_live_session_on_this_host_gets_a_suffixed_identity() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    // Three harness sessions, each with a process of its own that this test
    // starts and ends. Every join goes through a shell that exits immediately,
    // so nothing here depends on the CLI's parent still being around.
    let one = Session::start();
    let two = Session::start();
    let three = Session::start();

    let first = join_as_harness(
        &server,
        &directory,
        "brisk-otter --as reviewer",
        "session-one",
        one.pid(),
    );
    assert_eq!(
        first["identity"],
        serde_json::json!(format!("reviewer@{}", host()))
    );
    assert_eq!(first["suffixed"], serde_json::json!(false));
    // The harness's own process is recorded, not the shell that ran the
    // command and is already gone.
    assert_eq!(first["participant"]["pid"], serde_json::json!(one.pid()));
    assert!(
        first["participant"]["pid_started_at"].is_string(),
        "{first:?}"
    );

    // Another session, while the first is still running: a name of its own.
    let second = join_as_harness(
        &server,
        &directory,
        "brisk-otter --as reviewer",
        "session-two",
        two.pid(),
    );
    assert_eq!(
        second["identity"],
        serde_json::json!(format!("reviewer-2@{}", host()))
    );
    assert_eq!(second["suffixed"], serde_json::json!(true));
    assert_eq!(second["resumed"], serde_json::json!(false));
    assert_eq!(
        second["participant"]["name"],
        serde_json::json!("reviewer-2")
    );
    assert_eq!(second["participant"]["pid"], serde_json::json!(two.pid()));

    // A third walks past the name already taken. The suffix is explained on
    // standard error, so the identity is still the only thing on standard
    // output.
    let third = server
        .shell("\"$SANEHA\" join brisk-otter --as reviewer")
        .current_dir(&directory)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_SESSION_ID", "session-three")
        .env("CLAUDE_PID", three.pid().to_string())
        .output()
        .expect("run the saneha binary through a shell");
    assert_eq!(
        stdout_of("saneha join", &third),
        format!("reviewer-3@{}", host())
    );
    let stderr = String::from_utf8_lossy(&third.stderr);
    assert!(
        stderr.contains(&format!("reviewer@{}", host())) && stderr.contains("still running"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("reviewer-3@{}", host())),
        "{stderr}"
    );

    let participants = server
        .remote()
        .list_participants("brisk-otter")
        .expect("list");
    assert_eq!(participants.len(), 3);

    // The session holding the name comes back: its own session id, so this is
    // a resume however alive it is.
    let again = join_as_harness(
        &server,
        &directory,
        "brisk-otter --as reviewer",
        "session-one",
        one.pid(),
    );
    assert_eq!(
        again["identity"],
        serde_json::json!(format!("reviewer@{}", host()))
    );
    assert_eq!(again["resumed"], serde_json::json!(true));
}

#[test]
fn a_finished_session_is_resumed_rather_than_suffixed() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    let mut one = Session::start();
    let two = Session::start();

    let first = join_as_harness(
        &server,
        &directory,
        "brisk-otter --as reviewer",
        "session-one",
        one.pid(),
    );
    assert_eq!(first["resumed"], serde_json::json!(false));

    // The session ends, the way a harness that has been closed does.
    one.end();

    let second = join_as_harness(
        &server,
        &directory,
        "brisk-otter --as reviewer",
        "session-two",
        two.pid(),
    );
    assert_eq!(
        second["identity"],
        serde_json::json!(format!("reviewer@{}", host())),
        "a finished session must hand its identity on"
    );
    assert_eq!(second["resumed"], serde_json::json!(true));
    assert_eq!(second["suffixed"], serde_json::json!(false));
    assert_eq!(second["participant"]["pid"], serde_json::json!(two.pid()));
}

#[test]
fn a_reused_pid_does_not_make_a_finished_session_look_live() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    let one = Session::start();
    let two = Session::start();

    join_as_harness(
        &server,
        &directory,
        "brisk-otter --as reviewer",
        "session-one",
        one.pid(),
    );

    // What pid reuse looks like from the outside: the number is alive, but the
    // process wearing it is not the one that joined.
    let changed = server
        .database()
        .execute(
            "UPDATE participants SET pid_started_at = 'Thu Jan  1 00:00:00 2015'
              WHERE identity = ?1",
            [&format!("reviewer@{}", host())],
        )
        .expect("rewrite the recorded start time");
    assert_eq!(changed, 1);

    let second = join_as_harness(
        &server,
        &directory,
        "brisk-otter --as reviewer",
        "session-two",
        two.pid(),
    );
    assert_eq!(
        second["identity"],
        serde_json::json!(format!("reviewer@{}", host())),
        "a pid that is alive but is not the recorded process must not suffix"
    );
    assert_eq!(second["resumed"], serde_json::json!(true));
}

#[test]
fn a_join_made_against_a_participant_that_moved_on_is_refused() {
    let server = TestServer::start();
    let remote = server.remote();
    remote
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    // Someone joins, from wherever.
    let mut first = request("reviewer", "claude");
    first.session_id = Some("session-one".to_string());
    match remote.join("brisk-otter", &first).expect("join") {
        JoinAnswer::Granted(joined) => assert!(!joined.resumed),
        JoinAnswer::Stale(message) => panic!("a first join cannot be stale: {message}"),
    }

    // A join whose probe found nobody, arriving after that one: it would
    // otherwise resume a participant it has never seen and knows nothing
    // about.
    let mut racing = request("reviewer", "claude");
    racing.session_id = Some("session-two".to_string());
    match remote.join("brisk-otter", &racing).expect("join") {
        JoinAnswer::Granted(_) => panic!("a stale join must be refused"),
        JoinAnswer::Stale(message) => {
            assert!(message.contains("changed hands"), "{message}");
            assert!(message.contains("reviewer@"), "{message}");
        }
    }

    // Looking again and asking again is what the client does next, and it
    // works.
    racing.held_session_id = Some("session-one".to_string());
    match remote.join("brisk-otter", &racing).expect("join") {
        JoinAnswer::Granted(joined) => {
            assert!(joined.resumed);
            assert_eq!(
                joined.participant.session_id.as_deref(),
                Some("session-two")
            );
        }
        JoinAnswer::Stale(message) => panic!("a fresh view must be accepted: {message}"),
    }

    // Only one participant came out of all that.
    assert_eq!(
        remote.list_participants("brisk-otter").expect("list").len(),
        1
    );
}

#[test]
fn a_join_is_written_into_the_transcript() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    for args in [
        vec!["join", "brisk-otter", "--as", "reviewer"],
        vec!["join", "brisk-otter", "--as", "planner"],
        // A resume is still a join, and still shows in the transcript.
        vec!["join", "brisk-otter", "--as", "reviewer"],
    ] {
        let output = server.run_in(&directory, &args, &[]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // There is no read verb yet, so the transcript is read from the table.
    let database = server.database();
    let mut statement = database
        .prepare(
            "SELECT m.id, m.kind, m.body, m.from_participant, p.identity
               FROM messages m
               JOIN channels c ON c.id = m.channel_id
               JOIN participants p ON p.id = m.about_participant
              WHERE c.name = 'brisk-otter'
              ORDER BY m.id",
        )
        .expect("prepare");
    let rows: Vec<(i64, String, String, Option<i64>, String)> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");

    let host = host();
    assert_eq!(rows.len(), 3, "{rows:?}");
    for (index, (id, kind, body, from, identity)) in rows.iter().enumerate() {
        // Ids are per-channel and monotonic from 1.
        assert_eq!(*id, index as i64 + 1, "{rows:?}");
        assert_eq!(kind, "join", "{rows:?}");
        // A system message is written by the server about a participant, so it
        // is not from anyone.
        assert_eq!(*from, None, "{rows:?}");
        assert_eq!(body, &format!("{identity} joined"), "{rows:?}");
    }
    assert_eq!(rows[0].4, format!("reviewer@{host}"));
    assert_eq!(rows[1].4, format!("planner@{host}"));
    assert_eq!(rows[2].4, format!("reviewer@{host}"));

    // The channel's own allocator is where those ids came from.
    let last: i64 = database
        .query_row(
            "SELECT last_message_id FROM channels WHERE name = 'brisk-otter'",
            [],
            |row| row.get(0),
        )
        .expect("last_message_id");
    assert_eq!(last, 3);
}

/// The viewer is a browser: it has no working directory to record, so it joins
/// without the field at all, and nothing invents a placeholder for it. What
/// the CLI sends from a real directory is `participants_are_listed`.
#[test]
fn a_join_without_a_working_directory_records_none() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");

    // The field left out of the body altogether, which is what the page sends
    // and what an older client that never learnt about it would send too.
    let (status, body) = server.raw(
        "POST",
        "/channels/brisk-otter/participants",
        br#"{"name":"surdy","host":"web","harness":"web"}"#,
    );
    assert_eq!(status, 201, "{body}");
    let joined: serde_json::Value = serde_json::from_str(&body).expect("the join answers JSON");
    assert_eq!(joined["identity"], serde_json::json!("surdy@web"));
    assert_eq!(
        joined["participant"]["cwd"],
        serde_json::Value::Null,
        "a working directory was invented for a caller that has none"
    );

    // And one that does have a directory still records it, over the same
    // route, in the same channel.
    let with_one = server
        .remote()
        .join("brisk-otter", &request("agent", "claude"))
        .expect("join with a working directory");
    let JoinAnswer::Granted(with_one) = with_one else {
        panic!("the join was refused as stale");
    };
    assert_eq!(with_one.participant.cwd.as_deref(), Some("/repos/saneha"));

    // On the way out: null for the one, the path for the other.
    let listing = server.run(&["participants", "brisk-otter", "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout_of("saneha participants --json", &listing))
            .expect("--json prints JSON");
    let participants = parsed["participants"].as_array().expect("participants");
    assert_eq!(participants.len(), 2);
    assert_eq!(participants[0]["cwd"], serde_json::Value::Null);
    assert_eq!(
        participants[1]["cwd"],
        serde_json::json!("/repos/saneha"),
        "{parsed}"
    );

    // The table names a participant, where it is and what it runs in; a
    // directory is not one of its columns, so a missing one shows as nothing
    // at all rather than as a hole in a row.
    let table = stdout_of(
        "saneha participants",
        &server.run(&["participants", "brisk-otter"]),
    );
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 3, "{table}");
    assert!(lines[1].starts_with("surdy@web"), "{table}");
    assert!(
        lines[1].contains("web") && lines[1].contains("here"),
        "{table}"
    );
    assert!(!table.contains("/repos/saneha"), "{table}");
}

#[test]
fn participants_are_listed() {
    let server = TestServer::start();
    server
        .remote()
        .create_channel(Some("brisk-otter"), None)
        .expect("create");
    let parent = plain_directory("notes-method");
    let directory = parent.path().join("notes-method");

    let empty = server.run(&["participants", "brisk-otter"]);
    assert_eq!(
        stdout_of("saneha participants", &empty),
        "Nobody has joined brisk-otter yet. Join it with: saneha join brisk-otter"
    );

    server.run_in(
        &directory,
        &["join", "brisk-otter", "--as", "reviewer"],
        &[("CLAUDECODE", "1")],
    );
    server.run_in(
        &directory,
        &[
            "join",
            "brisk-otter",
            "--as",
            "planner",
            "--harness",
            "codex",
        ],
        &[],
    );

    let listed = server.run(&["participants", "brisk-otter"]);
    let table = stdout_of("saneha participants", &listed);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 3, "{table}");
    assert!(lines[0].starts_with("IDENTITY"), "{table}");
    assert!(
        lines[0].contains("HARNESS") && lines[0].contains("HOST"),
        "{table}"
    );
    assert!(
        lines[0].contains("AWAY") && lines[0].contains("JOINED"),
        "{table}"
    );
    // Listed in the order they joined.
    assert!(
        lines[1].starts_with(&format!("reviewer@{}", host())),
        "{table}"
    );
    assert!(
        lines[1].contains("claude") && lines[1].contains("here"),
        "{table}"
    );
    assert!(
        lines[2].starts_with(&format!("planner@{}", host())),
        "{table}"
    );
    assert!(lines[2].contains("codex"), "{table}");

    let json = server.run(&["participants", "brisk-otter", "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout_of("saneha participants --json", &json))
            .expect("--json prints JSON");
    let participants = parsed["participants"].as_array().expect("participants");
    assert_eq!(participants.len(), 2);
    assert_eq!(
        participants[0]["identity"],
        serde_json::json!(format!("reviewer@{}", host()))
    );
    assert_eq!(participants[0]["cwd"], serde_json::json!(real(&directory)));

    let unknown = server.run(&["participants", "nobody-here"]);
    assert!(!unknown.status.success());
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("saneha: there is no channel named \"nobody-here\""),
        "{stderr}"
    );
}
