//! The wire shapes shared by the server and the subcommands that talk to it.

use serde::{Deserialize, Serialize};

/// The state of a channel. A channel is open until someone closes it; a closed
/// channel accepts no more messages but can still be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelState {
    Open,
    Closed,
}

impl ChannelState {
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelState::Open => "open",
            ChannelState::Closed => "closed",
        }
    }
}

impl std::str::FromStr for ChannelState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(ChannelState::Open),
            "closed" => Ok(ChannelState::Closed),
            other => Err(format!("unknown channel state {other:?}")),
        }
    }
}

/// A channel as the server describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    /// The channel name: lowercase letters, digits and hyphens.
    pub name: String,
    /// The optional one-line statement of what the channel is for.
    pub purpose: Option<String>,
    pub state: ChannelState,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// The body of `POST /channels`. A missing name asks the server to mint one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewChannel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}

/// The body of `GET /channels`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelList {
    pub channels: Vec<Channel>,
}

/// Every failing response carries this, so the subcommands can print the
/// server's own words rather than guessing from a status code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
