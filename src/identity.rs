//! Who the caller is: the `name@host` identity a join claims, worked out from
//! the host, the harness and the project the command was run in.
//!
//! Identity is `name@host`. The host is the short hostname of the host it
//! runs on, lowercased. The name is `--as`, else `SANEHA_AS`, else
//! `<repo-basename>-<harness>`. The harness is recognised from an environment
//! marker, overridden by `--harness`, and is `unknown` when nothing matches.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::store::sanitize;

/// The environment variable that names the participant outright, ahead of any
/// derived name but behind `--as`.
pub const AS_ENV: &str = "SANEHA_AS";

/// The harness recorded when no marker matches and none was given. It is a
/// real value rather than a null so an identity always reads the same way.
pub const UNKNOWN_HARNESS: &str = "unknown";

/// The host recorded when this one will not say what it is called.
const UNKNOWN_HOST: &str = "localhost";

/// One harness and the environment it leaves behind. Adding a harness is one
/// entry here: the id that goes into an identity, the variable whose presence
/// means "running inside this", the variable holding the id of the session in
/// progress, and the variable holding the pid of the harness itself.
#[derive(Debug, Clone, Copy)]
pub struct HarnessMarker {
    /// The id that appears in a derived name and in the participant record.
    pub id: &'static str,
    /// Set, and not empty, when running inside this harness.
    pub env: &'static str,
    /// Where this harness publishes the id of the session in progress.
    pub session_env: Option<&'static str>,
    /// Where this harness publishes its own process id.
    ///
    /// This is the process that lives as long as the session does. It is not
    /// the parent of the `saneha` process: a harness that runs commands
    /// through a shell gives each one a fresh, short-lived parent, and a
    /// person at a terminal gives every command the same shell, which outlives
    /// the harness entirely. Either way the parent answers a different
    /// question from the one a join is asking.
    pub pid_env: Option<&'static str>,
}

/// The harnesses this build recognises, tried in order.
///
/// Only Claude Code is here: it is the one whose markers are confirmed. Codex
/// and Copilot CLI leave something behind too, but not something this project
/// has verified on a host where they are installed, so guessing at
/// `CODEX_*`-shaped names would detect the wrong thing rather than nothing.
/// When either is confirmed it is one more entry, and nothing else changes.
pub const HARNESSES: &[HarnessMarker] = &[HarnessMarker {
    id: "claude",
    env: "CLAUDECODE",
    session_env: Some("CLAUDE_CODE_SESSION_ID"),
    pid_env: Some("CLAUDE_PID"),
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
/// session id and the pid live.
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

/// The process whose life is this participant's presence: the harness itself
/// when it publishes its pid, and the process that started this command when
/// it does not.
///
/// The fallback is weak on purpose. It is better than nothing for a harness
/// this build does not know, and a join under it resumes rather than
/// multiplying identities when it is wrong.
pub fn pid(harness: &str) -> Option<u32> {
    pid_in(harness, env_value)
}

/// The pid lookup over any environment, for tests.
pub fn pid_in(harness: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<u32> {
    harness_marker(harness)
        .and_then(|marker| marker.pid_env)
        .and_then(&lookup)
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|pid| *pid > 0)
        .or_else(parent_pid)
}

/// The process that started this command.
fn parent_pid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(std::os::unix::process::parent_id())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// When a running process started, as the system reports it.
///
/// A pid on its own is not an identity: pids are reused, so a finished session
/// can be impersonated by whatever the kernel handed the number to next. The
/// start time pins it. `ps` is asked because it is everywhere and costs no
/// dependency; if it will not answer, the caller reads that as "not running",
/// which resumes rather than multiplies.
pub fn process_start(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // `lstart` is padded for a column; the value is the words, not the spaces.
    let started = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if started.is_empty() {
        None
    } else {
        Some(started)
    }
}

/// Whether the process recorded at join time is the one running now: the pid
/// is alive and it started when the record says it did. An unrecorded start
/// time is not proof of anything, so it is not live.
pub fn still_running(pid: u32, started_at: Option<&str>) -> bool {
    match (started_at, process_start(pid)) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => false,
    }
}

/// A set, non-empty environment variable. Empty is the same as unset: an
/// exported-but-blank marker means nothing.
fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// This host's own short hostname, lowercased, reduced to the characters an
/// identity may hold and cut to `max` characters.
pub fn host(max: usize) -> String {
    let raw = hostname_command()
        .or_else(|| env_value("HOSTNAME"))
        .unwrap_or_default();
    short_host(&raw, max)
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
/// an identity cannot hold folded to a hyphen, cut to `max`. A host that is
/// too long is cut rather than refused: it is not something the caller can
/// fix, and it must not stand between an agent and a channel.
pub fn short_host(raw: &str, max: usize) -> String {
    let short = raw.split('.').next().unwrap_or_default();
    let cleaned = cut(sanitize(short), max);
    if cleaned.is_empty() {
        UNKNOWN_HOST.to_string()
    } else {
        cleaned
    }
}

/// The project this command was run in: the basename of the repository when it
/// was run in one, and the basename of the working directory when it was not.
///
/// The repository is found through git's *common* directory, so every worktree
/// of one repository answers with the repository's own name. The name says
/// which project is talking; where that project is checked out is a placement
/// detail, and two live checkouts on one host is what suffixing is for.
pub fn project_basename() -> String {
    if let Some(common) = git_common_dir() {
        if let Some(name) = project_name(&common) {
            return name;
        }
    }
    if let Some(toplevel) = git(&["rev-parse", "--show-toplevel"]) {
        if let Some(name) = basename(Path::new(&toplevel)) {
            return name.to_string();
        }
    }
    std::env::current_dir()
        .ok()
        .and_then(|cwd| basename(&cwd).map(str::to_string))
        .unwrap_or_default()
}

/// The project a git common directory belongs to. `/w/madari/.git` and every
/// worktree of it answer `madari`, and so does a bare `/w/madari.git`.
pub fn project_name(common_dir: &Path) -> Option<String> {
    let name = common_dir.file_name()?.to_str()?;
    if name == ".git" {
        return common_dir.parent().and_then(basename).map(str::to_string);
    }
    Some(name.strip_suffix(".git").unwrap_or(name).to_string())
}

/// Git's common directory: the one directory every worktree of a repository
/// shares.
fn git_common_dir() -> Option<PathBuf> {
    // git 2.31 and newer can be asked for an absolute path outright.
    if let Some(absolute) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        return Some(PathBuf::from(absolute));
    }
    // Older git answers relative to the working directory.
    let path = PathBuf::from(git(&["rev-parse", "--git-common-dir"])?);
    if path.is_absolute() {
        return Some(path);
    }
    Some(std::env::current_dir().ok()?.join(path))
}

/// One line of git's answer, or `None` when git will not answer at all: not
/// installed, or not a repository.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let answer = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if answer.is_empty() {
        None
    } else {
        Some(answer)
    }
}

fn basename(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

/// The derived name: the project basename and the harness, joined, folded into
/// the characters a name may hold and cut to `max` characters.
pub fn derived_name(project: &str, harness: &str, max: usize) -> String {
    cut(sanitize(&format!("{project}-{harness}")), max)
}

/// A value cut to `max` characters without leaving a hyphen dangling at the
/// cut.
fn cut(value: String, max: usize) -> String {
    if value.chars().count() <= max {
        return value;
    }
    let cut: String = value.chars().take(max).collect();
    cut.trim_end_matches('-').to_string()
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
        assert_eq!(marker.pid_env, Some("CLAUDE_PID"));
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
    fn the_recorded_pid_is_the_harness_and_not_the_parent() {
        let parent = parent_pid().expect("a parent on unix");

        // The harness publishes its own pid, and that is the one recorded.
        let env = environment(&[("CLAUDE_PID", "4242")]);
        assert_eq!(pid_in("claude", &env), Some(4242));

        // Nothing published: fall back to the process that started this one.
        assert_eq!(pid_in("claude", environment(&[])), Some(parent));
        assert_eq!(pid_in(UNKNOWN_HARNESS, &env), Some(parent));

        // Nonsense, and a pid nothing can be: the fallback, not a bad number.
        assert_eq!(
            pid_in("claude", environment(&[("CLAUDE_PID", "not-a-pid")])),
            Some(parent)
        );
        assert_eq!(
            pid_in("claude", environment(&[("CLAUDE_PID", "0")])),
            Some(parent)
        );
    }

    #[test]
    fn a_process_is_running_only_when_it_started_when_the_record_says() {
        let mine = std::process::id();
        let started = process_start(mine).expect("this process is running");

        assert!(still_running(mine, Some(&started)));
        // The pid is alive, but it is not the process that was recorded: this
        // is what pid reuse looks like.
        assert!(!still_running(mine, Some("Thu Jan  1 00:00:00 2015")));
        // Nothing recorded is not evidence of anything.
        assert!(!still_running(mine, None));
        // A pid that cannot exist.
        assert!(!still_running(u32::MAX, Some(&started)));
        assert_eq!(process_start(u32::MAX), None);
    }

    #[test]
    fn a_host_is_short_and_lowercase() {
        assert_eq!(short_host("MacBookPro.local", 64), "macbookpro");
        assert_eq!(short_host("quadhost", 64), "quadhost");
        assert_eq!(short_host("build_box.example.com", 64), "build-box");
        assert_eq!(short_host("", 64), UNKNOWN_HOST);
        assert_eq!(short_host("...", 64), UNKNOWN_HOST);
    }

    #[test]
    fn a_host_too_long_for_an_identity_is_cut_rather_than_refused() {
        let host = short_host(&"h".repeat(200), 64);
        assert_eq!(host.chars().count(), 64);
        crate::store::validate_host(&host).expect("a cut host is still a host");
    }

    #[test]
    fn a_project_is_the_repository_every_worktree_shares() {
        let name = |path: &str| project_name(Path::new(path));
        assert_eq!(name("/w/madari/.git").as_deref(), Some("madari"));
        // A worktree's common directory is the main repository's own.
        assert_eq!(name("/w/madari/.git/").as_deref(), Some("madari"));
        // A bare repository has no `.git` directory to sit inside.
        assert_eq!(name("/w/madari.git").as_deref(), Some("madari"));
        assert_eq!(name("/w/madari").as_deref(), Some("madari"));
        assert_eq!(name("/"), None);
    }

    #[test]
    fn a_derived_name_is_the_project_and_the_harness() {
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
}
