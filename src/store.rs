//! The SQLite file the server owns.
//!
//! One file holds everything: channels, participants and their read cursors,
//! and the transcript; attachments follow in a later ticket. The schema is applied by the
//! versioned migration list below, run at every start, so an older file is
//! brought forward without any separate step.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};

use crate::api::{Channel, ChannelState, JoinRequest, Joined, Participant};
use crate::slug;

/// The longest a channel name may be.
pub const MAX_CHANNEL_NAME: usize = 64;

/// The longest a purpose may be. A purpose is one line saying what a channel is
/// for, not a document.
pub const MAX_PURPOSE: usize = 256;

/// The longest the name half of an identity may be. The same cap as a channel
/// name, because both are typed by people and pasted between machines.
pub const MAX_PARTICIPANT_NAME: usize = MAX_CHANNEL_NAME;

/// The longest the host half of an identity may be.
pub const MAX_HOST: usize = 64;

/// The longest a harness id may be.
pub const MAX_HARNESS: usize = 32;

/// The longest a working directory, harness session id or Madari pane id the
/// server will record.
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
/// frozen. Migration 2 adds participants and the transcript. `attachments` is
/// still to come. The forward-looking columns in migration 1 are the ones the
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
    // `id` is per-channel and monotonic, allocated from
    // `channels.last_message_id` in the same transaction as the insert, so a
    // transcript reads 1, 2, 3 and a read cursor is a number in that sequence.
    r#"
CREATE TABLE participants (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id  INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    identity    TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    host        TEXT    NOT NULL,
    harness     TEXT    NOT NULL,
    session_id  TEXT,
    pid         INTEGER,
    cwd         TEXT    NOT NULL,
    madari_pane TEXT,
    away        INTEGER NOT NULL DEFAULT 0 CHECK (away IN (0, 1)),
    read_cursor INTEGER NOT NULL DEFAULT 0,
    joined_at   TEXT    NOT NULL,
    left_at     TEXT
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
"#,
];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "channel names use lowercase letters, digits and hyphens, start with a letter or digit, \
         and are at most {max} characters (got {name:?})",
        max = MAX_CHANNEL_NAME
    )]
    InvalidChannelName { name: String },

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

    #[error("the channel {0:?} is closed, so nobody can join it")]
    ChannelClosed(String),

    #[error(
        "participant names use lowercase letters, digits and hyphens, start with a letter or \
         digit, and are at most {max} characters (got {name:?}); name yourself with --as",
        max = MAX_PARTICIPANT_NAME
    )]
    InvalidParticipantName { name: String },

    #[error(
        "a host is lowercase letters, digits and hyphens, at most {max} characters (got {host:?})",
        max = MAX_HOST
    )]
    InvalidHost { host: String },

    #[error(
        "a harness id is lowercase letters, digits and hyphens, at most {max} characters \
         (got {harness:?})",
        max = MAX_HARNESS
    )]
    InvalidHarness { harness: String },

    #[error("{what} must be one line of at most {max} characters (got {value:?})", max = MAX_RECORDED)]
    InvalidRecorded { what: &'static str, value: String },

    #[error("nobody named {identity:?} has joined the channel {channel:?}")]
    NoSuchParticipant { channel: String, identity: String },

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

/// The database, shared by every request the server handles.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Opens the database at `path`, creating the file, its parent directories
    /// and the schema if they are not there yet.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| StoreError::Directory {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(path).map_err(|source| StoreError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_connection(conn)
    }

    /// A database held in memory, for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&mut conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
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
            "SELECT name, purpose, state, created_at FROM channels ORDER BY created_at, id",
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
                "SELECT name, purpose, state, created_at FROM channels WHERE name = ?1",
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
    /// All of it, including the message id allocated from
    /// `channels.last_message_id`, happens in one transaction, so a join
    /// either lands whole or not at all.
    pub fn join(&self, channel: &str, request: &JoinRequest) -> Result<Joined, StoreError> {
        validate_participant_name(&request.name)?;
        validate_host(&request.host)?;
        validate_harness(&request.harness)?;
        validate_recorded("a working directory", &request.cwd)?;
        if let Some(session_id) = &request.session_id {
            validate_recorded("a harness session id", session_id)?;
        }
        if let Some(pane) = &request.madari_pane {
            validate_recorded("a Madari pane id", pane)?;
        }

        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let channel_id = open_channel_id(&tx, channel)?;

        let (participant_id, name, resumed, suffixed) = if request.same_host_session_live {
            let (id, name) = insert_suffixed(&tx, channel_id, channel, request)?;
            (id, name, false, true)
        } else {
            let identity = identity_of(&request.name, &request.host);
            match participant_id(&tx, channel_id, &identity)? {
                Some(id) => {
                    resume_participant(&tx, id, request)?;
                    (id, request.name.clone(), true, false)
                }
                None => {
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
            participant_id,
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
    /// join flow asks this first, to see whether the identity it wants is
    /// held by a session still running on this machine.
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
}

/// `name@host`, the one place the two halves are put together.
pub fn identity_of(name: &str, host: &str) -> String {
    format!("{name}@{host}")
}

/// The columns of a participant, in the order `read_participant_row` reads.
const PARTICIPANT_COLUMNS: &str = "identity, name, host, harness, session_id, pid, cwd, \
                                   madari_pane, away, read_cursor, joined_at, left_at";

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
        cwd: row.get(6)?,
        madari_pane: row.get(7)?,
        away: row.get::<_, i64>(8)? != 0,
        read_cursor: row.get(9)?,
        joined_at: row.get(10)?,
        left_at: row.get(11)?,
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

/// The row id of a channel that is still open. Nothing sets `closed` yet;
/// `saneha close` will, and a join must already refuse a closed channel.
fn open_channel_id(conn: &Connection, channel: &str) -> Result<i64, StoreError> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, state FROM channels WHERE name = ?1",
            [channel],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (id, state) = row.ok_or_else(|| StoreError::NoSuchChannel(channel.to_string()))?;
    match state.parse::<ChannelState>() {
        Ok(ChannelState::Open) => Ok(id),
        Ok(ChannelState::Closed) => Err(StoreError::ChannelClosed(channel.to_string())),
        Err(_) => Err(StoreError::UnknownChannelState(state)),
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
                 (channel_id, identity, name, host, harness, session_id, pid, cwd,
                  madari_pane, away, read_cursor, joined_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, {NOW})
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
            request.cwd,
            request.madari_pane,
        ],
        |row| row.get(0),
    )?)
}

/// Continues an existing participant. The name and the read cursor are what a
/// resume is for, so they are the two columns left alone.
fn resume_participant(conn: &Connection, id: i64, request: &JoinRequest) -> Result<(), StoreError> {
    conn.execute(
        &format!(
            "UPDATE participants
                SET harness = ?1, session_id = ?2, pid = ?3, cwd = ?4, madari_pane = ?5,
                    away = 0, left_at = NULL, joined_at = {NOW}
              WHERE id = ?6"
        ),
        rusqlite::params![
            request.harness,
            request.session_id,
            request.pid.map(i64::from),
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
    let room = MAX_PARTICIPANT_NAME - (MAX_SUFFIX.to_string().len() + 1);
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

/// Writes one system message, taking the next id from the channel's own
/// allocator so ids are monotonic within the transcript.
fn write_system_message(
    conn: &Connection,
    channel_id: i64,
    kind: &str,
    about_participant: i64,
    body: &str,
) -> Result<i64, StoreError> {
    let message_id: i64 = conn.query_row(
        "UPDATE channels SET last_message_id = last_message_id + 1
          WHERE id = ?1
      RETURNING last_message_id",
        [channel_id],
        |row| row.get(0),
    )?;
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

/// Channel names are typed by people and pasted between machines, so they are
/// deliberately narrow: lowercase letters, digits and hyphens only.
pub fn validate_channel_name(name: &str) -> Result<(), StoreError> {
    if is_slug(name, MAX_CHANNEL_NAME) {
        Ok(())
    } else {
        Err(StoreError::InvalidChannelName { name: echo(name) })
    }
}

/// The name half of an identity is typed and pasted the same way a channel
/// name is, so it is held to the same shape.
pub fn validate_participant_name(name: &str) -> Result<(), StoreError> {
    if is_slug(name, MAX_PARTICIPANT_NAME) {
        Ok(())
    } else {
        Err(StoreError::InvalidParticipantName { name: echo(name) })
    }
}

/// The host half of an identity. The client folds a real hostname into this
/// shape before sending it; the server still checks, because an identity is
/// split on `@` by everything that reads one.
pub fn validate_host(host: &str) -> Result<(), StoreError> {
    if is_slug(host, MAX_HOST) {
        Ok(())
    } else {
        Err(StoreError::InvalidHost { host: echo(host) })
    }
}

/// A harness id, which appears in a derived name, so it has a name's shape.
pub fn validate_harness(harness: &str) -> Result<(), StoreError> {
    if is_slug(harness, MAX_HARNESS) {
        Ok(())
    } else {
        Err(StoreError::InvalidHarness {
            harness: echo(harness),
        })
    }
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

type ChannelRow = (String, Option<String>, String, String);

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn build_channel(row: ChannelRow) -> Result<Channel, StoreError> {
    let (name, purpose, state, created_at) = row;
    Ok(Channel {
        name,
        purpose,
        state: state
            .parse()
            .map_err(|_| StoreError::UnknownChannelState(state.clone()))?,
        created_at,
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
            &"a".repeat(MAX_CHANNEL_NAME + 1),
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
