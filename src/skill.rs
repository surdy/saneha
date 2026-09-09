//! The instructions an agent reads to learn saneha, and the install that puts
//! them where each harness will find them.
//!
//! There is one SKILL.md and it lives in the repository, at
//! `skills/saneha/SKILL.md`. The binary carries a copy of that exact file
//! ([`SKILL`]), so `saneha skill` and `saneha init` cannot drift from what CI
//! checks and what a reader of the repository sees.
//!
//! What `init` writes is not that file. It is a short one saying where the
//! instructions are, so that updating the binary updates what an agent is told
//! and no copy is left to go stale (ADR-0006).
//!
//! The installed file is a file saneha owns. It carries a `saneha-managed`
//! field in its frontmatter, and `init` will only ever write over a file that
//! carries it. A `SKILL.md` at the same path without the marker is somebody
//! else's and is left exactly as it is.
//!
//! The marker says only that the file is saneha's. It named the version that
//! wrote it until 0.2.0, which was harmless while the version never moved and
//! a chore the moment it did: the staleness check compares the whole file, so
//! every release would have made every installed pointer stale and asked every
//! machine to run `init` again — the per-change chore ADR-0006 exists to end.
//! A pointer's whole claim is that the instructions are in the binary, so a
//! version stamped into it was the one part of that claim it did not keep.

use std::path::{Path, PathBuf};

/// The skill itself, as it sits in the repository.
pub const SKILL: &str = include_str!("../skills/saneha/SKILL.md");

/// The frontmatter field that says saneha wrote this file, and may write over
/// it again.
pub const MARKER: &str = "saneha-managed";

/// The digest of the skill this build carries.
///
/// The crate version cannot answer "are we running the same saneha", even now
/// that it moves: a release is cut when something is deployed, which is not
/// the same event as the instructions changing, and a build from `main`
/// between two releases carries the version of the older one. The skill's own
/// bytes move exactly when the instructions an agent follows move, which is
/// the thing worth noticing. So this is what the server reports and what a
/// client compares against its own.
pub fn digest() -> &'static str {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST.get_or_init(|| crate::digest_hex(SKILL.as_bytes()))
}

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

/// The body of the file `init` installs: a pointer at the binary, not a copy
/// of the instructions.
///
/// What an agent is told and what will actually happen have to be the same
/// thing, and a copy on disk is a second place for the truth to live. A copy
/// goes stale the moment the binary is updated and stays stale until somebody
/// remembers to run `init` again, which on the machine that prompted this went
/// unremembered for days. A pointer cannot: `saneha skill` prints what the
/// binary that will run the commands carries, so the two are the same file by
/// construction rather than by anybody's diligence.
///
/// What stays here is what a pointer cannot do without. The orientation, so a
/// person opening the file learns what saneha is. And the fallback, because an
/// agent that cannot run the binary can do nothing else this file might have
/// told it, and should say so rather than invent verbs.
const POINTER_BODY: &str = r#"# saneha

**Run `saneha skill` and follow what it prints.** Your instructions are not in
this file. They live in the binary that will run your commands, so that what
you are told and what will happen cannot drift apart. It is about two hundred
lines, needs no server, and is the whole of it: the verbs, how your identity is
worked out, the wake loop, and how to hand work over to a fresh session.

saneha is a self-hosted channel where coding agents on different machines, and
the person running them, talk to each other. Every instruction in it is a
`saneha` command in a shell; there is no MCP server.

If `saneha` is not on your PATH, it is usually at `~/.cargo/bin/saneha`, which
is where `cargo install` puts it. If it is not there either, ask the person
where it is. If it cannot be run at all, tell them — do not guess at the
commands, because nothing in this file says what they are.
"#;

/// The file `init` writes: the skill's own frontmatter, the marker, and the
/// pointer body.
///
/// The frontmatter is lifted from the skill rather than written out again. It
/// is what makes a harness load this at the right moment, so it is the one
/// part that has to stay on disk — and a second copy of it, hand-maintained,
/// would drift in exactly the way the body no longer can.
pub fn installed() -> String {
    // The embedded skill has frontmatter and `the_skill_has_the_frontmatter_a_harness_reads`
    // says so, so this cannot fail in a build that passed its own tests. It is
    // an expect rather than a fallback because the obvious fallback — install
    // the skill whole — writes the one file this module exists to stop writing,
    // and writes it without the marker, so no later `init` would ever touch it
    // again and nothing would ever say why.
    let end = frontmatter_end(SKILL).expect("the embedded skill has frontmatter; a test says so");
    format!("{}{MARKER}: true\n---\n\n{POINTER_BODY}", &SKILL[..end])
}

/// Whether this file is one of saneha's, and so may be written over.
///
/// Only the frontmatter is looked at: a `saneha-managed:` line in the body,
/// which is exactly what a document *about* this marker would contain, does
/// not make a file saneha's to overwrite.
///
/// Any value counts, which is what makes the pointers written before 0.2.0 —
/// the ones stamped `saneha-managed: 0.1.0` — still saneha's to replace. They
/// are replaced once, by the next `init` on that machine, and then stop
/// moving.
pub fn managed(text: &str) -> bool {
    let Some(end) = frontmatter_end(text) else {
        return false;
    };
    text[..end].lines().any(|line| {
        line.strip_prefix(MARKER)
            .is_some_and(|rest| rest.starts_with(':'))
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

/// Points every harness found under `home` at this binary.
///
/// Nothing outside `<skills dir>/saneha/SKILL.md` is created, read or written.
/// With `dry_run` the answer is what would happen and not one byte moves.
pub fn install(home: &Path, dry_run: bool) -> Vec<Outcome> {
    let wanted = installed();
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
            if !managed(&there) {
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
        assert!(!managed(SKILL));
    }

    #[test]
    fn the_installed_file_is_marked_as_saneha_s_and_says_no_version() {
        let text = installed();
        assert!(managed(&text), "{text}");
        assert!(text.starts_with("---\nname: saneha\n"), "{text}");
        // The version is deliberately not in it: the pointer would otherwise
        // go stale on every release and ask every machine to run `init` again.
        assert!(
            !text.contains(env!("CARGO_PKG_VERSION")),
            "the pointer must not move when the version does: {text}"
        );
    }

    #[test]
    fn a_pointer_written_before_0_2_0_is_still_saneha_s_to_replace() {
        // Those carry `saneha-managed: 0.1.0`. If the marker stopped counting
        // them, every machine's pointer would become somebody else's file and
        // `init` would refuse it forever.
        assert!(managed(
            "---\nname: saneha\nsaneha-managed: 0.1.0\n---\nbody\n"
        ));
    }

    #[test]
    fn the_installed_file_is_the_skill_s_own_frontmatter_and_a_pointer() {
        let text = installed();

        // The frontmatter is lifted, not written out again, so the line that
        // decides when a harness loads this cannot drift from the skill's.
        let front_of = |s: &str| s.split("\n---\n").next().unwrap().to_string();
        let theirs = front_of(SKILL);
        let mine = front_of(&text).replace("\nsaneha-managed: true", "");
        assert_eq!(mine, theirs);

        // And the body is a pointer rather than a copy: the instructions are
        // fetched from the binary that will run the commands.
        assert!(text.contains("Run `saneha skill`"), "{text}");
        assert!(
            !text.contains("## The wake loop"),
            "the instructions must not be copied in here: {text}"
        );
        // Short enough that it is obviously not the instructions themselves.
        let lines = text.lines().count();
        assert!(
            lines <= 30,
            "the installed file is {lines} lines; keep it small"
        );
    }

    #[test]
    fn a_marker_outside_the_frontmatter_marks_nothing() {
        assert!(!managed("saneha-managed: true\n"));
        assert!(!managed("---\nname: other\n---\n\nsaneha-managed: true\n"));
        // An unterminated frontmatter block is not one.
        assert!(!managed("---\nsaneha-managed: true\n"));
        // A file with no frontmatter at all.
        assert!(!managed("# just a document\n"));
    }

    #[test]
    fn a_marker_is_read_wherever_it_sits_in_the_frontmatter() {
        assert!(managed(
            "---\nname: saneha\nsaneha-managed:  true  \n---\nbody\n"
        ));
        assert!(managed("---\nsaneha-managed: true\nname: saneha\n---\n"));
        // A field that merely starts the same way is a different field.
        assert!(!managed("---\nsaneha-managed-by: someone\n---\n"));
    }
}
