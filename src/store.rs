//! The SQLite file the server owns.
//!
//! One file holds everything: channels now, and in later tickets participants,
//! transcripts, read cursors and attachments. The schema is applied by the
//! versioned migration list below, run at every start, so an older file is
//! brought forward without any separate step.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::api::{Channel, ChannelState};
use crate::slug;

/// The longest a channel name may be.
pub const MAX_CHANNEL_NAME: usize = 64;

/// How many slugs the server tries before giving up on minting a name.
const MINT_ATTEMPTS: usize = 24;

/// Applied in order; the index of the last applied entry is kept in SQLite's
/// `user_version`. Never edit an entry that has shipped, only append.
///
/// Migration 1 creates the channels table alone. The rest of v1 arrives as
/// further migrations: `participants` (identity, harness, host, read cursor),
/// `messages` (monotonic id allocated from `channels.last_message_id`,
/// recipients, kind) and `attachments`. The forward-looking columns here are
/// the ones the channels table itself will need for that: `closed_at` for
/// `saneha close`, and `last_message_id` as the per-channel allocator that
/// makes message ids monotonic within a transcript.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE channels (
    id              INTEGER PRIMARY KEY,
    name            TEXT    NOT NULL,
    purpose         TEXT,
    state           TEXT    NOT NULL DEFAULT 'open'
                            CHECK (state IN ('open', 'closed')),
    created_at      INTEGER NOT NULL,
    closed_at       INTEGER,
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

    #[error("a channel named {0:?} already exists")]
    ChannelExists(String),

    #[error("could not mint a channel name after {0} attempts; name one yourself")]
    MintExhausted(usize),

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
        let created_at = now_unix();

        match name {
            Some(name) => {
                let name = name.trim();
                validate_channel_name(name)?;
                self.insert_channel(name, purpose, created_at)
            }
            None => {
                for _ in 0..MINT_ATTEMPTS {
                    let minted = slug::mint();
                    match self.insert_channel(&minted, purpose, created_at) {
                        Err(StoreError::ChannelExists(_)) => continue,
                        other => return other,
                    }
                }
                Err(StoreError::MintExhausted(MINT_ATTEMPTS))
            }
        }
    }

    fn insert_channel(
        &self,
        name: &str,
        purpose: Option<&str>,
        created_at: i64,
    ) -> Result<Channel, StoreError> {
        let conn = self.conn();
        let result = conn.execute(
            "INSERT INTO channels (name, purpose, state, created_at)
             VALUES (?1, ?2, 'open', ?3)",
            rusqlite::params![name, purpose, created_at],
        );
        match result {
            Ok(_) => Ok(Channel {
                name: name.to_string(),
                purpose: purpose.map(str::to_string),
                state: ChannelState::Open,
                created_at: to_rfc3339(created_at),
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
        let rows = statement.query_map([], |row| {
            Ok(Channel {
                name: row.get(0)?,
                purpose: row.get(1)?,
                state: parse_state(&row.get::<_, String>(2)?),
                created_at: to_rfc3339(row.get(3)?),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// One channel by name, or `None` if there is no such channel.
    pub fn channel(&self, name: &str) -> Result<Option<Channel>, StoreError> {
        let conn = self.conn();
        let channel = conn
            .query_row(
                "SELECT name, purpose, state, created_at FROM channels WHERE name = ?1",
                [name],
                |row| {
                    Ok(Channel {
                        name: row.get(0)?,
                        purpose: row.get(1)?,
                        state: parse_state(&row.get::<_, String>(2)?),
                        created_at: to_rfc3339(row.get(3)?),
                    })
                },
            )
            .optional()?;
        Ok(channel)
    }
}

/// Channel names are typed by people and pasted between machines, so they are
/// deliberately narrow: lowercase letters, digits and hyphens only.
pub fn validate_channel_name(name: &str) -> Result<(), StoreError> {
    let invalid = || StoreError::InvalidChannelName {
        name: name.to_string(),
    };

    if name.is_empty() || name.len() > MAX_CHANNEL_NAME {
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

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let mut applied: usize =
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))? as usize;
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

fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                ..
            },
            _
        )
    )
}

fn parse_state(raw: &str) -> ChannelState {
    // The column has a CHECK constraint, so anything else is a database written
    // by a newer saneha; treat it as open rather than failing the whole listing.
    raw.parse().unwrap_or(ChannelState::Open)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn to_rfc3339(seconds: i64) -> String {
    OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| seconds.to_string())
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
    fn creates_lists_and_refuses_duplicates() {
        let store = Store::open_in_memory().expect("open");

        let created = store
            .create_channel(Some("brisk-otter"), Some("  the refactor  "))
            .expect("create");
        assert_eq!(created.name, "brisk-otter");
        assert_eq!(created.purpose.as_deref(), Some("the refactor"));
        assert_eq!(created.state, ChannelState::Open);

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
}
