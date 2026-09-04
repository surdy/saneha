//! The HTTP server behind `saneha serve`.
//!
//! There is no authentication by design (ADR-0003): the network the server
//! listens on is the trust boundary, so error text is written for whoever is
//! reading it rather than hidden.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use tokio::net::TcpListener;

use crate::api::{
    ApiError, Attachment, Channel, ChannelList, CursorUpdate, JoinRequest, Joined, Message,
    MessageList, MessageQuery, NewChannel, NewMessage, Participant, ParticipantList,
    DEFAULT_CONTENT_TYPE, DEFAULT_MESSAGE_LIMIT, FILENAME_HEADER, MAX_ATTACHMENT, MAX_BODY,
};
use crate::store::{PendingAttachment, Store, StoreError, UNBOUND_ATTACHMENT_TTL};

/// Where `saneha serve` listens unless told otherwise.
pub const DEFAULT_BIND: &str = "127.0.0.1:7343";

/// The database file name under the data directory.
const DATABASE_FILE: &str = "saneha.db";

/// How much of a request the server will read before refusing it.
///
/// A message body is capped at [`MAX_BODY`], and JSON escaping can make a body
/// several times its own length on the wire, so this sits well above it: what
/// it stops is something that was never going to be a message, and the body
/// cap itself is what refuses one that nearly was, in words that say so.
const MAX_REQUEST_BODY: usize = MAX_BODY * 8;

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
        // An upload is the one request whose body is meant to be large, and it
        // is counted as it is written rather than held in memory, so the limit
        // every other route wants is turned off here and [`MAX_ATTACHMENT`] is
        // enforced by the handler. A route layer is applied inside the
        // router's own, so this is the limit that stands for this route.
        .route(
            "/channels/{channel}/attachments",
            post(upload_attachment).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/channels/{channel}/attachments/{id}",
            get(download_attachment),
        )
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY))
        .with_state(store)
}

/// [`Json`] with axum's own refusals rewritten into saneha's `{"error": ...}`
/// shape, so a subcommand on the other end always has the server's own words
/// to print rather than a bare status and a framework's sentence.
struct ApiJson<T>(T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = Failure;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => Err(Failure::unreadable(
                rejection.status(),
                rejection.body_text(),
            )),
        }
    }
}

/// [`Query`], refused the same way: `?after=abc` is a bad request in saneha's
/// shape rather than a line of plain text.
struct ApiQuery<T>(T);

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(ApiQuery(value)),
            Err(rejection) => Err(Failure::unreadable(
                rejection.status(),
                rejection.body_text(),
            )),
        }
    }
}

/// Serves until the listener fails or the process is asked to stop.
pub async fn run(listener: TcpListener, store: Arc<Store>) -> anyhow::Result<()> {
    // Once at the start, because an upload interrupted by the stop that led to
    // this start left a file nothing will ever bind, and then on the hour.
    sweep(&store);
    let sweeper = tokio::spawn(sweep_hourly(Arc::clone(&store)));

    let served = axum::serve(listener, router(store))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    sweeper.abort();
    served?;
    Ok(())
}

/// How often the unbound attachments are swept up.
const SWEEP_EVERY: Duration = Duration::from_secs(UNBOUND_ATTACHMENT_TTL as u64);

/// The sweep, for as long as the server is serving.
async fn sweep_hourly(store: Arc<Store>) {
    let mut every = tokio::time::interval(SWEEP_EVERY);
    // The first tick is immediate, and the start has just swept.
    every.tick().await;
    loop {
        every.tick().await;
        sweep(&store);
    }
}

/// Removes the attachments nobody bound to a message and that are old enough
/// that nobody is going to. A sweep that fails is worth saying out loud and
/// nothing more: the next one is an hour away, and a server that stopped
/// serving over a file it could not delete would be worse than the file.
fn sweep(store: &Store) {
    match store.sweep_unbound_attachments(UNBOUND_ATTACHMENT_TTL) {
        Ok(0) => {}
        Ok(swept) => say(&format!(
            "removed {swept} attachment(s) no message ever carried"
        )),
        Err(err) => say(&format!("could not sweep unbound attachments: {err}")),
    }
}

/// Waits for the first signal that means stop, and says which one it was.
///
/// SIGTERM is what systemd, Podman and every other supervisor send first, and
/// the server is PID 1 in the container, where a SIGTERM nothing has handled is
/// ignored outright: without this the stop stalls and ends in SIGKILL.
#[cfg(unix)]
async fn shutdown_signal() {
    let signal = tokio::select! {
        signal = interrupt() => signal,
        signal = terminate() => signal,
    };
    say(&format!("saneha is stopping on {signal}"));
}

/// Waits for SIGINT.
///
/// A handler that will not install is nothing this can do anything about, and
/// resolving would stop the server the moment it started, so it waits forever
/// instead and leaves the shutdown to the other signal.
#[cfg(unix)]
async fn interrupt() -> &'static str {
    match tokio::signal::ctrl_c().await {
        Ok(()) => "SIGINT",
        Err(_) => std::future::pending().await,
    }
}

/// Waits for SIGTERM, and waits forever when its handler will not install, for
/// the same reason.
#[cfg(unix)]
async fn terminate() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};

    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            terminate.recv().await;
            "SIGTERM"
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        // No second signal to fall back to here, but stopping a server that
        // has only just started is worse than waiting.
        std::future::pending::<()>().await;
    }
    say("saneha is stopping on an interrupt");
}

/// One line on standard output, alongside the lines `saneha serve` prints when
/// it starts. Standard output that has gone away is not worth failing a
/// shutdown over.
fn say(line: &str) {
    use std::io::Write;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

async fn root() -> &'static str {
    "saneha is running. Point SANEHA_URL at this address.\n"
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "saneha" }))
}

async fn create_channel(
    State(store): State<Arc<Store>>,
    ApiJson(body): ApiJson<NewChannel>,
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
    ApiJson(body): ApiJson<JoinRequest>,
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
    ApiJson(body): ApiJson<NewMessage>,
) -> Result<(StatusCode, Json<Message>), Failure> {
    let message = store.send(
        &channel,
        &body.from,
        &body.body,
        &body.to,
        &body.attachments,
    )?;
    Ok((StatusCode::CREATED, Json(message)))
}

/// One file, uploaded before the message that will carry it.
///
/// The body is the file itself: nothing wraps it, so nothing has to be parsed
/// back out of it, and it is written to its place as it arrives rather than
/// held in memory. The name it had travels in `X-Saneha-Filename`, because a
/// name is not part of a file's bytes.
///
/// What comes back is an attachment nothing carries yet. The send that names
/// its id is what binds it; until then it is a file the sweep will remove.
async fn upload_attachment(
    State(store): State<Arc<Store>>,
    Path(channel): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Attachment>), Failure> {
    let filename = filename_of(&headers)?;
    let content_type = content_type_of(&headers);

    // A caller that says up front how much it is about to send is told before
    // it sends any of it.
    if let Some(size) = declared_length(&headers) {
        if size > MAX_ATTACHMENT {
            return Err(StoreError::AttachmentTooLarge { size }.into());
        }
    }

    let pending = store.new_attachment(&channel)?;
    let size = match write_body(&pending, body).await {
        Ok(size) => size,
        Err(failure) => {
            // Nothing was recorded, so what is on disk is a file nothing can
            // name. It goes now rather than waiting for the sweep.
            let _ = tokio::fs::remove_file(&pending.path).await;
            return Err(failure);
        }
    };
    if size == 0 {
        let _ = tokio::fs::remove_file(&pending.path).await;
        return Err(StoreError::EmptyAttachment { filename }.into());
    }

    match store.record_attachment(&pending, &filename, size, &content_type) {
        Ok(attachment) => Ok((StatusCode::CREATED, Json(attachment))),
        Err(err) => {
            // The bytes landed and the row did not, so there is a file no id
            // will ever name and no sweep will ever find. It goes now.
            let _ = tokio::fs::remove_file(&pending.path).await;
            Err(err.into())
        }
    }
}

/// Writes the request body to the file the attachment was given, counting as
/// it goes, and stopping the moment there is more of it than an attachment may
/// be. Answers with how many bytes landed.
async fn write_body(pending: &PendingAttachment, body: Body) -> Result<u64, Failure> {
    use tokio::io::AsyncWriteExt;

    let file = |doing: &'static str| {
        let path = pending.path.clone();
        move |source| {
            Failure::from(StoreError::AttachmentFile {
                doing,
                path: path.clone(),
                source,
            })
        }
    };

    let mut out = tokio::fs::File::create(&pending.path)
        .await
        .map_err(file("create"))?;
    let mut written: u64 = 0;
    let mut body = body;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| {
            Failure::unreadable(
                StatusCode::BAD_REQUEST,
                format!("the upload stopped part-way through ({err})"),
            )
        })?;
        let Ok(chunk) = frame.into_data() else {
            // Trailers carry nothing this reads.
            continue;
        };
        written += chunk.len() as u64;
        if written > MAX_ATTACHMENT {
            return Err(StoreError::AttachmentTooLarge { size: written }.into());
        }
        out.write_all(&chunk).await.map_err(file("write"))?;
    }
    // The bytes are the whole of the attachment, so they are on the disk
    // before anything says the upload succeeded.
    out.flush().await.map_err(file("write"))?;
    out.sync_all().await.map_err(file("write"))?;
    Ok(written)
}

/// One attachment, as the file it was uploaded as.
async fn download_attachment(
    State(store): State<Arc<Store>>,
    Path((channel, id)): Path<(String, String)>,
) -> Result<Response, Failure> {
    let (attachment, path) = store.attachment(&channel, &id)?;
    // An attachment is capped at 25 MiB, so it is read whole rather than
    // streamed: a body that streams would need a body type nothing else here
    // has any use for.
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|source| StoreError::AttachmentFile {
            doing: "read",
            path,
            source,
        })?;

    // The filename was made safe on the way in, so it holds no quote to end
    // this value early.
    let disposition = format!("attachment; filename=\"{}\"", attachment.filename);
    Ok((
        [
            (header::CONTENT_TYPE, attachment.content_type),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response())
}

/// The name an upload gave its file, made safe to store and to write out
/// again.
fn filename_of(headers: &HeaderMap) -> Result<String, Failure> {
    let given = headers
        .get(FILENAME_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    crate::api::attachment_filename(given).ok_or_else(|| {
        StoreError::InvalidFilename {
            value: given.to_string(),
        }
        .into()
    })
}

/// What the upload says the file is, or [`DEFAULT_CONTENT_TYPE`] when it says
/// nothing, or says it in something that is not a header value.
fn content_type_of(headers: &HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_CONTENT_TYPE
                && value.chars().all(|c| c.is_ascii_graphic() || c == ' ')
        })
        .unwrap_or(DEFAULT_CONTENT_TYPE)
        .to_string()
}

/// How much a caller says it is about to send, when it says.
fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}

/// The longest content type the server will record. A media type is short;
/// anything longer is not one.
const MAX_CONTENT_TYPE: usize = 128;

/// A page of a transcript. This moves nothing, so a caller that wants its read
/// cursor advanced asks for that separately.
async fn list_messages(
    State(store): State<Arc<Store>>,
    Path(channel): Path<String>,
    ApiQuery(query): ApiQuery<MessageQuery>,
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
    ApiJson(body): ApiJson<CursorUpdate>,
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

impl Failure {
    /// A request the server could not read at all: malformed JSON, a query it
    /// cannot make sense of, a body larger than it will take. Said in saneha's
    /// words, on one line, because that is what the other end prints.
    fn unreadable(status: StatusCode, detail: String) -> Failure {
        let message = if status == StatusCode::PAYLOAD_TOO_LARGE {
            format!(
                "that request is larger than saneha will read: a message body is at most \
                 {MAX_BODY} bytes, and anything longer goes as an attachment"
            )
        } else {
            let detail = detail.lines().next().unwrap_or_default();
            format!("saneha could not read that request: {detail}")
        };
        Failure {
            status,
            message,
            retry: false,
        }
    }
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
            | StoreError::EmptyAttachment { .. }
            | StoreError::InvalidFilename { .. }
            | StoreError::AmbiguousRecipient { .. } => StatusCode::BAD_REQUEST,
            StoreError::NoSuchChannel(_)
            | StoreError::NoSuchParticipant { .. }
            | StoreError::NoSuchAttachment { .. }
            | StoreError::NotAParticipant { .. } => StatusCode::NOT_FOUND,
            StoreError::ChannelExists(_)
            | StoreError::ChannelClosed { .. }
            | StoreError::ParticipantChanged { .. }
            | StoreError::AttachmentBound { .. }
            | StoreError::SuffixExhausted { .. } => StatusCode::CONFLICT,
            StoreError::AttachmentTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            StoreError::MintExhausted(_) => StatusCode::SERVICE_UNAVAILABLE,
            StoreError::NewerSchema { .. }
            | StoreError::UnknownChannelState(_)
            | StoreError::UnknownMessageKind(_)
            | StoreError::NoAttachmentStore
            | StoreError::AttachmentFile { .. }
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
