//! The SQLite file the server owns.
//!
//! One file holds everything: channels now, and in later tickets participants,
//! transcripts, read cursors and attachments. The schema is applied by the
//! versioned migration list below, run at every start, so an older file is
//! brought forward without any separate step.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};

use crate::api::{Channel, ChannelState};
use crate::slug;

/// The longest a channel name may be.
pub const MAX_CHANNEL_NAME: usize = 64;

/// The longest a purpose may be. A purpose is one line saying what a channel is
/// for, not a document.
pub const MAX_PURPOSE: usize = 256;

/// How much of a rejected value is echoed back in the error.
const MAX_ECHO: usize = 80;

/// How many slugs the server tries before giving up on minting a name.
const MINT_ATTEMPTS: usize = 24;

/// SQLite renders timestamps itself, so there is one clock and one format.
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%SZ', 'now')";

/// Applied in order; the number applied is kept in SQLite's `user_version`.
/// Never edit an entry that has shipped, only append.
///
/// Migration 1 creates the channels table alone. The rest of v1 arrives as
/// further migrations: `participants` (identity, harness, host, read cursor),
/// `messages` (monotonic id allocated from `channels.last_message_id`,
/// recipients, kind) and `attachments`. The forward-looking columns here are
/// the ones the channels table itself will need for that: `closed_at` for
/// `saneha close`, and `last_message_id` as the per-channel allocator that
/// makes message ids monotonic within a transcript. `AUTOINCREMENT` keeps a
/// deleted channel's id from being handed to a later channel, so anything
/// keyed by channel id stays unambiguous.
const MIGRATIONS: &[&str] = &[r#"
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
"#];

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
}

/// Channel names are typed by people and pasted between machines, so they are
/// deliberately narrow: lowercase letters, digits and hyphens only.
pub fn validate_channel_name(name: &str) -> Result<(), StoreError> {
    let invalid = || StoreError::InvalidChannelName { name: echo(name) };

    if name.is_empty() || name.chars().count() > MAX_CHANNEL_NAME {
        return Err(invalid());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(invalid());
    }
    let first = name.chars().next().expect("name is not empty");
    if first == '-' || name.ends_with('-') {
        return Err(invalid());
    }
    Ok(())
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
