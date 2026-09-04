//! The HTTP server behind `saneha serve`.
//!
//! There is no authentication by design (ADR-0003): the network the server
//! listens on is the trust boundary, so error text is written for whoever is
//! reading it rather than hidden.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;

use crate::api::{
    ApiError, Channel, ChannelList, JoinRequest, Joined, NewChannel, Participant, ParticipantList,
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
            | StoreError::InvalidRecorded { .. } => StatusCode::BAD_REQUEST,
            StoreError::NoSuchChannel(_) | StoreError::NoSuchParticipant { .. } => {
                StatusCode::NOT_FOUND
            }
            StoreError::ChannelExists(_)
            | StoreError::ChannelClosed(_)
            | StoreError::ParticipantChanged { .. }
            | StoreError::SuffixExhausted { .. } => StatusCode::CONFLICT,
            StoreError::MintExhausted(_) => StatusCode::SERVICE_UNAVAILABLE,
            StoreError::NewerSchema { .. }
            | StoreError::UnknownChannelState(_)
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
