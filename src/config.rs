//! Where the server address comes from when the environment does not say.
//!
//! `SANEHA_URL` was the only answer for the whole of v1, and the documented
//! way to make it stick was an export in a shell profile. That works for a
//! person and is unreliable for an agent: a harness runs each command in a
//! fresh non-interactive shell, and whether such a shell reads a profile
//! depends on the harness and on the shell. The result is an address that is
//! there when the person tries it and missing when the agent does, with
//! nothing on either side saying why.
//!
//! So the binary resolves it rather than the shell, which is the move
//! [ADR-0006] already made for the instructions themselves: one place, no
//! second copy to drift, and no dependence on what a harness happens to load.
//! The file is written by `saneha init --url`, because that is already the
//! step taken once per machine.
//!
//! The environment still wins. Tests point a child at a server on a port the
//! OS picked, deploy scripts name the server they mean, and a one-off against
//! a local `saneha serve` is a prefix on one command — all of which have to go
//! on beating whatever is on disk.
//!
//! [ADR-0006]: ../docs/adr/0006-the-installed-skill-is-a-pointer.md

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The file's name inside the configuration directory.
const CONFIG_FILE: &str = "config.json";

/// What the file holds. JSON because `serde_json` is already here and a
/// configuration format is not worth a dependency; one field because
/// [ADR-0007] says a channel name is a whole address and stays one.
///
/// Unknown fields are kept rather than refused, so a file written by a newer
/// saneha still parses here and this one does not silently drop what it does
/// not understand when it rewrites it.
///
/// [ADR-0007]: ../docs/adr/0007-the-binary-resolves-the-server.md
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    /// The server every verb talks to when `SANEHA_URL` is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Anything a newer saneha wrote, carried through a rewrite untouched.
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// Where the file lives: `$XDG_CONFIG_HOME/saneha/config.json`, falling back
/// to `$HOME/.config/saneha/config.json`, the same shape
/// [`crate::server::default_database_path`] uses for the database.
///
/// Nothing when neither variable is set. That is not an error to report on
/// every command: a machine with no home directory has nowhere to have been
/// configured, and the answer is the same as an empty file.
pub fn path() -> Option<PathBuf> {
    if let Some(config_home) = env_path("XDG_CONFIG_HOME") {
        return Some(config_home.join("saneha").join(CONFIG_FILE));
    }
    env_path("HOME").map(|home| home.join(".config").join("saneha").join(CONFIG_FILE))
}

/// What the file says, or an empty config.
///
/// A file that is not there is not a problem — it is a machine nobody has run
/// `saneha init --url` on. A file that is there and unreadable is, because
/// somebody meant something by it and acting as though it were absent would
/// point every verb somewhere else without saying so.
pub fn load() -> Result<Config> {
    let Some(path) = path() else {
        return Ok(Config::default());
    };
    read(&path)
}

/// The same, from a named path, which is what the tests use.
pub fn read(path: &Path) -> Result<Config> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("could not read {}", path.display()))
            );
        }
    };
    if raw.trim().is_empty() {
        return Ok(Config::default());
    }
    serde_json::from_str(&raw).with_context(|| {
        format!(
            "{} is not valid saneha configuration; fix it or delete it and run: \
             saneha init --url <URL>",
            path.display()
        )
    })
}

/// Writes the URL into the file, keeping everything else that was in it.
///
/// The write is a temporary file and a rename, for the reason `skill::install`
/// gives: a truncate interrupted halfway leaves a file that parses as nothing
/// and points every later command at no server at all.
pub fn set_url(path: &Path, url: &str) -> Result<()> {
    let mut config = read(path)?;
    config.url = Some(url.to_string());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not make {}", parent.display()))?;
    }

    let body = format!("{}\n", serde_json::to_string_pretty(&config)?);
    let temporary = path.with_extension("json.part");
    std::fs::write(&temporary, body)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("could not put {} in place", path.display()))?;
    Ok(())
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory to write a config into, removed when the test ends.
    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("saneha-config-{}", crate::api::mint_id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn a_machine_nobody_configured_is_not_an_error() {
        // The common case on a fresh machine, and it must not turn every verb
        // into a failure about a missing file: the address may still be in the
        // environment, which is what is asked first.
        let missing = scratch().join("config.json");
        let config = read(&missing).expect("an absent file is an empty config");
        assert!(config.url.is_none());
    }

    #[test]
    fn an_empty_file_says_nothing_rather_than_failing() {
        let path = scratch().join("config.json");
        std::fs::write(&path, "   \n").expect("write");
        assert!(read(&path).expect("blank is empty").url.is_none());
    }

    #[test]
    fn a_file_that_is_not_configuration_says_what_to_do() {
        // Acting as though it were absent would point every verb somewhere
        // else without saying so, which is the one outcome worth failing over.
        let path = scratch().join("config.json");
        std::fs::write(&path, "url = https://example.invalid\n").expect("write");
        let err = read(&path).expect_err("a TOML-looking file is refused");
        let said = format!("{err:#}");
        assert!(said.contains("init --url"), "it says the fix: {said}");
    }

    #[test]
    fn what_a_newer_saneha_wrote_survives_a_rewrite() {
        // This binary does not know every key the file may hold, and dropping
        // one on write would quietly undo whatever wrote it.
        let path = scratch().join("config.json");
        std::fs::write(&path, r#"{"url":"http://old.invalid","kept":{"a":1}}"#).expect("write");
        set_url(&path, "http://new.invalid").expect("set");

        let raw = std::fs::read_to_string(&path).expect("read back");
        assert!(raw.contains("new.invalid"), "the new address is in: {raw}");
        assert!(
            raw.contains("\"kept\""),
            "and what it did not understand: {raw}"
        );
    }

    #[test]
    fn the_address_is_written_whole_or_not_at_all() {
        // A rename rather than a truncate: a write cut short must not leave a
        // file that parses as nothing and points every command at no server.
        let path = scratch().join("config.json");
        set_url(&path, "http://first.invalid").expect("first");
        set_url(&path, "http://second.invalid").expect("second");

        assert_eq!(
            read(&path).expect("read").url.as_deref(),
            Some("http://second.invalid")
        );
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("a parent"))
            .expect("list")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".part"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temporary left behind: {leftovers:?}"
        );
    }
}
