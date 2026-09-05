//! The instructions an agent reads to learn saneha, and the install that puts
//! them where each harness will find them.
//!
//! There is one SKILL.md and it lives in the repository, at
//! `skills/saneha/SKILL.md`. The binary carries a copy of that exact file
//! ([`SKILL`]), so `saneha skill` and `saneha init` cannot drift from what CI
//! checks and what a reader of the repository sees.
//!
//! The installed file is a file saneha owns. It carries a `saneha-managed`
//! field in its frontmatter naming the version that wrote it, and `init` will
//! only ever write over a file that carries it. A `SKILL.md` at the same path
//! without the marker is somebody else's and is left exactly as it is.

use std::path::{Path, PathBuf};

/// The skill itself, as it sits in the repository.
pub const SKILL: &str = include_str!("../skills/saneha/SKILL.md");

/// The frontmatter field that says saneha wrote this file, and may write over
/// it again.
pub const MARKER: &str = "saneha-managed";

/// The directory `init` makes inside a harness's own skills directory. One
/// skill, one directory, named for the skill.
const SKILL_DIR: &str = "saneha";

/// The file every harness looks for, spelled exactly this way. Codex scans for
/// `SKILL.md` case-sensitively, so `SKILL.MD` is a skill nothing finds.
const SKILL_FILE: &str = "SKILL.md";

/// A harness and where it keeps the skills a person installs for themselves.
///
/// Detection is the directory existing. A harness that is installed has made
/// its own directory; saneha makes only the `saneha/` directory inside one,
/// never the harness's directory itself, because creating `~/.codex/skills` on
/// a machine with no Codex on it would be saneha inventing a harness.
#[derive(Debug, Clone, Copy)]
pub struct Harness {
    /// The harness, as `init` names it in its output.
    pub name: &'static str,
    /// Its user-level skills directory, relative to the home directory.
    pub skills_dir: &'static str,
}

/// Where each harness reads user-level skills from.
///
/// Codex is `~/.agents/skills` rather than `~/.codex/skills`: the OpenAI
/// documentation lists `$HOME/.agents/skills` as the personal scope and
/// `~/.codex/` as configuration only.
pub const HARNESSES: &[Harness] = &[
    Harness {
        name: "Claude Code",
        skills_dir: ".claude/skills",
    },
    Harness {
        name: "Copilot CLI",
        skills_dir: ".copilot/skills",
    },
    Harness {
        name: "Codex",
        skills_dir: ".agents/skills",
    },
];

/// What `init` did about one harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// There was no file there and now there is one.
    Installed,
    /// There was a saneha skill there and it said something else.
    Updated,
    /// There was a saneha skill there and it already says this.
    UpToDate,
    /// There is a `SKILL.md` there that saneha did not write.
    Skipped,
    /// The write did not happen.
    Failed,
}

impl Action {
    /// The word `init` prints in its first column.
    pub fn word(self) -> &'static str {
        match self {
            Action::Installed => "installed",
            Action::Updated => "updated",
            Action::UpToDate => "up to date",
            Action::Skipped => "skipped",
            Action::Failed => "failed",
        }
    }
}

/// One harness, and what happened to its copy of the skill.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Outcome {
    pub harness: &'static str,
    pub path: PathBuf,
    pub action: Action,
    /// Why, when the word alone does not say it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
}

/// The skill as it is written to disk: the embedded file with the ownership
/// marker added to its frontmatter.
///
/// The marker goes in last, immediately before the closing `---`, so the
/// `name` and `description` a harness reads stay where a reader expects them.
pub fn managed(version: &str) -> String {
    let Some(end) = frontmatter_end(SKILL) else {
        // No frontmatter to add to. The embedded skill has one and a test says
        // so; this is only here so a malformed file installs as itself rather
        // than as something with a marker line stuck at the top.
        return SKILL.to_string();
    };
    let mut out = String::with_capacity(SKILL.len() + MARKER.len() + version.len() + 3);
    out.push_str(&SKILL[..end]);
    out.push_str(MARKER);
    out.push_str(": ");
    out.push_str(version);
    out.push('\n');
    out.push_str(&SKILL[end..]);
    out
}

/// The version saneha stamped into a file's frontmatter, or `None` when the
/// file is not one of saneha's.
///
/// Only the frontmatter is looked at: a `saneha-managed:` line in the body,
/// which is exactly what a document *about* this marker would contain, does
/// not make a file saneha's to overwrite.
pub fn managed_version(text: &str) -> Option<String> {
    let end = frontmatter_end(text)?;
    text[..end].lines().find_map(|line| {
        let value = line.strip_prefix(MARKER)?.strip_prefix(':')?;
        Some(value.trim().to_string())
    })
}

/// The byte offset of the closing `---` of a YAML frontmatter block, which is
/// where a new field is inserted and where the search for one stops.
///
/// A frontmatter block is the first line of the file being `---` and a later
/// line being `---` on its own. Anything else is a file without frontmatter.
fn frontmatter_end(text: &str) -> Option<usize> {
    let body = text.strip_prefix("---\n")?;
    let mut offset = "---\n".len();
    for line in body.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Installs the skill into every harness found under `home`.
///
/// Nothing outside `<skills dir>/saneha/SKILL.md` is created, read or written.
/// With `dry_run` the answer is what would happen and not one byte moves.
pub fn install(home: &Path, version: &str, dry_run: bool) -> Vec<Outcome> {
    let wanted = managed(version);
    HARNESSES
        .iter()
        .filter(|harness| home.join(harness.skills_dir).is_dir())
        .map(|harness| {
            let path = home
                .join(harness.skills_dir)
                .join(SKILL_DIR)
                .join(SKILL_FILE);
            let (action, because) = one(&path, &wanted, dry_run);
            Outcome {
                harness: harness.name,
                path,
                action,
                because,
            }
        })
        .collect()
}

/// What to do about one path, and the doing of it.
fn one(path: &Path, wanted: &str, dry_run: bool) -> (Action, Option<String>) {
    // A directory where the file goes is nothing this can install over, and it
    // is checked before the read so that a dry run says the same thing the
    // real run would rather than promising an install that cannot happen.
    if path.is_dir() {
        return (
            Action::Failed,
            Some("a directory is in the way of the file".to_string()),
        );
    }
    match std::fs::read_to_string(path) {
        Ok(there) => {
            if managed_version(&there).is_none() {
                return (
                    Action::Skipped,
                    Some("not ours: no saneha-managed marker".to_string()),
                );
            }
            if there == wanted {
                return (Action::UpToDate, None);
            }
            write(path, wanted, dry_run, Action::Updated)
        }
        // Nothing there, or nothing readable. Either way this is a fresh
        // install, and the write is what reports whatever is really wrong.
        Err(_) => write(path, wanted, dry_run, Action::Installed),
    }
}

/// Writes the skill, making only the `saneha/` directory that holds it.
///
/// The write is atomic: the bytes go to a temporary file beside the
/// destination and are renamed onto it, which on this platform replaces it in
/// one step. `std::fs::write` truncates first, so an interruption partway
/// through would leave a stub without the marker in it — and a file without
/// the marker is one every later `init` refuses to touch, which would need a
/// person to go and delete it.
fn write(path: &Path, wanted: &str, dry_run: bool, done: Action) -> (Action, Option<String>) {
    if dry_run {
        return (done, None);
    }
    match write_atomically(path, wanted) {
        Ok(()) => (done, None),
        Err(err) => (Action::Failed, Some(err.to_string())),
    }
}

fn write_atomically(path: &Path, wanted: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    // The rename is what publishes the file, so a failure after this point
    // must not leave the temporary behind for the next run to trip over.
    if let Err(err) = std::fs::write(&temporary, wanted) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_skill_is_the_file_in_the_repository() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/saneha/SKILL.md");
        let on_disk = std::fs::read_to_string(&repo).expect("the skill in the repository");
        assert_eq!(SKILL, on_disk);
    }

    #[test]
    fn the_skill_has_the_frontmatter_a_harness_reads() {
        let end = frontmatter_end(SKILL).expect("frontmatter");
        let front = &SKILL[..end];
        assert!(front.contains("\nname: saneha\n"), "{front}");
        assert!(front.contains("\ndescription: "), "{front}");
        // The repository copy is not marked; only what init writes is.
        assert_eq!(managed_version(SKILL), None);
    }

    #[test]
    fn the_installed_skill_carries_the_version_that_wrote_it() {
        let text = managed("0.1.0");
        assert_eq!(managed_version(&text).as_deref(), Some("0.1.0"));
        // Everything else about the file is untouched.
        assert_eq!(text.replace("saneha-managed: 0.1.0\n", ""), SKILL);
        assert!(text.starts_with("---\nname: saneha\n"), "{text}");
    }

    #[test]
    fn a_marker_outside_the_frontmatter_marks_nothing() {
        assert_eq!(managed_version("saneha-managed: 9.9.9\n"), None);
        assert_eq!(
            managed_version("---\nname: other\n---\n\nsaneha-managed: 9.9.9\n"),
            None
        );
        // An unterminated frontmatter block is not one.
        assert_eq!(managed_version("---\nsaneha-managed: 9.9.9\n"), None);
        // A file with no frontmatter at all.
        assert_eq!(managed_version("# just a document\n"), None);
    }

    #[test]
    fn a_marker_is_read_wherever_it_sits_in_the_frontmatter() {
        assert_eq!(
            managed_version("---\nname: saneha\nsaneha-managed:  0.2.0  \n---\nbody\n").as_deref(),
            Some("0.2.0")
        );
        assert_eq!(
            managed_version("---\nsaneha-managed: 0.2.0\nname: saneha\n---\n").as_deref(),
            Some("0.2.0")
        );
    }
}
