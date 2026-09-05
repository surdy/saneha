//! The SQLite file the server owns, and the attachment files beside it.
//!
//! One file holds everything that is a row: channels, participants and their
//! read cursors, the transcript, and what each attachment is. The bytes of an
//! attachment are the one thing SQLite does not hold; they live in
//! `attachments/<channel id>/<attachment id>` next to the database, so the
//! volume that carries the database carries them too. The schema is applied by
//! the versioned migration list below, run at every start, so an older file is
//! brought forward without any separate step.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::RngExt;
use rusqlite::{Connection, OptionalExtension};

use crate::api::{
    Attachment, Channel, ChannelCounts, ChannelDetail, ChannelState, Closed, Deleted, JoinRequest,
    Joined, Left, Message, MessageKind, Participant, MAX_BODY, MAX_MESSAGE_LIMIT,
};
use crate::mention::{self, Candidate, Unresolved};
use crate::slug;

/// A kind of name, and how it is described when one is refused. Channel names,
/// the two halves of an identity and a harness id are all the same alphabet
/// with different caps, so they are all one check and one error.
pub struct Slug {
    /// The plural, as the error names it: "channel names use ...".
    what: &'static str,
    /// The longest one of these may be.
    pub max: usize,
    /// What the person reading the error can do about it, if anything.
    hint: &'static str,
}

/// A channel name: typed by people and pasted between hosts, so it is
/// deliberately narrow.
pub const CHANNEL_NAME: Slug = Slug {
    what: "channel names",
    max: 64,
    hint: "",
};

/// The name half of an identity, held to a channel name's shape for the same
/// reason.
pub const PARTICIPANT_NAME: Slug = Slug {
    what: "participant names",
    max: CHANNEL_NAME.max,
    hint: "; name yourself with --as",
};

/// The host half of an identity. The client folds a real hostname into this
/// shape before sending it; the server still checks, because an identity is
/// split on `@` by everything that reads one.
pub const HOST: Slug = Slug {
    what: "hosts",
    max: 64,
    hint: "",
};

/// A harness id, which appears in a derived name, so it has a name's shape.
pub const HARNESS: Slug = Slug {
    what: "harness ids",
    max: 32,
    hint: "",
};

/// The longest a purpose may be. A purpose is one line saying what a channel is
/// for, not a document.
pub const MAX_PURPOSE: usize = 256;

/// The longest a working directory, harness session id, process start time or
/// Madari pane id the server will record.
pub const MAX_RECORDED: usize = 1024;

/// How much of a rejected value is echoed back in the error.
const MAX_ECHO: usize = 80;

/// How far the suffix walk goes before a caller is told to name itself.
const MAX_SUFFIX: u32 = 99;

/// How many slugs the server tries before giving up on minting a name.
const MINT_ATTEMPTS: usize = 24;

/// SQLite renders timestamps itself, so there is one clock and one format.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

/// Applied in order; the number applied is kept in SQLite's `user_version`.
/// Never edit an entry that has shipped, only append.
///
/// Migration 1 creates the channels table alone; it is deployed, so it is
/// frozen. Migration 2 adds participants and the transcript, migration 3 the
/// attachments, and migration 4 makes a participant's working directory
/// nullable. The forward-looking columns in migration 1 are the ones the
/// channels table itself needs: `closed_at` for `saneha close`, and
/// `last_message_id` as the per-channel allocator that makes message ids
/// monotonic within a transcript. `AUTOINCREMENT` keeps a deleted channel's id
/// from being handed to a later channel, so anything keyed by channel id stays
/// unambiguous.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE channels (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT    NOT NULL,
    purpose         TEXT,
    state           TEXT    NOT NULL DEFAULT 'open'
                            CHECK (state IN ('open', 'closed')),
    created_at      TEXT    NOT NULL,
    closed_at       TEXT,
    last_message_id INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX channels_name ON channels (name);
"#,
    // A participant is one identity in one channel: the same person or agent
    // in two channels is two rows, because a read cursor belongs to a channel
    // (ADR-0004). `identity` is `name@host` and is stored whole as well as in
    // its two halves, so a lookup by identity is an index hit and the halves
    // never have to be parsed back out.
    //
    // A message carries whichever of the two participant columns its kind
    // means. A `message` is written BY a participant, so it fills
    // `from_participant`. A `join` or `leave` is written by the server ABOUT a
    // participant, so it fills `about_participant`; making the server
    // masquerade as the participant it is describing would make "who wrote
    // this" a lie. A `close` is about the channel and fills neither. The CHECK
    // holds that discipline in the schema rather than in prose.
    //
    // `pid` alone would not do: pids are reused, so a finished session could be
    // impersonated by whatever the kernel handed the number to next.
    // `pid_started_at` is that process's start time as the host reports it, and
    // a session counts as live only when both still match.
    //
    // `id` is per-channel and monotonic, allocated from
    // `channels.last_message_id` in the same transaction as the insert, so a
    // transcript reads 1, 2, 3 and a read cursor is a number in that sequence.
    r#"
CREATE TABLE participants (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id     INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    identity       TEXT    NOT NULL,
    name           TEXT    NOT NULL,
    host           TEXT    NOT NULL,
    harness        TEXT    NOT NULL,
    session_id     TEXT,
    pid            INTEGER,
    pid_started_at TEXT,
    cwd            TEXT    NOT NULL,
    madari_pane    TEXT,
    away           INTEGER NOT NULL DEFAULT 0 CHECK (away IN (0, 1)),
    read_cursor    INTEGER NOT NULL DEFAULT 0,
    joined_at      TEXT    NOT NULL,
    left_at        TEXT
);

CREATE UNIQUE INDEX participants_identity ON participants (channel_id, identity);

CREATE TABLE messages (
    channel_id        INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    id                INTEGER NOT NULL,
    kind              TEXT    NOT NULL
                              CHECK (kind IN ('message', 'join', 'leave', 'close')),
    from_participant  INTEGER REFERENCES participants (id) ON DELETE SET NULL,
    about_participant INTEGER REFERENCES participants (id) ON DELETE SET NULL,
    body              TEXT    NOT NULL,
    created_at        TEXT    NOT NULL,
    PRIMARY KEY (channel_id, id),
    CHECK (
        (kind = 'message' AND from_participant IS NOT NULL AND about_participant IS NULL)
        OR (kind IN ('join', 'leave') AND from_participant IS NULL AND about_participant IS NOT NULL)
        OR (kind = 'close' AND from_participant IS NULL AND about_participant IS NULL)
    )
);

CREATE TABLE recipients (
    channel_id     INTEGER NOT NULL,
    message_id     INTEGER NOT NULL,
    participant_id INTEGER NOT NULL REFERENCES participants (id) ON DELETE CASCADE,
    PRIMARY KEY (channel_id, message_id, participant_id),
    FOREIGN KEY (channel_id, message_id)
        REFERENCES messages (channel_id, id) ON DELETE CASCADE
);

-- The primary key answers "who is this message addressed to"; this answers
-- "which messages are addressed to me", which is what `wait --mentions` and
-- the relay ask.
CREATE INDEX recipients_participant ON recipients (participant_id, channel_id, message_id);
"#,
    // An attachment is uploaded before the message that carries it exists, so
    // it arrives unbound: `message_id` is NULL until the send that names its id
    // binds it. That is also why there is a sweep — an upload whose send never
    // came would otherwise sit there for good.
    //
    // The id is 32 hex characters minted by the server rather than the row id,
    // because it is a capability: it is the whole of what a fetch has to
    // present, so it must not be guessable by counting.
    //
    // The bytes are not in here. They are in
    // `attachments/<channel_id>/<id>` beside the database file, so the
    // foreign key deletes the row when a channel or a message goes and
    // `remove_channel_files` is what deletes the file.
    //
    // The composite foreign key needs `messages (channel_id, id)` to be
    // unique, which it is: that pair is its primary key. A NULL `message_id`
    // satisfies the constraint, which is what leaves an unbound attachment
    // free to point at nothing.
    r#"
CREATE TABLE attachments (
    id           TEXT    PRIMARY KEY,
    channel_id   INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    message_id   INTEGER,
    filename     TEXT    NOT NULL,
    size         INTEGER NOT NULL,
    content_type TEXT    NOT NULL,
    created_at   TEXT    NOT NULL,
    FOREIGN KEY (channel_id, message_id)
        REFERENCES messages (channel_id, id) ON DELETE CASCADE
);

-- "What does this message carry", which is what every read of a transcript
-- asks, once per page.
CREATE INDEX attachments_message ON attachments (channel_id, message_id);

-- "What is still waiting to be bound", which is what the sweep asks.
CREATE INDEX attachments_unbound ON attachments (created_at) WHERE message_id IS NULL;
"#,
    // A working directory is what a join happened to be run in, and a person
    // joining from the viewer is in a browser and was run in nothing: the
    // column was NOT NULL, so the page had to invent a directory to put there.
    // It is nullable from here on, and a missing one is null rather than a
    // word standing in for a path.
    //
    // SQLite cannot relax NOT NULL in place, and the table it holds is pointed
    // at by two foreign keys, so the usual rebuild — copy into a new table,
    // drop this one — is the one thing that must not happen here: a migration
    // runs inside a transaction, `PRAGMA foreign_keys` cannot be turned off
    // inside one, and the drop would take the recipient rows with it and null
    // the author of every message. Adding a column, copying into it, dropping
    // the old one and taking its name touches no other table, keeps the unique
    // index on (channel_id, identity), and leaves every row where it is; the
    // only visible change is that `cwd` is now the last column.
    r#"
ALTER TABLE participants ADD COLUMN cwd_optional TEXT;
UPDATE participants SET cwd_optional = cwd;
ALTER TABLE participants DROP COLUMN cwd;
ALTER TABLE participants RENAME COLUMN cwd_optional TO cwd;
"#,
];

/// How long an attachment nobody bound to a message is kept before the sweep
/// removes it, in seconds. Long enough that a send delayed by a slow upload
/// still finds its files, short enough that an abandoned upload is not stored
/// for ever.
pub const UNBOUND_ATTACHMENT_TTL: i64 = 60 * 60;

/// The directory attachment files live in, under the directory holding the
/// database file.
const ATTACHMENTS_DIR: &str = "attachments";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "{what} use lowercase letters, digits and hyphens, start with a letter or digit, \
         and are at most {max} characters (got {value:?}){hint}"
    )]
    InvalidSlug {
        what: &'static str,
        max: usize,
        value: String,
        hint: &'static str,
    },

    #[error(
        "a purpose is one line of at most {max} characters, with no line breaks (got {purpose:?})",
        max = MAX_PURPOSE
    )]
    InvalidPurpose { purpose: String },

    #[error("a channel named {0:?} already exists")]
    ChannelExists(String),

    #[error("could not mint a channel name after {0} attempts; name one yourself")]
    MintExhausted(usize),

    #[error("there is no channel named {0:?}; create it with: saneha new {0}")]
    NoSuchChannel(String),

    /// One condition, two refusals: a closed channel takes no joins and no
    /// messages, and each verb says which it is being refused.
    #[error("the channel {channel:?} is closed, so {refusal}")]
    ChannelClosed {
        channel: String,
        refusal: &'static str,
    },

    #[error("{what} must be one line of at most {max} characters (got {value:?})", max = MAX_RECORDED)]
    InvalidRecorded { what: &'static str, value: String },

    #[error("nobody named {identity:?} has joined the channel {channel:?}")]
    NoSuchParticipant { channel: String, identity: String },

    /// A close carries the identity closing it, and that identity goes into
    /// the transcript, so it has to be one.
    #[error("an identity is name@host, and {0:?} is not one")]
    InvalidIdentity(String),

    /// The caller is acting as somebody who is not in the channel. Every verb
    /// that acts as a participant says this in the same words, so an agent
    /// reading it always sees the same next step.
    #[error("{}", crate::api::not_a_participant(channel, identity))]
    NotAParticipant { channel: String, identity: String },

    #[error("a message needs a body")]
    EmptyBody,

    #[error("{}", crate::api::body_too_large(*size))]
    BodyTooLarge { size: usize },

    #[error("nobody in {channel:?} is called {written}; its participants are: {participants}")]
    NoSuchRecipient {
        channel: String,
        written: String,
        participants: String,
    },

    #[error("{written} is ambiguous in {channel:?}: {candidates}; name the one you mean in full")]
    AmbiguousRecipient {
        channel: String,
        written: String,
        candidates: String,
    },

    #[error("the database holds a message kind this build does not understand: {0:?}")]
    UnknownMessageKind(String),

    #[error(
        "there is no attachment {id:?} in the channel {channel:?}; \
         the ids are the ones under the messages in: saneha read {channel} --all"
    )]
    NoSuchAttachment { channel: String, id: String },

    /// The row is there and the bytes are not. Said without naming any path on
    /// the server, because the person reading it is on another machine and the
    /// path is nothing they can act on.
    #[error(
        "the attachment {id:?} in {channel:?} is recorded but its file is not on the server; \
         it was lost rather than removed, so ask whoever attached it to send it again"
    )]
    AttachmentGone { channel: String, id: String },

    /// An attachment belongs to the one message that carries it, so a second
    /// send naming the same id is refused rather than quietly moving it.
    #[error("the attachment {id:?} is already carried by message #{message_id} in {channel:?}")]
    AttachmentBound {
        channel: String,
        id: String,
        message_id: i64,
    },

    #[error("{}", crate::api::attachment_too_large(*size))]
    AttachmentTooLarge { size: u64 },

    #[error("{}", crate::api::attachment_is_empty(filename))]
    EmptyAttachment { filename: String },

    #[error(
        "an attachment needs a filename in the {header} header, and \
         {value:?} is not one",
        header = crate::api::FILENAME_HEADER
    )]
    InvalidFilename { value: String },

    #[error("could not {doing} the attachment file {path}")]
    AttachmentFile {
        doing: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A database with no file has no directory to keep attachments beside,
    /// which is only ever a store opened in memory by a test.
    #[error("this saneha server keeps its database in memory, so it stores no attachments")]
    NoAttachmentStore,

    #[error(
        "{identity:?} changed hands in {channel:?} while this join was being made; \
         look again and join again"
    )]
    ParticipantChanged { channel: String, identity: String },

    #[error(
        "every name from {name}-2 to {name}-{max} is taken in {channel:?}; name yourself with --as",
        max = MAX_SUFFIX
    )]
    SuffixExhausted { channel: String, name: String },

    #[error(
        "the database was written by a newer saneha (schema {found}, this build understands \
         {supported}); upgrade saneha or point --db at another file"
    )]
    NewerSchema { found: usize, supported: usize },

    #[error("the database holds a channel state this build does not understand: {0:?}")]
    UnknownChannelState(String),

    #[error("database error")]
    Sqlite(#[from] rusqlite::Error),

    #[error("could not open the database at {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("could not create the directory {path} for the database")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The database, and the attachment files beside it, shared by every request
/// the server handles.
pub struct Store {
    conn: Mutex<Connection>,
    /// `<the database's directory>/attachments`, and `None` for a database
    /// held in memory, which has no directory to put anything beside.
    attachments: Option<PathBuf>,
}

impl Store {
    /// Opens the database at `path`, creating the file, its parent directories
    /// and the schema if they are not there yet. Attachments go in
    /// `attachments/` beside it, made on the first upload.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let directory = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        std::fs::create_dir_all(&directory).map_err(|source| StoreError::Directory {
            path: directory.clone(),
            source,
        })?;
        let conn = Connection::open(path).map_err(|source| StoreError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_connection(conn, Some(directory.join(ATTACHMENTS_DIR)))
    }

    /// A database held in memory, for tests. It stores no attachments.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?, None)
    }

    fn from_connection(
        mut conn: Connection,
        attachments: Option<PathBuf>,
    ) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&mut conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
            attachments,
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while holding the lock would poison it; the connection is
        // still usable, so carry on rather than turning it into a second panic.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Creates a channel. `name` of `None` mints a readable two-word slug.
    pub fn create_channel(
        &self,
        name: Option<&str>,
        purpose: Option<&str>,
    ) -> Result<Channel, StoreError> {
        let purpose = purpose.map(str::trim).filter(|p| !p.is_empty());
        if let Some(purpose) = purpose {
            validate_purpose(purpose)?;
        }

        match name {
            Some(name) => {
                let name = name.trim();
                validate_channel_name(name)?;
                self.insert_channel(name, purpose)
            }
            None => {
                for _ in 0..MINT_ATTEMPTS {
                    let minted = slug::mint();
                    match self.insert_channel(&minted, purpose) {
                        Err(StoreError::ChannelExists(_)) => continue,
                        other => return other,
                    }
                }
                Err(StoreError::MintExhausted(MINT_ATTEMPTS))
            }
        }
    }

    fn insert_channel(&self, name: &str, purpose: Option<&str>) -> Result<Channel, StoreError> {
        let conn = self.conn();
        let created_at: Result<String, rusqlite::Error> = conn.query_row(
            &format!(
                "INSERT INTO channels (name, purpose, state, created_at)
                 VALUES (?1, ?2, 'open', {NOW})
                 RETURNING created_at"
            ),
            rusqlite::params![name, purpose],
            |row| row.get(0),
        );
        match created_at {
            Ok(created_at) => Ok(Channel {
                name: name.to_string(),
                purpose: purpose.map(str::to_string),
                state: ChannelState::Open,
                created_at,
                closed_at: None,
            }),
            Err(err) if is_unique_violation(&err) => {
                Err(StoreError::ChannelExists(name.to_string()))
            }
            Err(err) => Err(StoreError::Sqlite(err)),
        }
    }

    /// Every channel, oldest first.
    pub fn list_channels(&self) -> Result<Vec<Channel>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT name, purpose, state, created_at, closed_at FROM channels ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([], read_row)?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(build_channel)
            .collect()
    }

    /// One channel by name, or `None` if there is no such channel.
    pub fn channel(&self, name: &str) -> Result<Option<Channel>, StoreError> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT name, purpose, state, created_at, closed_at FROM channels WHERE name = ?1",
                [name],
                read_row,
            )
            .optional()?;
        row.map(build_channel).transpose()
    }

    /// Joins `channel` under `request`, and writes the join system message.
    ///
    /// Three outcomes, decided by whether `name@host` is already a participant
    /// and by the caller's `same_host_session_live` verdict:
    ///
    /// - not a participant: a new one, read cursor at 0, not away;
    /// - a participant, and the caller says its session is not live: that
    ///   participant resumes, keeping its name and read cursor;
    /// - a participant, and the caller says its session is live on this host:
    ///   the first free `name-2`, `name-3`, ... is granted as a new one.
    ///
    /// That verdict was formed against a participant the caller looked at
    /// before asking, so the request carries what it saw in `held_session_id`.
    /// If the row says something else by the time the join commits, someone
    /// else joined in between and the verdict is about a participant that no
    /// longer exists as described; the join is refused rather than resumed on
    /// top of a stranger.
    ///
    /// All of it, including the message id allocated from
    /// `channels.last_message_id`, happens in one transaction, so a join
    /// either lands whole or not at all.
    pub fn join(&self, channel: &str, request: &JoinRequest) -> Result<Joined, StoreError> {
        validate_participant_name(&request.name)?;
        validate_host(&request.host)?;
        validate_harness(&request.harness)?;
        for (what, value) in [
            ("a working directory", &request.cwd),
            ("a harness session id", &request.session_id),
            ("a process start time", &request.pid_started_at),
            ("a Madari pane id", &request.madari_pane),
        ] {
            if let Some(value) = value {
                validate_recorded(what, value)?;
            }
        }

        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let channel_id = open_channel_id(&tx, channel, "nobody can join it")?;

        let (participant_id, name, resumed, suffixed) = if request.same_host_session_live {
            let (id, name) = insert_suffixed(&tx, channel_id, channel, request)?;
            (id, name, false, true)
        } else {
            let identity = identity_of(&request.name, &request.host);
            match held_by(&tx, channel_id, &identity)? {
                Some((id, held_session_id)) => {
                    if held_session_id != request.held_session_id {
                        return Err(StoreError::ParticipantChanged {
                            channel: channel.to_string(),
                            identity,
                        });
                    }
                    resume_participant(&tx, id, request)?;
                    (id, request.name.clone(), true, false)
                }
                None => {
                    // Nobody held it a moment ago either, or the caller would
                    // have said so.
                    if request.held_session_id.is_some() {
                        return Err(StoreError::ParticipantChanged {
                            channel: channel.to_string(),
                            identity,
                        });
                    }
                    let id = insert_participant(&tx, channel_id, &request.name, request)?;
                    (id, request.name.clone(), false, false)
                }
            }
        };

        let identity = identity_of(&name, &request.host);
        // A resume is still a join: the transcript says who is present now,
        // not only who is present for the first time.
        write_system_message(
            &tx,
            channel_id,
            "join",
            Some(participant_id),
            &format!("{identity} joined"),
        )?;
        let participant = read_participant(&tx, participant_id)?;
        tx.commit()?;

        Ok(Joined {
            channel: channel.to_string(),
            identity,
            resumed,
            suffixed,
            participant,
        })
    }

    /// Everyone who has joined a channel, in the order they first joined.
    /// A closed channel still lists its participants.
    pub fn list_participants(&self, channel: &str) -> Result<Vec<Participant>, StoreError> {
        let conn = self.conn();
        let channel_id = channel_id(&conn, channel)?;
        let mut statement = conn.prepare(&format!(
            "SELECT {PARTICIPANT_COLUMNS} FROM participants WHERE channel_id = ?1 ORDER BY id"
        ))?;
        let rows = statement.query_map([channel_id], read_participant_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// One participant by identity, or `None` if nobody holds it here. The
    /// join flow asks this first, to see whether the identity it wants is held
    /// by a harness session still running on that host.
    pub fn participant(
        &self,
        channel: &str,
        identity: &str,
    ) -> Result<Option<Participant>, StoreError> {
        let conn = self.conn();
        let channel_id = channel_id(&conn, channel)?;
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {PARTICIPANT_COLUMNS} FROM participants
                     WHERE channel_id = ?1 AND identity = ?2"
                ),
                rusqlite::params![channel_id, identity],
                read_participant_row,
            )
            .optional()?)
    }

    /// Marks a participant away and writes the leave system message.
    ///
    /// A leave is a declaration, not a departure: the participant stays in the
    /// channel, stays in the transcript, can still be mentioned, and goes on
    /// accumulating unread messages. A later join resumes it and clears both
    /// `away` and `left_at` (see [`resume_participant`]).
    ///
    /// Asking twice is not an error. The second call finds `away` already set,
    /// writes nothing, and says so with `left: false` — a verb that failed the
    /// second time would make "leave, then leave again after a crash" into
    /// something a skill has to reason about.
    ///
    /// A closed channel is the other way of changing nothing. A leave is a
    /// message, and a closed channel accepts no more messages; leaving one is
    /// therefore accepted and does nothing, rather than refused, because there
    /// is nothing left to leave.
    pub fn leave(&self, channel: &str, identity: &str) -> Result<Left, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (channel_id, state) = channel_and_state(&tx, channel)?;
        let participant_id = participant_id(&tx, channel_id, identity)?.ok_or_else(|| {
            StoreError::NotAParticipant {
                channel: channel.to_string(),
                identity: identity.to_string(),
            }
        })?;

        let away: bool = tx.query_row(
            "SELECT away FROM participants WHERE id = ?1",
            [participant_id],
            |row| row.get(0),
        )?;
        let left = state == ChannelState::Open && !away;
        if left {
            tx.execute(
                &format!("UPDATE participants SET away = 1, left_at = {NOW} WHERE id = ?1"),
                [participant_id],
            )?;
            write_system_message(
                &tx,
                channel_id,
                "leave",
                Some(participant_id),
                &format!("{identity} left"),
            )?;
        }
        let participant = read_participant(&tx, participant_id)?;
        tx.commit()?;

        Ok(Left {
            channel: channel.to_string(),
            identity: identity.to_string(),
            left,
            channel_state: state,
            participant,
        })
    }

    /// Closes a channel and writes the close system message.
    ///
    /// `by` is an identity and not a participant. The scope says any
    /// participant or a person may close, and a person closing from the viewer
    /// is `surdy@web`, who has joined nothing; so who closed it is recorded in
    /// the body of the message rather than in `about_participant`, which the
    /// schema holds NULL for a `close` because a close is about the channel.
    ///
    /// Closing a closed channel changes nothing and is not an error, for the
    /// same reason leaving twice is not.
    pub fn close(&self, channel: &str, by: &str) -> Result<Closed, StoreError> {
        validate_identity(by)?;

        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (channel_id, state) = channel_and_state(&tx, channel)?;
        let closed = state == ChannelState::Open;
        if closed {
            tx.execute(
                &format!("UPDATE channels SET state = 'closed', closed_at = {NOW} WHERE id = ?1"),
                [channel_id],
            )?;
            write_system_message(
                &tx,
                channel_id,
                "close",
                None,
                &format!("{by} closed the channel"),
            )?;
        }
        let row = tx.query_row(
            "SELECT name, purpose, state, created_at, closed_at FROM channels WHERE id = ?1",
            [channel_id],
            read_row,
        )?;
        tx.commit()?;

        Ok(Closed {
            channel: build_channel(row)?,
            closed,
        })
    }

    /// One channel and what it holds. This is what a delete asks before it is
    /// confirmed, so that what is about to go is said in numbers.
    pub fn channel_detail(&self, channel: &str) -> Result<ChannelDetail, StoreError> {
        let conn = self.conn();
        let channel_id = channel_id(&conn, channel)?;
        let row = conn.query_row(
            "SELECT name, purpose, state, created_at, closed_at FROM channels WHERE id = ?1",
            [channel_id],
            read_row,
        )?;
        Ok(ChannelDetail {
            channel: build_channel(row)?,
            counts: channel_counts(&conn, channel_id)?,
        })
    }

    /// Deletes a channel row, and with it — by the cascade the schema
    /// declares — its participants, its transcript, its recipients and its
    /// attachment rows. Answers with the channel's row id and what went, so
    /// the caller can remove the files that row id names.
    ///
    /// The row goes first and the files after it, which is the order the
    /// attachments work settled on: a channel whose files were removed and
    /// whose row survived would be a live transcript naming attachments
    /// nothing can fetch, while a row removed and files left behind is a
    /// directory nobody can reach, which the hourly sweep takes within the
    /// hour. Open or closed makes no difference here: delete is for when you
    /// want one gone.
    pub fn remove_channel(&self, channel: &str) -> Result<(i64, Deleted), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let channel_id = channel_id(&tx, channel)?;
        let counts = channel_counts(&tx, channel_id)?;
        tx.execute("DELETE FROM channels WHERE id = ?1", [channel_id])?;
        tx.commit()?;

        Ok((
            channel_id,
            Deleted {
                channel: channel.to_string(),
                counts,
            },
        ))
    }

    /// Writes one message from a participant, with everyone it is addressed
    /// to, in one transaction.
    ///
    /// Recipients are the mentions in the body and whatever `to` adds, both
    /// resolved against the channel's own participants: a short form when
    /// exactly one participant answers to it, a full `name@host` always,
    /// `@all` for everyone including the away ones. A mention that resolves to
    /// nobody, or to more than one, fails the whole send rather than quietly
    /// addressing fewer participants than it looks like it does; because it
    /// all happens in one transaction, nothing is written when it does.
    ///
    /// The id comes from the channel's own allocator, so a transcript reads 1,
    /// 2, 3 whatever else the server is doing at the time.
    ///
    /// `attachments` are ids already uploaded to this channel, bound to the
    /// message inside the same transaction. An id that names nothing here, or
    /// one another message already carries, fails the send: the same
    /// discipline as a mention that names nobody, and for the same reason —
    /// a message that quietly carried less than it was asked to would be worse
    /// than no message at all.
    pub fn send(
        &self,
        channel: &str,
        from: &str,
        body: &str,
        to: &[String],
        attachments: &[String],
    ) -> Result<Message, StoreError> {
        validate_body(body)?;

        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let channel_id = open_channel_id(&tx, channel, "nothing more can be sent to it")?;
        let from_id =
            participant_id(&tx, channel_id, from)?.ok_or_else(|| StoreError::NotAParticipant {
                channel: channel.to_string(),
                identity: from.to_string(),
            })?;

        let roster = roster(&tx, channel_id)?;
        let mut wanted = mention::mentions(body);
        for given in to {
            let mention = mention::given(given).ok_or_else(|| StoreError::NoSuchRecipient {
                channel: channel.to_string(),
                written: format!("{given:?}"),
                participants: identities(&roster),
            })?;
            if !wanted.contains(&mention) {
                wanted.push(mention);
            }
        }
        let recipients = mention::resolve(&wanted, &roster)
            .map_err(|unresolved| unresolved_recipient(channel, unresolved, &roster))?;

        let message_id = next_message_id(&tx, channel_id)?;
        let created_at: String = tx.query_row(
            &format!(
                "INSERT INTO messages
                     (channel_id, id, kind, from_participant, about_participant, body, created_at)
                 VALUES (?1, ?2, 'message', ?3, NULL, ?4, {NOW})
                 RETURNING created_at"
            ),
            rusqlite::params![channel_id, message_id, from_id, body],
            |row| row.get(0),
        )?;
        for recipient in &recipients {
            tx.execute(
                "INSERT INTO recipients (channel_id, message_id, participant_id)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![channel_id, message_id, recipient],
            )?;
        }

        // The same file named twice is one attachment, not a refusal: the
        // second mention asks for nothing the first did not already do.
        let mut bound: Vec<Attachment> = Vec::new();
        for id in attachments {
            if bound.iter().any(|attachment| &attachment.id == id) {
                continue;
            }
            bound.push(bind_attachment(&tx, channel, channel_id, id, message_id)?);
        }

        // A sender that had read everything up to this point stays caught up.
        // Nobody has to be told what they just said: without this a `read`
        // echoes the message back to whoever wrote it, and a `wait` wakes on
        // it, so the skill loop — wait, read, reply, wait — would wake itself
        // once per reply and never idle.
        //
        // The condition is the whole of it: `read_cursor = ?2 - 1` is true
        // only when the cursor was on the message before this one, which is
        // what "nothing unread" means at this instant. A sender that was
        // already behind is left exactly where it is, so what it had not read
        // is still unread and its own message arrives in that backlog like
        // anything else.
        tx.execute(
            "UPDATE participants SET read_cursor = ?2
              WHERE id = ?1 AND read_cursor = ?2 - 1",
            rusqlite::params![from_id, message_id],
        )?;

        tx.commit()?;

        // Everything the message is made of was in hand before it was written,
        // so it is described from that rather than read back.
        Ok(Message {
            id: message_id,
            channel: channel.to_string(),
            kind: MessageKind::Message,
            from: Some(from.to_string()),
            about: None,
            recipients: recipients
                .iter()
                .filter_map(|id| roster.iter().find(|candidate| candidate.id == *id))
                .map(|candidate| candidate.identity.clone())
                .collect(),
            body: body.to_string(),
            attachments: bound,
            created_at,
        })
    }

    /// The transcript of a channel after `after`, oldest first, at most
    /// `limit` of them and never more than [`MAX_MESSAGE_LIMIT`]. System
    /// messages are in it: a join is part of the conversation.
    ///
    /// Reading here moves nothing. A read cursor is advanced by its own
    /// request, so `wait` and a history read use this and leave no trace
    /// (ADR-0004).
    pub fn messages(
        &self,
        channel: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError> {
        let conn = self.conn();
        let channel_id = channel_id(&conn, channel)?;
        read_messages(
            &conn,
            channel,
            channel_id,
            after,
            limit.min(MAX_MESSAGE_LIMIT),
        )
    }

    /// What a participant has not read yet, and the state of the channel it is
    /// in. This is the one question `wait` asks, and it asks it over and over,
    /// so it is one call rather than three.
    ///
    /// `mentions` keeps only the messages that wake this participant, by the
    /// rule in [`Message::wakes`]. The filter runs over a page at a time and
    /// moves on to the next page while a full one filters down to nothing, so
    /// a thousand messages between other people still leave the one addressed
    /// to this participant found rather than sitting behind a page boundary.
    ///
    /// Nothing here writes: waiting never moves a read cursor (ADR-0004), and
    /// this is what makes a wait that ends in a read, rather than a wait that
    /// swallows what it was waiting for.
    pub fn unread(
        &self,
        channel: &str,
        identity: &str,
        mentions: bool,
        limit: usize,
    ) -> Result<(Vec<Message>, ChannelState), StoreError> {
        let limit = limit.clamp(1, MAX_MESSAGE_LIMIT);
        let conn = self.conn();
        let (channel_id, state) = channel_and_state(&conn, channel)?;
        let cursor: i64 = conn
            .query_row(
                "SELECT read_cursor FROM participants WHERE channel_id = ?1 AND identity = ?2",
                rusqlite::params![channel_id, identity],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotAParticipant {
                channel: channel.to_string(),
                identity: identity.to_string(),
            })?;

        let mut after = cursor;
        loop {
            let page = read_messages(&conn, channel, channel_id, after, limit)?;
            let more = page.len() == limit;
            let last = page.last().map(|message| message.id);
            let kept: Vec<Message> = if mentions {
                page.into_iter()
                    .filter(|message| message.wakes(identity))
                    .collect()
            } else {
                page
            };
            match last {
                Some(last) if kept.is_empty() && more => after = last,
                _ => return Ok((kept, state)),
            }
        }
    }

    /// Moves a participant's read cursor to `read_cursor` and returns the
    /// participant as it now stands.
    ///
    /// A cursor never moves backwards: a value below the one recorded leaves
    /// it where it is, so two readers of one identity cannot make each other
    /// re-read, and a value past the end of the transcript is held at the last
    /// message rather than skipping whatever arrives next. A closed channel
    /// still takes this: a channel that accepts no more messages can still be
    /// read to the end.
    pub fn set_read_cursor(
        &self,
        channel: &str,
        identity: &str,
        read_cursor: i64,
    ) -> Result<Participant, StoreError> {
        let conn = self.conn();
        let channel_id = channel_id(&conn, channel)?;
        let changed = conn.execute(
            "UPDATE participants
                SET read_cursor = MAX(
                        read_cursor,
                        MIN(?3, (SELECT last_message_id FROM channels WHERE id = ?1))
                    )
              WHERE channel_id = ?1 AND identity = ?2",
            rusqlite::params![channel_id, identity, read_cursor],
        )?;
        if changed == 0 {
            return Err(StoreError::NotAParticipant {
                channel: channel.to_string(),
                identity: identity.to_string(),
            });
        }
        Ok(conn.query_row(
            &format!(
                "SELECT {PARTICIPANT_COLUMNS} FROM participants
                 WHERE channel_id = ?1 AND identity = ?2"
            ),
            rusqlite::params![channel_id, identity],
            read_participant_row,
        )?)
    }

    /// Mints an id for a file about to be uploaded to an open channel, and
    /// makes the directory its bytes go in.
    ///
    /// Nothing is recorded here. The row is written by
    /// [`Store::record_attachment`] once the bytes have landed, so an upload
    /// that fails part-way through leaves a file nobody can name rather than a
    /// row pointing at a file that is not all there. The sweep is what removes
    /// the one it leaves behind, along with the uploads whose send never came.
    pub fn new_attachment(&self, channel: &str) -> Result<PendingAttachment, StoreError> {
        let channel_id = {
            let conn = self.conn();
            open_channel_id(&conn, channel, "nothing more can be attached to it")?
        };
        let directory = self.channel_attachments(channel_id)?;
        std::fs::create_dir_all(&directory).map_err(|source| StoreError::AttachmentFile {
            doing: "make the directory for",
            path: directory.clone(),
            source,
        })?;
        let id = mint_attachment_id();
        Ok(PendingAttachment {
            path: directory.join(&id),
            id,
            channel_id,
        })
    }

    /// Records an attachment whose bytes have landed. It is unbound until the
    /// send that names its id binds it.
    pub fn record_attachment(
        &self,
        pending: &PendingAttachment,
        filename: &str,
        size: u64,
        content_type: &str,
    ) -> Result<Attachment, StoreError> {
        let conn = self.conn();
        let created_at: String = conn.query_row(
            &format!(
                "INSERT INTO attachments
                     (id, channel_id, message_id, filename, size, content_type, created_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5, {NOW})
                 RETURNING created_at"
            ),
            rusqlite::params![
                pending.id,
                pending.channel_id,
                filename,
                size as i64,
                content_type
            ],
            |row| row.get(0),
        )?;
        Ok(Attachment {
            id: pending.id.clone(),
            filename: filename.to_string(),
            size,
            content_type: content_type.to_string(),
            created_at,
        })
    }

    /// One attachment of a channel, and the file holding its bytes. A closed
    /// channel still hands its attachments over: a channel that takes no more
    /// messages can still be read, and so can what its messages carried.
    pub fn attachment(&self, channel: &str, id: &str) -> Result<(Attachment, PathBuf), StoreError> {
        let missing = || StoreError::NoSuchAttachment {
            channel: channel.to_string(),
            id: id.to_string(),
        };
        // An id is a path segment by the time this is done with it, so
        // anything that is not one of the shapes this server mints is not
        // looked up at all.
        if !is_attachment_id(id) {
            return Err(missing());
        }

        let conn = self.conn();
        let channel_id = channel_id(&conn, channel)?;
        let attachment = conn
            .query_row(
                &format!(
                    "SELECT {ATTACHMENT_COLUMNS} FROM attachments
                      WHERE id = ?1 AND channel_id = ?2"
                ),
                rusqlite::params![id, channel_id],
                read_attachment_row,
            )
            .optional()?
            .ok_or_else(missing)?;
        drop(conn);

        let path = self.channel_attachments(channel_id)?.join(&attachment.id);
        Ok((attachment, path))
    }

    /// Removes what no message will ever carry, and answers with what went.
    ///
    /// Two things go, and they are not the same thing:
    ///
    /// - a row nothing bound to a message, older than `max_age_seconds`: an
    ///   upload whose send failed, or never came;
    /// - a file no row names, last written longer ago than that: an upload the
    ///   server was killed in the middle of, which never got as far as its
    ///   row, and the directory a deleted channel left behind.
    ///
    /// The second is why this walks the directories as well as the table.
    /// Nothing in the database records a file that was being written when the
    /// process died, so no row-driven pass can ever find it, and it would sit
    /// on the volume for good. The age is what makes the walk safe: an upload
    /// in flight has a file and no row too, and it is minutes old rather than
    /// hours.
    ///
    /// The server runs this when it starts and every hour after that. The age
    /// is a parameter so a test can sweep what it has just made.
    pub fn sweep_unbound_attachments(&self, max_age_seconds: i64) -> Result<Swept, StoreError> {
        let older_than = format!("-{max_age_seconds} seconds");
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, channel_id FROM attachments
              WHERE message_id IS NULL
                AND created_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        )?;
        let stale = statement
            .query_map([&older_than], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        for (id, channel_id) in &stale {
            // The file goes first: a row with no file is something a fetch
            // reports, and a file with no row is something the walk below
            // finds.
            if let Some(root) = self.attachments.as_deref() {
                remove_file(&root.join(channel_id.to_string()).join(id))?;
            }
            conn.execute("DELETE FROM attachments WHERE id = ?1", [id])?;
        }
        drop(conn);

        Ok(Swept {
            unbound: stale.len(),
            orphans: self.sweep_orphan_files(max_age_seconds)?,
        })
    }

    /// The walk: every file under the attachments directory that no row names
    /// and that was last written more than `max_age_seconds` ago, and then any
    /// channel directory left empty by it.
    fn sweep_orphan_files(&self, max_age_seconds: i64) -> Result<usize, StoreError> {
        let Some(root) = self.attachments.as_deref() else {
            return Ok(0);
        };
        let cutoff = match std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(max_age_seconds.max(0) as u64))
        {
            Some(cutoff) => cutoff,
            // A machine whose clock is not far enough from the epoch for the
            // subtraction to hold is a machine whose file times mean nothing;
            // nothing is removed on the strength of them.
            None => return Ok(0),
        };

        let mut removed = 0;
        for directory in read_directory(root)? {
            let directory = directory?;
            // Read before anything is removed from it: taking a file out of a
            // directory is a write to the directory, so afterwards every
            // directory this touched looks new.
            let untouched_since = older_than(&directory, cutoff);
            let mut left = 0;
            for file in read_directory(&directory)? {
                let file = file?;
                // Only files are attachments. Anything else under here was put
                // there by a person, and a sweep that walked into it, or
                // failed on it, would be a sweep that stopped doing its job
                // over something that is not its business.
                let is_file = std::fs::metadata(&file).is_ok_and(|about| about.is_file());
                if !is_file || !older_than(&file, cutoff) || self.is_recorded(&file)? {
                    left += 1;
                    continue;
                }
                remove_file(&file)?;
                removed += 1;
            }
            // An empty directory is a channel that has been deleted, or one
            // whose uploads have all just gone. Either way the next upload
            // makes it again, and a failure here is not worth a word.
            if left == 0 && untouched_since {
                let _ = std::fs::remove_dir(&directory);
            }
        }
        Ok(removed)
    }

    /// Whether a file under the attachments directory is one the database
    /// knows about. A name that is not an id cannot be.
    fn is_recorded(&self, file: &Path) -> Result<bool, StoreError> {
        let Some(id) = file.file_name().and_then(|name| name.to_str()) else {
            return Ok(false);
        };
        if !is_attachment_id(id) {
            return Ok(false);
        }
        Ok(self
            .conn()
            .query_row("SELECT 1 FROM attachments WHERE id = ?1", [id], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .is_some())
    }

    /// Removes the attachment files of the channel with this row id.
    ///
    /// The rows go by themselves: deleting a channel cascades to its
    /// attachments. The bytes are outside SQLite, so nothing cascades to them,
    /// and the delete verb ([issue #6]) has to remove them itself, in this
    /// order: read the channel's row id, delete the channel row, then call
    /// this with the id.
    ///
    /// That order and not the other one. Files first would mean that a delete
    /// which then failed to remove the row left a channel that is still there,
    /// whose transcript names attachments nothing can fetch. This way the
    /// failure leaves a directory nobody can reach, which the sweep removes
    /// within the hour.
    ///
    /// [issue #6]: https://github.com/surdy/saneha/issues/6
    pub fn remove_channel_files(&self, channel_id: i64) -> Result<(), StoreError> {
        let directory = self.channel_attachments(channel_id)?;
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => Ok(()),
            // A channel nothing was ever attached to has no directory, which
            // is the same outcome by another route.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::AttachmentFile {
                doing: "remove",
                path: directory,
                source,
            }),
        }
    }

    /// The row id of a channel, which is what names its directory of files.
    /// The delete verb reads this before it deletes the row, because
    /// afterwards there is nothing left to read it from.
    pub fn channel_row_id(&self, channel: &str) -> Result<i64, StoreError> {
        let conn = self.conn();
        channel_id(&conn, channel)
    }

    /// Where a channel's attachment files live.
    fn channel_attachments(&self, channel_id: i64) -> Result<PathBuf, StoreError> {
        let root = self
            .attachments
            .as_deref()
            .ok_or(StoreError::NoAttachmentStore)?;
        Ok(root.join(channel_id.to_string()))
    }
}

/// What one sweep removed: rows nothing bound, and files no row named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Swept {
    /// Uploads whose send never came.
    pub unbound: usize,
    /// Files with no row: an upload the server was killed in the middle of, or
    /// what a deleted channel left behind.
    pub orphans: usize,
}

impl Swept {
    /// Whether the sweep found anything at all to do.
    pub fn is_empty(self) -> bool {
        self.unbound == 0 && self.orphans == 0
    }
}

impl std::fmt::Display for Swept {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "{} upload(s) no message carried and {} file(s) with no record",
            self.unbound, self.orphans
        )
    }
}

/// Removes one file, and says nothing about one that is already gone.
fn remove_file(path: &Path) -> Result<(), StoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreError::AttachmentFile {
            doing: "remove",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// What is in a directory, as paths, and nothing at all when there is no such
/// directory: nothing has been uploaded on this server yet.
fn read_directory(
    path: &Path,
) -> Result<Box<dyn Iterator<Item = Result<PathBuf, StoreError>>>, StoreError> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Box::new(std::iter::empty()))
        }
        Err(source) => {
            return Err(StoreError::AttachmentFile {
                doing: "read",
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let path = path.to_path_buf();
    Ok(Box::new(entries.map(move |entry| {
        entry
            .map(|entry| entry.path())
            .map_err(|source| StoreError::AttachmentFile {
                doing: "read",
                path: path.clone(),
                source,
            })
    })))
}

/// Whether this was last written before `cutoff`. Something whose age cannot
/// be read is treated as new, so the sweep never removes what it cannot date.
fn older_than(path: &Path, cutoff: std::time::SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified < cutoff)
}

/// An attachment that has been given an id and a place to be written, and is
/// not in the database yet.
#[derive(Debug, Clone)]
pub struct PendingAttachment {
    pub id: String,
    /// The file the bytes go in.
    pub path: PathBuf,
    channel_id: i64,
}

/// Everyone in a channel, as recipient resolution sees them: away participants
/// included, because a message to one accumulates as unread until it comes
/// back.
fn roster(conn: &Connection, channel_id: i64) -> Result<Vec<Candidate>, StoreError> {
    let mut statement = conn
        .prepare("SELECT id, identity, name FROM participants WHERE channel_id = ?1 ORDER BY id")?;
    let rows = statement.query_map([channel_id], |row| {
        Ok(Candidate {
            id: row.get(0)?,
            identity: row.get(1)?,
            name: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The channel's participants as an error lists them.
fn identities(roster: &[Candidate]) -> String {
    roster
        .iter()
        .map(|candidate| candidate.identity.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A mention nobody answers to, said in the server's own words. An unknown one
/// carries the participant list, because the useful next step is to look at
/// who is actually here; an ambiguous one carries the two it could be.
fn unresolved_recipient(channel: &str, unresolved: Unresolved, roster: &[Candidate]) -> StoreError {
    match unresolved {
        Unresolved::Unknown { written } => StoreError::NoSuchRecipient {
            channel: channel.to_string(),
            written,
            participants: identities(roster),
        },
        Unresolved::Ambiguous {
            written,
            candidates,
        } => StoreError::AmbiguousRecipient {
            channel: channel.to_string(),
            written,
            candidates: candidates.join(", "),
        },
    }
}

/// A message body: markdown, not empty, and small enough to be a message
/// rather than an attachment.
fn validate_body(body: &str) -> Result<(), StoreError> {
    if body.trim().is_empty() {
        return Err(StoreError::EmptyBody);
    }
    if body.len() > MAX_BODY {
        return Err(StoreError::BodyTooLarge { size: body.len() });
    }
    Ok(())
}

/// The columns of a message, in the order `read_message_row` reads. The two
/// participant columns are joined out to identities here, because an identity
/// is what a message is written by and addressed to everywhere else.
const MESSAGE_COLUMNS: &str = "m.id, m.kind, sender.identity, subject.identity, m.body, \
                               m.created_at";

/// A page of a transcript, with everyone each message is addressed to.
fn read_messages(
    conn: &Connection,
    channel: &str,
    channel_id: i64,
    after: i64,
    limit: usize,
) -> Result<Vec<Message>, StoreError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS}
           FROM messages m
           LEFT JOIN participants sender ON sender.id = m.from_participant
           LEFT JOIN participants subject ON subject.id = m.about_participant
          WHERE m.channel_id = ?1 AND m.id > ?2
          ORDER BY m.id
          LIMIT ?3"
    ))?;
    let rows = statement.query_map(
        rusqlite::params![channel_id, after, limit as i64],
        read_message_row,
    )?;
    let rows = rows.collect::<Result<Vec<_>, _>>()?;

    let Some(last) = rows.last().map(|row| row.0) else {
        return Ok(Vec::new());
    };
    let mut recipients = read_recipients(conn, channel_id, after, last)?;
    let mut attachments = read_attachments(conn, channel_id, after, last)?;

    rows.into_iter()
        .map(|(id, kind, from, about, body, created_at)| {
            Ok(Message {
                id,
                channel: channel.to_string(),
                kind: kind
                    .parse()
                    .map_err(|_| StoreError::UnknownMessageKind(kind.clone()))?,
                from,
                about,
                recipients: recipients.remove(&id).unwrap_or_default(),
                body,
                attachments: attachments.remove(&id).unwrap_or_default(),
                created_at,
            })
        })
        .collect()
}

/// The columns of an attachment, in the order `read_attachment_row` reads.
const ATTACHMENT_COLUMNS: &str = "id, filename, size, content_type, created_at";

fn read_attachment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attachment> {
    read_attachment_at(row, 0)
}

/// The same columns wherever they start, because the queries that carry a
/// message id in front of them read the rest exactly the same way.
fn read_attachment_at(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: row.get(first)?,
        filename: row.get(first + 1)?,
        // Nothing writes a negative size, and a size that will not fit reads
        // as nothing rather than as an error.
        size: row.get::<_, i64>(first + 2)?.try_into().unwrap_or(0),
        content_type: row.get(first + 3)?,
        created_at: row.get(first + 4)?,
    })
}

/// What each message in a range carries, in one query rather than one per
/// message.
///
/// The order is the order the rows were written, which is the order the files
/// were uploaded, which is the order `--file` was given in. `created_at` is
/// only good to the second, and two files attached to one message are usually
/// uploaded inside the same one, so the row order is what makes a read agree
/// with what the send answered.
fn read_attachments(
    conn: &Connection,
    channel_id: i64,
    after: i64,
    last: i64,
) -> Result<HashMap<i64, Vec<Attachment>>, StoreError> {
    let mut statement = conn.prepare(&format!(
        "SELECT message_id, {ATTACHMENT_COLUMNS}
           FROM attachments
          WHERE channel_id = ?1 AND message_id > ?2 AND message_id <= ?3
          ORDER BY message_id, rowid"
    ))?;
    let rows = statement.query_map(rusqlite::params![channel_id, after, last], |row| {
        Ok((row.get::<_, i64>(0)?, read_attachment_at(row, 1)?))
    })?;

    let mut carried: HashMap<i64, Vec<Attachment>> = HashMap::new();
    for row in rows {
        let (message_id, attachment) = row?;
        carried.entry(message_id).or_default().push(attachment);
    }
    Ok(carried)
}

/// Binds one uploaded attachment to the message that carries it. An id that
/// names nothing in this channel, or one another message already carries,
/// fails the send it is part of, and the transaction takes the message with
/// it.
fn bind_attachment(
    conn: &Connection,
    channel: &str,
    channel_id: i64,
    id: &str,
    message_id: i64,
) -> Result<Attachment, StoreError> {
    let missing = || StoreError::NoSuchAttachment {
        channel: channel.to_string(),
        id: id.to_string(),
    };
    if !is_attachment_id(id) {
        return Err(missing());
    }

    let row: Option<(Option<i64>, Attachment)> = conn
        .query_row(
            &format!(
                "SELECT message_id, {ATTACHMENT_COLUMNS} FROM attachments
                  WHERE id = ?1 AND channel_id = ?2"
            ),
            rusqlite::params![id, channel_id],
            |row| Ok((row.get(0)?, read_attachment_at(row, 1)?)),
        )
        .optional()?;
    let (bound_to, attachment) = row.ok_or_else(missing)?;
    if let Some(bound_to) = bound_to {
        return Err(StoreError::AttachmentBound {
            channel: channel.to_string(),
            id: id.to_string(),
            message_id: bound_to,
        });
    }

    conn.execute(
        "UPDATE attachments SET message_id = ?3 WHERE id = ?1 AND channel_id = ?2",
        rusqlite::params![id, channel_id, message_id],
    )?;
    Ok(attachment)
}

/// A fresh attachment id: 128 random bits as 32 lowercase hex characters.
fn mint_attachment_id() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    let mut id = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        // Writing to a String cannot fail.
        let _ = write!(id, "{byte:02x}");
    }
    id
}

/// Whether this is the shape [`mint_attachment_id`] makes. An id becomes a
/// path segment, so anything else is refused before it is looked up rather
/// than after.
fn is_attachment_id(id: &str) -> bool {
    id.len() == 32
        && id
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

/// Who each message in a range is addressed to, in one query rather than one
/// per message.
fn read_recipients(
    conn: &Connection,
    channel_id: i64,
    after: i64,
    last: i64,
) -> Result<HashMap<i64, Vec<String>>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT r.message_id, p.identity
           FROM recipients r
           JOIN participants p ON p.id = r.participant_id
          WHERE r.channel_id = ?1 AND r.message_id > ?2 AND r.message_id <= ?3
          ORDER BY r.message_id, p.id",
    )?;
    let rows = statement.query_map(rusqlite::params![channel_id, after, last], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut addressed: HashMap<i64, Vec<String>> = HashMap::new();
    for row in rows {
        let (message_id, identity) = row?;
        addressed.entry(message_id).or_default().push(identity);
    }
    Ok(addressed)
}

type MessageRow = (i64, String, Option<String>, Option<String>, String, String);

fn read_message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

/// `name@host`, the one place the two halves are put together.
pub fn identity_of(name: &str, host: &str) -> String {
    format!("{name}@{host}")
}

/// The columns of a participant, in the order `read_participant_row` reads.
const PARTICIPANT_COLUMNS: &str = "identity, name, host, harness, session_id, pid, \
                                   pid_started_at, cwd, madari_pane, away, read_cursor, \
                                   joined_at, left_at";

fn read_participant_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Participant> {
    let pid: Option<i64> = row.get(5)?;
    Ok(Participant {
        identity: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        harness: row.get(3)?,
        session_id: row.get(4)?,
        // A pid too large for the platform's own type is a pid nothing can be
        // asked about, so it reads as no pid at all rather than as an error.
        pid: pid.and_then(|pid| u32::try_from(pid).ok()),
        pid_started_at: row.get(6)?,
        cwd: row.get(7)?,
        madari_pane: row.get(8)?,
        away: row.get::<_, i64>(9)? != 0,
        read_cursor: row.get(10)?,
        joined_at: row.get(11)?,
        left_at: row.get(12)?,
    })
}

/// The row id of a channel, whatever state it is in.
fn channel_id(conn: &Connection, channel: &str) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT id FROM channels WHERE name = ?1",
        [channel],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::NoSuchChannel(channel.to_string()))
}

/// The row id of a channel and the state it is in, for the verbs that read
/// rather than write. A closed channel is not refused here: it can still be
/// read to the end, and a waiter is told it closed rather than told off.
fn channel_and_state(conn: &Connection, channel: &str) -> Result<(i64, ChannelState), StoreError> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, state FROM channels WHERE name = ?1",
            [channel],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (id, state) = row.ok_or_else(|| StoreError::NoSuchChannel(channel.to_string()))?;
    let state = state
        .parse()
        .map_err(|_| StoreError::UnknownChannelState(state))?;
    Ok((id, state))
}

/// The row id of a channel that is still open, for the verbs that add to a
/// transcript. `refusal` finishes the sentence a closed channel is refused
/// with, because joining and sending are refused for the same reason and read
/// differently.
fn open_channel_id(
    conn: &Connection,
    channel: &str,
    refusal: &'static str,
) -> Result<i64, StoreError> {
    match channel_and_state(conn, channel)? {
        (id, ChannelState::Open) => Ok(id),
        (_, ChannelState::Closed) => Err(StoreError::ChannelClosed {
            channel: channel.to_string(),
            refusal,
        }),
    }
}

fn participant_id(
    conn: &Connection,
    channel_id: i64,
    identity: &str,
) -> Result<Option<i64>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id FROM participants WHERE channel_id = ?1 AND identity = ?2",
            rusqlite::params![channel_id, identity],
            |row| row.get(0),
        )
        .optional()?)
}

/// Who holds an identity right now, and under which harness session. The
/// session id is what a caller's verdict was formed against, so it is what the
/// join checks has not moved.
fn held_by(
    conn: &Connection,
    channel_id: i64,
    identity: &str,
) -> Result<Option<(i64, Option<String>)>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id, session_id FROM participants WHERE channel_id = ?1 AND identity = ?2",
            rusqlite::params![channel_id, identity],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn insert_participant(
    conn: &Connection,
    channel_id: i64,
    name: &str,
    request: &JoinRequest,
) -> Result<i64, StoreError> {
    let identity = identity_of(name, &request.host);
    Ok(conn.query_row(
        &format!(
            "INSERT INTO participants
                 (channel_id, identity, name, host, harness, session_id, pid, pid_started_at,
                  cwd, madari_pane, away, read_cursor, joined_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 0, {NOW})
             RETURNING id"
        ),
        rusqlite::params![
            channel_id,
            identity,
            name,
            request.host,
            request.harness,
            request.session_id,
            request.pid.map(i64::from),
            request.pid_started_at,
            request.cwd,
            request.madari_pane,
        ],
        |row| row.get(0),
    )?)
}

/// Continues an existing participant. The name and the read cursor are what a
/// resume is for, so they are the two columns left alone. Everything else is
/// what the caller says about itself now, the working directory included: a
/// resume from a caller that has none records none, because the column says
/// where the last join was made from and not where some earlier one was.
fn resume_participant(conn: &Connection, id: i64, request: &JoinRequest) -> Result<(), StoreError> {
    conn.execute(
        &format!(
            "UPDATE participants
                SET harness = ?1, session_id = ?2, pid = ?3, pid_started_at = ?4, cwd = ?5,
                    madari_pane = ?6, away = 0, left_at = NULL, joined_at = {NOW}
              WHERE id = ?7"
        ),
        rusqlite::params![
            request.harness,
            request.session_id,
            request.pid.map(i64::from),
            request.pid_started_at,
            request.cwd,
            request.madari_pane,
            id,
        ],
    )?;
    Ok(())
}

/// The first free `name-2`, `name-3`, ... on this host. The base is cut short
/// enough that the suffix always fits inside the name cap.
fn insert_suffixed(
    conn: &Connection,
    channel_id: i64,
    channel: &str,
    request: &JoinRequest,
) -> Result<(i64, String), StoreError> {
    let room = PARTICIPANT_NAME.max - (MAX_SUFFIX.to_string().len() + 1);
    let base: String = request.name.chars().take(room).collect();
    let base = base.trim_end_matches('-');

    for suffix in 2..=MAX_SUFFIX {
        let name = format!("{base}-{suffix}");
        let identity = identity_of(&name, &request.host);
        if participant_id(conn, channel_id, &identity)?.is_none() {
            let id = insert_participant(conn, channel_id, &name, request)?;
            return Ok((id, name));
        }
    }
    Err(StoreError::SuffixExhausted {
        channel: channel.to_string(),
        name: base.to_string(),
    })
}

/// The next id in a channel's transcript, taken from the channel's own
/// allocator so ids are monotonic within it. Called inside the transaction
/// that writes the message it belongs to, so an id is never handed out twice
/// and never left unused.
fn next_message_id(conn: &Connection, channel_id: i64) -> Result<i64, StoreError> {
    Ok(conn.query_row(
        "UPDATE channels SET last_message_id = last_message_id + 1
          WHERE id = ?1
      RETURNING last_message_id",
        [channel_id],
        |row| row.get(0),
    )?)
}

/// What a channel holds, counted rather than listed: the numbers a delete is
/// confirmed against.
///
/// One statement rather than four, because these numbers are read together and
/// are only ever shown together. The unbound uploads are counted too: they are
/// files this channel is holding, and a delete takes them with the rest.
fn channel_counts(conn: &Connection, channel_id: i64) -> Result<ChannelCounts, StoreError> {
    let (participants, messages, attachments, bytes): (i64, i64, i64, i64) = conn.query_row(
        "SELECT
             (SELECT COUNT(*) FROM participants WHERE channel_id = ?1),
             (SELECT COUNT(*) FROM messages WHERE channel_id = ?1),
             (SELECT COUNT(*) FROM attachments WHERE channel_id = ?1),
             (SELECT COALESCE(SUM(size), 0) FROM attachments WHERE channel_id = ?1)",
        [channel_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;

    Ok(ChannelCounts {
        participants: participants.max(0) as usize,
        messages: messages.max(0) as usize,
        attachments: attachments.max(0) as usize,
        attachment_bytes: bytes.max(0) as u64,
    })
}

/// Writes one system message: the server's own entry in the transcript, about
/// a participant rather than by one.
///
/// `about_participant` is `None` for a `close`, which is about the channel and
/// not about anybody; the schema's CHECK holds that difference, so this passes
/// it through rather than deciding it.
fn write_system_message(
    conn: &Connection,
    channel_id: i64,
    kind: &str,
    about_participant: Option<i64>,
    body: &str,
) -> Result<i64, StoreError> {
    let message_id = next_message_id(conn, channel_id)?;
    conn.execute(
        &format!(
            "INSERT INTO messages
                 (channel_id, id, kind, from_participant, about_participant, body, created_at)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, {NOW})"
        ),
        rusqlite::params![channel_id, message_id, kind, about_participant, body],
    )?;
    Ok(message_id)
}

fn read_participant(conn: &Connection, id: i64) -> Result<Participant, StoreError> {
    Ok(conn.query_row(
        &format!("SELECT {PARTICIPANT_COLUMNS} FROM participants WHERE id = ?1"),
        [id],
        read_participant_row,
    )?)
}

pub fn validate_channel_name(name: &str) -> Result<(), StoreError> {
    validate_slug(name, &CHANNEL_NAME)
}

pub fn validate_participant_name(name: &str) -> Result<(), StoreError> {
    validate_slug(name, &PARTICIPANT_NAME)
}

pub fn validate_host(host: &str) -> Result<(), StoreError> {
    validate_slug(host, &HOST)
}

pub fn validate_harness(harness: &str) -> Result<(), StoreError> {
    validate_slug(harness, &HARNESS)
}

/// A whole identity, `name@host`, both halves held to the shape they have
/// everywhere else. A close records who closed it in the transcript, and what
/// goes into a transcript is checked before it gets there.
pub fn validate_identity(identity: &str) -> Result<(), StoreError> {
    let (name, host) = identity
        .split_once('@')
        .ok_or_else(|| StoreError::InvalidIdentity(echo(identity)))?;
    validate_participant_name(name)?;
    validate_host(host)?;
    Ok(())
}

/// A name of the given kind: lowercase letters, digits and hyphens; no hyphen
/// at either end; not empty; no longer than the kind's cap.
pub fn validate_slug(value: &str, kind: &Slug) -> Result<(), StoreError> {
    if is_slug(value, kind.max) {
        return Ok(());
    }
    Err(StoreError::InvalidSlug {
        what: kind.what,
        max: kind.max,
        value: echo(value),
        hint: kind.hint,
    })
}

/// The free-text things a join records about the caller: a working directory,
/// a harness session id, a Madari pane id. They are shown to people, so they
/// stay one line and stay short.
fn validate_recorded(what: &'static str, value: &str) -> Result<(), StoreError> {
    if value.is_empty()
        || value.chars().count() > MAX_RECORDED
        || value.contains('\n')
        || value.contains('\r')
    {
        return Err(StoreError::InvalidRecorded {
            what,
            value: echo(value),
        });
    }
    Ok(())
}

/// Folds arbitrary text into the alphabet `is_slug` checks: lowercase letters,
/// digits and single hyphens, with no hyphen at either end. It lives next to
/// the check so the alphabet is written down once. A hostname or a directory
/// name goes through here on its way into an identity; what comes out is
/// something `is_slug` accepts, though it may still be too long.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Lowercase letters, digits and hyphens; no hyphen at either end; not empty;
/// no longer than `max`. Channel names, participant names, hosts and harness
/// ids are all this shape, so they are all this function.
fn is_slug(value: &str, max: usize) -> bool {
    if value.is_empty() || value.chars().count() > max {
        return false;
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return false;
    }
    !value.starts_with('-') && !value.ends_with('-')
}

/// A purpose is the one line a channel is listed with, so it holds one line.
pub fn validate_purpose(purpose: &str) -> Result<(), StoreError> {
    if purpose.chars().count() > MAX_PURPOSE || purpose.contains('\n') || purpose.contains('\r') {
        return Err(StoreError::InvalidPurpose {
            purpose: echo(purpose),
        });
    }
    Ok(())
}

/// A rejected value, short enough to sit in a one-line error.
fn echo(value: &str) -> String {
    if value.chars().count() <= MAX_ECHO {
        return value.to_string();
    }
    let head: String = value.chars().take(MAX_ECHO).collect();
    format!("{head}...")
}

type ChannelRow = (String, Option<String>, String, String, Option<String>);

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn build_channel(row: ChannelRow) -> Result<Channel, StoreError> {
    let (name, purpose, state, created_at, closed_at) = row;
    Ok(Channel {
        name,
        purpose,
        state: state
            .parse()
            .map_err(|_| StoreError::UnknownChannelState(state.clone()))?,
        created_at,
        closed_at,
    })
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let mut applied: usize =
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))? as usize;
    if applied > MIGRATIONS.len() {
        // The file has been through migrations this build has never heard of,
        // so its tables may not mean what this build thinks they mean.
        return Err(StoreError::NewerSchema {
            found: applied,
            supported: MIGRATIONS.len(),
        });
    }
    while applied < MIGRATIONS.len() {
        let tx = conn.transaction()?;
        tx.execute_batch(MIGRATIONS[applied])?;
        applied += 1;
        // PRAGMA does not take bound parameters; `applied` is a usize we own.
        tx.execute_batch(&format!("PRAGMA user_version = {applied}"))?;
        tx.commit()?;
    }
    Ok(())
}

/// A duplicate name specifically, not any constraint: a CHECK or NOT NULL
/// failure must not be reported as "already exists", and must not send the
/// mint loop round again.
fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                extended_code: rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
                ..
            },
            _
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_slug_shaped_names() {
        for name in ["brisk-otter", "a", "team-2", "0agent", "x-y-z"] {
            validate_channel_name(name).unwrap_or_else(|err| panic!("{name}: {err}"));
        }
    }

    #[test]
    fn rejects_anything_else() {
        for name in [
            "",
            "Brisk-Otter",
            "brisk otter",
            "brisk_otter",
            "-brisk",
            "brisk-",
            "brisk.otter",
            "brisk/otter",
            "ਸੁਨੇਹਾ",
            &"a".repeat(CHANNEL_NAME.max + 1),
        ] {
            assert!(
                validate_channel_name(name).is_err(),
                "expected {name:?} to be rejected"
            );
        }
    }

    #[test]
    fn a_rejected_value_is_echoed_short() {
        let long = "a".repeat(10_000);
        let message = validate_channel_name(&long).unwrap_err().to_string();
        assert!(message.chars().count() < 300, "{} chars", message.len());
        assert!(message.contains("..."), "{message}");
    }

    #[test]
    fn accepts_one_line_purposes() {
        for purpose in ["", "coordinating the refactor", &"p".repeat(MAX_PURPOSE)] {
            validate_purpose(purpose).unwrap_or_else(|err| panic!("{purpose:?}: {err}"));
        }
    }

    #[test]
    fn rejects_purposes_that_are_not_one_line() {
        for purpose in [
            "line one\nline two",
            "line one\r\nline two",
            "trailing\r",
            &"p".repeat(MAX_PURPOSE + 1),
        ] {
            assert!(
                validate_purpose(purpose).is_err(),
                "expected {purpose:?} to be rejected"
            );
        }
    }

    #[test]
    fn a_channel_will_not_take_a_multiline_purpose() {
        let store = Store::open_in_memory().expect("open");
        let err = store
            .create_channel(Some("brisk-otter"), Some("line one\nline two"))
            .expect_err("multi-line purpose");
        assert!(matches!(err, StoreError::InvalidPurpose { .. }), "{err}");
        assert!(store.list_channels().expect("list").is_empty());
    }

    #[test]
    fn creates_lists_and_refuses_duplicates() {
        let store = Store::open_in_memory().expect("open");

        let created = store
            .create_channel(Some("brisk-otter"), Some("  the refactor  "))
            .expect("create");
        assert_eq!(created.name, "brisk-otter");
        assert_eq!(created.purpose.as_deref(), Some("the refactor"));
        assert_eq!(created.state, ChannelState::Open);
        assert!(created.created_at.ends_with('Z'), "{}", created.created_at);
        assert_eq!(created.created_at.len(), 20, "{}", created.created_at);

        let minted = store.create_channel(None, None).expect("mint");
        assert!(minted.name.contains('-'), "minted {:?}", minted.name);
        validate_channel_name(&minted.name).expect("minted name is valid");
        assert!(minted.purpose.is_none());

        let err = store
            .create_channel(Some("brisk-otter"), None)
            .expect_err("duplicate");
        assert!(matches!(err, StoreError::ChannelExists(_)), "{err}");

        let channels = store.list_channels().expect("list");
        assert_eq!(channels.len(), 2);
        assert!(channels.iter().any(|c| c.name == "brisk-otter"));
    }

    #[test]
    fn migrations_are_idempotent_across_opens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("saneha.db");

        let store = Store::open(&path).expect("first open");
        store
            .create_channel(Some("quiet-heron"), None)
            .expect("create");
        drop(store);

        let store = Store::open(&path).expect("second open");
        assert_eq!(store.list_channels().expect("list").len(), 1);
        assert!(store.channel("quiet-heron").expect("lookup").is_some());
        assert!(store.channel("nobody-here").expect("lookup").is_none());
    }

    #[test]
    fn a_database_from_before_the_attachments_is_brought_forward() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("saneha.db");

        // A file as an earlier saneha left it: everything up to the
        // transcript, and no attachments.
        let conn = Connection::open(&path).expect("open directly");
        for migration in &MIGRATIONS[..2] {
            conn.execute_batch(migration).expect("apply a migration");
        }
        conn.execute_batch("PRAGMA user_version = 2")
            .expect("record what was applied");
        drop(conn);

        let store = Store::open(&path).expect("open the older database");
        store
            .create_channel(Some("quiet-heron"), None)
            .expect("create");
        let attachment = store
            .new_attachment("quiet-heron")
            .expect("the attachments table is there now");
        std::fs::write(&attachment.path, b"forward\n").expect("write the file");
        let recorded = store
            .record_attachment(&attachment, "notes.md", 8, "text/markdown")
            .expect("record");
        assert_eq!(recorded.filename, "notes.md");
    }

    #[test]
    fn a_database_from_before_the_nullable_working_directory_is_brought_forward() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("saneha.db");

        // A file as the saneha before migration 4 left it, with rows in every
        // table the participants table is pointed at from: the migration must
        // not cost a message its author or a mention its recipient.
        let conn = Connection::open(&path).expect("open directly");
        for migration in &MIGRATIONS[..3] {
            conn.execute_batch(migration).expect("apply a migration");
        }
        conn.execute_batch(
            "PRAGMA user_version = 3;
             INSERT INTO channels (id, name, created_at, last_message_id)
                 VALUES (1, 'quiet-heron', '2026-09-04T09:00:00Z', 1);
             INSERT INTO participants
                 (id, channel_id, identity, name, host, harness, cwd, joined_at)
                 VALUES (1, 1, 'alpha@macbookpro', 'alpha', 'macbookpro', 'claude',
                         '/repos/saneha', '2026-09-04T09:00:00Z');
             INSERT INTO messages (channel_id, id, kind, from_participant, body, created_at)
                 VALUES (1, 1, 'message', 1, 'hello', '2026-09-04T09:00:01Z');
             INSERT INTO recipients (channel_id, message_id, participant_id) VALUES (1, 1, 1);",
        )
        .expect("fill the older database");
        drop(conn);

        let store = Store::open(&path).expect("open the older database");
        let participants = store.list_participants("quiet-heron").expect("list");
        assert_eq!(participants.len(), 1);
        // The directory that was recorded is still recorded.
        assert_eq!(
            participants[0].cwd.as_deref(),
            Some("/repos/saneha"),
            "the working directory did not survive the migration"
        );

        // What pointed at the participant still does.
        let conn = store.conn();
        let messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE from_participant = 1",
                [],
                |row| row.get(0),
            )
            .expect("count messages");
        let recipients: i64 = conn
            .query_row("SELECT COUNT(*) FROM recipients", [], |row| row.get(0))
            .expect("count recipients");
        assert_eq!((messages, recipients), (1, 1));

        // And the unique index came through with the column it never named:
        // two rows for one identity in one channel are still refused.
        let duplicate = conn.execute(
            "INSERT INTO participants
                 (channel_id, identity, name, host, harness, joined_at)
             VALUES (1, 'alpha@macbookpro', 'alpha', 'macbookpro', 'claude', '2026-09-04T09:00:02Z')",
            [],
        );
        assert!(
            duplicate.is_err_and(|err| is_unique_violation(&err)),
            "the unique index on (channel_id, identity) went with the rebuild"
        );
        drop(conn);

        // A join with nothing to record now lands, which is what the whole
        // migration is for.
        let joined = store
            .join("quiet-heron", &request_without_a_directory())
            .expect("join without a working directory");
        assert_eq!(joined.participant.cwd, None);
    }

    /// What the viewer sends: everything a browser knows about itself, which
    /// does not include a working directory.
    fn request_without_a_directory() -> JoinRequest {
        JoinRequest {
            name: "surdy".to_string(),
            host: "web".to_string(),
            harness: "web".to_string(),
            session_id: None,
            pid: None,
            pid_started_at: None,
            cwd: None,
            madari_pane: None,
            same_host_session_live: false,
            held_session_id: None,
        }
    }

    #[test]
    fn deleting_a_channel_takes_its_attachment_rows_with_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("saneha.db")).expect("open");
        store
            .create_channel(Some("quiet-heron"), None)
            .expect("create");

        let pending = store.new_attachment("quiet-heron").expect("mint an id");
        std::fs::write(&pending.path, b"bytes\n").expect("write the file");
        store
            .record_attachment(&pending, "notes.md", 6, "text/markdown")
            .expect("record");

        // The rows go by cascade; the files are what `remove_channel_files`
        // is for, and what the delete verb has to call.
        store
            .conn()
            .execute("DELETE FROM channels WHERE name = ?1", ["quiet-heron"])
            .expect("delete the channel");
        let left: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM attachments", [], |row| row.get(0))
            .expect("count");
        assert_eq!(left, 0);
        assert!(pending.path.exists(), "the file went without being asked");
    }

    #[test]
    fn remove_channel_takes_the_whole_channel_and_says_what_went() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("saneha.db")).expect("open");
        store
            .create_channel(Some("quiet-heron"), None)
            .expect("create");
        store
            .create_channel(Some("brisk-otter"), None)
            .expect("the one left alone");

        let pending = store.new_attachment("quiet-heron").expect("mint an id");
        std::fs::write(&pending.path, b"bytes\n").expect("write the file");
        store
            .record_attachment(&pending, "notes.md", 6, "text/markdown")
            .expect("record");

        let (channel_id, removed) = store.remove_channel("quiet-heron").expect("remove");
        assert_eq!(removed.channel, "quiet-heron");
        assert_eq!(removed.counts.attachments, 1);
        assert_eq!(removed.counts.attachment_bytes, 6);

        // The rows go with the row that owned them; the files are the caller's
        // next step, and until it takes it they are still there.
        let left: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM attachments", [], |row| row.get(0))
            .expect("count");
        assert_eq!(left, 0);
        assert!(pending.path.exists(), "the file went without being asked");

        store
            .remove_channel_files(channel_id)
            .expect("remove the files");
        assert!(!pending.path.exists(), "the file stayed after being asked");

        // One channel, and no more than one.
        assert!(store.channel("brisk-otter").expect("lookup").is_some());
        assert!(matches!(
            store.remove_channel("quiet-heron"),
            Err(StoreError::NoSuchChannel(_))
        ));
    }

    #[test]
    fn a_close_names_who_closed_it_in_the_body_and_nobody_in_the_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("saneha.db")).expect("open");
        store
            .create_channel(Some("quiet-heron"), None)
            .expect("create");

        let closed = store.close("quiet-heron", "surdy@web").expect("close");
        assert!(closed.closed);
        assert_eq!(closed.channel.state, ChannelState::Closed);

        // The schema's CHECK holds a close to naming neither participant
        // column, so the identity that closed it lives in the body. A row that
        // said otherwise would not have been written at all.
        let (kind, from, about, body): (String, Option<i64>, Option<i64>, String) = store
            .conn()
            .query_row(
                "SELECT kind, from_participant, about_participant, body FROM messages",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("the close message");
        assert_eq!(kind, "close");
        assert_eq!(from, None);
        assert_eq!(about, None);
        assert_eq!(body, "surdy@web closed the channel");

        // Closing it again writes nothing and is not an error.
        let again = store
            .close("quiet-heron", "surdy@web")
            .expect("close again");
        assert!(!again.closed);
        let messages: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .expect("count");
        assert_eq!(messages, 1);

        // And the closed_at it recorded is the one thing a second close must
        // not move.
        let closed_at: Option<String> = store
            .conn()
            .query_row("SELECT closed_at FROM channels", [], |row| row.get(0))
            .expect("the closed channel");
        assert!(closed_at.is_some(), "a close recorded no time it happened");
    }

    #[test]
    fn a_close_by_something_that_is_not_an_identity_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("saneha.db")).expect("open");
        store
            .create_channel(Some("quiet-heron"), None)
            .expect("create");

        for by in ["", "surdy", "@web", "surdy@", "Surdy@web", "surdy@web@x"] {
            assert!(
                matches!(
                    store.close("quiet-heron", by),
                    Err(StoreError::InvalidIdentity(_) | StoreError::InvalidSlug { .. })
                ),
                "{by:?} was taken for an identity"
            );
        }
        assert_eq!(
            store
                .channel("quiet-heron")
                .expect("lookup")
                .expect("it is there")
                .state,
            ChannelState::Open
        );
    }

    #[test]
    fn an_attachment_id_is_128_random_bits_of_hex() {
        let first = mint_attachment_id();
        assert_eq!(first.len(), 32);
        assert!(is_attachment_id(&first), "{first}");
        assert_ne!(first, mint_attachment_id());

        // Anything that is not that shape is not looked up at all, because an
        // id becomes a path segment.
        for id in [
            "",
            "0123456789abcdef0123456789abcde",
            "0123456789ABCDEF0123456789ABCDEF",
            "../../../../etc/passwd/0123456789",
            "0123456789abcdef0123456789abcdeg",
        ] {
            assert!(!is_attachment_id(id), "{id}");
        }
    }

    #[test]
    fn refuses_a_database_from_a_newer_saneha() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("saneha.db");
        drop(Store::open(&path).expect("create the database"));

        let conn = Connection::open(&path).expect("open directly");
        conn.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len() + 2))
            .expect("pretend a newer saneha migrated it");
        drop(conn);

        let err = match Store::open(&path) {
            Err(err) => err,
            Ok(_) => panic!("a newer schema must be refused"),
        };
        assert!(
            matches!(err, StoreError::NewerSchema { found, supported }
                if found == MIGRATIONS.len() + 2 && supported == MIGRATIONS.len()),
            "{err}"
        );
        assert!(err.to_string().contains("newer saneha"), "{err}");
    }

    #[test]
    fn refuses_a_channel_state_it_does_not_understand() {
        // What a row written by a newer saneha, past a relaxed CHECK
        // constraint, would look like on the way out of the database.
        let row = (
            "brisk-otter".to_string(),
            None,
            "paused".to_string(),
            "2026-09-04T09:00:00Z".to_string(),
            None,
        );
        let err = build_channel(row).expect_err("unknown state");
        assert!(
            matches!(&err, StoreError::UnknownChannelState(state) if state == "paused"),
            "{err}"
        );

        let known = (
            "brisk-otter".to_string(),
            None,
            "closed".to_string(),
            "2026-09-04T09:00:00Z".to_string(),
            Some("2026-09-04T10:00:00Z".to_string()),
        );
        assert_eq!(
            build_channel(known).expect("closed is understood").state,
            ChannelState::Closed
        );
    }

    #[test]
    fn a_constraint_that_is_not_a_duplicate_is_not_reported_as_one() {
        let store = Store::open_in_memory().expect("open");
        let err = store
            .conn()
            .execute(
                "INSERT INTO channels (name, purpose, state, created_at)
                 VALUES ('brisk-otter', NULL, 'paused', '2026-09-04T09:00:00Z')",
                [],
            )
            .expect_err("the CHECK constraint rejects it");
        assert!(!is_unique_violation(&err), "{err}");
    }
}
