//! The command line: one binary, one server subcommand, and subcommands that
//! talk to a running server.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::api::{Channel, JoinRequest, Message, NewMessage, Participant, ParticipantList};
use crate::client::{JoinAnswer, Remote, URL_ENV};
use crate::identity;
use crate::server;
use crate::store::{self, Store};

#[derive(Debug, Parser)]
#[command(
    name = "saneha",
    version,
    about = "A self-hosted channel where coding agents, and the person running them, talk",
    long_about = "A self-hosted channel where coding agents on different machines, and the \
                  person running them, talk to each other.\n\n\
                  `saneha serve` is the server and owns one SQLite file. Every other subcommand \
                  talks to a running server over HTTP and finds it in SANEHA_URL, for example \
                  `export SANEHA_URL=http://localhost:7343`."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the saneha server
    Serve(ServeArgs),
    /// Create a channel
    New(NewArgs),
    /// List channels
    List(ListArgs),
    /// Join a channel and claim an identity in it
    Join(JoinArgs),
    /// List the participants of a channel
    Participants(ParticipantsArgs),
    /// Write a message to a channel
    Send(SendArgs),
    /// Read the messages of a channel that this participant has not read
    Read(ReadArgs),
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address to listen on
    #[arg(long, value_name = "ADDR", env = "SANEHA_BIND", default_value = server::DEFAULT_BIND)]
    pub bind: String,

    /// SQLite file to store channels in; created on first start, along with any
    /// missing parent directories
    ///
    /// [default: $XDG_DATA_HOME/saneha/saneha.db, or
    /// ~/.local/share/saneha/saneha.db when XDG_DATA_HOME is unset]
    #[arg(long, value_name = "PATH", env = "SANEHA_DB")]
    pub db: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Channel name: lowercase letters, digits and hyphens. Omit it and the
    /// server mints a readable slug such as brisk-otter
    #[arg(value_name = "NAME")]
    pub name: Option<String>,

    /// One line saying what the channel is for
    #[arg(long, value_name = "TEXT")]
    pub purpose: Option<String>,

    /// Print the channel as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Print the channels as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct JoinArgs {
    /// The channel to join; it must already exist
    #[arg(value_name = "CHANNEL")]
    pub channel: String,

    /// The name half of the identity to claim. Without it the name is derived
    /// from the repository this is run in and the harness it is run under, as
    /// <repo-basename>-<harness>. Every worktree of a repository derives the
    /// same name, so two live checkouts are told apart by the suffix rather
    /// than by where they happen to sit
    #[arg(long = "as", value_name = "NAME", env = "SANEHA_AS")]
    pub as_name: Option<String>,

    /// The harness to record, overriding what the environment says
    ///
    /// A recognised harness publishes the id of the session in progress and
    /// its own process id, which is how a later join tells a session that is
    /// still running from one that has finished. A harness that publishes
    /// neither cannot be told apart from itself, so a second session of it on
    /// this host resumes the first rather than being granted a name of its
    /// own. Only Claude Code is recognised so far.
    #[arg(long, value_name = "ID")]
    pub harness: Option<String>,

    /// Print the granted identity as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ParticipantsArgs {
    /// The channel to list the participants of
    #[arg(value_name = "CHANNEL")]
    pub channel: String,

    /// Print the participants as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// The channel to write to; this identity must already have joined it
    #[arg(value_name = "CHANNEL")]
    pub channel: String,

    /// The message body, as markdown. The words are joined with single spaces,
    /// so quote anything whose spacing matters. A body of a single `-` is read
    /// from standard input instead, which is how a multi-line body is sent:
    /// `saneha send brisk-otter - <<'EOF'`
    #[arg(value_name = "BODY", num_args = 1.., required = true)]
    pub body: Vec<String>,

    /// Address the message to a participant, as a name, a full name@host, or
    /// all. Repeatable, and on top of whatever the body mentions
    #[arg(long = "to", value_name = "IDENTITY")]
    pub to: Vec<String>,

    /// The name half of this identity, as `saneha join` works it out
    #[arg(long = "as", value_name = "NAME", env = "SANEHA_AS")]
    pub as_name: Option<String>,

    /// The harness to derive the name from, overriding what the environment says
    #[arg(long, value_name = "ID")]
    pub harness: Option<String>,

    /// Print the message as JSON rather than only its id
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// The channel to read; this identity must already have joined it
    #[arg(value_name = "CHANNEL")]
    pub channel: String,

    /// Read the whole transcript without moving the read cursor
    #[arg(long, conflicts_with = "since")]
    pub all: bool,

    /// Read everything after this message id without moving the read cursor
    #[arg(long, value_name = "ID")]
    pub since: Option<i64>,

    /// The name half of this identity, as `saneha join` works it out
    #[arg(long = "as", value_name = "NAME", env = "SANEHA_AS")]
    pub as_name: Option<String>,

    /// The harness to derive the name from, overriding what the environment says
    #[arg(long, value_name = "ID")]
    pub harness: Option<String>,

    /// Print the messages as JSON
    #[arg(long)]
    pub json: bool,
}

/// Parses the command line and runs it.
pub fn run() -> Result<()> {
    execute(Cli::parse())
}

/// Runs an already-parsed command line.
pub fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve(args) => serve(args),
        Command::New(args) => new(args),
        Command::List(args) => list(args),
        Command::Join(args) => join(args),
        Command::Participants(args) => participants(args),
        Command::Send(args) => send(args),
        Command::Read(args) => read(args),
    }
}

fn serve(args: ServeArgs) -> Result<()> {
    let database = args.db.unwrap_or_else(server::default_database_path);
    let store = Arc::new(Store::open(&database)?);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;

    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&args.bind)
            .await
            .with_context(|| format!("could not listen on {}", args.bind))?;
        let address = listener
            .local_addr()
            .context("could not read the address the server is listening on")?;

        say(&format!("saneha is serving on http://{address}"))?;
        say(&format!("database: {}", database.display()))?;
        if address.ip().is_unspecified() {
            // http://0.0.0.0:7343 is not an address anything can connect to.
            say(&format!(
                "listening on every interface; point subcommands at a host that reaches \
                 this one, as {URL_ENV}=http://<host>:{}",
                address.port()
            ))?;
        } else {
            say(&format!(
                "point subcommands at it with: export {URL_ENV}=http://{address}"
            ))?;
        }

        server::run(listener, store).await
    })
}

fn new(args: NewArgs) -> Result<()> {
    let remote = Remote::from_env()?;
    let channel = remote.create_channel(args.name.as_deref(), args.purpose.as_deref())?;
    if args.json {
        say(&serde_json::to_string_pretty(&channel)?)
    } else {
        say(&channel.name)
    }
}

fn list(args: ListArgs) -> Result<()> {
    let remote = Remote::from_env()?;
    let channels = remote.list_channels()?;
    if args.json {
        return say(&serde_json::to_string_pretty(&crate::api::ChannelList {
            channels,
        })?);
    }
    write_out(&channel_table(&channels))
}

/// Who this command is: the identity a join would claim, worked out the same
/// way wherever it is asked for. `send` and `read` act as a participant, so
/// they have to arrive at the identity `join` was granted rather than at one
/// of their own.
struct Caller {
    name: String,
    host: String,
    harness: String,
    /// True when `--harness` said which, rather than the environment.
    harness_given: bool,
}

impl Caller {
    fn identity(&self) -> String {
        store::identity_of(&self.name, &self.host)
    }
}

/// The identity behind this command: `--as`, else `SANEHA_AS`, else the
/// project and the harness, on this host.
fn caller(as_name: Option<&str>, harness: Option<&str>) -> Result<Caller> {
    let given_harness = trimmed(harness);
    let harness = match &given_harness {
        Some(given) => {
            store::validate_harness(given)?;
            given.clone()
        }
        None => identity::detect_harness()
            .map(|marker| marker.id.to_string())
            .unwrap_or_else(|| identity::UNKNOWN_HARNESS.to_string()),
    };

    let host = identity::host(store::HOST.max);
    store::validate_host(&host)?;

    let name = match trimmed(as_name) {
        Some(given) => given,
        None => identity::derived_name(
            &identity::project_basename(),
            &harness,
            store::PARTICIPANT_NAME.max,
        ),
    };
    store::validate_participant_name(&name)?;

    Ok(Caller {
        name,
        host,
        harness,
        harness_given: given_harness.is_some(),
    })
}

/// How many times a join looks again after finding that the participant it
/// reasoned about is not the one the server has.
const JOIN_ATTEMPTS: usize = 2;

/// Joins a channel: work out who this is, ask the server whether that identity
/// is already held by a session still running here, and join accordingly.
///
/// The liveness question is the client's to answer, and only this client's: the
/// server cannot reach across the network to ask whether a process on another
/// host is still there. So `join` probes first
/// (`GET /channels/{channel}/participants/{identity}`) and sends its verdict
/// with the join. One extra round trip buys a server that never guesses.
///
/// The probe and the join are two requests, so the participant can change
/// hands in between. The request carries what the probe saw, the server
/// refuses a join whose view has gone stale, and this looks again once.
fn join(args: JoinArgs) -> Result<()> {
    let remote = Remote::from_env()?;

    let caller = caller(args.as_name.as_deref(), args.harness.as_deref())?;
    let Caller {
        name,
        host,
        harness,
        harness_given,
    } = caller;

    let cwd = std::env::current_dir()
        .context("could not read the working directory to record with the join")?
        .to_string_lossy()
        .into_owned();
    let session_id = identity::session_id(&harness);
    let pid = identity::pid(&harness);
    let pid_started_at = pid.and_then(identity::process_start);

    let wanted = store::identity_of(&name, &host);
    let mut held = None;
    let mut granted = None;
    let mut stale = String::new();

    for _ in 0..JOIN_ATTEMPTS {
        held = remote.participant(&args.channel, &wanted)?;
        let request = JoinRequest {
            name: name.clone(),
            host: host.clone(),
            harness: harness.clone(),
            session_id: session_id.clone(),
            pid,
            pid_started_at: pid_started_at.clone(),
            cwd: cwd.clone(),
            madari_pane: None,
            same_host_session_live: held
                .as_ref()
                .is_some_and(|held| session_is_live(held, session_id.as_deref())),
            held_session_id: held.as_ref().and_then(|held| held.session_id.clone()),
        };
        match remote.join(&args.channel, &request)? {
            JoinAnswer::Granted(joined) => {
                granted = Some(joined);
                break;
            }
            JoinAnswer::Stale(message) => stale = message,
        }
    }
    let Some(joined) = granted else {
        return Err(anyhow!(stale));
    };

    if args.json {
        say(&serde_json::to_string_pretty(&joined)?)?;
    } else {
        say(&joined.identity)?;
    }

    if joined.suffixed {
        let pid = held
            .as_ref()
            .and_then(|held| held.pid)
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        warn(&format!(
            "{} is held by a harness session still running on this host (pid {pid}); \
             joined as {} instead",
            held.as_ref().map_or(wanted.as_str(), |held| &held.identity),
            joined.identity
        ));
    }
    if !harness_given && harness == identity::UNKNOWN_HARNESS {
        warn(&format!(
            "no harness was recognised, so this joined as {}; pass --harness ID or set {} \
             to name yourself",
            joined.identity,
            identity::AS_ENV
        ));
    }
    Ok(())
}

/// Whether the participant already holding an identity is a harness session
/// still running here.
///
/// The host is not compared: an identity carries its host, so anyone holding
/// this identity is on this host by construction. A participant with no
/// recorded session id, or one recorded under the session asking, is this same
/// participant coming back, which is a resume and not a collision. Neither is
/// a recorded process that has since finished, or a pid that is alive but
/// started at some other time, which is the number handed on to something
/// else.
fn session_is_live(held: &Participant, mine: Option<&str>) -> bool {
    match held.session_id.as_deref() {
        Some(theirs) if Some(theirs) != mine => held
            .pid
            .is_some_and(|pid| identity::still_running(pid, held.pid_started_at.as_deref())),
        _ => false,
    }
}

fn participants(args: ParticipantsArgs) -> Result<()> {
    let remote = Remote::from_env()?;
    let participants = remote.list_participants(&args.channel)?;
    if args.json {
        return say(&serde_json::to_string_pretty(&ParticipantList {
            participants,
        })?);
    }
    write_out(&participant_table(&args.channel, &participants))
}

/// The marker that says the body is on standard input, so a multi-line
/// markdown body does not have to survive a shell's quoting.
const BODY_FROM_STDIN: &str = "-";

/// Writes one message. Who it is addressed to is the server's to work out: it
/// holds the participants, and it refuses the whole send if a mention names
/// nobody, so nothing here has to guess.
fn send(args: SendArgs) -> Result<()> {
    let remote = Remote::from_env()?;
    let caller = caller(args.as_name.as_deref(), args.harness.as_deref())?;
    let body = message_body(&args.body)?;

    let message = remote.send_message(
        &args.channel,
        &NewMessage {
            from: caller.identity(),
            body,
            to: args.to,
        },
    )?;

    if args.json {
        say(&serde_json::to_string_pretty(&message)?)
    } else {
        // Only the id, so `ID=$(saneha send ...)` is safe.
        say(&message.id.to_string())
    }
}

/// The body as it was given: the words joined with single spaces, or standard
/// input when the body is a single `-`.
fn message_body(words: &[String]) -> Result<String> {
    use std::io::Read;

    if words.len() == 1 && words[0] == BODY_FROM_STDIN {
        let mut body = String::new();
        std::io::stdin()
            .read_to_string(&mut body)
            .context("could not read the message body from standard input")?;
        // A heredoc ends in a newline that is not part of what was written.
        return Ok(body.trim_end().to_string());
    }
    Ok(words.join(" "))
}

/// Reads a channel. By default that is the unread messages, and reading them
/// is what advances the read cursor; `--all` and `--since` are history reads
/// and move nothing (ADR-0004).
///
/// The fetch and the advance are two requests on purpose: `wait` will reuse
/// the fetch, and waiting never moves a cursor.
fn read(args: ReadArgs) -> Result<()> {
    let remote = Remote::from_env()?;
    let caller = caller(args.as_name.as_deref(), args.harness.as_deref())?;
    let identity = caller.identity();

    // One request answers three questions: does the channel exist, is this
    // caller in it, and how far has it read.
    let participants = remote.list_participants(&args.channel)?;
    let me = participants
        .iter()
        .find(|participant| participant.identity == identity)
        .ok_or_else(|| store::not_a_participant(&args.channel, &identity))?;

    let (after, advance) = match (args.all, args.since) {
        (true, _) => (0, false),
        (false, Some(since)) => (since, false),
        (false, None) => (me.read_cursor, true),
    };

    let messages = remote.messages_after(&args.channel, after)?;
    if advance {
        if let Some(last) = messages.last() {
            remote.set_read_cursor(&args.channel, &identity, last.id)?;
        }
    }

    if args.json {
        return say(&serde_json::to_string_pretty(&messages)?);
    }
    // Nothing unread prints nothing at all, so a skill can run this on a loop
    // and say something only when there is something to say.
    write_out(&transcript(&messages, &participants))
}

/// An argument or environment variable that is there and says something.
fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Everything printed goes through here. A closed pipe (`saneha list | head`)
/// is the reader walking away, not a failure: Rust does not restore the
/// default SIGPIPE, so writing to one is an error the process must swallow
/// rather than a signal that ends it.
fn write_out(text: &str) -> Result<()> {
    use std::io::Write;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let written = write!(out, "{text}").and_then(|()| out.flush());
    match written {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(anyhow::Error::new(err).context("could not write to standard output")),
    }
}

/// One line on standard output.
fn say(line: &str) -> Result<()> {
    write_out(&format!("{line}\n"))
}

/// One line on standard error, for something a person should know that is not
/// the answer. `saneha join` prints only the granted identity on standard
/// output, so anything explaining that identity goes here instead. A closed
/// standard error is not worth failing over.
fn warn(line: &str) {
    use std::io::Write;

    let stderr = std::io::stderr();
    let mut err = stderr.lock();
    let _ = writeln!(err, "saneha: {line}");
}

/// The `list` output: aligned columns, purpose last so it can run long.
fn channel_table(channels: &[Channel]) -> String {
    if channels.is_empty() {
        return "No channels yet. Create one with: saneha new\n".to_string();
    }

    let name_width = channels
        .iter()
        .map(|c| c.name.chars().count())
        .chain(std::iter::once("NAME".len()))
        .max()
        .unwrap_or(4);
    let state_width = channels
        .iter()
        .map(|c| c.state.as_str().len())
        .chain(std::iter::once("STATE".len()))
        .max()
        .unwrap_or(5);

    let mut out = format!(
        "{:name_width$}  {:state_width$}  {}\n",
        "NAME", "STATE", "PURPOSE"
    );
    for channel in channels {
        let purpose = channel.purpose.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "{:name_width$}  {:state_width$}  {}\n",
            channel.name,
            channel.state.as_str(),
            purpose
        ));
    }
    out
}

/// The `participants` output: identity first, because that is what a message
/// is addressed to, then where and what it is running in.
fn participant_table(channel: &str, participants: &[Participant]) -> String {
    if participants.is_empty() {
        return format!("Nobody has joined {channel} yet. Join it with: saneha join {channel}\n");
    }

    let width = |header: &str, of: fn(&Participant) -> &str| {
        participants
            .iter()
            .map(|p| of(p).chars().count())
            .chain(std::iter::once(header.chars().count()))
            .max()
            .unwrap_or_else(|| header.chars().count())
    };
    let identity_width = width("IDENTITY", |p| &p.identity);
    let harness_width = width("HARNESS", |p| &p.harness);
    let host_width = width("HOST", |p| &p.host);
    let away = |p: &Participant| if p.away { "away" } else { "here" };
    // The read cursor is a message id, so it is as wide as the widest id.
    let read_width = participants
        .iter()
        .map(|p| p.read_cursor.to_string().len())
        .chain(std::iter::once("READ".len()))
        .max()
        .unwrap_or(4);

    let mut out = format!(
        "{:identity_width$}  {:harness_width$}  {:host_width$}  {:4}  {:read_width$}  {}\n",
        "IDENTITY", "HARNESS", "HOST", "AWAY", "READ", "JOINED"
    );
    for participant in participants {
        out.push_str(&format!(
            "{:identity_width$}  {:harness_width$}  {:host_width$}  {:4}  {:read_width$}  {}\n",
            participant.identity,
            participant.harness,
            participant.host,
            away(participant),
            participant.read_cursor.to_string(),
            participant.joined_at,
        ));
    }
    out
}

/// How far a message body is set in under its header.
const INDENT: &str = "    ";

/// The `read` output: a header line naming the message, then its body indented
/// under it, with a blank line between messages. A system message is one line,
/// because the server writes it and it is one sentence.
///
/// Recipients are shown by name alone when only one participant in the channel
/// answers to that name, which is the same rule a mention is resolved by, so
/// what is printed is what can be typed back.
fn transcript(messages: &[Message], participants: &[Participant]) -> String {
    let mut out = String::new();
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if message.kind.is_system() {
            out.push_str(&format!(
                "#{}  {}  ·  {}\n",
                message.id, message.created_at, message.body
            ));
            continue;
        }

        let from = message.from.as_deref().unwrap_or("system");
        let to = if message.recipients.is_empty() {
            "all".to_string()
        } else {
            message
                .recipients
                .iter()
                .map(|identity| short_name(identity, participants))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!(
            "#{}  {}  {}  → {}\n",
            message.id, message.created_at, from, to
        ));
        for line in message.body.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("{INDENT}{line}\n"));
            }
        }
    }
    out
}

/// An identity as short as it can be said without becoming ambiguous: the name
/// alone when one participant has it, the whole identity when two do.
fn short_name(identity: &str, participants: &[Participant]) -> String {
    let Some((name, _)) = identity.split_once('@') else {
        return identity.to_string();
    };
    let sharing = participants
        .iter()
        .filter(|participant| participant.name == name)
        .count();
    if sharing == 1 {
        name.to_string()
    } else {
        identity.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ChannelState;

    fn channel(name: &str, purpose: Option<&str>) -> Channel {
        Channel {
            name: name.to_string(),
            purpose: purpose.map(str::to_string),
            state: ChannelState::Open,
            created_at: "2026-09-04T09:00:00Z".to_string(),
        }
    }

    #[test]
    fn command_line_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn empty_listing_says_so() {
        assert_eq!(
            channel_table(&[]),
            "No channels yet. Create one with: saneha new\n"
        );
    }

    #[test]
    fn listing_aligns_on_the_longest_name() {
        let table = channel_table(&[
            channel("brisk-otter", Some("the refactor")),
            channel("a-much-longer-channel", None),
        ]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("NAME "), "{:?}", lines[0]);
        assert!(lines[0].contains("STATE"));
        assert!(lines[0].ends_with("PURPOSE"));
        assert!(lines[1].starts_with("brisk-otter "), "{:?}", lines[1]);
        assert!(lines[1].ends_with("the refactor"), "{:?}", lines[1]);
        // A channel with no purpose still fills the column.
        assert!(lines[2].ends_with(" -"), "{:?}", lines[2]);

        let state_column = |line: &str| line.find("open").or_else(|| line.find("STATE"));
        assert_eq!(state_column(lines[0]), state_column(lines[1]));
        assert_eq!(state_column(lines[1]), state_column(lines[2]));
    }

    fn participant(identity: &str, session_id: Option<&str>, pid: Option<u32>) -> Participant {
        held(
            identity,
            session_id,
            pid,
            pid.and_then(identity::process_start),
        )
    }

    fn held(
        identity: &str,
        session_id: Option<&str>,
        pid: Option<u32>,
        pid_started_at: Option<String>,
    ) -> Participant {
        let (name, host) = identity.split_once('@').expect("name@host");
        Participant {
            identity: identity.to_string(),
            name: name.to_string(),
            host: host.to_string(),
            harness: "claude".to_string(),
            session_id: session_id.map(str::to_string),
            pid,
            pid_started_at,
            cwd: "/repos/saneha".to_string(),
            madari_pane: None,
            away: false,
            read_cursor: 0,
            joined_at: "2026-09-04T09:00:00Z".to_string(),
            left_at: None,
        }
    }

    #[test]
    fn an_empty_participant_listing_says_how_to_join() {
        assert_eq!(
            participant_table("brisk-otter", &[]),
            "Nobody has joined brisk-otter yet. Join it with: saneha join brisk-otter\n"
        );
    }

    #[test]
    fn the_participant_listing_aligns_and_names_the_away_ones() {
        let mut left = participant("saneha-claude@quadhost", None, None);
        left.away = true;
        let table = participant_table(
            "brisk-otter",
            &[
                participant("notes-method-claude@macbookpro", None, None),
                left,
            ],
        );
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("IDENTITY "), "{:?}", lines[0]);
        assert!(lines[0].ends_with("JOINED"), "{:?}", lines[0]);
        assert!(lines[1].contains("here"), "{:?}", lines[1]);
        assert!(lines[2].contains("away"), "{:?}", lines[2]);

        // The identity column is padded to the longest identity, so the
        // harness starts in the same place on every line.
        let harness_column = |line: &str| line.find("  claude").or_else(|| line.find("  HARNESS"));
        assert_eq!(harness_column(lines[0]), harness_column(lines[1]));
        assert_eq!(harness_column(lines[1]), harness_column(lines[2]));
    }

    #[test]
    fn a_live_session_is_one_that_is_someone_else_and_still_running() {
        let mine = Some("session-mine");
        let running = std::process::id();
        let live = participant("a@h", Some("session-theirs"), Some(running));
        assert!(session_is_live(&live, mine));

        // The same session coming back is a resume, however alive it is.
        let same = participant("a@h", mine, Some(running));
        assert!(!session_is_live(&same, mine));

        // A process that has finished, and a participant that recorded no
        // session at all.
        let gone = participant("a@h", Some("session-theirs"), Some(u32::MAX));
        assert!(!session_is_live(&gone, mine));
        let unrecorded = participant("a@h", None, Some(running));
        assert!(!session_is_live(&unrecorded, mine));
        let pidless = participant("a@h", Some("session-theirs"), None);
        assert!(!session_is_live(&pidless, mine));

        // The pid is alive but it is not the process that was recorded: the
        // number was handed on, and the session it belonged to is over.
        let reused = held(
            "a@h",
            Some("session-theirs"),
            Some(running),
            Some("Thu Jan  1 00:00:00 2015".to_string()),
        );
        assert!(!session_is_live(&reused, mine));
        // Nothing recorded about the process is not evidence that it is there.
        let unpinned = held("a@h", Some("session-theirs"), Some(running), None);
        assert!(!session_is_live(&unpinned, mine));
    }

    fn message(id: i64, from: Option<&str>, recipients: &[&str], body: &str) -> Message {
        use crate::api::MessageKind;

        Message {
            id,
            channel: "brisk-otter".to_string(),
            kind: if from.is_some() {
                MessageKind::Message
            } else {
                MessageKind::Join
            },
            from: from.map(str::to_string),
            about: None,
            recipients: recipients.iter().map(|to| (*to).to_string()).collect(),
            body: body.to_string(),
            created_at: "2026-09-04T09:00:00Z".to_string(),
        }
    }

    #[test]
    fn nothing_unread_prints_nothing_at_all() {
        assert_eq!(transcript(&[], &[]), "");
    }

    #[test]
    fn a_transcript_sets_each_body_under_its_own_header() {
        let roster = [
            participant("alice@macbookpro", None, None),
            participant("bob@quadhost", None, None),
        ];
        let printed = transcript(
            &[
                message(1, None, &[], "alice@macbookpro joined"),
                message(
                    2,
                    Some("alice@macbookpro"),
                    &["bob@quadhost"],
                    "first\n\nthird",
                ),
                message(3, Some("bob@quadhost"), &[], "and back"),
            ],
            &roster,
        );

        assert_eq!(
            printed,
            "#1  2026-09-04T09:00:00Z  ·  alice@macbookpro joined\n\
             \n\
             #2  2026-09-04T09:00:00Z  alice@macbookpro  → bob\n\
             \x20   first\n\
             \n\
             \x20   third\n\
             \n\
             #3  2026-09-04T09:00:00Z  bob@quadhost  → all\n\
             \x20   and back\n"
        );
    }

    #[test]
    fn a_recipient_is_shortened_only_while_the_name_is_its_own() {
        let alone = [participant("bob@quadhost", None, None)];
        assert_eq!(short_name("bob@quadhost", &alone), "bob");

        let shared = [
            participant("bob@quadhost", None, None),
            participant("bob@macbookpro", None, None),
        ];
        assert_eq!(short_name("bob@quadhost", &shared), "bob@quadhost");
        // Anything that is not an identity is printed as it came.
        assert_eq!(short_name("bob", &shared), "bob");
    }

    #[test]
    fn the_participant_listing_says_how_far_each_one_has_read() {
        let mut read = participant("bob@quadhost", None, None);
        read.read_cursor = 42;
        let table = participant_table("brisk-otter", &[read]);
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[0].contains("READ"), "{:?}", lines[0]);
        assert!(lines[1].contains("42"), "{:?}", lines[1]);

        let read_column = |line: &str| line.find("42").or_else(|| line.find("READ"));
        assert_eq!(read_column(lines[0]), read_column(lines[1]));
    }

    #[test]
    fn a_body_is_the_words_as_typed() {
        assert_eq!(
            message_body(&["hello".to_string(), "there".to_string()]).expect("body"),
            "hello there"
        );
        // A lone `-` means standard input, which this test does not feed.
        assert_eq!(
            message_body(&["-".to_string(), "x".to_string()]).expect("body"),
            "- x"
        );
    }

    #[test]
    fn an_argument_that_is_only_whitespace_is_not_an_argument() {
        assert_eq!(trimmed(Some("  brisk  ")).as_deref(), Some("brisk"));
        assert_eq!(trimmed(Some("   ")), None);
        assert_eq!(trimmed(None), None);
    }
}
