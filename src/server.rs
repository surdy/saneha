//! The HTTP server behind `saneha serve`.
//!
//! There is no authentication by design (ADR-0003): the network the server
//! listens on is the trust boundary, so error text is written for whoever is
//! reading it rather than hidden.

use std::collections::HashMap;
use std::future::IntoFuture;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
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
use tokio::sync::{watch, Notify};

use crate::api::{
    ApiError, Attachment, Channel, ChannelDetail, ChannelList, ChannelState, CloseRequest, Closed,
    CursorUpdate, DeleteQuery, Deleted, JoinRequest, Joined, Left, Message, MessageList,
    MessageQuery, NewChannel, NewMessage, Participant, ParticipantList, WaitQuery, Waited,
    DEFAULT_CONTENT_TYPE, DEFAULT_MESSAGE_LIMIT, FILENAME_HEADER, MAX_ATTACHMENT, MAX_BODY,
    MAX_HOLD,
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

/// How long the server goes on draining after it has been asked to stop
/// before it stops anyway and says so.
///
/// A graceful shutdown waits for every request still in flight with no
/// deadline of its own, and `wait` is a request that is meant to be in flight
/// for an hour. Ending the held waits (which is what [`Serving::stop`] does)
/// is what makes the drain finish in milliseconds; this is the backstop for
/// anything else that will not let go, and it sits under the Quadlet unit's
/// `TimeoutStopSec=30`, so the process stops itself rather than being killed.
const DRAIN_LIMIT: Duration = Duration::from_secs(20);

/// Everything a request handler is given: the database, the waiters to wake
/// when a transcript changes, and the switch that ends every held wait.
pub struct Serving {
    store: Arc<Store>,
    waiters: Waiters,
    /// Flipped once, when the server has been asked to stop. Held as the
    /// sender so it outlives every receiver and a waiter's `changed()` only
    /// ever resolves because the server really is stopping.
    stopping: watch::Sender<bool>,
}

impl Serving {
    fn new(store: Arc<Store>) -> Serving {
        Serving {
            store,
            waiters: Waiters::default(),
            stopping: watch::channel(false).0,
        }
    }

    fn stopping(&self) -> watch::Receiver<bool> {
        self.stopping.subscribe()
    }

    /// Ends every wait stream being held open, at once.
    fn stop(&self) {
        let _ = self.stopping.send(true);
    }
}

/// Who is waiting on which channel.
///
/// One [`Notify`] per channel, made when the first waiter on that channel
/// settles down to wait. Only a channel that exists gets one: the wait handler
/// makes its first look before it registers, and that look is what refuses a
/// channel that is not there, so a made-up name in a URL leaves nothing
/// behind. What is left is one entry per channel that has ever been waited on,
/// dropped by [`Waiters::forget`] when the channel itself goes.
///
/// The wake is `notify_waiters`, which wakes every `Notified` made before the
/// call and remembers nothing afterwards. A waiter therefore makes its
/// `Notified` *before* it looks at the transcript, so a message written in
/// between is a wake it already holds rather than one it sleeps through.
#[derive(Default)]
struct Waiters {
    channels: Mutex<HashMap<String, Arc<Notify>>>,
    /// How many requests are being held open right now, across every channel:
    /// the per-participant waits, and the transcript polls a viewer follows a
    /// channel with. Reported by `GET /health` as `held_waits`.
    held: AtomicUsize,
}

impl Waiters {
    fn on(&self, channel: &str) -> Arc<Notify> {
        Arc::clone(self.channels().entry(channel.to_string()).or_default())
    }

    /// Registers this request as one of the waits being held, and answers with
    /// the notifier to sleep on and a guard that counts it.
    ///
    /// The count comes back down when the guard is dropped, which is every way
    /// out of a wait — including the one that is not a return at all: a caller
    /// that hangs up drops the whole future, and a decrement written into each
    /// return path would miss exactly that.
    fn hold(&self, channel: &str) -> Held<'_> {
        let notify = self.on(channel);
        self.held.fetch_add(1, Ordering::Relaxed);
        Held {
            waiters: self,
            notify,
        }
    }

    /// How many waits are being held open. It is a number for a person and for
    /// a test — "is the waiter really waiting yet" is otherwise something only
    /// a sleep can guess at — so it is read without ordering ceremony.
    fn held(&self) -> usize {
        self.held.load(Ordering::Relaxed)
    }

    /// Wakes everyone waiting on `channel`. Called after a write has
    /// committed, never before: a waiter woken by a transaction still open
    /// would look and find nothing.
    fn wake(&self, channel: &str) {
        if let Some(notify) = self.channels().get(channel) {
            notify.notify_waiters();
        }
    }

    /// Forgets a channel, because the channel itself is gone.
    ///
    /// [`delete_channel`] is the only caller, and it wakes before it forgets:
    /// the wake is what sends every held wait back to look, find the channel
    /// gone and say so, and the forget is what stops the map growing an entry
    /// per channel that has ever been deleted. The other way round would leave
    /// waiters holding a notifier nothing will ever ring, asleep until their
    /// hold elapsed.
    fn forget(&self, channel: &str) {
        self.channels().remove(channel);
    }

    fn channels(&self) -> MutexGuard<'_, HashMap<String, Arc<Notify>>> {
        // A poisoned lock still holds a usable map, for the same reason the
        // store's connection does.
        self.channels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One held wait, counted for as long as it is held.
struct Held<'a> {
    waiters: &'a Waiters,
    /// The notifier for the channel this wait is on.
    notify: Arc<Notify>,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.waiters.held.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Every route the server answers.
///
/// The state is filled in by [`run`] rather than here, because half of what a
/// handler is given only means anything once there is a shutdown to be told
/// about: a router built on the side would hold waits open that nothing could
/// ever end.
fn routes() -> Router<Arc<Serving>> {
    Router::new()
        // The viewer, and the same viewer under a channel's own path so that a
        // channel is a link somebody can send. The page reads the path and
        // opens that channel; there is nothing per-channel to render on the
        // server, and one page in the binary is the whole of it.
        .route("/", get(viewer))
        .route("/c/{channel}", get(viewer))
        .route("/health", get(health))
        .route("/channels", post(create_channel).get(list_channels))
        // One channel, and the counts a delete is confirmed against; and the
        // delete itself, which needs `?confirm=true` before it will do
        // anything.
        .route(
            "/channels/{channel}",
            get(channel_detail).delete(delete_channel),
        )
        // Closing is not an edit of the channel resource: it writes a system
        // message and wakes every waiter, so it is a verb of its own, with the
        // identity closing it in the body.
        .route("/channels/{channel}/close", post(close_channel))
        .route(
            "/channels/{channel}/participants",
            post(join).get(list_participants),
        )
        // A leave belongs to the participant leaving, which is why it hangs
        // off that participant's own path rather than the channel's.
        .route(
            "/channels/{channel}/participants/{identity}/leave",
            post(leave),
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
        // One request, held open until this participant has something unread.
        // It is a read like any other and moves no cursor: what a waiter is
        // told about is still there for its next `read`.
        .route(
            "/channels/{channel}/participants/{identity}/wait",
            get(wait),
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
///
/// Stopping is two things rather than one. The signal starts axum's graceful
/// drain, which waits for every request still in flight; and the same signal
/// ends every held wait stream, because a wait is in flight on purpose for as
/// long as an hour and a drain that waited for those would stall until the
/// supervisor lost patience and sent SIGKILL. [`DRAIN_LIMIT`] is the backstop
/// under all of it.
pub async fn run(listener: TcpListener, store: Arc<Store>) -> anyhow::Result<()> {
    // Once at the start, and then on the hour. A stop the server took part in
    // finishes the uploads in flight, so what the start finds is what a kill
    // left: a file part-written and never recorded, which the sweep's walk of
    // the directories is what catches.
    sweep(&store);
    let sweeper = tokio::spawn(sweep_hourly(Arc::clone(&store)));

    let serving = Arc::new(Serving::new(store));
    let stopping = serving.stopping();

    let stop = Arc::clone(&serving);
    let serve = axum::serve(listener, routes().with_state(serving))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            stop.stop();
        })
        .into_future();

    let mut serve = std::pin::pin!(serve);
    tokio::select! {
        served = &mut serve => {
            sweeper.abort();
            served?
        }
        () = drain_limit(stopping) => {
            sweeper.abort();
            say(&format!(
                "saneha stopped with requests still in flight after {} seconds",
                DRAIN_LIMIT.as_secs()
            ));
        }
    }
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

/// Removes the uploads nobody bound to a message and the files no row names,
/// once they are old enough that nobody is coming back for them. A sweep that
/// fails is worth saying out loud and nothing more: the next one is an hour
/// away, and a server that stopped serving over a file it could not delete
/// would be worse than the file.
fn sweep(store: &Store) {
    match store.sweep_unbound_attachments(UNBOUND_ATTACHMENT_TTL) {
        Ok(swept) if swept.is_empty() => {}
        Ok(swept) => say(&format!("swept up {swept}")),
        Err(err) => say(&format!("could not sweep unbound attachments: {err}")),
    }
}

/// Resolves [`DRAIN_LIMIT`] after the server has been asked to stop, and never
/// before: a server that is still serving must not be raced by its own
/// deadline.
async fn drain_limit(mut stopping: watch::Receiver<bool>) {
    while !*stopping.borrow_and_update() {
        if stopping.changed().await.is_err() {
            // Nothing left to be told by, so there is no shutdown to put a
            // limit on.
            std::future::pending::<()>().await;
        }
    }
    tokio::time::sleep(DRAIN_LIMIT).await;
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

/// The viewer: one page, built into the binary.
///
/// It is served here rather than from a directory because the whole of it is
/// one file with no build step and nothing to fetch: a deploy is the binary,
/// and a server on a LAN with no route to the internet still renders it.
pub const VIEWER: &str = include_str!("../web/index.html");

async fn viewer() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
        VIEWER,
    )
        .into_response()
}

/// Whether the server is up, and the one number about it that is not visible
/// from anywhere else: how many requests it is holding open right now, waits
/// and viewer polls together.
async fn health(State(serving): State<Arc<Serving>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "saneha",
        "held_waits": serving.waiters.held(),
    }))
}

async fn create_channel(
    State(serving): State<Arc<Serving>>,
    ApiJson(body): ApiJson<NewChannel>,
) -> Result<(StatusCode, Json<Channel>), Failure> {
    let channel = serving
        .store
        .create_channel(body.name.as_deref(), body.purpose.as_deref())?;
    Ok((StatusCode::CREATED, Json(channel)))
}

async fn list_channels(State(serving): State<Arc<Serving>>) -> Result<Json<ChannelList>, Failure> {
    let channels = serving.store.list_channels()?;
    Ok(Json(ChannelList { channels }))
}

/// One channel, with what it holds counted: the numbers `saneha delete` prints
/// before it is told to go ahead.
async fn channel_detail(
    State(serving): State<Arc<Serving>>,
    Path(channel): Path<String>,
) -> Result<Json<ChannelDetail>, Failure> {
    Ok(Json(serving.store.channel_detail(&channel)?))
}

/// Closes a channel: no more messages, no more joins, still readable.
///
/// The wake is the point of it. Every held wait is looking for two things, and
/// the channel ending is the second; a close that did not wake would leave
/// every waiter asleep until its hold elapsed, reading a channel that was over
/// before it started waiting.
async fn close_channel(
    State(serving): State<Arc<Serving>>,
    Path(channel): Path<String>,
    ApiJson(body): ApiJson<CloseRequest>,
) -> Result<Json<Closed>, Failure> {
    let closed = serving.store.close(&channel, &body.by)?;
    if closed.closed {
        // After the write has committed, so every waiter it sends back to look
        // sees the channel it is being told about.
        serving.waiters.wake(&channel);
    }
    Ok(Json(closed))
}

/// Removes a channel, its transcript, its participants and its attachments.
///
/// Four steps, in this order and no other. The row goes first, which cascades
/// away everything keyed to it; then the files that row id named, because
/// afterwards there is nothing left to name them by, and a failure there
/// leaves a directory nobody can reach for the sweep rather than a live
/// channel whose attachments have gone. Then the wake, which sends every held
/// wait back to look and find the channel gone, and only then the forget,
/// which drops the notifier: forgetting first would leave those waits holding
/// a notifier nothing would ever ring.
async fn delete_channel(
    State(serving): State<Arc<Serving>>,
    Path(channel): Path<String>,
    ApiQuery(query): ApiQuery<DeleteQuery>,
) -> Result<Json<Deleted>, Failure> {
    if !query.confirm {
        return Err(Failure::unconfirmed(&channel));
    }

    let (channel_id, removed) = serving.store.remove_channel(&channel)?;
    if let Err(err) = serving.store.remove_channel_files(channel_id) {
        // The rows are gone and the request succeeded; what is left is an
        // unreachable directory, which the hourly sweep removes. Worth a line
        // on the server's own output and nothing more.
        say(&format!(
            "could not remove the files of the deleted channel {channel:?}: {err}"
        ));
    }
    serving.waiters.wake(&channel);
    serving.waiters.forget(&channel);
    Ok(Json(removed))
}

/// Marks a participant away and writes the leave into the transcript.
///
/// A leave wakes, because it is a transcript event and a plain `wait` is
/// woken by anything it has not read. A `--mentions` wait is not woken by it,
/// and that is decided by [`crate::api::Message::wakes`] rather than here: the
/// wake goes out and the waiter that does not care goes back to sleep.
async fn leave(
    State(serving): State<Arc<Serving>>,
    Path((channel, identity)): Path<(String, String)>,
) -> Result<Json<Left>, Failure> {
    let left = serving.store.leave(&channel, &identity)?;
    if left.left {
        serving.waiters.wake(&channel);
    }
    Ok(Json(left))
}

/// A join, or a resume, or the grant of a suffixed identity: which one it was
/// is in the body rather than in the status, because all three succeeded.
async fn join(
    State(serving): State<Arc<Serving>>,
    Path(channel): Path<String>,
    ApiJson(body): ApiJson<JoinRequest>,
) -> Result<(StatusCode, Json<Joined>), Failure> {
    let joined = serving.store.join(&channel, &body)?;
    // A join writes a system message, so it is something to be woken for.
    serving.waiters.wake(&channel);
    let status = if joined.resumed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(joined)))
}

async fn list_participants(
    State(serving): State<Arc<Serving>>,
    Path(channel): Path<String>,
) -> Result<Json<ParticipantList>, Failure> {
    let participants = serving.store.list_participants(&channel)?;
    Ok(Json(ParticipantList { participants }))
}

async fn participant(
    State(serving): State<Arc<Serving>>,
    Path((channel, identity)): Path<(String, String)>,
) -> Result<Json<Participant>, Failure> {
    let participant = serving
        .store
        .participant(&channel, &identity)?
        .ok_or_else(|| {
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
    State(serving): State<Arc<Serving>>,
    Path(channel): Path<String>,
    ApiJson(body): ApiJson<NewMessage>,
) -> Result<(StatusCode, Json<Message>), Failure> {
    let message = serving.store.send(
        &channel,
        &body.from,
        &body.body,
        &body.to,
        &body.attachments,
    )?;
    // After the write has committed, so everyone woken by it can see it.
    serving.waiters.wake(&channel);
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
    State(serving): State<Arc<Serving>>,
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

    let pending = serving.store.new_attachment(&channel)?;
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

    match serving
        .store
        .record_attachment(&pending, &filename, size, &content_type)
    {
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
    State(serving): State<Arc<Serving>>,
    Path((channel, id)): Path<(String, String)>,
) -> Result<Response, Failure> {
    let (attachment, path) = serving.store.attachment(&channel, &id)?;
    // An attachment is capped at 25 MiB, so it is read whole rather than
    // streamed: a body that streams would need a body type nothing else here
    // has any use for.
    let bytes = tokio::fs::read(&path).await.map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            // The row is there and the bytes are not: a restore that took the
            // database without the files, or a file removed by hand. The
            // caller is told what is true, and not where on this server it
            // would have been.
            StoreError::AttachmentGone {
                channel: channel.clone(),
                id: attachment.id.clone(),
            }
        } else {
            StoreError::AttachmentFile {
                doing: "read",
                path,
                source,
            }
        }
    })?;

    Ok((
        [
            (header::CONTENT_TYPE, attachment.content_type),
            (
                header::CONTENT_DISPOSITION,
                disposition_of(&attachment.filename),
            ),
            // The stored type is whatever the uploader said, and the viewer
            // (issue #8) will serve this from its own origin: nothing here is
            // to be sniffed into something a browser will run, and every
            // attachment is a download rather than a page.
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response())
}

/// The `Content-Disposition` of a download: always an attachment, with the
/// name twice.
///
/// `filename` is the old form and holds ASCII, so a name in any other script
/// arrives there as underscores; `filename*` is RFC 8187 and holds the name
/// itself, percent-encoded. Everything that reads one of them prefers the
/// second when it is there, so both can be sent and the right one is used.
fn disposition_of(filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // The name was made safe on the way in, so it holds no quote to end this
    // value early.
    //
    // `filename*` comes first, and is the only one of the two whose value can
    // hold a `;` at all: a filename is allowed to contain one, and a reader
    // scanning for `filename*=` without minding quotes would otherwise find
    // one written inside the quoted `filename`. Such a reader is wrong, and
    // this order means it is not wrong here.
    format!(
        "attachment; filename*=UTF-8''{}; filename=\"{ascii}\"",
        crate::api::encode_filename(filename)
    )
}

/// The name an upload gave its file, decoded and then made safe to store and
/// to write out again.
///
/// The header is read as bytes rather than as a string: percent-encoding is
/// what a caller is asked to send, and a caller that sent the name as it was
/// written is still understood rather than told it sent nothing.
fn filename_of(headers: &HeaderMap) -> Result<String, Failure> {
    let given = headers
        .get(FILENAME_HEADER)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .unwrap_or_default();
    let given = crate::api::decode_filename(&given);
    crate::api::attachment_filename(&given)
        .ok_or_else(|| StoreError::InvalidFilename { value: given }.into())
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

/// One request, held open until this participant has something unread, the
/// channel closes, or `hold` seconds pass.
///
/// Three answers: `200` with what arrived and the state of the channel, `204`
/// when the hold elapsed with nothing to say, and `503` with `retry` when the
/// server is stopping. The caller asks again after a `204`, which is how a
/// hold short enough to survive a reverse proxy adds up to an hour of waiting.
///
/// The first look comes before anything is registered, because it is also what
/// says the channel exists and this identity is in it: a name nobody has ever
/// used must not leave a notifier behind on its way to a 404. After that the
/// loop is register, look, sleep, and the order is what makes it safe — a
/// `Notified` created before a `notify_waiters` is woken by it, so a message
/// written between the look and the sleep is a wake this request finds rather
/// than one it missed.
///
/// Nothing here holds the store's lock across an await: the look is one call
/// that takes the lock, answers, and gives it back, so a wait costs the rest
/// of the server nothing while it sits there.
async fn wait(
    State(serving): State<Arc<Serving>>,
    Path((channel, identity)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<WaitQuery>,
) -> Result<Response, Failure> {
    let hold = Duration::from_secs(query.hold.unwrap_or(MAX_HOLD).clamp(1, MAX_HOLD));
    let deadline = tokio::time::Instant::now() + hold;
    let mut stopping = serving.stopping();

    if *stopping.borrow_and_update() {
        return Err(Failure::stopping());
    }
    // Costs one look that the loop below would make anyway, and buys a
    // notifier map that only ever holds channels somebody may really be
    // woken on.
    if let Some(answer) = look(&serving, &channel, &identity, query.mentions)? {
        return Ok(answer);
    }
    // Registered, and counted for as long as this request is held: `/health`
    // reports the count, which is what makes "the wait is holding now"
    // something a caller can see rather than something a sleep guesses at.
    let held = serving.waiters.hold(&channel);

    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }

        // Made before the look that follows it. A `Notified` is woken by any
        // `notify_waiters` after the moment it was created, not after the
        // moment it is first polled, so a message written between this look
        // and the sleep below is a wake this request already holds.
        let woken = held.notify.notified();
        tokio::pin!(woken);

        if *stopping.borrow_and_update() {
            return Err(Failure::stopping());
        }
        if let Some(answer) = look(&serving, &channel, &identity, query.mentions)? {
            return Ok(answer);
        }

        tokio::select! {
            () = woken => {}
            () = tokio::time::sleep(left) => return Ok(StatusCode::NO_CONTENT.into_response()),
            _ = stopping.changed() => return Err(Failure::stopping()),
        }
    }
}

/// One look at what a waiting participant has not read: `Some` when that is
/// an answer to give it, `None` when there is nothing to say yet.
///
/// The lock is taken and given back inside this call, so a waiter never holds
/// it across an await.
fn look(
    serving: &Serving,
    channel: &str,
    identity: &str,
    mentions: bool,
) -> Result<Option<Response>, Failure> {
    let (messages, state) =
        serving
            .store
            .unread(channel, identity, mentions, DEFAULT_MESSAGE_LIMIT)?;

    // Nobody is woken by their own message. A send leaves the sender caught up
    // when it was caught up, so this only arises for a sender that was already
    // behind: its own message sits in that backlog, is printed with the rest
    // of it, and is not by itself a reason to have been woken. Without this
    // the skill loop — wait, read, reply, wait — would wake itself once per
    // reply and never idle.
    let for_me = messages.iter().any(|message| !message.written_by(identity));
    if !for_me && state == ChannelState::Open {
        return Ok(None);
    }
    Ok(Some(
        Json(Waited {
            messages,
            channel_state: state,
        })
        .into_response(),
    ))
}

/// A page of a transcript, and with `hold` a page the server will wait for.
///
/// This moves nothing, so a caller that wants its read cursor advanced asks
/// for that separately, and a held request moves nothing either: what the
/// viewer is shown is still unread for every participant it belongs to.
///
/// Without `hold` this answers at once, with an empty list when there is
/// nothing after `after`. With it, an empty page is held open on the same
/// notifier the per-participant wait uses, and ends in one of four ways: the
/// messages, when a send, a join, a leave or a close lands; `204`, when the
/// hold elapses and the caller asks again; `503`, when the server is
/// stopping; and `404`, when the channel is deleted underneath it, because the
/// wake sends it back to look and there is nothing left to look at.
///
/// The order inside the loop is the wait route's, for the reason given there:
/// the `Notified` is made before the look, so a message written between the
/// look and the sleep is a wake this request already holds rather than one it
/// sleeps through. The first look happens before anything is registered, so a
/// channel name nobody has ever used leaves no notifier behind on its way to
/// a `404`.
async fn list_messages(
    State(serving): State<Arc<Serving>>,
    Path(channel): Path<String>,
    ApiQuery(query): ApiQuery<MessageQuery>,
) -> Result<Response, Failure> {
    let after = query.after.unwrap_or(0);
    let limit = query.limit.unwrap_or(DEFAULT_MESSAGE_LIMIT);

    let Some(hold) = hold_of(query.hold) else {
        let messages = serving.store.messages(&channel, after, limit)?;
        return Ok(Json(MessageList { messages }).into_response());
    };

    let deadline = tokio::time::Instant::now() + hold;
    let mut stopping = serving.stopping();

    if *stopping.borrow_and_update() {
        return Err(Failure::stopping());
    }
    if let Some(answer) = page(&serving, &channel, after, limit)? {
        return Ok(answer);
    }
    // Counted alongside the per-participant waits: a viewer following a
    // channel is a request the server is holding open, and what `held_waits`
    // answers is how many of those there are.
    let held = serving.waiters.hold(&channel);
    // Whether a wake has already sent this request back to look. The first
    // pass has not looked because anything happened; every later one has.
    let mut woken_once = false;

    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }

        let woken = held.notify.notified();
        tokio::pin!(woken);

        if *stopping.borrow_and_update() {
            return Err(Failure::stopping());
        }
        if let Some(answer) = page(&serving, &channel, after, limit)? {
            return Ok(answer);
        }
        // Woken, and yet there is nothing after `after`: what changed was not a
        // message, and the only thing that changes without writing one is a
        // read cursor. The caller is told to come and look rather than held for
        // the rest of the minute, because a cursor is something the viewer
        // draws — "alpha has read to here" — and a rule that moves a minute
        // after the read did is a rule that is wrong for a minute.
        if woken_once {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }

        tokio::select! {
            () = woken => woken_once = true,
            () = tokio::time::sleep(left) => return Ok(StatusCode::NO_CONTENT.into_response()),
            _ = stopping.changed() => return Err(Failure::stopping()),
        }
    }
}

/// How long a transcript poll may be held open.
///
/// Absent is not held at all: a page that asks for what is there now is the
/// fetch this route has always been. `hold=0` is a caller asking for the
/// shortest hold there is rather than for none, so it is floored at a second;
/// anything above [`MAX_HOLD`] is the server's cap rather than the caller's
/// request, for the reason [`MAX_HOLD`] gives.
fn hold_of(asked: Option<u64>) -> Option<Duration> {
    asked.map(|seconds| Duration::from_secs(seconds.clamp(1, MAX_HOLD)))
}

/// One look at what a channel holds after `after`: `Some` when that is an
/// answer to give, `None` when the transcript has nothing newer yet.
///
/// The store's lock is taken and given back inside this call, so a held
/// request never holds it across an await.
fn page(
    serving: &Serving,
    channel: &str,
    after: i64,
    limit: usize,
) -> Result<Option<Response>, Failure> {
    let messages = serving.store.messages(channel, after, limit)?;
    if messages.is_empty() {
        return Ok(None);
    }
    Ok(Some(Json(MessageList { messages }).into_response()))
}

/// Moves a read cursor forward. A value below the one recorded is ignored
/// rather than refused, so a cursor never moves backwards however many readers
/// share an identity.
///
/// This wakes, which nothing else that writes no message does. A cursor is
/// something the viewer draws — "alpha has read to here" — and a `saneha read`
/// moves one without adding a line to the transcript, so without this the
/// viewer's rule would sit where it was until the next message happened to
/// land, up to a whole hold later. What a wake costs the waits is one look
/// each: a `wait` re-reads its unread, finds the same nothing, and goes back
/// to holding.
async fn set_read_cursor(
    State(serving): State<Arc<Serving>>,
    Path((channel, identity)): Path<(String, String)>,
    ApiJson(body): ApiJson<CursorUpdate>,
) -> Result<Json<Participant>, Failure> {
    let participant = serving
        .store
        .set_read_cursor(&channel, &identity, body.read_cursor)?;
    serving.waiters.wake(&channel);
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

    /// A delete that did not say it meant it. Nothing has been touched, and
    /// the words are the ones the subcommand prints, so a caller that reached
    /// the route by hand is told the same thing `saneha delete` would say.
    fn unconfirmed(channel: &str) -> Failure {
        Failure {
            status: StatusCode::BAD_REQUEST,
            message: crate::api::delete_needs_confirmation(channel),
            retry: false,
        }
    }

    /// A wait that was ended by the server stopping rather than by anything
    /// about the request. `retry` says so, and the caller waits again.
    fn stopping() -> Failure {
        Failure {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: crate::api::STOPPING.to_string(),
            retry: true,
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
            | StoreError::InvalidIdentity(_)
            | StoreError::AmbiguousRecipient { .. } => StatusCode::BAD_REQUEST,
            StoreError::NoSuchChannel(_)
            | StoreError::NoSuchParticipant { .. }
            | StoreError::NoSuchAttachment { .. }
            | StoreError::NotAParticipant { .. } => StatusCode::NOT_FOUND,
            // Gone rather than not found: this id named something, and the
            // difference is what tells a restore gone wrong from a typo.
            StoreError::AttachmentGone { .. } => StatusCode::GONE,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What a transcript poll is allowed to ask for. The cap is the server's
    /// and not the caller's, and asking for none of it is not the same request
    /// as asking for as little as there is.
    #[test]
    fn a_hold_is_what_the_server_allows() {
        assert_eq!(hold_of(None), None, "a fetch with no hold was held anyway");
        assert_eq!(hold_of(Some(0)), Some(Duration::from_secs(1)));
        assert_eq!(hold_of(Some(55)), Some(Duration::from_secs(55)));
        assert_eq!(hold_of(Some(MAX_HOLD)), Some(Duration::from_secs(MAX_HOLD)));
        assert_eq!(
            hold_of(Some(3600)),
            Some(Duration::from_secs(MAX_HOLD)),
            "an hour was not cut down to the cap"
        );
    }

    /// The order the wait handler works in, on its own: a `Notified` made
    /// before the wake is woken by it, even though nothing polled it in
    /// between. That is what lets the handler look at the transcript after
    /// making this and still not miss a message written while it looked.
    #[tokio::test]
    async fn a_notified_made_before_the_wake_is_woken_by_it() {
        let waiters = Waiters::default();
        let notify = waiters.on("brisk-otter");
        let woken = notify.notified();

        waiters.wake("brisk-otter");

        tokio::time::timeout(Duration::from_secs(1), woken)
            .await
            .expect("the waiter was not woken");
    }

    /// And a channel that is forgotten takes its waiters' notifier with it,
    /// which is what `saneha delete` will need (#6).
    #[tokio::test]
    async fn a_forgotten_channel_keeps_nothing() {
        let waiters = Waiters::default();
        let notify = waiters.on("brisk-otter");
        waiters.forget("brisk-otter");

        // The wake goes nowhere, because the map no longer holds this.
        let woken = notify.notified();
        waiters.wake("brisk-otter");
        tokio::time::timeout(Duration::from_millis(100), woken)
            .await
            .expect_err("a forgotten channel still woke its waiters");
        assert!(waiters.channels().is_empty());
    }

    /// A wake on one channel is not a wake on another, and a wake on a channel
    /// nobody is waiting on is not an error.
    #[tokio::test]
    async fn a_wake_reaches_one_channel_only() {
        let waiters = Waiters::default();
        let notify = waiters.on("brisk-otter");
        let woken = notify.notified();

        waiters.wake("keen-heron");
        waiters.wake("nobody-is-here");

        tokio::time::timeout(Duration::from_millis(100), woken)
            .await
            .expect_err("a wake on another channel woke this waiter");
    }

    /// The drain backstop is a deadline on stopping, not on serving: until the
    /// server is asked to stop it never resolves.
    #[tokio::test]
    async fn the_drain_limit_waits_for_the_shutdown_before_it_starts() {
        let serving = Arc::new(Serving::new(Arc::new(
            Store::open_in_memory().expect("a database"),
        )));
        let limit = drain_limit(serving.stopping());
        tokio::pin!(limit);

        tokio::time::timeout(Duration::from_millis(100), &mut limit)
            .await
            .expect_err("the drain limit started before the server was asked to stop");

        // After the shutdown it is a deadline rather than a stop: the drain
        // gets its DRAIN_LIMIT before this says anything.
        serving.stop();
        tokio::time::timeout(Duration::from_millis(100), &mut limit)
            .await
            .expect_err("the drain limit left no time to drain in");
    }
}
