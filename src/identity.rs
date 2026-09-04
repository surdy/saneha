//! Who the caller is: the `name@host` identity a join claims, worked out from
//! the machine, the harness and the directory the command was run in.
//!
//! Identity is `name@host`. The host is the machine's short hostname,
//! lowercased. The name is `--as`, else `SANEHA_AS`, else
//! `<repo-basename>-<harness>`. The harness is recognised from an environment
//! marker, overridden by `--harness`, and is `unknown` when nothing matches.

use std::path::Path;
use std::process::{Command, Stdio};

/// The environment variable that names the participant outright, ahead of any
/// derived name but behind `--as`.
pub const AS_ENV: &str = "SANEHA_AS";

/// The harness recorded when no marker matches and none was given. It is a
/// real value rather than a null so an identity always reads the same way.
pub const UNKNOWN_HARNESS: &str = "unknown";

/// The host used when the machine will not say what it is called.
const UNKNOWN_HOST: &str = "localhost";

/// One harness and the environment it leaves behind. Adding a harness is one
/// entry here: the id that goes into an identity, the variable whose presence
/// means "running inside this", and the variable holding its session id, if it
/// publishes one.
#[derive(Debug, Clone, Copy)]
pub struct HarnessMarker {
    /// The id that appears in a derived name and in the participant record.
    pub id: &'static str,
    /// Set, and not empty, when running inside this harness.
    pub env: &'static str,
    /// Where this harness publishes the id of the session in progress.
    pub session_env: Option<&'static str>,
}

/// The harnesses this build recognises, tried in order.
///
/// Only Claude Code is here: it is the one whose marker is confirmed. Codex
/// and Copilot CLI leave something behind too, but not something this project
/// has verified on a machine where they are installed, so guessing at
/// `CODEX_*`-shaped names would detect the wrong thing rather than nothing.
/// When either is confirmed it is one more entry, and nothing else changes.
pub const HARNESSES: &[HarnessMarker] = &[HarnessMarker {
    id: "claude",
    env: "CLAUDECODE",
    session_env: Some("CLAUDE_CODE_SESSION_ID"),
}];

/// The harness this process is running inside, or `None` if nothing matched.
pub fn detect_harness() -> Option<&'static HarnessMarker> {
    detect_harness_in(env_value)
}

/// The detection itself, over any environment, so it can be tested without
/// touching this process's own.
pub fn detect_harness_in(
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<&'static HarnessMarker> {
    HARNESSES
        .iter()
        .find(|harness| lookup(harness.env).is_some())
}

/// A recognised harness by id, so `--harness claude` still knows where the
/// session id lives.
pub fn harness_marker(id: &str) -> Option<&'static HarnessMarker> {
    HARNESSES.iter().find(|harness| harness.id == id)
}

/// The id of the harness session in progress, when the harness publishes one.
pub fn session_id(harness: &str) -> Option<String> {
    session_id_in(harness, env_value)
}

/// The session lookup over any environment, for tests.
pub fn session_id_in(harness: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    harness_marker(harness)
        .and_then(|marker| marker.session_env)
        .and_then(lookup)
}

/// A set, non-empty environment variable. Empty is the same as unset: an
/// exported-but-blank marker means nothing.
fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The process that started this command. It is not the harness session, but
/// it is a handle on the same terminal, so a later join can ask whether that
/// terminal is still there.
pub fn parent_pid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(std::os::unix::process::parent_id())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Whether a process is still running on this machine.
///
/// `kill -0` sends no signal and only reports whether the process is there.
/// Not every image ships a `kill` binary, so `ps -p` is the fallback; if
/// neither runs, the honest answer is "cannot tell", and the caller treats
/// that as not live, so a join resumes rather than multiplying identities.
pub fn is_alive(pid: u32) -> bool {
    let pid = pid.to_string();
    for (program, args) in [("kill", ["-0", pid.as_str()]), ("ps", ["-p", pid.as_str()])] {
        let status = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(status) = status {
            return status.success();
        }
    }
    false
}

/// This machine's short hostname, lowercased and reduced to the characters an
/// identity may hold.
pub fn host() -> String {
    let raw = hostname_command()
        .or_else(|| env_value("HOSTNAME"))
        .unwrap_or_default();
    short_host(&raw)
}

fn hostname_command() -> Option<String> {
    let output = Command::new("hostname")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// The short host: everything before the first dot, lowercased, with anything
/// an identity cannot hold folded to a hyphen.
pub fn short_host(raw: &str) -> String {
    let short = raw.split('.').next().unwrap_or_default();
    let cleaned = sanitize(short);
    if cleaned.is_empty() {
        UNKNOWN_HOST.to_string()
    } else {
        cleaned
    }
}

/// The basename of the git repository this command was run in, falling back to
/// the basename of the working directory when it was not run in one.
pub fn repo_basename() -> String {
    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|toplevel| !toplevel.is_empty());

    let directory = match toplevel {
        Some(toplevel) => Some(std::path::PathBuf::from(toplevel)),
        None => std::env::current_dir().ok(),
    };
    directory
        .as_deref()
        .and_then(basename)
        .unwrap_or_default()
        .to_string()
}

fn basename(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

/// The derived name: the repository basename and the harness, joined, folded
/// into the characters a name may hold and cut to `max` characters.
pub fn derived_name(repo: &str, harness: &str, max: usize) -> String {
    let joined = sanitize(&format!("{repo}-{harness}"));
    if joined.chars().count() <= max {
        return joined;
    }
    let cut: String = joined.chars().take(max).collect();
    cut.trim_end_matches('-').to_string()
}

/// Folds arbitrary text into the alphabet a name or a host may use: lowercase
/// letters, digits and single hyphens, with no hyphen at either end.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn claude_code_is_recognised_by_its_marker() {
        let marker = detect_harness_in(environment(&[("CLAUDECODE", "1")])).expect("claude");
        assert_eq!(marker.id, "claude");
        assert_eq!(marker.session_env, Some("CLAUDE_CODE_SESSION_ID"));
    }

    #[test]
    fn an_unmarked_environment_matches_nothing() {
        assert!(detect_harness_in(environment(&[("SHELL", "/bin/bash")])).is_none());
        assert!(detect_harness_in(environment(&[])).is_none());
    }

    #[test]
    fn a_session_id_is_read_from_the_harness_that_publishes_one() {
        let env = environment(&[("CLAUDE_CODE_SESSION_ID", "abc-123")]);
        assert_eq!(session_id_in("claude", &env).as_deref(), Some("abc-123"));
        // An unrecognised harness has nowhere to read a session id from.
        assert_eq!(session_id_in(UNKNOWN_HARNESS, &env), None);
    }

    #[test]
    fn a_host_is_short_and_lowercase() {
        assert_eq!(short_host("MacBookPro.local"), "macbookpro");
        assert_eq!(short_host("quadhost"), "quadhost");
        assert_eq!(short_host("build_box.example.com"), "build-box");
        assert_eq!(short_host(""), UNKNOWN_HOST);
        assert_eq!(short_host("..."), UNKNOWN_HOST);
    }

    #[test]
    fn a_derived_name_is_the_repository_and_the_harness() {
        assert_eq!(
            derived_name("notes-method", "claude", 64),
            "notes-method-claude"
        );
        assert_eq!(
            derived_name("saneha", UNKNOWN_HARNESS, 64),
            "saneha-unknown"
        );
        assert_eq!(derived_name("My_Repo", "claude", 64), "my-repo-claude");
        assert_eq!(derived_name("", "claude", 64), "claude");
    }

    #[test]
    fn a_derived_name_is_cut_to_the_cap_without_a_trailing_hyphen() {
        let long = "a".repeat(200);
        let name = derived_name(&long, "claude", 64);
        assert_eq!(name.chars().count(), 64);
        assert!(!name.ends_with('-'), "{name}");

        // The cut lands exactly on a hyphen, which must not be left dangling.
        assert_eq!(derived_name(&"b".repeat(9), "claude", 10), "bbbbbbbbb");
    }

    #[test]
    fn sanitising_collapses_runs_and_trims_the_ends() {
        assert_eq!(sanitize("  Brisk   Otter  "), "brisk-otter");
        assert_eq!(sanitize("-leading-and-trailing-"), "leading-and-trailing");
        assert_eq!(sanitize("ਸੁਨੇਹਾ"), "");
        assert_eq!(sanitize("agent/2"), "agent-2");
    }

    #[test]
    fn this_process_is_alive_and_a_pid_that_cannot_exist_is_not() {
        assert!(is_alive(std::process::id()));
        assert!(!is_alive(u32::MAX));
    }
}
