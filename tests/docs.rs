//! Every public function says what it is for.
//!
//! This exists because of a defect the compiler cannot see and review keeps
//! missing: a doc comment landing on the wrong item. It has happened three
//! times here — `Message::wakes`'s four paragraphs sat above `written_by`
//! ([#75](https://github.com/surdy/saneha/issues/75)), and 8f0b543 fixed the
//! same thing for `warn`. The diff reads correctly in both places, because the
//! comment is right and only its position is wrong.
//!
//! What is checkable is the wreckage. A comment that moves onto its neighbour
//! leaves the function it was written for bare, so a public function with no
//! doc comment is the footprint of the theft. That is what this asserts.
//!
//! It is deliberately about functions and not about fields: turning on
//! `missing_docs` for the crate reports 160 items, most of them wire-struct
//! fields whose meaning is in the struct's own comment, and a rule that noisy
//! would be turned off rather than obeyed.

/// Every module in the crate, as source text. `include_str!` is relative to
/// this file, and reading the sources is the only way to ask a question about
/// where a comment sits — by the time anything is compiled, a doc comment that
/// landed on the wrong item is simply that item's doc comment.
const SOURCES: &[(&str, &str)] = &[
    ("api.rs", include_str!("../src/api.rs")),
    ("cli.rs", include_str!("../src/cli.rs")),
    ("client.rs", include_str!("../src/client.rs")),
    ("config.rs", include_str!("../src/config.rs")),
    ("identity.rs", include_str!("../src/identity.rs")),
    ("lib.rs", include_str!("../src/lib.rs")),
    ("mention.rs", include_str!("../src/mention.rs")),
    ("server.rs", include_str!("../src/server.rs")),
    ("skill.rs", include_str!("../src/skill.rs")),
    ("slug.rs", include_str!("../src/slug.rs")),
    ("store.rs", include_str!("../src/store.rs")),
];

/// Whether this line declares a public function.
fn declares_public_fn(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("pub fn ") || line.starts_with("pub async fn ")
}

/// The line above a declaration, stepping over the attributes between them.
///
/// An attribute sits under the doc comment rather than over it, so
/// `#[allow(...)]` between the two must not read as the comment being absent.
fn line_above<'a>(lines: &[&'a str], at: usize) -> Option<&'a str> {
    let mut above = at.checked_sub(1)?;
    while lines[above].trim_start().starts_with("#[") {
        above = above.checked_sub(1)?;
    }
    Some(lines[above])
}

#[test]
fn every_public_function_has_a_doc_comment() {
    let mut bare = Vec::new();
    for (name, source) in SOURCES {
        let lines: Vec<&str> = source.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            if !declares_public_fn(line) {
                continue;
            }
            let documented = line_above(&lines, at)
                .map(|above| above.trim_start().starts_with("///"))
                .unwrap_or(false);
            if !documented {
                bare.push(format!("{name}:{} {}", at + 1, line.trim()));
            }
        }
    }

    assert!(
        bare.is_empty(),
        "these public functions have no doc comment. If one of them is a \
         function whose comment you can see just above its neighbour, that is \
         the bug this test is for:\n  {}",
        bare.join("\n  ")
    );
}

#[test]
fn the_check_can_tell_a_documented_function_from_a_bare_one() {
    // A test that only ever passes proves nothing about what it would catch,
    // and this one's whole value is in the case it fails on.
    let stolen: Vec<&str> = vec![
        "    /// Whether this wakes somebody.",
        "    pub fn written_by(&self, identity: &str) -> bool {",
        "",
        "    pub fn wakes(&self, identity: &str) -> bool {",
    ];
    assert!(declares_public_fn(stolen[3]), "the victim is a public fn");
    assert!(
        !line_above(&stolen, 3)
            .expect("a line above")
            .trim_start()
            .starts_with("///"),
        "and it is bare, which is the footprint the theft leaves"
    );

    let attributed: Vec<&str> = vec![
        "    /// Says what it does.",
        "    #[allow(dead_code)]",
        "    pub fn documented() {}",
    ];
    assert!(
        line_above(&attributed, 2)
            .expect("a line above")
            .trim_start()
            .starts_with("///"),
        "an attribute between the two is stepped over, not mistaken for absence"
    );
}
