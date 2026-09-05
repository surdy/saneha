//! Attachments: what an upload stores, what a message carries, what a fetch
//! writes, and what is refused on the way.

use std::path::{Path, PathBuf};

use saneha::api::{
    attachment_filename, Attachment, JoinRequest, Message, NewMessage, FILENAME_HEADER,
    MAX_ATTACHMENT,
};
use saneha::client::{JoinAnswer, Remote};
use saneha::store::UNBOUND_ATTACHMENT_TTL;

mod support;

use support::{stdout_of, TestServer};

/// This machine as the binary under test sees it, which is the host half of
/// every identity a subcommand claims here.
fn host() -> String {
    saneha::identity::host(saneha::store::HOST.max)
}

fn channel(remote: &Remote, name: &str) {
    remote.create_channel(Some(name), None).expect("create");
}

/// Joins `name` on this host and returns the identity granted.
fn join(remote: &Remote, channel: &str, name: &str) -> String {
    let request = JoinRequest {
        name: name.to_string(),
        host: host(),
        harness: "claude".to_string(),
        session_id: None,
        pid: None,
        pid_started_at: None,
        cwd: Some("/repos/saneha".to_string()),
        madari_pane: None,
        same_host_session_live: false,
        held_session_id: None,
    };
    match remote.join(channel, &request).expect("join") {
        JoinAnswer::Granted(joined) => joined.identity,
        JoinAnswer::Stale(message) => panic!("{message}"),
    }
}

/// A file of `contents`, in a directory that lives as long as the test.
fn file(directory: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, contents).expect("write the file to attach");
    path
}

/// Sends a message carrying `attachments`, whatever the ids are.
fn send_with(
    remote: &Remote,
    channel: &str,
    from: &str,
    body: &str,
    attachments: &[String],
) -> anyhow::Result<Message> {
    remote.send_message(
        channel,
        &NewMessage {
            from: from.to_string(),
            body: body.to_string(),
            to: Vec::new(),
            attachments: attachments.to_vec(),
        },
    )
}

/// The file the server stored an attachment in.
fn stored(server: &TestServer, channel_id: i64, attachment: &Attachment) -> PathBuf {
    server
        .attachments_path()
        .join(channel_id.to_string())
        .join(&attachment.id)
}

/// Makes a file, or a directory, look `seconds` older than it is, which is how
/// a test says "this was left here a long time ago".
fn backdate(path: &Path, seconds: i64) {
    let when = std::time::SystemTime::now()
        - std::time::Duration::from_secs(u64::try_from(seconds).expect("a positive age"));
    std::fs::File::open(path)
        .expect("open it to backdate it")
        .set_modified(when)
        .expect("backdate it");
}

/// The row id of a channel, which is what names its directory of files.
fn channel_id(server: &TestServer, name: &str) -> i64 {
    server
        .database()
        .query_row("SELECT id FROM channels WHERE name = ?1", [name], |row| {
            row.get(0)
        })
        .expect("the channel is there")
}

#[test]
fn an_upload_is_stored_beside_the_database_and_bound_by_the_send() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let path = file(
        workspace.path(),
        "handoff.md",
        b"## what changed\n\nthe tests pass\n",
    );
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");

    assert_eq!(attachment.filename, "handoff.md");
    assert_eq!(attachment.size, 32);
    assert_eq!(attachment.content_type, "text/markdown");
    assert_eq!(attachment.id.len(), 32);
    assert!(
        attachment.id.chars().all(|c| c.is_ascii_hexdigit()),
        "{}",
        attachment.id
    );

    // The bytes are on the disk beside the database, under the channel.
    let file = stored(&server, channel_id(&server, "brisk-otter"), &attachment);
    assert_eq!(
        std::fs::read(&file).expect("the stored file"),
        b"## what changed\n\nthe tests pass\n"
    );

    // Nothing carries it until a send says so.
    let unbound: Option<i64> = server
        .database()
        .query_row(
            "SELECT message_id FROM attachments WHERE id = ?1",
            [&attachment.id],
            |row| row.get(0),
        )
        .expect("the attachment row");
    assert_eq!(unbound, None);

    let message = send_with(
        &remote,
        "brisk-otter",
        &alice,
        "the handoff",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");
    assert_eq!(message.attachments, vec![attachment.clone()]);

    // And a read of the transcript says so too.
    let carried = remote
        .messages_after("brisk-otter", 0)
        .expect("read")
        .into_iter()
        .find(|message| message.id == 2)
        .expect("the message");
    assert_eq!(carried.attachments, vec![attachment]);
}

#[test]
fn the_binary_attaches_a_file_and_shows_it_under_the_message() {
    let server = TestServer::start();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    stdout_of("new", &server.run(&["new", "brisk-otter"]));
    stdout_of(
        "join",
        &server.run(&["join", "brisk-otter", "--as", "alice"]),
    );

    let path = file(workspace.path(), "handoff.md", &b"x".repeat(2048));
    let sent = stdout_of(
        "send",
        &server.run(&[
            "send",
            "brisk-otter",
            "the handoff is attached",
            "--file",
            path.to_str().expect("a path"),
            "--as",
            "alice",
            "--json",
        ]),
    );
    let sent: Message = serde_json::from_str(&sent).expect("the message as JSON");
    let attachment = sent.attachments.first().expect("one attachment");
    assert_eq!(attachment.filename, "handoff.md");
    assert_eq!(attachment.size, 2048);

    // `read` names it under the message, id first, because the id is what a
    // fetch is given.
    let read = stdout_of(
        "read",
        &server.run(&["read", "brisk-otter", "--as", "alice"]),
    );
    assert!(
        read.contains(&format!(
            "attachment  {}  handoff.md  2.0 KiB",
            attachment.id
        )),
        "{read}"
    );

    // And `--json` carries the whole of it.
    let json = stdout_of(
        "read --json",
        &server.run(&["read", "brisk-otter", "--all", "--as", "alice", "--json"]),
    );
    let messages: Vec<Message> = serde_json::from_str(&json).expect("messages as JSON");
    let carried = messages
        .iter()
        .find(|message| !message.attachments.is_empty())
        .expect("a message carrying something");
    assert_eq!(&carried.attachments[0], attachment);
    assert!(json.contains("\"size\": 2048"), "{json}");
}

#[test]
fn a_fetch_writes_the_attachment_where_it_is_told() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let contents = b"\x00\x01\x02 not text at all \xff".to_vec();
    let path = file(workspace.path(), "notes.bin", &contents);
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");

    let out = workspace.path().join("elsewhere.bin");
    let said = stdout_of(
        "fetch",
        &server.run(&[
            "fetch",
            "brisk-otter",
            &attachment.id,
            "--out",
            out.to_str().expect("a path"),
        ]),
    );
    assert!(said.contains("elsewhere.bin"), "{said}");
    assert_eq!(std::fs::read(&out).expect("the fetched file"), contents);
}

#[test]
fn a_fetch_with_no_out_writes_the_name_it_was_attached_under() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let path = file(workspace.path(), "handoff.md", b"the handoff\n");
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");

    let output = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    stdout_of("fetch", &output);
    assert_eq!(
        std::fs::read(landing.path().join("handoff.md")).expect("the fetched file"),
        b"the handoff\n"
    );

    // A second fetch would write over what the first one left, so it refuses.
    let again = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    assert!(!again.status.success());
    let said = String::from_utf8_lossy(&again.stderr);
    assert!(said.contains("already there"), "{said}");
    assert!(said.contains("--force"), "{said}");

    // Unless it is told to.
    std::fs::write(landing.path().join("handoff.md"), b"mine\n").expect("write over it");
    let forced = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id, "--force"],
        &[],
    );
    stdout_of("fetch --force", &forced);
    assert_eq!(
        std::fs::read(landing.path().join("handoff.md")).expect("the fetched file"),
        b"the handoff\n"
    );
}

#[test]
fn a_fetch_of_an_id_that_names_nothing_says_so() {
    let server = TestServer::start();
    let remote = server.remote();
    channel(&remote, "brisk-otter");

    for id in [
        // The right shape, and nothing here.
        "0123456789abcdef0123456789abcdef",
        // Not a shape this server ever mints.
        "not-an-id",
    ] {
        let output = server.run(&["fetch", "brisk-otter", id]);
        assert!(!output.status.success(), "{id} was fetched");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains("no attachment"), "{said}");
    }

    // An id becomes a path segment on the server, so one that would climb out
    // of the channel's directory is refused before anything is opened rather
    // than after.
    let (status, body) = server.raw_with(
        "GET",
        "/channels/brisk-otter/attachments/..%2f..%2f..%2fetc%2fpasswd",
        &[],
        b"",
    );
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("no attachment"), "{body}");
}

#[test]
fn a_file_over_the_cap_is_refused_by_both_sides() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    // The subcommand refuses it from the file's own size, before anything is
    // uploaded at all.
    let path = file(
        workspace.path(),
        "far-too-much.bin",
        &vec![b'x'; (MAX_ATTACHMENT + 1) as usize],
    );
    let output = server.run(&[
        "send",
        "brisk-otter",
        "too much",
        "--file",
        path.to_str().expect("a path"),
        "--as",
        "alice",
    ]);
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("at most 26214400 bytes"), "{said}");
    // Nothing was uploaded, so nothing is waiting to be swept.
    assert_eq!(attachment_rows(&server), 0);

    // And the server refuses one that says up front how large it is.
    let (status, body) = server.raw_with(
        "POST",
        "/channels/brisk-otter/attachments",
        &[
            (FILENAME_HEADER, "far-too-much.bin"),
            ("content-type", "application/octet-stream"),
            ("content-length", &(MAX_ATTACHMENT + 1).to_string()),
        ],
        b"only the beginning of it",
    );
    assert_eq!(status, 413, "{body}");
    assert!(body.contains("an attachment is at most"), "{body}");
    assert_eq!(attachment_rows(&server), 0);
}

#[test]
fn an_upload_that_frames_its_own_body_is_cut_off_at_the_cap() {
    let server = TestServer::start();
    channel(&server.remote(), "brisk-otter");

    // A chunked upload says nothing up front about how large it is, so the
    // only thing that stops it is the count the server keeps as it writes.
    const CHUNK: usize = 4 * 1024 * 1024;
    let mut body = Vec::new();
    for _ in 0..8 {
        body.extend_from_slice(format!("{CHUNK:x}\r\n").as_bytes());
        body.extend_from_slice(&vec![b'x'; CHUNK]);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");

    let (status, said) = server.raw_with(
        "POST",
        "/channels/brisk-otter/attachments",
        &[
            (FILENAME_HEADER, "far-too-much.bin"),
            ("transfer-encoding", "chunked"),
        ],
        &body,
    );
    assert_eq!(status, 413, "{said}");
    assert!(said.contains("an attachment is at most"), "{said}");
    // The part that did arrive was thrown away with the request.
    assert_eq!(attachment_rows(&server), 0);
    assert_eq!(stored_files(&server), 0);
}

#[test]
fn an_empty_file_is_not_worth_attaching() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    join(&remote, "brisk-otter", "alice");

    let path = file(workspace.path(), "nothing.md", b"");
    let output = server.run(&[
        "send",
        "brisk-otter",
        "here is nothing",
        "--file",
        path.to_str().expect("a path"),
        "--as",
        "alice",
    ]);
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("is empty"), "{said}");

    // The server says the same of an empty body, whatever a caller thinks.
    let (status, body) = server.raw_with(
        "POST",
        "/channels/brisk-otter/attachments",
        &[(FILENAME_HEADER, "nothing.md")],
        b"",
    );
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("is empty"), "{body}");
    assert_eq!(attachment_rows(&server), 0);
    assert_eq!(stored_files(&server), 0);
}

#[test]
fn an_attachment_id_that_names_nothing_or_something_else_fails_the_send() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    channel(&remote, "other-channel");
    let alice = join(&remote, "brisk-otter", "alice");

    // An id nothing was ever uploaded under.
    let failed = send_with(
        &remote,
        "brisk-otter",
        &alice,
        "carrying nothing",
        &["0123456789abcdef0123456789abcdef".to_string()],
    )
    .expect_err("an unknown attachment fails the send");
    assert!(failed.to_string().contains("no attachment"), "{failed}");
    // The message went with it.
    assert_eq!(
        remote.messages_after("brisk-otter", 0).expect("read").len(),
        1
    );

    let path = file(workspace.path(), "handoff.md", b"the handoff\n");
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");

    // An id uploaded to another channel is nothing to this one.
    let elsewhere = remote
        .upload_attachment("other-channel", &path)
        .expect("upload");
    let failed = send_with(
        &remote,
        "brisk-otter",
        &alice,
        "not from here",
        std::slice::from_ref(&elsewhere.id),
    )
    .expect_err("an attachment of another channel fails the send");
    assert!(failed.to_string().contains("no attachment"), "{failed}");

    // Bound once, and then not again: an attachment belongs to its message.
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");
    let failed = send_with(
        &remote,
        "brisk-otter",
        &alice,
        "and again",
        std::slice::from_ref(&attachment.id),
    )
    .expect_err("a bound attachment fails the send");
    assert!(
        failed.to_string().contains("already carried by message #2"),
        "{failed}"
    );
}

#[test]
fn the_sweep_removes_what_nothing_ever_carried_and_leaves_the_rest() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let path = file(workspace.path(), "handoff.md", b"the handoff\n");

    let carried = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    let abandoned = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    let fresh = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&carried.id),
    )
    .expect("send");

    // One upload is made to look like an upload from before the last hour.
    server
        .database()
        .execute(
            "UPDATE attachments
                SET created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-2 hours')
              WHERE id = ?1",
            [&abandoned.id],
        )
        .expect("backdate the abandoned upload");

    let store = server.store();
    let swept = store
        .sweep_unbound_attachments(UNBOUND_ATTACHMENT_TTL)
        .expect("sweep");
    assert_eq!(swept.unbound, 1);
    assert_eq!(swept.orphans, 0);

    let id = channel_id(&server, "brisk-otter");
    assert!(stored(&server, id, &carried).exists(), "the bound one went");
    assert!(stored(&server, id, &fresh).exists(), "the fresh one went");
    assert!(
        !stored(&server, id, &abandoned).exists(),
        "the abandoned one stayed"
    );
    assert_eq!(attachment_rows(&server), 2);

    // A fetch of what went says what a fetch of anything else unknown says.
    let output = server.run(&["fetch", "brisk-otter", &abandoned.id]);
    assert!(!output.status.success());
}

#[test]
fn removing_a_channels_files_takes_its_attachments_with_them() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    channel(&remote, "other-channel");
    let alice = join(&remote, "brisk-otter", "alice");
    let path = file(workspace.path(), "handoff.md", b"the handoff\n");

    let carried = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    let elsewhere = remote
        .upload_attachment("other-channel", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&carried.id),
    )
    .expect("send");

    // The order the delete verb (issue #6) has to use: read the id, delete the
    // row, then remove the files by that id. The rows go by cascade; the
    // files go by nothing at all until this is called.
    let store = server.store();
    let brisk = store.channel_row_id("brisk-otter").expect("the row id");
    let other = store.channel_row_id("other-channel").expect("the row id");
    server
        .database()
        .execute("DELETE FROM channels WHERE name = ?1", ["brisk-otter"])
        .expect("delete the channel");
    store.remove_channel_files(brisk).expect("remove the files");

    assert!(!stored(&server, brisk, &carried).exists());
    assert!(
        stored(&server, other, &elsewhere).exists(),
        "another channel's files went too"
    );

    // A channel nothing was attached to has nothing to remove, and says so by
    // not failing.
    store.remove_channel_files(other).expect("remove the files");
    store
        .remove_channel_files(other)
        .expect("removing them again is the same answer");
}

#[test]
fn a_file_no_row_names_is_swept_up_once_it_is_old_enough() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");
    let path = file(workspace.path(), "handoff.md", b"the handoff\n");

    let carried = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&carried.id),
    )
    .expect("send");

    // What a kill in the middle of an upload leaves: bytes on the disk under
    // an id, and no row anywhere that names them. Nothing row-driven can ever
    // find this, which is why the sweep walks the directories.
    let id = channel_id(&server, "brisk-otter");
    let directory = server.attachments_path().join(id.to_string());
    let killed = directory.join("aaaaaaaabbbbbbbbccccccccdddddddd");
    std::fs::write(&killed, b"half an upload").expect("write the orphan");
    backdate(&killed, UNBOUND_ATTACHMENT_TTL + 60);

    // And what an upload in flight looks like: the same thing, only new.
    let in_flight = directory.join("eeeeeeeeffffffff0000000011111111");
    std::fs::write(&in_flight, b"still arriving").expect("write the fresh one");

    // Something a person left here, which is not an attachment and is not the
    // sweep's business. It must not stop the sweep doing its own.
    let theirs = directory.join("looked-at-this");
    std::fs::create_dir(&theirs).expect("make the directory");
    backdate(&theirs, UNBOUND_ATTACHMENT_TTL + 60);

    let swept = server
        .store()
        .sweep_unbound_attachments(UNBOUND_ATTACHMENT_TTL)
        .expect("sweep");
    assert_eq!(swept.orphans, 1);
    assert_eq!(swept.unbound, 0);

    assert!(!killed.exists(), "the orphan stayed");
    assert!(in_flight.exists(), "an upload in flight was swept up");
    assert!(theirs.exists(), "something that is not an attachment went");
    assert!(
        stored(&server, id, &carried).exists(),
        "a file a message carries went"
    );
}

#[test]
fn a_directory_a_deleted_channel_left_behind_is_swept_up_too() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    channel(&remote, "brisk-otter");
    let path = file(workspace.path(), "handoff.md", b"the handoff\n");
    let orphaned = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");

    // A delete that removed the row and then failed to remove the files: the
    // rows cascade away, the directory stays.
    let id = channel_id(&server, "brisk-otter");
    server
        .database()
        .execute("DELETE FROM channels WHERE name = ?1", ["brisk-otter"])
        .expect("delete the channel");
    let directory = server.attachments_path().join(id.to_string());
    let left = directory.join(&orphaned.id);
    assert!(left.exists());
    backdate(&left, UNBOUND_ATTACHMENT_TTL + 60);
    backdate(&directory, UNBOUND_ATTACHMENT_TTL + 60);

    let swept = server
        .store()
        .sweep_unbound_attachments(UNBOUND_ATTACHMENT_TTL)
        .expect("sweep");
    assert_eq!(swept.orphans, 1);
    assert!(!left.exists(), "the file stayed");
    assert!(!directory.exists(), "the empty directory stayed");
}

#[test]
fn a_filename_is_stored_as_a_basename_whatever_it_arrives_as() {
    let server = TestServer::start();
    let remote = server.remote();
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let (status, body) = server.raw_with(
        "POST",
        "/channels/brisk-otter/attachments",
        &[(FILENAME_HEADER, "../../../etc/passwd")],
        b"not the real one\n",
    );
    assert_eq!(status, 201, "{body}");
    let attachment: Attachment = serde_json::from_str(&body).expect("the attachment as JSON");
    assert_eq!(attachment.filename, "passwd");
    assert_eq!(
        attachment_filename(&attachment.filename).as_deref(),
        Some("passwd")
    );

    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "nothing to see",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");

    // Which is the name the fetch writes, in the directory it was run from and
    // nowhere above it.
    let output = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    stdout_of("fetch", &output);
    assert_eq!(
        std::fs::read(landing.path().join("passwd")).expect("the fetched file"),
        b"not the real one\n"
    );

    // A name nothing is left of is refused rather than guessed at.
    let (status, body) = server.raw_with(
        "POST",
        "/channels/brisk-otter/attachments",
        &[(FILENAME_HEADER, "..")],
        b"nor this one\n",
    );
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("needs a filename"), "{body}");
}

#[test]
fn a_filename_in_any_script_survives_the_round_trip() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    stdout_of(
        "join",
        &server.run(&["join", "brisk-otter", "--as", "alice"]),
    );

    // A header value carries bytes, and most of what reads one reads ASCII, so
    // a name in any other script has to be encoded on the way there and
    // decoded on the way back — twice, because it comes back in the
    // Content-Disposition of the download as well.
    for name in ["résumé.md", "設計メモ.md", "100%-done.md", "naïve payé.txt"] {
        let path = file(workspace.path(), name, name.as_bytes());
        let attachment = remote
            .upload_attachment("brisk-otter", &path)
            .expect("upload");
        assert_eq!(attachment.filename, name);

        let sent = stdout_of(
            "send",
            &server.run(&[
                "send",
                "brisk-otter",
                "in another script",
                "--file",
                path.to_str().expect("a path"),
                "--as",
                "alice",
                "--json",
            ]),
        );
        let sent: Message = serde_json::from_str(&sent).expect("the message as JSON");
        let carried = sent.attachments.first().expect("one attachment");
        assert_eq!(carried.filename, name);

        let output = server.run_in(landing.path(), &["fetch", "brisk-otter", &carried.id], &[]);
        stdout_of("fetch", &output);
        assert_eq!(
            std::fs::read(landing.path().join(name)).expect("the fetched file"),
            name.as_bytes(),
            "{name} did not come back as itself"
        );
    }
}

#[test]
fn an_attachment_of_exactly_the_cap_goes_and_comes_back() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    // The cap is what an attachment may be, not what it may not: a file of
    // exactly 25 MiB is one the server stores, so it is one a fetch has to be
    // able to read back whole.
    let path = file(
        workspace.path(),
        "the-whole-cap.bin",
        &vec![b'x'; MAX_ATTACHMENT as usize],
    );
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    assert_eq!(attachment.size, MAX_ATTACHMENT);
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "all of it",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");

    let output = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    let said = stdout_of("fetch", &output);
    assert!(said.contains("25.0 MiB"), "{said}");
    assert_eq!(
        std::fs::metadata(landing.path().join("the-whole-cap.bin"))
            .expect("the fetched file")
            .len(),
        MAX_ATTACHMENT
    );
}

#[test]
fn an_attachment_whose_file_is_gone_says_so_without_naming_a_path() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let path = file(workspace.path(), "handoff.md", b"the handoff\n");
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");

    // What a restore that took the database without the files leaves behind.
    let id = channel_id(&server, "brisk-otter");
    std::fs::remove_file(stored(&server, id, &attachment)).expect("remove the file");

    let output = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("recorded but its file is not"), "{said}");
    // The path on the server is nothing the person reading this can act on,
    // and nothing they should be told.
    assert!(!said.contains("attachments/"), "{said}");
    // And nothing was left in the directory the fetch was run from.
    assert_eq!(std::fs::read_dir(landing.path()).expect("read").count(), 0);
}

#[test]
fn a_fetch_that_fails_leaves_the_name_free_for_the_next_one() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let path = file(workspace.path(), "handoff.md", b"the handoff\n");
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");

    // A fetch into an empty directory that cannot finish: nothing is written
    // where the file was going, so nothing has to be cleaned up by hand and
    // the next attempt is not refused as "already there".
    let stored_file = stored(&server, channel_id(&server, "brisk-otter"), &attachment);
    let bytes = std::fs::read(&stored_file).expect("the stored file");
    std::fs::remove_file(&stored_file).expect("remove the file");

    let failed = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    assert!(!failed.status.success());
    assert_eq!(
        std::fs::read_dir(landing.path()).expect("read").count(),
        0,
        "a failed fetch left something behind"
    );

    // And when it can finish, it does, into the same name.
    std::fs::write(&stored_file, &bytes).expect("put the file back");
    let output = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    stdout_of("fetch", &output);
    assert_eq!(
        std::fs::read(landing.path().join("handoff.md")).expect("the fetched file"),
        b"the handoff\n"
    );
    assert_eq!(std::fs::read_dir(landing.path()).expect("read").count(), 1);
}

#[test]
fn a_filename_cannot_write_a_second_filename_into_the_download() {
    let server = TestServer::start();
    let remote = server.remote();
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    // A name written to look like the rest of a Content-Disposition. It is a
    // filename, semicolons and all, and the download must come back under it
    // rather than under the one written inside it.
    let name = "a; filename*=UTF-8''elsewhere.sh";
    let (status, body) = server.raw_with(
        "POST",
        "/channels/brisk-otter/attachments",
        &[(FILENAME_HEADER, "a;%20filename*=UTF-8''elsewhere.sh")],
        b"nothing to run\n",
    );
    assert_eq!(status, 201, "{body}");
    let attachment: Attachment = serde_json::from_str(&body).expect("the attachment as JSON");
    assert_eq!(attachment.filename, name);

    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "a name that is a sentence",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");

    let output = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id],
        &[],
    );
    stdout_of("fetch", &output);
    assert!(
        !landing.path().join("elsewhere.sh").exists(),
        "the name inside the name was used"
    );
    assert_eq!(
        std::fs::read(landing.path().join(name)).expect("the fetched file"),
        b"nothing to run\n"
    );
}

#[test]
fn a_fetch_that_fails_leaves_what_was_there_alone() {
    let server = TestServer::start();
    let remote = server.remote();
    let workspace = tempfile::tempdir().expect("a directory to attach from");
    let landing = tempfile::tempdir().expect("a directory to fetch into");
    channel(&remote, "brisk-otter");
    let alice = join(&remote, "brisk-otter", "alice");

    let path = file(workspace.path(), "handoff.md", b"the handoff\n");
    let attachment = remote
        .upload_attachment("brisk-otter", &path)
        .expect("upload");
    send_with(
        &remote,
        "brisk-otter",
        &alice,
        "here it is",
        std::slice::from_ref(&attachment.id),
    )
    .expect("send");
    let id = channel_id(&server, "brisk-otter");
    std::fs::remove_file(stored(&server, id, &attachment)).expect("remove the file");

    // A copy already worked on, and a fetch that is going to fail. `--force`
    // must not destroy the one before the other has finished, so the bytes
    // land beside it and are renamed on only once they are all there.
    let mine = landing.path().join("handoff.md");
    std::fs::write(&mine, b"mine, and worked on\n").expect("write the copy");
    let output = server.run_in(
        landing.path(),
        &["fetch", "brisk-otter", &attachment.id, "--force"],
        &[],
    );
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read(&mine).expect("the copy"),
        b"mine, and worked on\n"
    );
    // Not even the part-written file is left behind.
    assert_eq!(
        std::fs::read_dir(landing.path()).expect("read").count(),
        1,
        "the fetch left something behind"
    );
}

/// How many attachments the server has recorded, over every channel.
fn attachment_rows(server: &TestServer) -> i64 {
    server
        .database()
        .query_row("SELECT COUNT(*) FROM attachments", [], |row| row.get(0))
        .expect("count the attachments")
}

/// How many attachment files are on the disk, over every channel.
fn stored_files(server: &TestServer) -> usize {
    let root = server.attachments_path();
    let Ok(channels) = std::fs::read_dir(&root) else {
        return 0;
    };
    channels
        .filter_map(|channel| channel.ok())
        .filter_map(|channel| std::fs::read_dir(channel.path()).ok())
        .map(|files| files.filter_map(|file| file.ok()).count())
        .sum()
}
