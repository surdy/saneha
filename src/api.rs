//! The wire shapes shared by the server and the subcommands that talk to it,
//! along with the limits both sides hold to and the two sentences both sides
//! have to be able to say.

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
    /// RFC 3339, UTC: when it was closed, if it has been. The viewer renders
    /// "closed at T" from this rather than scanning the transcript for the
    /// close system message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
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

/// What a message is: one written by a participant, or one the server wrote
/// into the transcript about a participant or about the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    /// Written by a participant. Everything else is a system message.
    Message,
    Join,
    Leave,
    Close,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageKind::Message => "message",
            MessageKind::Join => "join",
            MessageKind::Leave => "leave",
            MessageKind::Close => "close",
        }
    }

    /// Whether the server wrote this rather than a participant.
    pub fn is_system(self) -> bool {
        !matches!(self, MessageKind::Message)
    }
}

impl std::str::FromStr for MessageKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "message" => Ok(MessageKind::Message),
            "join" => Ok(MessageKind::Join),
            "leave" => Ok(MessageKind::Leave),
            "close" => Ok(MessageKind::Close),
            other => Err(format!("unknown message kind {other:?}")),
        }
    }
}

/// A file carried with a message and stored by the server.
///
/// The id is 128 random bits as 32 hex characters, minted by the server on
/// upload. It is random rather than the row id because the id is the whole of
/// what a fetch needs: an id that could be guessed by counting would hand
/// anyone who can reach the server the attachments of channels they were never
/// given the name of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// 32 lowercase hex characters.
    pub id: String,
    /// The basename it was uploaded under, as [`attachment_filename`] leaves
    /// it.
    pub filename: String,
    /// The size of the stored file, in bytes.
    pub size: u64,
    /// What the uploader said it is, or [`DEFAULT_CONTENT_TYPE`].
    pub content_type: String,
    /// RFC 3339, UTC: when it was uploaded, which is not when the message
    /// carrying it was written.
    pub created_at: String,
}

/// A message as the server describes it. Ids increase within a channel, so a
/// read cursor is a number in this sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub channel: String,
    pub kind: MessageKind,
    /// The identity that wrote it. A system message has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The identity a join or leave is about. A message written by a
    /// participant has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// The identities this is addressed to. Empty is a broadcast.
    pub recipients: Vec<String>,
    /// Markdown, as it was written.
    pub body: String,
    /// The files carried with this message, in the order they were uploaded.
    /// It arrived after the rest of a message did, so a server that does not
    /// send it reads as a message carrying nothing rather than as a message
    /// this build cannot understand.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// RFC 3339, UTC.
    pub created_at: String,
}

impl Message {
    /// Whether this message wakes a participant waiting with `--mentions`.
    ///
    /// The rule is the one the scope states: what is addressed to me, or what
    /// is addressed to everyone. `@all` is expanded to explicit recipients when
    /// the message is written, so "everyone" here means a broadcast, which is a
    /// message written with no recipients at all.
    ///
    /// System messages are decided by kind rather than by recipients, which
    /// they do not have. A `close` always wakes: the channel being over is the
    /// one thing a waiter must not sleep through, and it is what its exit code
    /// says. A `join` or a `leave` does not: a participant that asked to be
    /// woken only for what is addressed to it did not ask to be woken because
    /// somebody arrived, and it will see the arrival in its next `read`.
    /// Whether `identity` is who wrote this. A system message was written by
    /// nobody, so it is never anyone's own.
    pub fn written_by(&self, identity: &str) -> bool {
        self.from.as_deref() == Some(identity)
    }

    pub fn wakes(&self, identity: &str) -> bool {
        match self.kind {
            MessageKind::Close => true,
            MessageKind::Join | MessageKind::Leave => false,
            MessageKind::Message => {
                self.recipients.is_empty() || self.recipients.iter().any(|to| to == identity)
            }
        }
    }
}

/// The body of `POST /channels/{name}/messages`. Recipients come from the
/// mentions in the body and from `to`, which holds what `--to` was given:
/// `bob`, `bob@quadhost` or `all`, with or without a leading `@`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMessage {
    /// The identity writing it, which must already be a participant.
    pub from: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    /// The attachments this message carries, by the ids
    /// `POST /channels/{name}/attachments` handed back. An id that names
    /// nothing in this channel, or one another message already carries, fails
    /// the whole send.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
}

/// The body of `GET /channels/{name}/messages`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageList {
    pub messages: Vec<Message>,
}

/// The query of `GET /channels/{name}/messages`: everything after `after`, at
/// most `limit` of them. Fetching never moves a read cursor, so `wait` and a
/// history read use the same route as `read` does.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageQuery {
    /// Messages with an id greater than this. Absent means from the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// At most this many, capped at [`MAX_MESSAGE_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// The query of `GET /channels/{name}/participants/{identity}/wait`: hold this
/// request open until something this participant has not read arrives, the
/// channel closes, or `hold` seconds pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WaitQuery {
    /// Wake only for what this participant is addressed by, as
    /// [`Message::wakes`] decides it. Absent is false.
    #[serde(default)]
    pub mentions: bool,
    /// How long the server may hold this request open, in seconds, capped at
    /// [`MAX_HOLD`]. Absent is the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<u64>,
}

/// The body of a 200 from the wait route: what arrived, and the state of the
/// channel it arrived in.
///
/// The state is here because a waiter has two questions and one answer to read
/// them from: "is there anything for me" and "is this channel over". A closed
/// channel answers with the last of the transcript and `closed`, and the
/// waiter prints what it got and stops waiting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Waited {
    pub messages: Vec<Message>,
    pub channel_state: ChannelState,
}

/// The longest the server holds one wait request open before answering `204`
/// and letting the caller ask again.
///
/// A minute sits under the idle timeout of any reverse proxy this is likely to
/// sit behind, so a held request is never cut by something in the middle that
/// thinks it has gone quiet. The caller re-issues until its own `--timeout`,
/// so what a person sees is still one blocking command.
pub const MAX_HOLD: u64 = 60;

/// How long `saneha wait` blocks when `--timeout` does not say: an hour, long
/// enough that a participant left waiting overnight is woken by the message
/// rather than by its own clock.
pub const DEFAULT_WAIT_TIMEOUT: u64 = 3600;

/// What a held request is ended with when the server is stopping. It comes
/// back as a `503` with `retry` set, because there is nothing wrong with the
/// request: the server will take it again when it is back.
pub const STOPPING: &str =
    "the saneha server is stopping; this wait ended early and can be started again";

/// How many messages one fetch returns when the caller does not say.
pub const DEFAULT_MESSAGE_LIMIT: usize = 500;

/// The most one fetch will return, however large a limit is asked for. A
/// caller with a longer backlog than this asks again from the last id it got.
pub const MAX_MESSAGE_LIMIT: usize = 1000;

/// The longest a message body may be, in bytes. Anything longer is an
/// attachment rather than a message (v1 scope). Both sides hold callers to it:
/// the subcommand so nothing large is sent at all, and the server because it
/// is the one that must be right.
pub const MAX_BODY: usize = 64 * 1024;

/// Why a body was refused, in the one wording both sides use.
pub fn body_too_large(size: usize) -> String {
    format!(
        "a message body is at most {MAX_BODY} bytes and this one is {size}; \
         send the long part as an attachment instead"
    )
}

/// The most an attachment may be, in bytes. Both sides hold callers to it: the
/// subcommand from the file's own size, so nothing too large is uploaded at
/// all, and the server as the bytes arrive, because it is the one that must be
/// right.
pub const MAX_ATTACHMENT: u64 = 25 * 1024 * 1024;

/// Why an attachment was refused, in the one wording both sides use.
pub fn attachment_too_large(size: u64) -> String {
    format!(
        "an attachment is at most {MAX_ATTACHMENT} bytes and this one is {size}; \
         send a smaller file"
    )
}

/// Why an empty file was refused, in the one wording both sides use.
pub fn attachment_is_empty(filename: &str) -> String {
    format!("{filename:?} is empty, and an empty file is not worth attaching")
}

/// The header an upload carries its filename in. The body of that request is
/// the file itself, so the name it had travels alongside it rather than in it.
///
/// The value is percent-encoded by [`encode_filename`]. A header value is
/// bytes and is read as ASCII by most of what handles one, and a filename is
/// whatever language it was written in: `résumé.md` and `設計.md` are names,
/// and they have to arrive as themselves.
pub const FILENAME_HEADER: &str = "x-saneha-filename";

/// What an attachment is stored as when the uploader does not say what it is.
pub const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// The longest filename saneha will store, or write out again.
pub const MAX_FILENAME: usize = 128;

/// A filename as saneha will store it and as a fetch will write it: the
/// basename alone, with no path separators, no control characters, nothing
/// that would end a quoted header value, and no longer than [`MAX_FILENAME`]
/// characters. `None` when nothing usable is left of it.
///
/// Both sides run it, and the subcommand runs it twice. On the way out,
/// because the name that crosses the wire should already be a name; on the
/// server, because that name was chosen by somebody else; and on the way back,
/// because a stored name becomes a path on this machine.
pub fn attachment_filename(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\\' | '/'))
        .take(MAX_FILENAME)
        .collect();
    let cleaned = cleaned.trim();
    // `.` and `..` name directories, wherever they are written; everything
    // else that survives the filter is a filename.
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return None;
    }
    Some(cleaned.to_string())
}

/// A filename as it travels in a header: percent-encoded, so a name in any
/// language survives a hop that only carries bytes a header may hold.
///
/// Everything but the unreserved characters is encoded, `%` included, so
/// encoding and decoding are exact inverses and a name with a `%` in it comes
/// back as it went. This is the `value-chars` of RFC 8187, which is what
/// `filename*=UTF-8''...` in a `Content-Disposition` is made of, so one
/// encoder does both headers.
pub fn encode_filename(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    for byte in name.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            use std::fmt::Write;
            // Writing to a String cannot fail.
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The inverse, as forgiving as a reader has to be: a `%` that does not start
/// a pair of hex digits is the character it is, and bytes that turn out not to
/// be UTF-8 are replaced rather than refused. What comes out still goes
/// through [`attachment_filename`] before it is used for anything.
pub fn decode_filename(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        // Both halves have to be hex digits and nothing else: parsing a pair
        // as a number would take `+A` as ten, and `%+A` is three characters of
        // a filename rather than a newline.
        let pair = (index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit())
        .then(|| std::str::from_utf8(&bytes[index + 1..index + 3]).ok())
        .flatten()
        .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (byte, pair) {
            (b'%', Some(decoded_byte)) => {
                decoded.push(decoded_byte);
                index += 3;
            }
            _ => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// The one sentence every verb uses when the caller is acting as somebody who
/// has not joined the channel: the server when it refuses a send, the
/// subcommand when it looks at the participants and does not find itself. It
/// lives here so both sides say it the same way.
pub fn not_a_participant(channel: &str, identity: &str) -> String {
    format!("{identity} has not joined {channel:?}; join it first: saneha join {channel}")
}

/// The body of `POST /channels/{name}/participants/{identity}/cursor`.
/// Advancing is its own request because reading is the only thing that moves a
/// cursor (ADR-0004), and a cursor never moves backwards: a value below the
/// one already recorded is ignored, and one beyond the end of the transcript
/// is held at the last message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorUpdate {
    pub read_cursor: i64,
}

/// The answer to `POST /channels/{name}/participants/{identity}/leave`.
///
/// A leave is idempotent, so the interesting field is `left`: it says whether
/// this was the call that marked the participant away and wrote the system
/// message, or whether there was nothing left to do. Either way the request
/// succeeded, because asking twice for something that has already happened is
/// not an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Left {
    pub channel: String,
    pub identity: String,
    /// True when this call marked the participant away and wrote the leave
    /// into the transcript.
    pub left: bool,
    /// The state of the channel the leave was asked of. A closed channel takes
    /// no more messages, so a leave in one changes nothing.
    pub channel_state: ChannelState,
    pub participant: Participant,
}

/// The body of `POST /channels/{name}/close`: who is closing it.
///
/// The scope says any participant or a person may close a channel, so `by` is
/// an identity rather than a participant: `surdy@web` closing from the viewer
/// has never joined anything. It is recorded in the body of the close system
/// message, because the schema says a `close` is about the channel and not
/// about a participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseRequest {
    pub by: String,
}

/// The answer to a close, idempotent in the same way a leave is: `closed` says
/// whether this call was the one that closed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Closed {
    pub channel: Channel,
    pub closed: bool,
}

/// How much a channel holds. It is what a delete asks for before it is
/// confirmed, so that what is about to go is said in numbers rather than in
/// the word "everything".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCounts {
    pub participants: usize,
    /// The whole transcript, system messages included.
    pub messages: usize,
    pub attachments: usize,
    /// What those attachments take up on the server, in bytes.
    pub attachment_bytes: u64,
}

/// The body of `GET /channels/{name}`: one channel and what it holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDetail {
    pub channel: Channel,
    pub counts: ChannelCounts,
}

/// The body of a `DELETE /channels/{name}?confirm=true` that went through:
/// what is no longer there. Deleted is the word CONTEXT.md uses for a channel
/// whose transcript has been removed, as distinct from closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deleted {
    pub channel: String,
    pub counts: ChannelCounts,
}

/// The query of `DELETE /channels/{name}`. The confirmation is in the URL
/// rather than in a body, because a `DELETE` with a body is a request some
/// proxies and clients drop the body from, and a deletion must never be
/// decided by something that got lost on the way.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub confirm: bool,
}

/// Why an unconfirmed delete was refused, in the one wording both sides use.
pub fn delete_needs_confirmation(channel: &str) -> String {
    format!(
        "deleting {channel:?} removes its transcript, its participants and its attachments, \
         and nothing brings them back; say so with: saneha delete {channel} --yes"
    )
}

/// Every failing response carries this, so the subcommands can print the
/// server's own words rather than guessing from a status code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    /// Set when the request failed on something that has since changed and is
    /// worth looking at again: a join whose view of a participant went stale,
    /// or a wait ended early because the server is stopping.
    #[serde(default)]
    pub retry: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filename_is_the_basename_and_nothing_else() {
        assert_eq!(
            attachment_filename("/etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(
            attachment_filename("../../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(
            attachment_filename("C:\\Windows\\notes.md").as_deref(),
            Some("notes.md")
        );
        assert_eq!(
            attachment_filename("handoff.md").as_deref(),
            Some("handoff.md")
        );
        assert_eq!(
            attachment_filename(".gitignore").as_deref(),
            Some(".gitignore")
        );
    }

    #[test]
    fn a_filename_that_would_end_a_header_or_a_line_is_cleaned_of_it() {
        assert_eq!(
            attachment_filename("no\"quotes\".md").as_deref(),
            Some("noquotes.md")
        );
        assert_eq!(
            attachment_filename("one\nline\ronly.md").as_deref(),
            Some("onelineonly.md")
        );
        assert_eq!(attachment_filename("nul\0byte").as_deref(), Some("nulbyte"));
    }

    #[test]
    fn a_filename_that_is_not_one_is_refused() {
        assert_eq!(attachment_filename(""), None);
        assert_eq!(attachment_filename("   "), None);
        assert_eq!(attachment_filename("."), None);
        assert_eq!(attachment_filename(".."), None);
        assert_eq!(attachment_filename("/"), None);
        assert_eq!(attachment_filename("some/directory/"), None);
    }

    #[test]
    fn a_filename_in_any_script_survives_a_header() {
        for name in [
            "handoff.md",
            "résumé.md",
            "設計メモ.md",
            "100%-done.md",
            "a b;c\"d.md",
        ] {
            let encoded = encode_filename(name);
            assert!(
                encoded.is_ascii() && !encoded.contains([' ', ';', '"']),
                "{encoded}"
            );
            assert_eq!(decode_filename(&encoded), name);
        }
    }

    #[test]
    fn decoding_takes_what_it_is_given() {
        // A name that was never encoded is the name it is.
        assert_eq!(decode_filename("handoff.md"), "handoff.md");
        // A stray `%` is a character, not the start of anything.
        assert_eq!(decode_filename("100% done.md"), "100% done.md");
        assert_eq!(decode_filename("ends-in-%"), "ends-in-%");
        assert_eq!(decode_filename("%zz.md"), "%zz.md");
        // Only hex digits are a pair: a number parser would read `+A` as ten
        // and turn three characters of a name into a newline.
        assert_eq!(decode_filename("a%+Ab.md"), "a%+Ab.md");
        assert_eq!(decode_filename("a% Ab.md"), "a% Ab.md");
        // Bytes that are not UTF-8 are replaced rather than refused, and what
        // comes out still goes through `attachment_filename`.
        assert!(!decode_filename("%FF%FE.md").is_empty());
    }

    #[test]
    fn a_filename_is_no_longer_than_the_cap() {
        let long = format!("{}.md", "a".repeat(500));
        let cleaned = attachment_filename(&long).expect("a filename");
        assert_eq!(cleaned.chars().count(), MAX_FILENAME);
    }

    fn message(kind: MessageKind, recipients: &[&str]) -> Message {
        Message {
            id: 7,
            channel: "brisk-otter".to_string(),
            kind,
            from: Some("alice@macbookpro".to_string()),
            about: None,
            recipients: recipients.iter().map(|to| (*to).to_string()).collect(),
            body: "the tests pass".to_string(),
            attachments: Vec::new(),
            created_at: "2026-09-04T09:00:00Z".to_string(),
        }
    }

    const ME: &str = "bob@quadhost";

    #[test]
    fn a_message_addressed_to_me_wakes_me() {
        assert!(message(MessageKind::Message, &[ME]).wakes(ME));
        assert!(message(MessageKind::Message, &["alice@macbookpro", ME]).wakes(ME));
    }

    #[test]
    fn a_broadcast_is_addressed_to_everyone_including_me() {
        assert!(message(MessageKind::Message, &[]).wakes(ME));
    }

    #[test]
    fn a_message_addressed_to_somebody_else_does_not_wake_me() {
        assert!(!message(MessageKind::Message, &["carol@quadhost"]).wakes(ME));
        // The same name on another host is a different participant, and so is
        // a name this one is only the beginning of.
        assert!(!message(MessageKind::Message, &["bob@macbookpro"]).wakes(ME));
        assert!(!message(MessageKind::Message, &["bobby@quadhost"]).wakes(ME));
    }

    #[test]
    fn a_close_wakes_everyone_and_an_arrival_wakes_nobody() {
        assert!(message(MessageKind::Close, &[]).wakes(ME));
        assert!(!message(MessageKind::Join, &[]).wakes(ME));
        assert!(!message(MessageKind::Leave, &[]).wakes(ME));
    }
}
