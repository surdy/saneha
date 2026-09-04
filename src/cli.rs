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
    /// Download an attachment by id
    Fetch(FetchArgs),
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
    #[arg(long = "to", value_name = "NAME")]
    pub to: Vec<String>,

    /// Attach a file to the message, at most 25 MiB of it. Repeatable. Each
    /// file is uploaded first and the message carries its id; `saneha fetch`
    /// downloads one by that id
    #[arg(long = "file", value_name = "PATH")]
    pub file: Vec<PathBuf>,

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

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// The channel the attachment was sent to
    #[arg(value_name = "CHANNEL")]
    pub channel: String,

    /// The attachment id, as `saneha read` shows it
    #[arg(value_name = "ID")]
    pub id: String,

    /// Where to write it [default: the name it was attached under, in the
    /// working directory]
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,

    /// Overwrite the file if it is already there
    #[arg(long)]
    pub force: bool,
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
        Command::Fetch(args) => fetch(args),
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
///
/// `--file` is uploaded first, in the order it was given, and the message
/// carries the ids that came back. Upload before send, because a message
/// naming an attachment that does not exist yet would be a message the server
/// has to refuse; the other way round, a send that fails leaves files nothing
/// carries, and the server sweeps those up within the hour.
fn send(args: SendArgs) -> Result<()> {
    let remote = Remote::from_env()?;
    let caller = caller(args.as_name.as_deref(), args.harness.as_deref())?;
    let body = message_body(&args.body)?;

    let mut attachments = Vec::with_capacity(args.file.len());
    for file in &args.file {
        attachments.push(remote.upload_attachment(&args.channel, file)?.id);
    }

    let message = remote.send_message(
        &args.channel,
        &NewMessage {
            from: caller.identity(),
            body,
            to: args.to,
            attachments,
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
///
/// The cap is checked here as well as by the server. A body larger than a
/// message can hold is not worth a round trip, and one large enough for the
/// server to refuse before it has read the JSON would come back in the
/// framework's words rather than in saneha's.
fn message_body(words: &[String]) -> Result<String> {
    use std::io::Read;

    let body = if words.len() == 1 && words[0] == BODY_FROM_STDIN {
        let mut body = String::new();
        std::io::stdin()
            .read_to_string(&mut body)
            .context("could not read the message body from standard input")?;
        // A heredoc ends in a newline that is not part of what was written.
        body.trim_end().to_string()
    } else {
        words.join(" ")
    };

    if body.len() > crate::api::MAX_BODY {
        return Err(anyhow!(crate::api::body_too_large(body.len())));
    }
    Ok(body)
}

/// Reads a channel. By default that is the unread messages, and reading them
/// is what advances the read cursor; `--all` and `--since` are history reads
/// and move nothing (ADR-0004).
///
/// The fetch and the advance are two requests on purpose: `wait` will reuse
/// the fetch, and waiting never moves a cursor.
///
/// The order is fetch, print, then advance, and the cursor lands on the last
/// message that actually reached the reader. `saneha read brisk-otter | head`
/// closes the pipe part-way through, and a cursor moved before that would
/// leave everything after the first message neither seen nor unread.
/// Advancing last means the worst a crash, or a reader walking away, can cost
/// is reading something twice.
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
        .ok_or_else(|| anyhow!(crate::api::not_a_participant(&args.channel, &identity)))?;

    let (after, advance) = match (args.all, args.since) {
        (true, _) => (0, false),
        (false, Some(since)) => (since, false),
        (false, None) => (me.read_cursor, true),
    };

    let messages = remote.messages_after(&args.channel, after)?;
    let delivered = if args.json {
        // An array is all or nothing: half of one is not JSON.
        let printed = deliver(&format!("{}\n", serde_json::to_string_pretty(&messages)?))?;
        match printed {
            Printed::Delivered => messages.last().map(|last| last.id),
            Printed::ReaderGone => None,
        }
    } else {
        // Nothing unread prints nothing at all, so a skill can run this on a
        // loop and say something only when there is something to say.
        transcript(&messages, &participants, deliver)?
    };

    if advance {
        if let Some(last) = delivered {
            remote.set_read_cursor(&args.channel, &identity, last)?;
        }
    }
    Ok(())
}

/// Downloads one attachment by id.
///
/// It goes to `--out`, or to the name it was attached under, in the working
/// directory. An existing file is never written over unless `--force` says so:
/// a handoff document fetched twice must not quietly replace the copy that was
/// already worked on.
///
/// The bytes land in a temporary file beside the destination and are renamed
/// onto it once they have all arrived, so the path asked for holds either the
/// file that was there before or the whole attachment, and never half of
/// either. `--force` destroying a copy before the transfer that replaces it
/// has finished is exactly the accident this is here to prevent.
fn fetch(args: FetchArgs) -> Result<()> {
    let remote = Remote::from_env()?;
    let fetched = remote.fetch_attachment(&args.channel, &args.id)?;

    let destination = match args.out {
        Some(out) => out,
        None => PathBuf::from(&fetched.filename),
    };
    let shown = destination.display().to_string();

    // Claimed before anything is downloaded, and claimed by making it:
    // asking whether the file is there and then writing it would be two
    // answers with a gap between them. `--force` claims nothing, because
    // there is something there to keep until the last moment.
    let claimed = if args.force {
        None
    } else {
        Some(claim(&destination, &shown)?)
    };

    let landing = beside(&destination);
    let written = write_beside(fetched, &landing).and_then(|size| {
        std::fs::rename(&landing, &destination)
            .with_context(|| format!("could not put the attachment in place as {shown}"))?;
        Ok(size)
    });

    let size = match written {
        Ok(size) => size,
        Err(err) => {
            // Nothing arrived, so nothing is left behind: not the part that
            // did arrive, and not the empty file that claimed the name.
            let _ = std::fs::remove_file(&landing);
            if let Some(claimed) = claimed {
                let _ = std::fs::remove_file(&claimed);
            }
            return Err(err);
        }
    };
    say(&format!("{shown}  {}", human_size(size)))
}

/// Makes an empty file at `destination`, and refuses if something is already
/// there.
fn claim(destination: &std::path::Path, shown: &str) -> Result<PathBuf> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::AlreadyExists => anyhow!(
                "{shown} is already there; write it somewhere else with --out, \
                 or replace it with --force"
            ),
            _ => anyhow::Error::new(err).context(format!("could not write {shown}")),
        })?;
    Ok(destination.to_path_buf())
}

/// Where the bytes land before they are renamed onto the destination: a hidden
/// name in the same directory, so the rename is within one filesystem and
/// therefore the one step it has to be.
fn beside(destination: &std::path::Path) -> PathBuf {
    use rand::RngExt;

    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".to_string());
    let tag: u32 = rand::rng().random();
    let landing = format!(".{name}.{tag:08x}.part");
    match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(landing),
        _ => PathBuf::from(landing),
    }
}

/// Writes the attachment into `landing`, and answers with how much of it there
/// was.
fn write_beside(fetched: crate::client::Fetched, landing: &std::path::Path) -> Result<u64> {
    let shown = landing.display().to_string();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(landing)
        .with_context(|| format!("could not write {shown}"))?;
    let size = fetched.write_to(&mut file)?;
    // On the disk before it is put in place, so what the rename publishes is
    // the whole file and not the promise of one.
    file.sync_all()
        .with_context(|| format!("could not write {shown}"))?;
    Ok(size)
}

/// An argument or environment variable that is there and says something.
fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Whether what was printed reached the reader.
///
/// It is not the same question as whether the command failed. A closed pipe is
/// a success for `saneha list | head`, and it must not be one for `saneha read
/// | head`: nothing that was not delivered may be marked as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Printed {
    Delivered,
    /// The reader walked away part-way through, or before this started.
    ReaderGone,
}

/// Everything printed goes through here. A closed pipe (`saneha list | head`)
/// is the reader walking away, not a failure: Rust does not restore the
/// default SIGPIPE, so writing to one is an error the process must swallow
/// rather than a signal that ends it.
fn write_out(text: &str) -> Result<()> {
    deliver(text).map(|_| ())
}

/// The same write, with the answer to whether it arrived. Only `read` needs
/// that answer, and it needs it before it can say anything has been read.
fn deliver(text: &str) -> Result<Printed> {
    use std::io::Write;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let written = write!(out, "{text}").and_then(|()| out.flush());
    match written {
        Ok(()) => Ok(Printed::Delivered),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(Printed::ReaderGone),
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
/// what is printed is what can be typed back. A broadcast is `everyone`, which
/// nothing can be confused with: `→ all` would read as the `@all` mention,
/// which is a list of names and not the same message at all.
/// It prints one message at a time, through `print`, and answers with the id
/// of the last message that reached the reader, or `None` if none did.
///
/// One write per message rather than one for the whole lot, so that when a
/// reader walks away part-way through, what it did take can be marked as read
/// and what it did not is still waiting. Nothing that was not delivered is
/// ever counted, which is what makes `saneha read brisk-otter | head` safe.
fn transcript(
    messages: &[Message],
    participants: &[Participant],
    mut print: impl FnMut(&str) -> Result<Printed>,
) -> Result<Option<i64>> {
    let mut delivered = None;
    for (index, message) in messages.iter().enumerate() {
        // Every message but the first has a blank line before it, and it
        // belongs to the message it precedes: a message that never arrives
        // must not leave a stray line behind either.
        let separator = if index > 0 { "\n" } else { "" };
        let block = format!("{separator}{}", entry(message, participants));
        if print(&block)? == Printed::ReaderGone {
            break;
        }
        delivered = Some(message.id);
    }
    Ok(delivered)
}

/// One message: its header line, and its body set in under it.
fn entry(message: &Message, participants: &[Participant]) -> String {
    if message.kind.is_system() {
        return format!(
            "#{}  {}  ·  {}\n",
            message.id, message.created_at, message.body
        );
    }

    let from = message.from.as_deref().unwrap_or("system");
    let to = if message.recipients.is_empty() {
        "everyone".to_string()
    } else {
        message
            .recipients
            .iter()
            .map(|identity| short_name(identity, participants))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = format!(
        "#{}  {}  {}  → {}\n",
        message.id, message.created_at, from, to
    );
    for line in message.body.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("{INDENT}{line}\n"));
        }
    }
    // What the message carries, under what it says, with the id first because
    // the id is what `saneha fetch` is given.
    for attachment in &message.attachments {
        out.push_str(&format!(
            "{INDENT}{ATTACHMENT_MARK}  {}  {}  {}\n",
            attachment.id,
            attachment.filename,
            human_size(attachment.size)
        ));
    }
    out
}

/// What marks an attachment line, so a reader can tell one from a line of the
/// body without reading it. A word rather than a glyph: most of what reads a
/// transcript is a model, and a word is what a word means.
const ATTACHMENT_MARK: &str = "attachment";

/// A size as a person reads it. The exact number is in `--json`; this is for
/// deciding whether to fetch the thing.
fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    match bytes {
        bytes if bytes >= MIB => format!("{:.1} MiB", bytes as f64 / MIB as f64),
        bytes if bytes >= KIB => format!("{:.1} KiB", bytes as f64 / KIB as f64),
        1 => "1 byte".to_string(),
        bytes => format!("{bytes} bytes"),
    }
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
            attachments: Vec::new(),
            created_at: "2026-09-04T09:00:00Z".to_string(),
        }
    }

    /// One attachment, as a message carries it.
    fn attachment(id: &str, filename: &str, size: u64) -> crate::api::Attachment {
        crate::api::Attachment {
            id: id.to_string(),
            filename: filename.to_string(),
            size,
            content_type: "text/markdown".to_string(),
            created_at: "2026-09-04T09:00:00Z".to_string(),
        }
    }

    /// The three messages a rendering test needs: a system one, one addressed
    /// to somebody with a body that breaks across lines, and a broadcast.
    fn conversation() -> Vec<Message> {
        vec![
            message(1, None, &[], "alice@macbookpro joined"),
            message(
                2,
                Some("alice@macbookpro"),
                &["bob@quadhost"],
                "first\n\nthird",
            ),
            message(3, Some("bob@quadhost"), &[], "and back"),
        ]
    }

    fn roster() -> Vec<Participant> {
        vec![
            participant("alice@macbookpro", None, None),
            participant("bob@quadhost", None, None),
        ]
    }

    #[test]
    fn nothing_unread_prints_nothing_at_all() {
        let mut printed = String::new();
        let delivered = transcript(&[], &[], |block| {
            printed.push_str(block);
            Ok(Printed::Delivered)
        })
        .expect("print");

        assert_eq!(printed, "");
        // Nothing arrived because there was nothing to send, so there is
        // nothing to mark as read either.
        assert_eq!(delivered, None);
    }

    #[test]
    fn a_transcript_sets_each_body_under_its_own_header() {
        let mut printed = String::new();
        let delivered = transcript(&conversation(), &roster(), |block| {
            printed.push_str(block);
            Ok(Printed::Delivered)
        })
        .expect("print");

        assert_eq!(delivered, Some(3));
        assert_eq!(
            printed,
            "#1  2026-09-04T09:00:00Z  ·  alice@macbookpro joined\n\
             \n\
             #2  2026-09-04T09:00:00Z  alice@macbookpro  → bob\n\
             \x20   first\n\
             \n\
             \x20   third\n\
             \n\
             #3  2026-09-04T09:00:00Z  bob@quadhost  → everyone\n\
             \x20   and back\n"
        );
    }

    #[test]
    fn what_a_message_carries_is_listed_under_it() {
        let mut carrying = message(2, Some("alice@macbookpro"), &[], "the handoff");
        carrying.attachments = vec![
            attachment("0123456789abcdef0123456789abcdef", "handoff.md", 2048),
            attachment("fedcba9876543210fedcba9876543210", "notes.txt", 1),
        ];

        let printed = entry(&carrying, &roster());
        assert_eq!(
            printed,
            "#2  2026-09-04T09:00:00Z  alice@macbookpro  → everyone\n\
             \x20   the handoff\n\
             \x20   attachment  0123456789abcdef0123456789abcdef  handoff.md  2.0 KiB\n\
             \x20   attachment  fedcba9876543210fedcba9876543210  notes.txt  1 byte\n"
        );
    }

    #[test]
    fn a_size_is_written_for_someone_deciding_whether_to_fetch_it() {
        assert_eq!(human_size(0), "0 bytes");
        assert_eq!(human_size(1), "1 byte");
        assert_eq!(human_size(512), "512 bytes");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(25 * 1024 * 1024), "25.0 MiB");
    }

    #[test]
    fn a_reader_that_walks_away_leaves_the_rest_unread() {
        // The reader takes the first message and goes, the way `head` does.
        let mut printed = String::new();
        let mut taken = 0;
        let delivered = transcript(&conversation(), &roster(), |block| {
            taken += 1;
            if taken > 1 {
                return Ok(Printed::ReaderGone);
            }
            printed.push_str(block);
            Ok(Printed::Delivered)
        })
        .expect("print");

        // Only what arrived counts as read; the rest is still waiting.
        assert_eq!(delivered, Some(1));
        assert_eq!(
            printed,
            "#1  2026-09-04T09:00:00Z  ·  alice@macbookpro joined\n"
        );

        // A reader that was never there marks nothing at all.
        let delivered =
            transcript(&conversation(), &roster(), |_| Ok(Printed::ReaderGone)).expect("print");
        assert_eq!(delivered, None);

        // A write that failed for a real reason is a failure, not an empty
        // read: nothing is marked and the command says so.
        let broken = transcript(&conversation(), &roster(), |_| {
            Err(anyhow!("the disk is full"))
        });
        assert!(broken.is_err());
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
