//! `saneha skill` and `saneha init`: the instructions an agent reads, and
//! putting them where each harness will find them.
//!
//! Nothing here needs a server. `init` is run with `HOME` pointed at a
//! temporary directory holding whichever harness directories the case is
//! about, so a run says what it would say on a machine with exactly those
//! harnesses on it and touches nothing of the machine it actually runs on.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The skill as the repository holds it, which is what `skill` must print and
/// what `init` must write (with the marker added).
fn repository_skill() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/saneha/SKILL.md");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// A home directory with the given harness skills directories already in it,
/// and nothing else.
fn home_with(harnesses: &[&str]) -> TempDir {
    let home = tempfile::tempdir().expect("temporary home");
    for harness in harnesses {
        std::fs::create_dir_all(home.path().join(harness)).expect("a harness skills directory");
    }
    home
}

/// The `saneha` binary with `HOME` pointed at this home and nothing of the
/// running machine's own environment that would change the answer.
fn saneha(home: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_saneha"))
        .args(args)
        .env("HOME", home.path())
        .output()
        .expect("run the saneha binary")
}

fn stdout_of(what: &str, output: &Output) -> String {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn installed_at(home: &TempDir, harness: &str) -> PathBuf {
    home.path().join(harness).join("saneha").join("SKILL.md")
}

/// A home whose one harness cannot be written into: a directory sits exactly
/// where the SKILL.md goes. It is the failure a test can arrange on any
/// machine and in any container — a read-only directory is not, because a
/// process running as root writes into one regardless.
fn home_that_cannot_be_written() -> TempDir {
    let home = home_with(&[".claude/skills"]);
    std::fs::create_dir_all(installed_at(&home, ".claude/skills")).expect("a directory in the way");
    home
}

#[test]
fn skill_prints_the_file_in_the_repository_byte_for_byte() {
    let home = home_with(&[]);
    let output = saneha(&home, &["skill"]);
    assert_eq!(stdout_of("skill", &output), repository_skill());
}

#[test]
fn the_skill_says_what_a_harness_needs_to_find_and_follow_it() {
    let skill = repository_skill();
    let frontmatter = skill
        .split("\n---\n")
        .next()
        .expect("a frontmatter block")
        .to_string();
    assert!(frontmatter.starts_with("---\nname: saneha\n"), "{skill}");
    assert!(frontmatter.contains("\ndescription: "), "{skill}");
    for word in ["channel", "agent", "handoff", "saneha"] {
        assert!(
            frontmatter.to_lowercase().contains(word),
            "the description has to trigger on {word:?}: {frontmatter}"
        );
    }

    // The wake loop is the point of the skill, so its four exit codes and the
    // background rule are all there in the body.
    for said in [
        "saneha wait",
        "run_in_background",
        "SANEHA_URL",
        "saneha leave",
        "saneha close",
        "<repo>-<harness>@<host>",
    ] {
        assert!(skill.contains(said), "the skill has to say {said:?}");
    }

    // The things an agent got wrong, or would have, are said outright. Each of
    // these is a real failure somebody hit; losing the sentence brings it back.
    for said in [
        // The CLI does not carry a granted name from one command to the next.
        "the CLI does not remember the grant",
        // Two hosts running the same repository derive the same short name.
        "@saneha-claude@otherhost",
        // Shell state does not survive between harness tool calls.
        "does not reach the next",
        // An unrecognised harness needs telling on every verb, not just join.
        "--harness codex",
        // A system message wakes a plain wait and does not want an answer.
        "it needs no reply",
        // Two waits on one channel means two wakes and two replies.
        "exactly one wait at a time",
        // A close prints what arrived with it; read takes it.
        "`read` once more",
    ] {
        assert!(skill.contains(said), "the skill has to say {said:?}");
    }

    // CONTEXT.md's avoid-list: the machine part of an identity is the host.
    assert!(
        !skill.contains("this machine's short hostname"),
        "the host is called the host"
    );
}

#[test]
fn init_installs_into_every_harness_that_is_there() {
    let home = home_with(&[".claude/skills", ".copilot/skills"]);
    let output = saneha(&home, &["init"]);
    let said = stdout_of("init", &output);

    // The marker is the last field of the frontmatter, so name and description
    // stay where a harness and a reader both expect them.
    let marker = format!("saneha-managed: {}\n---\n", env!("CARGO_PKG_VERSION"));
    for harness in [".claude/skills", ".copilot/skills"] {
        let path = installed_at(&home, harness);
        let written = std::fs::read_to_string(&path).expect("the installed skill");
        assert!(written.starts_with("---\nname: saneha\n"), "{written}");
        assert!(written.contains(&marker), "{written}");
        // The marker is the only difference from the repository's own copy.
        assert_eq!(
            written.replacen(
                &format!("saneha-managed: {}\n", env!("CARGO_PKG_VERSION")),
                "",
                1
            ),
            repository_skill()
        );
        assert!(
            said.contains("installed") && said.contains(&path.display().to_string()),
            "{said}"
        );
    }
    assert!(
        said.contains("Claude Code") && said.contains("Copilot CLI"),
        "{said}"
    );
}

#[test]
fn a_second_init_says_it_is_up_to_date_and_leaves_the_file_alone() {
    let home = home_with(&[".claude/skills"]);
    stdout_of("first init", &saneha(&home, &["init"]));

    let path = installed_at(&home, ".claude/skills");
    let first = std::fs::read_to_string(&path).expect("the installed skill");
    let before = std::fs::metadata(&path).expect("metadata").modified().ok();

    let said = stdout_of("second init", &saneha(&home, &["init"]));
    assert!(said.contains("up to date"), "{said}");
    assert!(!said.contains("installed"), "{said}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("still there"),
        first,
        "an up-to-date install must not rewrite the file"
    );
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").modified().ok(),
        before
    );
}

#[test]
fn an_older_saneha_skill_is_updated_in_place() {
    let home = home_with(&[".claude/skills"]);
    let path = installed_at(&home, ".claude/skills");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("the saneha directory");
    std::fs::write(
        &path,
        "---\nname: saneha\nsaneha-managed: 0.0.1\n---\n\nwhatever it used to say\n",
    )
    .expect("an older skill");

    let said = stdout_of("init", &saneha(&home, &["init"]));
    assert!(said.contains("updated"), "{said}");
    let written = std::fs::read_to_string(&path).expect("the installed skill");
    assert!(written.contains("## The wake loop"), "{written}");
    assert!(
        written.contains(&format!("saneha-managed: {}\n", env!("CARGO_PKG_VERSION"))),
        "{written}"
    );
}

#[test]
fn a_skill_saneha_did_not_write_is_skipped_and_left_exactly_as_it_was() {
    let home = home_with(&[".claude/skills"]);
    let path = installed_at(&home, ".claude/skills");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("the saneha directory");
    let theirs = "---\nname: saneha\ndescription: somebody else's skill of the same name\n---\n\nhands off\n";
    std::fs::write(&path, theirs).expect("somebody else's skill");

    let output = saneha(&home, &["init"]);
    let said = stdout_of("init", &output);
    assert!(said.contains("skipped"), "{said}");
    assert!(
        said.contains("saneha-managed"),
        "the reason has to say why: {said}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("still there"),
        theirs,
        "a file saneha does not own must not be touched"
    );
}

#[test]
fn a_neighbouring_skill_is_never_looked_at() {
    let home = home_with(&[".claude/skills"]);
    let neighbour = home.path().join(".claude/skills/notesmith-memory/SKILL.md");
    std::fs::create_dir_all(neighbour.parent().expect("parent")).expect("a neighbour");
    let theirs = "---\nname: notesmith-memory\n---\n\nnothing to do with saneha\n";
    std::fs::write(&neighbour, theirs).expect("a neighbouring skill");

    stdout_of("init", &saneha(&home, &["init"]));
    assert_eq!(
        std::fs::read_to_string(&neighbour).expect("still there"),
        theirs
    );
    assert!(installed_at(&home, ".claude/skills").is_file());
}

#[test]
fn a_harness_that_is_not_installed_gets_no_directory_made_for_it() {
    let home = home_with(&[".claude/skills"]);
    let said = stdout_of("init", &saneha(&home, &["init"]));

    for absent in [".copilot", ".agents"] {
        assert!(
            !home.path().join(absent).exists(),
            "init must not invent {absent}"
        );
    }
    assert!(!said.contains("Copilot CLI"), "{said}");
    assert!(!said.contains("Codex"), "{said}");
}

#[test]
fn codex_is_installed_into_the_agents_directory() {
    let home = home_with(&[".agents/skills"]);
    let said = stdout_of("init", &saneha(&home, &["init"]));
    assert!(said.contains("Codex"), "{said}");
    assert!(installed_at(&home, ".agents/skills").is_file());
}

#[test]
fn no_harness_at_all_is_said_plainly_and_is_not_a_failure() {
    let home = home_with(&[]);
    let output = saneha(&home, &["init"]);
    let said = stdout_of("init", &output);
    assert!(said.contains("no harness found"), "{said}");
    // The paths it looked at are named, so a person can see what to make.
    assert!(said.contains("~/.claude/skills"), "{said}");
    assert!(said.contains("~/.agents/skills"), "{said}");
    assert!(!home.path().join(".claude").exists());
}

#[test]
fn a_dry_run_says_what_would_happen_and_writes_nothing() {
    let home = home_with(&[".claude/skills", ".copilot/skills"]);
    let said = stdout_of("dry run", &saneha(&home, &["init", "--dry-run"]));

    assert!(said.contains("dry run"), "{said}");
    assert!(said.contains("installed"), "{said}");
    for harness in [".claude/skills", ".copilot/skills"] {
        assert!(
            !installed_at(&home, harness).exists(),
            "a dry run must not write"
        );
        assert!(
            !home.path().join(harness).join("saneha").exists(),
            "a dry run must not even make the directory"
        );
    }
}

#[test]
fn init_json_says_the_same_thing_a_program_can_read() {
    let home = home_with(&[".claude/skills"]);
    let said = stdout_of("init --json", &saneha(&home, &["init", "--json"]));
    let outcomes: serde_json::Value = serde_json::from_str(&said).expect("JSON");
    let outcomes = outcomes.as_array().expect("an array");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["harness"], "Claude Code");
    assert_eq!(outcomes[0]["action"], "installed");
    assert_eq!(
        outcomes[0]["path"],
        installed_at(&home, ".claude/skills").display().to_string()
    );
}

/// Both output modes have to agree about whether the run worked. `--json`
/// returning early past the failure count is how a caller ends up believing an
/// install landed that did not.
#[test]
fn a_failed_write_exits_one_however_the_answer_was_asked_for() {
    for mode in [vec!["init"], vec!["init", "--json"]] {
        let home = home_that_cannot_be_written();
        let output = saneha(&home, &mode);
        assert!(
            !output.status.success(),
            "{mode:?} must not exit 0 after a failed write: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(output.status.code(), Some(1), "{mode:?}");
        // What did happen is still printed, whichever mode asked.
        let said = String::from_utf8_lossy(&output.stdout);
        assert!(said.contains("failed"), "{mode:?}: {said}");
        // And the error says so on standard error, not only in the code.
        let complaint = String::from_utf8_lossy(&output.stderr);
        assert!(complaint.contains("could not be written"), "{complaint}");
    }
}

/// A dry run has to predict the real run. Reporting `installed` for a path a
/// real run cannot write is the one way a dry run can lie.
#[test]
fn a_dry_run_calls_a_write_that_cannot_happen_a_failure() {
    let home = home_that_cannot_be_written();
    let output = saneha(&home, &["init", "--dry-run"]);
    let said = String::from_utf8_lossy(&output.stdout);

    assert!(said.contains("failed"), "{said}");
    assert!(!said.contains("installed"), "{said}");
    assert_eq!(output.status.code(), Some(1));
    // The directory that is in the way is still a directory.
    assert!(installed_at(&home, ".claude/skills").is_dir());
}

/// The write is a rename onto the destination, so nothing is left beside it
/// and the destination is never a half-written file.
#[test]
fn the_write_leaves_no_temporary_file_behind() {
    let home = home_with(&[".claude/skills"]);
    stdout_of("init", &saneha(&home, &["init"]));

    let directory = home.path().join(".claude/skills/saneha");
    let left: Vec<String> = std::fs::read_dir(&directory)
        .expect("the saneha directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into()
        })
        .collect();
    assert_eq!(left, vec!["SKILL.md".to_string()], "{left:?}");

    // And again over an existing file, which is the rename-onto-it case.
    stdout_of("second init", &saneha(&home, &["init"]));
    let left = std::fs::read_dir(&directory).expect("still there").count();
    assert_eq!(left, 1);
}
