//! The HTTP server behind `saneha serve`.
//!
//! There is no authentication by design (ADR-0003): the network the server
//! listens on is the trust boundary, so error text is written for whoever is
//! reading it rather than hidden.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;

use crate::api::{
    ApiError, Channel, ChannelList, CursorUpdate, JoinRequest, Joined, Message, MessageList,
    MessageQuery, NewChannel, NewMessage, Participant, ParticipantList, DEFAULT_MESSAGE_LIMIT,
};
use crate::store::{Store, StoreError};

/// Where `saneha serve` listens unless told otherwise.
pub const DEFAULT_BIND: &str = "127.0.0.1:7343";

/// The database file name under the data directory.
const DATABASE_FILE: &str = "saneha.db";

/// Where `saneha serve` keeps its SQLite file unless told otherwise:
/// `$XDG_DATA_HOME/saneha/saneha.db`, falling back to
/// `$HOME/.local/share/saneha/saneha.db`, and to `./saneha.db` when neither
/// variable is set.
pub fn default_database_path() -> PathBuf {
    if let Some(data_home) = env_path("XDG_DATA_HOME") {
        return data_home.join("saneha").join(DATABASE_FILE);
    }
    if let Some(home) = env_path("HOME") {
        return home
            .join(".local")
            .join("share")
            .join("saneha")
            .join(DATABASE_FILE);
    }
    PathBuf::from(DATABASE_FILE)
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Every route the server answers.
pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/channels", post(create_channel).get(list_channels))
        .route(
            "/channels/{channel}/participants",
            post(join).get(list_participants),
        )
        .route(
            "/channels/{channel}/participants/{identity}",
            get(participant),
        )
        // Advancing a read cursor is its own route, and not something a fetch
        // does on the way past: only `read` moves a cursor, and `wait` and a
        // history read use the same fetch without moving anything (ADR-0004).
        .route(
            "/channels/{channel}/participants/{identity}/cursor",
            post(set_read_cursor),
        )
        .route(
            "/channels/{channel}/messages",
            post(send_message).get(list_messages),
        )
        .with_state(store)
}

/// Serves until the listener fails or the process is asked to stop.
pub async fn run(listener: TcpListener, store: Arc<Store>) -> anyhow::Result<()> {
    axum::serve(listener, router(store))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn root() -> &'static str {
    "saneha is running. Point SANEHA_URL at this address.\n"
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "saneha" }))
}

async fn create_channel(
    State(store): State<Arc<Store>>,
    Json(body): Json<NewChannel>,
) -> Result<(StatusCode, Json<Channel>), Failure> {
    let channel = store.create_channel(body.name.as_deref(), body.purpose.as_deref())?;
    Ok((StatusCode::CREATED, Json(channel)))
}

async fn list_channels(State(store): State<Arc<Store>>) -> Result<Json<ChannelList>, Failure> {
    let channels = store.list_channels()?;
    Ok(Json(ChannelList { channels }))
}

/// A join, or a resume, or the grant of a suffixed identity: which one it was
/// is in the body rather than in the status, because all three succeeded.
async fn join(
    State(store): State<Arc<Store>>,
    Path(channel): Path<String>,
    Json(body): Json<JoinRequest>,
) -> Result<(StatusCode, Json<Joined>), Failure> {
    let joined = store.join(&channel, &body)?;
    let status = if joined.resumed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(joined)))
}

async fn list_participants(
    State(store): State<Arc<Store>>,
    Path(channel): Path<String>,
) -> Result<Json<ParticipantList>, Failure> {
    let participants = store.list_participants(&channel)?;
    Ok(Json(ParticipantList { participants }))
}

async fn participant(
    State(store): State<Arc<Store>>,
    Path((channel, identity)): Path<(String, String)>,
) -> Result<Json<Participant>, Failure> {
    let participant = store.participant(&channel, &identity)?.ok_or_else(|| {
        Failure::from(StoreError::NoSuchParticipant {
            channel: channel.clone(),
            identity: identity.clone(),
        })
    })?;
    Ok(Json(participant))
}

/// One message from a participant, with everyone it is addressed to worked out
/// from its mentions and from `to`.
async fn send_message(
    State(store): State<Arc<Store>>,
    Path(channel): Path<String>,
    Json(body): Json<NewMessage>,
) -> Result<(StatusCode, Json<Message>), Failure> {
    let message = store.send(&channel, &body.from, &body.body, &body.to)?;
    Ok((StatusCode::CREATED, Json(message)))
}

/// A page of a transcript. This moves nothing, so a caller that wants its read
/// cursor advanced asks for that separately.
async fn list_messages(
    State(store): State<Arc<Store>>,
    Path(channel): Path<String>,
    Query(query): Query<MessageQuery>,
) -> Result<Json<MessageList>, Failure> {
    let messages = store.messages(
        &channel,
        query.after.unwrap_or(0),
        query.limit.unwrap_or(DEFAULT_MESSAGE_LIMIT),
    )?;
    Ok(Json(MessageList { messages }))
}

/// Moves a read cursor forward. A value below the one recorded is ignored
/// rather than refused, so a cursor never moves backwards however many readers
/// share an identity.
async fn set_read_cursor(
    State(store): State<Arc<Store>>,
    Path((channel, identity)): Path<(String, String)>,
    Json(body): Json<CursorUpdate>,
) -> Result<Json<Participant>, Failure> {
    let participant = store.set_read_cursor(&channel, &identity, body.read_cursor)?;
    Ok(Json(participant))
}

/// A failed request, rendered as `{"error": "..."}` so the subcommand on the
/// other end can print the server's own words.
pub struct Failure {
    status: StatusCode,
    message: String,
    /// Whether looking again and asking again could get a different answer.
    retry: bool,
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiError {
                error: self.message,
                retry: self.retry,
            }),
        )
            .into_response()
    }
}

impl From<StoreError> for Failure {
    fn from(err: StoreError) -> Self {
        let status = match err {
            StoreError::InvalidSlug { .. }
            | StoreError::InvalidPurpose { .. }
            | StoreError::InvalidRecorded { .. }
            | StoreError::EmptyBody
            | StoreError::BodyTooLarge { .. }
            | StoreError::NoSuchRecipient { .. }
            | StoreError::AmbiguousRecipient { .. } => StatusCode::BAD_REQUEST,
            StoreError::NoSuchChannel(_)
            | StoreError::NoSuchParticipant { .. }
            | StoreError::NotAParticipant { .. } => StatusCode::NOT_FOUND,
            StoreError::ChannelExists(_)
            | StoreError::ChannelClosed { .. }
            | StoreError::ParticipantChanged { .. }
            | StoreError::SuffixExhausted { .. } => StatusCode::CONFLICT,
            StoreError::MintExhausted(_) => StatusCode::SERVICE_UNAVAILABLE,
            StoreError::NewerSchema { .. }
            | StoreError::UnknownChannelState(_)
            | StoreError::UnknownMessageKind(_)
            | StoreError::Sqlite(_)
            | StoreError::Open { .. }
            | StoreError::Directory { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Failure {
            status,
            // A participant that changed hands is the one failure a caller can
            // do something about without a person: look again, ask again.
            retry: matches!(err, StoreError::ParticipantChanged { .. }),
            message: err.to_string(),
        }
    }
}
