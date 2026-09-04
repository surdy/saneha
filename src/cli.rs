//! The command line: one binary, one server subcommand, and subcommands that
//! talk to a running server.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::api::Channel;
use crate::client::{Remote, URL_ENV};
use crate::server;
use crate::store::Store;

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
}
