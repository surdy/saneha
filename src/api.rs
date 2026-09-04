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

/// A participant as the server describes it: who joined a channel, from where,
/// and how far through the transcript they have read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    /// `name@host`, unique within the channel.
    pub identity: String,
    /// The name half of the identity.
    pub name: String,
    /// The host half: the short hostname, lowercased.
    pub host: String,
    /// The coding-agent program this participant runs inside, or `unknown`.
    pub harness: String,
    /// The id of the harness session that last joined, when the harness
    /// publishes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The process whose life is this participant's presence: the harness
    /// itself when it publishes its pid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// When that process started, as the host reports it. A pid is reused; a
    /// pid and a start time together are not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started_at: Option<String>,
    /// The working directory the last join was made from.
    pub cwd: String,
    /// The Madari pane a future relay would nudge. Recorded, unused in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub madari_pane: Option<String>,
    /// Set once this participant has left and not resumed.
    pub away: bool,
    /// The message id this participant has read up to. 0 means nothing read.
    pub read_cursor: i64,
    /// RFC 3339, UTC: when the current join, or resume, happened.
    pub joined_at: String,
    /// RFC 3339, UTC: when this participant last left, if it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_at: Option<String>,
}

/// The body of `POST /channels/{name}/participants`: everything the caller
/// knows about itself, plus its verdict on the identity it is claiming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub name: String,
    pub host: String,
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started_at: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub madari_pane: Option<String>,
    /// The caller's answer to the one question the server cannot ask: is the
    /// harness session already holding `name@host` still running on that host?
    /// False resumes that participant, true asks for a suffixed name. Only a
    /// caller on the same host can tell, so only a caller decides.
    #[serde(default)]
    pub same_host_session_live: bool,
    /// The harness session id of the participant the caller looked at before
    /// forming that verdict, and `None` when it found nobody holding the
    /// identity. If the row says otherwise by the time the join is made, the
    /// verdict is stale and the join is refused rather than acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_session_id: Option<String>,
}

/// The answer to a join: the identity that was granted, and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Joined {
    pub channel: String,
    /// The granted identity, which is not always the one that was asked for.
    pub identity: String,
    /// True when an existing participant was continued rather than created.
    pub resumed: bool,
    /// True when a live session held the requested name, so a suffixed one was
    /// granted instead.
    pub suffixed: bool,
    pub participant: Participant,
}

/// The body of `GET /channels/{name}/participants`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantList {
    pub participants: Vec<Participant>,
}

/// Every failing response carries this, so the subcommands can print the
/// server's own words rather than guessing from a status code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    /// Set when the request failed on something that has since changed and is
    /// worth looking at again: a join whose view of a participant went stale.
    /// Nothing else asks a caller to try twice.
    #[serde(default)]
    pub retry: bool,
}
