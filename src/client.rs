//! Talking to a running server.
//!
//! Every subcommand except `serve` goes through here: it finds the server in
//! `SANEHA_URL`, makes one request, and turns anything that goes wrong into a
//! single line a person can act on.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use ureq::http::Response;
use ureq::{Agent, Body, SendBody};

use crate::api::{
    attachment_filename, attachment_is_empty, attachment_too_large, decode_filename,
    encode_filename, ApiError, Attachment, Channel, ChannelDetail, ChannelList, ChannelState,
    CloseRequest, Closed, CursorUpdate, Deleted, JoinRequest, Joined, Left, Message, MessageList,
    NewMessage, Participant, ParticipantList, Waited, DEFAULT_CONTENT_TYPE, DEFAULT_MESSAGE_LIMIT,
    FILENAME_HEADER, MAX_ATTACHMENT,
};

/// The environment variable that points every subcommand at the server.
pub const URL_ENV: &str = "SANEHA_URL";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What an attachment gets instead. Thirty seconds is plenty for a message and
/// nothing at all for 25 MiB over a home connection.
const ATTACHMENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

const MAX_ERROR_BODY: usize = 200;

/// How many more times a request a signal interrupted is made before the
/// interruption is reported as a server that cannot be reached.
const INTERRUPTED_RETRIES: usize = 5;

/// How much longer than the hold it asked for a wait request will sit there
/// before it decides the answer is not coming. The server answers at the hold
/// whatever happens, so anything past this is the connection and not the wait.
const WAIT_MARGIN: Duration = Duration::from_secs(10);

/// What a join came back with: the identity granted, or the server saying the
/// participant this join reasoned about is not the participant it found, so
/// the caller should look again before asking again.
#[derive(Debug)]
pub enum JoinAnswer {
    Granted(Box<Joined>),
    Stale(String),
}

/// How one held wait request ended.
#[derive(Debug)]
pub enum Waiting {
    /// Something unread, and the state of the channel it is in: `Closed` means
    /// this is the end of the transcript and nothing more is coming.
    Arrived {
        messages: Vec<Message>,
        channel_state: ChannelState,
    },
    /// The hold elapsed with nothing to say. Ask again.
    Nothing,
    /// The channel, or this participant in it, is not there any more. The wait
    /// began by checking both, so this is a `saneha delete` that landed while
    /// the request was held open: there is nothing left to wait on and nothing
    /// to be gained by asking again.
    Gone,
    /// Nothing is wrong with the request, but the server could not answer it
    /// now: it is stopping, or it is not there. Ask again shortly, in these
    /// words if it never comes back.
    Later(String),
}

/// The server as seen from a subcommand.
pub struct Remote {
    base: String,
    agent: Agent,
}

impl Remote {
    /// Reads `SANEHA_URL`.
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var(URL_ENV).ok().unwrap_or_default();
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(anyhow!(
                "{URL_ENV} is not set; point it at your saneha server, \
                 for example: export {URL_ENV}=http://localhost:7343"
            ));
        }
        Self::at(raw)
    }

    /// Talks to the server at `url`.
    pub fn at(url: &str) -> Result<Self> {
        let url = url.trim().trim_end_matches('/');
        // A bare host:port is what people type; treat it as plain HTTP rather
        // than failing on a missing scheme.
        let base = if url.contains("://") {
            url.to_string()
        } else {
            format!("http://{url}")
        };
        let config = Agent::config_builder()
            // Read the server's own `{"error": "..."}` instead of turning a
            // status code into an error before the body is seen.
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build();
        Ok(Remote {
            base,
            agent: Agent::new_with_config(config),
        })
    }

    /// The server address this is pointed at.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Creates a channel. A `name` of `None` asks the server to mint one.
    pub fn create_channel(&self, name: Option<&str>, purpose: Option<&str>) -> Result<Channel> {
        let body = crate::api::NewChannel {
            name: name.map(str::to_string),
            purpose: purpose.map(str::to_string),
        };
        let response = self.check(
            retrying(|| self.agent.post(self.url("/channels")).send_json(&body))
                .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "channel")
    }

    /// Every channel the server knows about.
    /// Every channel. `as_identity` asks for each channel to carry that
    /// identity's read cursor as well, which is what an unread count is worked
    /// out from; it moves nothing.
    pub fn list_channels(&self, as_identity: Option<&str>) -> Result<Vec<Channel>> {
        let response = self.check(
            retrying(|| match as_identity {
                Some(identity) => self
                    .agent
                    .get(self.url("/channels"))
                    .query("as", identity)
                    .call(),
                None => self.agent.get(self.url("/channels")).call(),
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        let list: ChannelList = read_json(response, "channel list")?;
        Ok(list.channels)
    }

    /// One channel and what it holds: the counts `delete` prints before it is
    /// told to go ahead.
    pub fn channel_detail(&self, channel: &str) -> Result<ChannelDetail> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .get(self.url(&format!("/channels/{channel}")))
                    .call()
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "channel")
    }

    /// Closes a channel, as `by`. Closing a closed channel is not an error;
    /// what comes back says whether this was the call that closed it.
    pub fn close_channel(&self, channel: &str, by: &str) -> Result<Closed> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .post(self.url(&format!("/channels/{channel}/close")))
                    .send_json(&CloseRequest { by: by.to_string() })
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "close")
    }

    /// Removes a channel and everything in it. The confirmation is in the URL
    /// because a `DELETE` body is a thing that gets dropped on the way, and a
    /// deletion must not be decided by something that went missing.
    pub fn delete_channel(&self, channel: &str) -> Result<Deleted> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .delete(self.url(&format!("/channels/{channel}?confirm=true")))
                    .call()
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "deletion")
    }

    /// Marks a participant away. Leaving twice is not an error; what comes
    /// back says whether this was the call that did it.
    pub fn leave(&self, channel: &str, identity: &str) -> Result<Left> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .post(self.url(&format!(
                        "/channels/{channel}/participants/{identity}/leave"
                    )))
                    .send_empty()
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "leave")
    }

    /// Joins a channel and returns the identity the server granted, or
    /// `Stale` when the participant the request was reasoned about changed
    /// hands in between and the caller should look again.
    pub fn join(&self, channel: &str, request: &JoinRequest) -> Result<JoinAnswer> {
        let response = retrying(|| {
            self.agent
                .post(self.url(&format!("/channels/{channel}/participants")))
                .send_json(request)
        })
        .map_err(|err| self.unreachable(&err))?;
        if response.status().is_success() {
            return Ok(JoinAnswer::Granted(Box::new(read_json(response, "join")?)));
        }
        let (message, retry) = server_message(response);
        if retry {
            Ok(JoinAnswer::Stale(message))
        } else {
            Err(anyhow!(message))
        }
    }

    /// Everyone who has joined a channel.
    pub fn list_participants(&self, channel: &str) -> Result<Vec<Participant>> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .get(self.url(&format!("/channels/{channel}/participants")))
                    .call()
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        let list: ParticipantList = read_json(response, "participant list")?;
        Ok(list.participants)
    }

    /// One participant by identity, or `None` if nobody holds that identity
    /// here. An unknown channel reads as `None` too: the join that follows is
    /// what reports it, in the server's own words.
    pub fn participant(&self, channel: &str, identity: &str) -> Result<Option<Participant>> {
        let response = retrying(|| {
            self.agent
                .get(self.url(&format!("/channels/{channel}/participants/{identity}")))
                .call()
        })
        .map_err(|err| self.unreachable(&err))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        let response = self.check(response)?;
        read_json(response, "participant").map(Some)
    }

    /// Writes one message. The server works out who it is addressed to and
    /// refuses the whole thing if a mention names nobody, so what comes back
    /// is the message as the transcript now holds it.
    ///
    /// This goes through `retrying` because of the send key the message
    /// carries: a signal can land on the read of the answer to a send the
    /// server has already written down, and the attempt that follows says
    /// which message it is, so the server answers with that one rather than
    /// writing a second. A `NewMessage` with no `key` is written every time it
    /// arrives, so an interruption on one of those is a message in the
    /// transcript twice — which is why the CLI mints one for every send
    /// (issue #38).
    ///
    /// The answer is read inside the retry too, and not after it. The read of
    /// the body is where the interruption that started all this actually
    /// lands: the request is out, the server has written the message, and the
    /// signal arrives on the way back. Reading it outside would leave exactly
    /// that case failing, and a person rerunning `saneha send` after it would
    /// mint a new key and write the message twice.
    pub fn send_message(&self, channel: &str, message: &NewMessage) -> Result<Message> {
        let (status, body) = retrying(|| {
            let mut response = self
                .agent
                .post(self.url(&format!("/channels/{channel}/messages")))
                .send_json(message)?;
            let status = response.status();
            let body = response.body_mut().read_to_vec()?;
            Ok((status, body))
        })
        .map_err(|err| self.unreachable(&err))?;

        if !status.is_success() {
            return Err(anyhow!(refusal(status, &String::from_utf8_lossy(&body)).0));
        }
        parse_json(&body, "message")
    }

    /// One page of a transcript: at most `limit` messages after `after`.
    pub fn messages(&self, channel: &str, after: i64, limit: usize) -> Result<Vec<Message>> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .get(self.url(&format!(
                        "/channels/{channel}/messages?after={after}&limit={limit}"
                    )))
                    .call()
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        let list: MessageList = read_json(response, "message list")?;
        Ok(list.messages)
    }

    /// Every message after `after`, however many pages that takes. A backlog
    /// longer than one page is still read whole, and reading it moves nothing:
    /// the cursor is advanced by its own request, if at all.
    pub fn messages_after(&self, channel: &str, after: i64) -> Result<Vec<Message>> {
        let mut all: Vec<Message> = Vec::new();
        let mut after = after;
        loop {
            let page = self.messages(channel, after, DEFAULT_MESSAGE_LIMIT)?;
            let full = page.len() == DEFAULT_MESSAGE_LIMIT;
            if let Some(last) = page.last() {
                after = last.id;
            }
            all.extend(page);
            if !full {
                return Ok(all);
            }
        }
    }

    /// Holds one request open on the server until this participant has
    /// something unread, the channel closes, or `hold` seconds pass.
    ///
    /// The hold is the server's business and this only asks for it, so the
    /// transport is given the hold plus a margin rather than the agent's usual
    /// timeout: a wait is a request that is meant to be slow. Everything that
    /// is worth asking again for — the server stopping, the server not being
    /// there at all — comes back as `Later` with the words to print if it
    /// never does come back, because the caller is the one holding the
    /// stopwatch.
    pub fn wait(
        &self,
        channel: &str,
        identity: &str,
        mentions: bool,
        hold: u64,
    ) -> Result<Waiting> {
        let url = self.url(&format!(
            "/channels/{channel}/participants/{identity}/wait?mentions={mentions}&hold={hold}"
        ));
        let response = match retrying(|| {
            self.agent
                .get(url.as_str())
                .config()
                .timeout_global(Some(Duration::from_secs(hold) + WAIT_MARGIN))
                .build()
                .call()
        }) {
            Ok(response) => response,
            Err(err) => {
                let unreachable = self.unreachable(&err);
                return match err {
                    // A URL that is not a URL will not become one by asking
                    // again; anything else is the server being away.
                    ureq::Error::BadUri(_) => Err(unreachable),
                    _ => Ok(Waiting::Later(unreachable.to_string())),
                };
            }
        };

        if response.status() == 204 {
            return Ok(Waiting::Nothing);
        }
        // The channel and this participant were both there when the wait
        // started, so a 404 now is a delete that landed under it.
        if response.status() == 404 {
            return Ok(Waiting::Gone);
        }
        if response.status().is_success() {
            return Ok(match read_json::<Waited>(response, "wait") {
                Ok(waited) => Waiting::Arrived {
                    messages: waited.messages,
                    channel_state: waited.channel_state,
                },
                // A body that stops in the middle is the connection going
                // away, and nothing has been printed or advanced yet, so it is
                // worth asking again rather than failing outright. A server
                // that is genuinely speaking a shape this version cannot read
                // says so too, in the same words, once the asking runs out.
                Err(err) => Waiting::Later(err.to_string()),
            });
        }
        let (message, retry) = server_message(response);
        if retry {
            Ok(Waiting::Later(message))
        } else {
            Err(anyhow!(message))
        }
    }

    /// Moves a participant's read cursor, and returns the participant as it
    /// now stands. The server holds it where it is if this is behind.
    pub fn set_read_cursor(
        &self,
        channel: &str,
        identity: &str,
        read_cursor: i64,
    ) -> Result<Participant> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .post(self.url(&format!(
                        "/channels/{channel}/participants/{identity}/cursor"
                    )))
                    .send_json(&CursorUpdate { read_cursor })
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "participant")
    }

    /// Uploads one file to a channel, and answers with the attachment the
    /// server minted. Nothing carries it until a send names its id; an upload
    /// whose send never comes is swept up by the server within the hour.
    ///
    /// The file is streamed rather than read into memory, and its size is
    /// checked here first: 25 MiB is not worth sending to be refused, and the
    /// cap is the same number on both sides.
    pub fn upload_attachment(&self, channel: &str, file: &Path) -> Result<Attachment> {
        let shown = file.display();
        let size = std::fs::metadata(file)
            .with_context(|| format!("could not read {shown}"))?
            .len();
        let filename = file
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(attachment_filename)
            .ok_or_else(|| anyhow!("{shown} has no filename saneha can attach it under"))?;
        if size == 0 {
            return Err(anyhow!(attachment_is_empty(&filename)));
        }
        if size > MAX_ATTACHMENT {
            return Err(anyhow!("{shown}: {}", attachment_too_large(size)));
        }

        let mut reading =
            std::fs::File::open(file).with_context(|| format!("could not open {shown}"))?;
        // Made once, and not through `retrying`: an upload the server has
        // already stored leaves a second, unbound attachment behind if it is
        // made again, and the body is a file being streamed rather than
        // something this can hand over twice.
        let response = self.check(
            self.agent
                .post(self.url(&format!("/channels/{channel}/attachments")))
                .config()
                .timeout_global(Some(ATTACHMENT_TIMEOUT))
                .build()
                // Percent-encoded, because a header value carries ASCII and a
                // filename is written in whatever language it was named in.
                .header(FILENAME_HEADER, encode_filename(&filename))
                .header("content-type", content_type_of(&filename))
                // The size is known, so the request is length-delimited and
                // the server can refuse an oversize one before reading it.
                .header("content-length", size.to_string())
                .send(SendBody::from_reader(&mut reading))
                .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "attachment")
    }

    /// Fetches one attachment by id. What comes back is the name it was stored
    /// under and the bytes still to be read, so the caller can decide where
    /// they go before any of them arrive.
    pub fn fetch_attachment(&self, channel: &str, id: &str) -> Result<Fetched> {
        let response = self.check(
            retrying(|| {
                self.agent
                    .get(self.url(&format!("/channels/{channel}/attachments/{id}")))
                    .config()
                    .timeout_global(Some(ATTACHMENT_TIMEOUT))
                    .build()
                    .call()
            })
            .map_err(|err| self.unreachable(&err))?,
        )?;

        // The server sanitises what it stores, and this sanitises what it
        // writes: a name that arrived over the network becomes a path here.
        let filename = filename_from(response.headers())
            .and_then(|name| attachment_filename(&name))
            .unwrap_or_else(|| id.to_string());
        let body = response.into_body();
        Ok(Fetched {
            filename,
            // One past the cap: the limit is what a body may not exceed, and
            // an attachment of exactly 25 MiB is one the server stored and
            // must therefore be able to hand back.
            reader: Box::new(body.into_with_config().limit(MAX_ATTACHMENT + 1).reader()),
        })
    }

    fn check(&self, response: Response<Body>) -> Result<Response<Body>> {
        if response.status().is_success() {
            return Ok(response);
        }
        Err(anyhow!(server_message(response).0))
    }

    fn unreachable(&self, err: &ureq::Error) -> anyhow::Error {
        match err {
            // A bad SANEHA_URL is worth saying out loud; everything else is
            // the server not being there.
            ureq::Error::BadUri(detail) => anyhow!(
                "{} is not a usable server address: {detail}",
                self.base_url()
            ),
            other => anyhow!(
                "cannot reach the saneha server at {} ({})",
                self.base,
                describe(other)
            ),
        }
    }
}

/// An attachment coming down the wire: the name it was stored under, and the
/// bytes still to be read.
pub struct Fetched {
    /// The stored filename, made safe to use as a path on this machine.
    pub filename: String,
    reader: Box<dyn Read>,
}

impl Fetched {
    /// Reads the attachment into `out`, and answers with how many bytes went.
    pub fn write_to(mut self, out: &mut impl Write) -> Result<u64> {
        let copied = std::io::copy(&mut self.reader, out)
            .context("the attachment stopped part-way through")?;
        out.flush().context("could not write the attachment")?;
        Ok(copied)
    }
}

/// The filename out of a `Content-Disposition`, before it is made safe.
///
/// `filename*` first, because that is the one that can hold a name in any
/// script (RFC 8187, percent-encoded UTF-8); `filename` is the fallback, and
/// what an older saneha sends. The header is read as bytes, so a server that
/// put the name in it as it was written is understood too.
fn filename_from(headers: &ureq::http::HeaderMap) -> Option<String> {
    let disposition =
        String::from_utf8_lossy(headers.get("content-disposition")?.as_bytes()).into_owned();
    let parameters = parameters_of(&disposition);

    if let Some(value) = parameter(&parameters, "filename*") {
        // `UTF-8''name`: the charset, the language that is always empty here,
        // and then the name.
        let encoded = value.rsplit('\'').next().unwrap_or_default();
        let decoded = decode_filename(encoded);
        if !decoded.trim().is_empty() {
            return Some(decoded);
        }
    }

    let value = parameter(&parameters, "filename")?;
    let name = match value.strip_prefix('"') {
        Some(quoted) => quoted.strip_suffix('"').unwrap_or(quoted),
        None => value,
    };
    Some(name.to_string())
}

/// A header value split on the semicolons that separate its parameters, and
/// not on the ones written inside a quoted value.
///
/// A filename may contain a semicolon, and a name such as
/// `a; filename*=UTF-8''elsewhere.sh` is a name and not a second parameter.
/// Anything that scans a `Content-Disposition` without minding quotes can be
/// made to read a filename out of a filename.
fn parameters_of(disposition: &str) -> Vec<&str> {
    let mut parameters = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (at, character) in disposition.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parameters.push(disposition[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    parameters.push(disposition[start..].trim());
    parameters
}

/// The value of one parameter, by name, case-insensitively as a header
/// parameter is written.
fn parameter<'a>(parameters: &[&'a str], name: &str) -> Option<&'a str> {
    parameters.iter().find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

/// What a file is, as far as its name says. A short table rather than a
/// dependency: everything not in it is bytes, which is what an attachment is
/// to saneha anyway.
fn content_type_of(filename: &str) -> &'static str {
    let extension = filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" | "log" | "diff" | "patch" | "rs" | "toml" | "yaml" | "yml" | "sql" => "text/plain",
        "json" => "application/json",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        _ => DEFAULT_CONTENT_TYPE,
    }
}

fn read_json<T: serde::de::DeserializeOwned>(
    mut response: Response<Body>,
    what: &str,
) -> Result<T> {
    response.body_mut().read_json::<T>().map_err(|err| {
        anyhow!("the saneha server sent a {what} this version does not understand ({err})")
    })
}

/// The same, for a caller holding the bytes rather than the response: a send
/// reads its answer inside the retry, so by the time anything is parsed the
/// response is long since finished with.
fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(body).map_err(|err| {
        anyhow!("the saneha server sent a {what} this version does not understand ({err})")
    })
}

/// The server's own words when it sends them, a readable fallback when it does
/// not, and whether the server said the request is worth making again.
fn server_message(mut response: Response<Body>) -> (String, bool) {
    let status = response.status();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    refusal(status, &body)
}

/// The same, for a caller that has already read the body.
fn refusal(status: ureq::http::StatusCode, body: &str) -> (String, bool) {
    if let Ok(api_error) = serde_json::from_str::<ApiError>(body) {
        return (api_error.error, api_error.retry);
    }
    let body = body.trim();
    if body.is_empty() {
        return (
            format!("the saneha server refused the request ({status})"),
            false,
        );
    }
    let mut summary: String = body.lines().next().unwrap_or_default().to_string();
    if summary.chars().count() > MAX_ERROR_BODY {
        summary = summary.chars().take(MAX_ERROR_BODY).collect::<String>() + "...";
    }
    (
        format!("the saneha server refused the request ({status}): {summary}"),
        false,
    )
}

/// Makes a request, and makes it again when a signal interrupted it.
///
/// A connect, a send, or a read that a signal lands on comes back as `EINTR`,
/// which ureq does not retry on its own: without this a stray `SIGCHLD` on a
/// busy machine reads as a saneha server that is not there. There is nothing
/// wrong with such a request, so it goes again straight away — a bounded
/// number of times, so a machine being signalled steadily still ends in an
/// answer rather than a loop. An interrupted attempt fails at once rather than
/// sitting out its timeout, so the whole budget still fits inside the deadline
/// the caller set for one request.
///
/// **Only a request that is harmless to repeat goes through here.** An
/// interruption is not proof that nothing reached the server: it can land on
/// the read of the answer to a request the server has already carried out. So
/// this is for the reads, and for the writes that land in the same place
/// however many times they are made — a join, a leave, a close, a cursor that
/// only moves forward, a delete whose second answer the caller already reads
/// as gone. A send is one of those now: it carries a key the sender minted, so
/// a repeat of it is the same message and the server answers with the one it
/// already wrote. An upload is not: it has no such key, so a second one leaves
/// a second, unbound attachment behind, and `upload_attachment` is made once
/// and says so at its call site.
///
/// Nothing but an interruption is retried: every other error is the server's
/// answer or a real failure to reach it, and is reported in the same words as
/// before.
fn retrying<T>(mut attempt: impl FnMut() -> Result<T, ureq::Error>) -> Result<T, ureq::Error> {
    for _ in 0..INTERRUPTED_RETRIES {
        match attempt() {
            Err(err) if interrupted(&err) => continue,
            outcome => return outcome,
        }
    }
    attempt()
}

/// Whether this is a signal arriving mid-syscall and nothing else.
fn interrupted(err: &ureq::Error) -> bool {
    matches!(err, ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::Interrupted)
}

/// One line saying why the request never got an answer. ureq's own Display
/// prefixes the transport kind, which is noise next to the address we name.
fn describe(err: &ureq::Error) -> String {
    match err {
        ureq::Error::Io(io) => io.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Content-Disposition` as the server writes one, as a header map.
    fn disposition(value: &str) -> ureq::http::HeaderMap {
        let mut headers = ureq::http::HeaderMap::new();
        headers.insert(
            "content-disposition",
            ureq::http::HeaderValue::from_str(value).expect("a header value"),
        );
        headers
    }

    #[test]
    fn a_download_takes_the_name_that_can_hold_any_script() {
        // Both forms, which is what saneha sends: the encoded one wins.
        let both =
            disposition("attachment; filename=\"r_sum_.md\"; filename*=UTF-8''r%C3%A9sum%C3%A9.md");
        assert_eq!(filename_from(&both).as_deref(), Some("résumé.md"));

        // The old form alone, which is what an older saneha sends.
        let plain = disposition("attachment; filename=\"handoff.md\"");
        assert_eq!(filename_from(&plain).as_deref(), Some("handoff.md"));

        // An encoded form that decodes to nothing falls back rather than
        // taking the name away.
        let empty = disposition("attachment; filename=\"handoff.md\"; filename*=UTF-8''");
        assert_eq!(filename_from(&empty).as_deref(), Some("handoff.md"));

        // Nothing to take a name from at all.
        assert_eq!(filename_from(&ureq::http::HeaderMap::new()), None);
    }

    #[test]
    fn a_name_cannot_write_a_second_name_inside_itself() {
        // A filename may hold a semicolon, so a name written to look like the
        // rest of the header is still one name. Scanning for `filename*=`
        // without minding the quotes would take the file somewhere else
        // entirely.
        let smuggled = disposition(
            "attachment; filename*=UTF-8''a%3B%20filename%2A%3DUTF-8%27%27elsewhere.sh; \
             filename=\"a; filename*=UTF-8''elsewhere.sh\"",
        );
        assert_eq!(
            filename_from(&smuggled).as_deref(),
            Some("a; filename*=UTF-8''elsewhere.sh")
        );

        // And the same when only the old form is there to read.
        let plain = disposition("attachment; filename=\"a; filename*=UTF-8''elsewhere.sh\"");
        assert_eq!(
            filename_from(&plain).as_deref(),
            Some("a; filename*=UTF-8''elsewhere.sh")
        );
    }

    /// A request that fails with `kind` `failures` times over and then
    /// answers with the number of attempts it took.
    fn flaky(
        failures: usize,
        kind: std::io::ErrorKind,
    ) -> impl FnMut() -> Result<usize, ureq::Error> {
        let mut attempts = 0;
        move || {
            attempts += 1;
            if attempts <= failures {
                Err(ureq::Error::Io(std::io::Error::new(kind, "under test")))
            } else {
                Ok(attempts)
            }
        }
    }

    #[test]
    fn a_signal_during_a_request_is_not_a_server_that_is_not_there() {
        use std::io::ErrorKind::Interrupted;

        // A request interrupted on the way out goes again, and the answer is
        // the one the server gave rather than the interruption.
        assert_eq!(retrying(flaky(0, Interrupted)).expect("first try"), 1);
        assert_eq!(retrying(flaky(1, Interrupted)).expect("second try"), 2);
        assert_eq!(
            retrying(flaky(INTERRUPTED_RETRIES, Interrupted)).expect("the last try in the budget"),
            INTERRUPTED_RETRIES + 1
        );
    }

    #[test]
    fn asking_again_runs_out() {
        // A machine being signalled steadily ends in the error of the day
        // rather than in a loop, and stops after the bound.
        let mut attempts = 0;
        let err = retrying(|| -> Result<usize, ureq::Error> {
            attempts += 1;
            Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Interrupted system call (os error 4)",
            )))
        })
        .expect_err("the budget to run out");
        assert_eq!(attempts, INTERRUPTED_RETRIES + 1);
        assert!(interrupted(&err));

        let remote = Remote::at("127.0.0.1:41905").expect("remote");
        assert_eq!(
            remote.unreachable(&err).to_string(),
            "cannot reach the saneha server at http://127.0.0.1:41905 \
             (Interrupted system call (os error 4))"
        );
    }

    #[test]
    fn nothing_but_a_signal_is_asked_again() {
        // A refused connection is the server not being there, which asking
        // again will not change: it is reported the first time, unchanged.
        let refused = retrying(flaky(1, std::io::ErrorKind::ConnectionRefused))
            .expect_err("a refusal to stand");
        assert!(!interrupted(&refused));

        let remote = Remote::at("127.0.0.1:41905").expect("remote");
        assert!(remote
            .unreachable(&refused)
            .to_string()
            .starts_with("cannot reach the saneha server at http://127.0.0.1:41905 ("));

        // Nor is an error that never touched the transport at all.
        let mut attempts = 0;
        let bad = retrying(|| -> Result<usize, ureq::Error> {
            attempts += 1;
            Err(ureq::Error::BadUri("not a url".to_string()))
        })
        .expect_err("a bad address to stand");
        assert_eq!(attempts, 1);
        assert!(!interrupted(&bad));
    }

    /// The text of one method of `Remote`, out of this file's own source.
    fn body_of(method: &str) -> &'static str {
        const SOURCE: &str = include_str!("client.rs");
        let from = SOURCE.find(method).expect("the method to be in this file");
        let body = &SOURCE[from..];
        let to = body.find("\n    }\n").expect("the method to end");
        &body[..to]
    }

    #[test]
    fn what_cannot_be_repeated_safely_is_made_once() {
        // An interruption is not proof that nothing reached the server: it can
        // land on the read of the answer to a POST the server has already
        // carried out. An upload has nothing on it that says which upload it
        // is, so making that one again leaves a second attachment behind and
        // it does not go through `retrying`. The exclusion is the absence of a
        // call, which is why this is asserted over the source.
        assert!(
            !body_of("pub fn upload_attachment").contains("retrying("),
            "an upload must not be made again: a second one is a second file"
        );

        // And the rule says nothing unless the helper is used where repeating
        // a request only asks the same question twice. A send is in that list
        // now: it carries the key its sender minted, so the server answers a
        // repeat with the message it already wrote (issue #38).
        for repeatable in [
            "pub fn messages",
            "pub fn join",
            "pub fn set_read_cursor",
            "pub fn send_message",
        ] {
            assert!(
                body_of(repeatable).contains("retrying("),
                "{repeatable} is safe to make again and should be"
            );
        }
    }

    #[test]
    fn a_send_reads_its_answer_inside_the_retry() {
        // The interruption #38 is about lands on the read of the answer to a
        // send the server has already carried out, so the read has to be
        // inside the closure: a body read after `retrying` has returned is one
        // more place an `EINTR` ends the send, and the rerun that follows
        // mints another key and writes the message twice. `read_json` takes
        // the response, so it can only be called outside; the shape that is
        // wanted reads the body to bytes in the closure and parses after.
        let send = body_of("pub fn send_message");
        let retry = send.find("retrying(").expect("a send is retried");
        let read = send
            .find("read_to_vec()")
            .expect("a send reads its own answer");
        assert!(
            read > retry,
            "the answer to a send must be read inside the retry"
        );
        assert!(
            !send.contains("read_json("),
            "read_json takes the response, so it reads the answer outside the retry"
        );
    }

    #[test]
    fn a_send_a_signal_lands_on_is_made_again_and_is_one_message() {
        // What `send_message` does when a signal arrives mid-request: the same
        // body goes again, key and all, and what comes back is the one message
        // — the id the server answered the attempt that reached it with, not a
        // second message written by the attempt that followed.
        let mut attempts = 0;
        let id = retrying(|| -> Result<i64, ureq::Error> {
            attempts += 1;
            if attempts == 1 {
                return Err(ureq::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Interrupted system call (os error 4)",
                )));
            }
            // The server deduplicates on the key, so every attempt after the
            // first answers with the message the first one wrote.
            Ok(7)
        })
        .expect("the send to be made again");
        assert_eq!(attempts, 2);
        assert_eq!(id, 7, "a repeat must answer with the message already there");
    }

    #[test]
    fn adds_a_scheme_and_trims_trailing_slashes() {
        let remote = Remote::at("localhost:7343/").expect("remote");
        assert_eq!(remote.base_url(), "http://localhost:7343");
        assert_eq!(remote.url("/channels"), "http://localhost:7343/channels");

        let remote = Remote::at("https://saneha.example.com").expect("remote");
        assert_eq!(remote.base_url(), "https://saneha.example.com");
    }
}
