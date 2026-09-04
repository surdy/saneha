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
    ApiError, Channel, ChannelList, JoinRequest, Joined, Participant, ParticipantList,
};

/// The environment variable that points every subcommand at the server.
pub const URL_ENV: &str = "SANEHA_URL";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY: usize = 200;

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

    /// Joins a channel and returns the identity the server granted.
    pub fn join(&self, channel: &str, request: &JoinRequest) -> Result<Joined> {
        let response = self.check(
            self.agent
                .post(self.url(&format!("/channels/{channel}/participants")))
                .send_json(request)
                .map_err(|err| self.unreachable(&err))?,
        )?;
        read_json(response, "join")
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

    fn check(&self, response: Response<Body>) -> Result<Response<Body>> {
        if response.status().is_success() {
            return Ok(response);
        }
        Err(anyhow!(server_message(response)))
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

/// The server's own words when it sends them, a readable fallback when it does not.
fn server_message(mut response: Response<Body>) -> String {
    let status = response.status();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    if let Ok(api_error) = serde_json::from_str::<ApiError>(&body) {
        return api_error.error;
    }
    let body = body.trim();
    if body.is_empty() {
        return format!("the saneha server refused the request ({status})");
    }
    let mut summary: String = body.lines().next().unwrap_or_default().to_string();
    if summary.chars().count() > MAX_ERROR_BODY {
        summary = summary.chars().take(MAX_ERROR_BODY).collect::<String>() + "...";
    }
    format!("the saneha server refused the request ({status}): {summary}")
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
