//! Talking to a running server.
//!
//! Every subcommand except `serve` goes through here: it finds the server in
//! `SANEHA_URL`, makes one request, and turns anything that goes wrong into a
//! single line a person can act on.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{RequestBuilder, Response};
use reqwest::StatusCode;

use crate::api::{ApiError, Channel, ChannelList, NewChannel};

/// The environment variable that points every subcommand at the server.
pub const URL_ENV: &str = "SANEHA_URL";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY: usize = 200;

/// The server as seen from a subcommand.
pub struct Remote {
    base: String,
    http: reqwest::blocking::Client,
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
        let http = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("could not start the HTTP client")?;
        Ok(Remote { base, http })
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
        let body = NewChannel {
            name: name.map(str::to_string),
            purpose: purpose.map(str::to_string),
        };
        let response = self.send(self.http.post(self.url("/channels")).json(&body))?;
        response
            .json::<Channel>()
            .context("the saneha server sent a channel this version does not understand")
    }

    /// Every channel the server knows about.
    pub fn list_channels(&self) -> Result<Vec<Channel>> {
        let response = self.send(self.http.get(self.url("/channels")))?;
        let list = response
            .json::<ChannelList>()
            .context("the saneha server sent a channel list this version does not understand")?;
        Ok(list.channels)
    }

    fn send(&self, request: RequestBuilder) -> Result<Response> {
        let response = request.send().map_err(|err| {
            anyhow!(
                "cannot reach the saneha server at {} ({})",
                self.base,
                root_cause(&err)
            )
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        Err(anyhow!(server_message(status, response)))
    }
}

/// The server's own words when it sends them, a readable fallback when it does not.
fn server_message(status: StatusCode, response: Response) -> String {
    let body = response.text().unwrap_or_default();
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

/// The innermost cause, which is the part worth printing: reqwest's own
/// Display repeats the URL we are already naming.
fn root_cause(err: &dyn std::error::Error) -> String {
    let mut current: &dyn std::error::Error = err;
    while let Some(source) = current.source() {
        current = source;
    }
    current.to_string()
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

    #[test]
    fn unreachable_server_names_the_url() {
        // Port 1 on the loopback refuses connections promptly.
        let remote = Remote::at("http://127.0.0.1:1").expect("remote");
        let err = remote.list_channels().expect_err("should not connect");
        let message = format!("{err:#}");
        assert!(
            message.starts_with("cannot reach the saneha server at http://127.0.0.1:1"),
            "{message}"
        );
        assert_eq!(message.lines().count(), 1, "{message}");
    }
}
