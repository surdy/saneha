//! Talking to a running server.
//!
//! Every subcommand except `serve` goes through here: it finds the server in
//! `SANEHA_URL`, makes one request, and turns anything that goes wrong into a
//! single line a person can act on.

use std::time::Duration;

use anyhow::{anyhow, Result};
use ureq::http::Response;
use ureq::{Agent, Body};

use crate::api::{
    ApiError, Channel, ChannelList, ChannelState, CursorUpdate, JoinRequest, Joined, Message,
    MessageList, NewMessage, Participant, ParticipantList, Waited, DEFAULT_MESSAGE_LIMIT,
};

/// The environment variable that points every subcommand at the server.
pub const URL_ENV: &str = "SANEHA_URL";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY: usize = 200;

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
            self.agent
                .post(self.url("/channels"))
                .send_json(&body)
                .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "channel")
    }

    /// Every channel the server knows about.
    pub fn list_channels(&self) -> Result<Vec<Channel>> {
        let response = self.check(
            self.agent
                .get(self.url("/channels"))
                .call()
                .map_err(|err| self.unreachable(&err))?,
        )?;
        let list: ChannelList = read_json(response, "channel list")?;
        Ok(list.channels)
    }

    /// Joins a channel and returns the identity the server granted, or
    /// `Stale` when the participant the request was reasoned about changed
    /// hands in between and the caller should look again.
    pub fn join(&self, channel: &str, request: &JoinRequest) -> Result<JoinAnswer> {
        let response = self
            .agent
            .post(self.url(&format!("/channels/{channel}/participants")))
            .send_json(request)
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
            self.agent
                .get(self.url(&format!("/channels/{channel}/participants")))
                .call()
                .map_err(|err| self.unreachable(&err))?,
        )?;
        let list: ParticipantList = read_json(response, "participant list")?;
        Ok(list.participants)
    }

    /// One participant by identity, or `None` if nobody holds that identity
    /// here. An unknown channel reads as `None` too: the join that follows is
    /// what reports it, in the server's own words.
    pub fn participant(&self, channel: &str, identity: &str) -> Result<Option<Participant>> {
        let response = self
            .agent
            .get(self.url(&format!("/channels/{channel}/participants/{identity}")))
            .call()
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
    pub fn send_message(&self, channel: &str, message: &NewMessage) -> Result<Message> {
        let response = self.check(
            self.agent
                .post(self.url(&format!("/channels/{channel}/messages")))
                .send_json(message)
                .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "message")
    }

    /// One page of a transcript: at most `limit` messages after `after`.
    pub fn messages(&self, channel: &str, after: i64, limit: usize) -> Result<Vec<Message>> {
        let response = self.check(
            self.agent
                .get(self.url(&format!(
                    "/channels/{channel}/messages?after={after}&limit={limit}"
                )))
                .call()
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
        let response = match self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(Duration::from_secs(hold) + WAIT_MARGIN))
            .build()
            .call()
        {
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
            self.agent
                .post(self.url(&format!(
                    "/channels/{channel}/participants/{identity}/cursor"
                )))
                .send_json(&CursorUpdate { read_cursor })
                .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "participant")
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

fn read_json<T: serde::de::DeserializeOwned>(
    mut response: Response<Body>,
    what: &str,
) -> Result<T> {
    response.body_mut().read_json::<T>().map_err(|err| {
        anyhow!("the saneha server sent a {what} this version does not understand ({err})")
    })
}

/// The server's own words when it sends them, a readable fallback when it does
/// not, and whether the server said the request is worth making again.
fn server_message(mut response: Response<Body>) -> (String, bool) {
    let status = response.status();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    if let Ok(api_error) = serde_json::from_str::<ApiError>(&body) {
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

    #[test]
    fn adds_a_scheme_and_trims_trailing_slashes() {
        let remote = Remote::at("localhost:7343/").expect("remote");
        assert_eq!(remote.base_url(), "http://localhost:7343");
        assert_eq!(remote.url("/channels"), "http://localhost:7343/channels");

        let remote = Remote::at("https://saneha.example.com").expect("remote");
        assert_eq!(remote.base_url(), "https://saneha.example.com");
    }
}
