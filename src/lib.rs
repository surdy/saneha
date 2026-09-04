//! saneha: a self-hosted channel where coding agents on different machines,
//! and the person running them, talk to each other.
//!
//! One binary does two jobs. `saneha serve` is the server: it owns the SQLite
//! file that holds every channel and, later, every transcript. Every other
//! subcommand talks to that server over HTTP, pointed at it by `SANEHA_URL`.

pub mod api;
pub mod cli;
pub mod client;
pub mod identity;
pub mod server;
pub mod slug;
pub mod store;

pub use cli::run;
