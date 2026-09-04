//! Mentions: the `@name` in a message body that names a recipient.
//!
//! A mention counts only in prose, and only at the start of a line or after
//! whitespace, so `mail me at a@b.com` addresses nobody and `@bob` addresses
//! bob. The token after the `@` is a name in the alphabet an identity uses,
//! letters, digits and hyphens, so it ends at the first character a name
//! cannot hold and a trailing `.`, `,` or `:` is punctuation rather than part
//! of the name. It is folded to lowercase, because an identity is lowercase
//! and a person typing `@Beta` in the viewer means beta. `@name@host` names an
//! identity outright; `@all` names everyone in the channel.
//!
//! Code is not prose. A message between coding agents carries code, and
//! `@dataclass` in a Python block or `npm i @types/node` in a shell one must
//! not fail the send. So a fenced block is skipped from its opening fence to
//! its matching closing one, an inline code span is skipped whole, and an
//! `@name` followed immediately by `/` is a package scope rather than a
//! mention. An unterminated fence makes the rest of the body code, which is
//! the reading that refuses least.
//!
//! Resolution is separate from parsing: parsing says what was written, and
//! resolution says who that is in one channel. A mention that names nobody, or
//! a short form that names two people, is an error rather than text, so a
//! message never quietly goes to fewer participants than it looks like it does.

/// The name that means every participant in the channel, away ones included.
pub const EVERYONE: &str = "all";

/// One mention, as it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mention {
    /// `@all`: every participant in the channel.
    Everyone,
    /// `@name`: one participant, if exactly one in the channel has that name.
    Short(String),
    /// `@name@host`: an identity, which either exists here or does not.
    Full { name: String, host: String },
}

impl Mention {
    /// The mention as an error quotes it back, which is how it was written.
    pub fn written(&self) -> String {
        match self {
            Mention::Everyone => format!("@{EVERYONE}"),
            Mention::Short(name) => format!("@{name}"),
            Mention::Full { name, host } => format!("@{name}@{host}"),
        }
    }
}

/// A participant as recipient resolution sees one: the row it stands for, the
/// identity it is addressed by, and the name half a short form matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Whatever the caller uses to point back at this participant.
    pub id: i64,
    pub identity: String,
    pub name: String,
}

/// A mention that names nobody, or names too many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// Nobody in the channel answers to this.
    Unknown { written: String },
    /// A short form that more than one participant answers to.
    Ambiguous {
        written: String,
        /// The identities that match, in the order they joined.
        candidates: Vec<String>,
    },
}

/// Every mention in a body, in the order they were written, each one once.
/// Only the prose is looked at: fenced blocks and inline code spans are what
/// they say they are and address nobody.
pub fn mentions(body: &str) -> Vec<Mention> {
    let mut found: Vec<Mention> = Vec::new();
    let mut fence: Option<Fence> = None;

    for line in body.lines() {
        match &fence {
            // Inside a fenced block, the only thing being looked for is the
            // fence that ends it.
            Some(open) => {
                if closes(line, open) {
                    fence = None;
                }
            }
            None => match opens(line) {
                Some(open) => fence = Some(open),
                None => {
                    for (prose, at_boundary) in prose_runs(line) {
                        scan(prose, at_boundary, &mut found);
                    }
                }
            },
        }
    }
    found
}

/// An open code fence: the character it is made of and how long it is, both of
/// which the fence that closes it has to match.
struct Fence {
    marker: char,
    length: usize,
}

/// The fence a line opens, if it opens one. The info string after it, as in
/// ```` ```python ````, is part of the fence line rather than of the block.
fn opens(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = trimmed.chars().take_while(|c| *c == marker).count();
    if length < 3 {
        return None;
    }
    Some(Fence { marker, length })
}

/// Whether a line closes the fence that is open: the same character, at least
/// as long, and nothing else on the line.
fn closes(line: &str, open: &Fence) -> bool {
    let trimmed = line.trim();
    let length = trimmed.chars().take_while(|c| *c == open.marker).count();
    length >= open.length && trimmed.chars().count() == length
}

/// The runs of prose in one line: what is left once the inline code spans are
/// taken out, each with whether it begins somewhere a mention may begin. Only
/// the start of the line is such a place: a run that begins where a code span
/// ended begins against a backtick, which is not whitespace.
fn prose_runs(line: &str) -> Vec<(&str, bool)> {
    let mut runs: Vec<(&str, bool)> = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0;
    let mut at_boundary = true;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let ticks = backticks_at(bytes, index);
        match closing_run(bytes, index + ticks, ticks) {
            Some(close) => {
                runs.push((&line[start..index], at_boundary));
                index = close + ticks;
                start = index;
                at_boundary = false;
            }
            // Backticks with no partner are text, so the line reads on.
            None => index += ticks,
        }
    }
    runs.push((&line[start..], at_boundary));
    runs
}

/// How many backticks start at `from`.
fn backticks_at(bytes: &[u8], from: usize) -> usize {
    bytes[from..].iter().take_while(|b| **b == b'`').count()
}

/// Where the run of exactly `ticks` backticks that closes a code span starts,
/// if the line holds one. A longer run does not close it and is stepped over
/// whole, the way markdown reads ``` `` ` `` ```.
fn closing_run(bytes: &[u8], from: usize, ticks: usize) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let length = backticks_at(bytes, index);
        if length == ticks {
            return Some(index);
        }
        index += length;
    }
    None
}

/// Every mention in one run of prose, told whether the run begins somewhere a
/// mention may begin. A mention starts a line or follows whitespace; nothing
/// else counts, which is what leaves an email address alone.
fn scan(prose: &str, at_boundary: bool, found: &mut Vec<Mention>) {
    let mut after_space = at_boundary;
    let mut cursor = 0;

    while cursor < prose.len() {
        let rest = &prose[cursor..];
        let c = rest
            .chars()
            .next()
            .expect("cursor is on a character boundary");
        if c == '@' && after_space {
            if let Some((mention, used)) = at(rest) {
                if !found.contains(&mention) {
                    found.push(mention);
                }
                cursor += used;
                after_space = false;
                continue;
            }
        }
        after_space = c.is_whitespace();
        cursor += c.len_utf8();
    }
}

/// A `--to` value, which is a mention written without having to say `@`:
/// `bob`, `@bob`, `bob@quadhost` and `all` all say what they look like.
/// `None` when it is not shaped like one at all, which the caller reports the
/// same way as a mention naming nobody.
pub fn given(value: &str) -> Option<Mention> {
    let value = value.trim();
    let value = value.strip_prefix('@').unwrap_or(value);
    let (mention, used) = at(&format!("@{value}"))?;
    // Half a value is not the value: `bob!` names nobody rather than bob.
    if used != value.len() + 1 {
        return None;
    }
    Some(mention)
}

/// Who the mentions name, as ids from the roster, each participant once and in
/// the order they joined. An empty answer is a broadcast.
pub fn resolve(mentions: &[Mention], roster: &[Candidate]) -> Result<Vec<i64>, Unresolved> {
    let mut chosen: Vec<i64> = Vec::new();
    for mention in mentions {
        match mention {
            Mention::Everyone => {
                for candidate in roster {
                    push(&mut chosen, candidate.id);
                }
            }
            Mention::Full { name, host } => {
                let identity = format!("{name}@{host}");
                let found = roster.iter().find(|c| c.identity == identity);
                match found {
                    Some(candidate) => push(&mut chosen, candidate.id),
                    None => {
                        return Err(Unresolved::Unknown {
                            written: mention.written(),
                        })
                    }
                }
            }
            Mention::Short(name) => {
                let matching: Vec<&Candidate> = roster.iter().filter(|c| &c.name == name).collect();
                match matching.as_slice() {
                    [only] => push(&mut chosen, only.id),
                    [] => {
                        return Err(Unresolved::Unknown {
                            written: mention.written(),
                        })
                    }
                    many => {
                        return Err(Unresolved::Ambiguous {
                            written: mention.written(),
                            candidates: many.iter().map(|c| c.identity.clone()).collect(),
                        })
                    }
                }
            }
        }
    }
    // Sorted by roster order rather than by mention order, so who a message is
    // addressed to reads the same however it was written.
    chosen.sort_by_key(|id| {
        roster
            .iter()
            .position(|c| c.id == *id)
            .unwrap_or(usize::MAX)
    });
    Ok(chosen)
}

fn push(chosen: &mut Vec<i64>, id: i64) {
    if !chosen.contains(&id) {
        chosen.push(id);
    }
}

/// One mention at the front of `rest`, which starts with `@`, and how many
/// bytes of it the mention used.
fn at(rest: &str) -> Option<(Mention, usize)> {
    let after = rest.strip_prefix('@')?;
    let name = name_prefix(after)?;
    let mut used = '@'.len_utf8() + name.len();
    let name = name.to_ascii_lowercase();

    let mention = match rest[used..]
        .strip_prefix('@')
        .and_then(|after_name| name_prefix(after_name))
    {
        Some(host) => {
            used += '@'.len_utf8() + host.len();
            Mention::Full {
                name,
                host: host.to_ascii_lowercase(),
            }
        }
        None if name == EVERYONE => Mention::Everyone,
        None => Mention::Short(name),
    };

    // `@types/node` is a package, not a person. A name that runs straight into
    // a path separator was never addressing anybody.
    if rest[used..].starts_with('/') {
        return None;
    }
    Some((mention, used))
}

/// The name at the front of `value`, as it was written: the run of characters
/// a name may hold, with a trailing hyphen dropped because a name does not end
/// in one. `None` when there is no name there at all, which is what makes `@`
/// on its own, `@ bob` and `@-bob` text rather than mentions.
fn name_prefix(value: &str) -> Option<&str> {
    let end = value
        .find(|c: char| !is_name_char(c))
        .unwrap_or(value.len());
    let name = value[..end].trim_end_matches('-');
    if name.is_empty() || name.starts_with('-') {
        return None;
    }
    Some(name)
}

/// The alphabet a mention is written in. An identity holds only the lowercase
/// half of it, so a name is folded before it is resolved; what is not in it at
/// all ends the token, which is what leaves the full stop at the end of a
/// sentence out of the name.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(name: &str) -> Mention {
        Mention::Short(name.to_string())
    }

    fn full(name: &str, host: &str) -> Mention {
        Mention::Full {
            name: name.to_string(),
            host: host.to_string(),
        }
    }

    #[test]
    fn a_mention_starts_the_body_or_follows_whitespace() {
        assert_eq!(mentions("@bob hello"), vec![short("bob")]);
        assert_eq!(mentions("hello @bob"), vec![short("bob")]);
        assert_eq!(mentions("hello\n@bob"), vec![short("bob")]);
        assert_eq!(mentions("hello\t@bob"), vec![short("bob")]);
        // Anything else in front of it is not whitespace, so it is not a
        // mention: the rule is deliberately narrow.
        assert_eq!(mentions("hello(@bob)"), vec![]);
        assert_eq!(mentions("re:@bob"), vec![]);
    }

    #[test]
    fn an_email_address_is_not_a_mention() {
        assert_eq!(mentions("write to a@b.com about it"), vec![]);
        assert_eq!(mentions("a@b.com"), vec![]);
        assert_eq!(mentions("harpreet.surdy@gmail.com"), vec![]);
    }

    #[test]
    fn a_mention_ends_at_punctuation_a_name_cannot_hold() {
        assert_eq!(mentions("@bob."), vec![short("bob")]);
        assert_eq!(
            mentions("@bob, @carol: done"),
            vec![short("bob"), short("carol")]
        );
        assert_eq!(mentions("@bob's turn"), vec![short("bob")]);
        assert_eq!(mentions("@bob!"), vec![short("bob")]);
        assert_eq!(mentions("(@bob) no, @carol"), vec![short("carol")]);
        // A name does not end in a hyphen, so a trailing one is not part of it.
        assert_eq!(mentions("@bob- said"), vec![short("bob")]);
        assert_eq!(
            mentions("@saneha-claude works"),
            vec![short("saneha-claude")]
        );
    }

    #[test]
    fn a_full_mention_carries_its_host() {
        assert_eq!(mentions("@bob@quadhost hi"), vec![full("bob", "quadhost")]);
        assert_eq!(mentions("@bob@quadhost."), vec![full("bob", "quadhost")]);
        // A host that is not there leaves a short form and some text.
        assert_eq!(mentions("@bob@ hi"), vec![short("bob")]);
        assert_eq!(mentions("@bob@-nowhere"), vec![short("bob")]);
    }

    #[test]
    fn a_mention_is_folded_to_the_case_an_identity_is_written_in() {
        // A person typing in the viewer capitalises a name; an identity never
        // is, so the mention is folded rather than left to address nobody.
        assert_eq!(mentions("@Beta please look"), vec![short("beta")]);
        assert_eq!(mentions("@BETA"), vec![short("beta")]);
        assert_eq!(mentions("@Bob@QuadHost"), vec![full("bob", "quadhost")]);
        assert_eq!(mentions("@All"), vec![Mention::Everyone]);
        assert_eq!(given("Beta"), Some(short("beta")));
    }

    #[test]
    fn code_is_not_prose() {
        // A fenced block, whichever fence it is made of.
        let fenced = "look at this\n\
                      ```python\n\
                      @dataclass\n\
                      class Thing: ...\n\
                      ```\n\
                      @bob what do you think";
        assert_eq!(mentions(fenced), vec![short("bob")]);

        let tilde = "~~~\n@dataclass\n~~~\nplain @bob";
        assert_eq!(mentions(tilde), vec![short("bob")]);

        // A longer fence is closed only by one at least as long, and the info
        // string is part of the fence line.
        let nested = "````md\n```\n@dataclass\n```\n````\n@bob";
        assert_eq!(mentions(nested), vec![short("bob")]);

        // An inline code span, and prose either side of one.
        assert_eq!(mentions("run `npm i @types/node` first"), vec![]);
        assert_eq!(
            mentions("`@dataclass` is what @bob meant"),
            vec![short("bob")]
        );
        assert_eq!(mentions("``@a `@b` @c`` @bob"), vec![short("bob")]);
        // Backticks with no partner are text, so the line still reads.
        assert_eq!(mentions("` @bob"), vec![short("bob")]);
        // A run that begins where a code span ended begins against a backtick,
        // which is not whitespace.
        assert_eq!(mentions("`code`@bob"), vec![]);

        // A package scope is not a person, fenced or not.
        assert_eq!(mentions("npm i @types/node"), vec![]);
        assert_eq!(mentions("@bob/thing"), vec![]);
        assert_eq!(mentions("@bob@quadhost/thing"), vec![]);
    }

    #[test]
    fn a_fence_that_is_never_closed_makes_the_rest_of_the_body_code() {
        let unterminated = "here is the file\n```\n@dataclass\n@bob\nstill code";
        assert_eq!(mentions(unterminated), vec![]);
    }

    #[test]
    fn everyone_is_its_own_mention() {
        assert_eq!(mentions("@all please look"), vec![Mention::Everyone]);
        assert_eq!(mentions("@all"), vec![Mention::Everyone]);
        // Only the bare short form; `all@host` is an identity like any other.
        assert_eq!(mentions("@all@quadhost"), vec![full("all", "quadhost")]);
    }

    #[test]
    fn nothing_that_is_not_a_name_is_a_mention() {
        for body in ["@", "@ bob", "@-bob", "@_bob", "@ਸੁਨੇਹਾ", "email@", "@@bob"] {
            assert_eq!(mentions(body), vec![], "{body:?}");
        }
    }

    #[test]
    fn a_mention_written_twice_is_one_recipient() {
        assert_eq!(mentions("@bob and again @bob"), vec![short("bob")]);
        assert_eq!(
            mentions("@bob @bob@quadhost"),
            vec![short("bob"), full("bob", "quadhost")]
        );
    }

    #[test]
    fn multibyte_text_does_not_shift_the_scan() {
        assert_eq!(mentions("ਸੁਨੇਹਾ @bob"), vec![short("bob")]);
        assert_eq!(mentions("— @bob"), vec![short("bob")]);
        assert_eq!(mentions("ਸੁਨੇਹਾ@bob"), vec![]);
    }

    #[test]
    fn a_given_recipient_may_be_written_with_or_without_the_at() {
        assert_eq!(given("bob"), Some(short("bob")));
        assert_eq!(given("@bob"), Some(short("bob")));
        assert_eq!(given(" bob "), Some(short("bob")));
        assert_eq!(given("bob@quadhost"), Some(full("bob", "quadhost")));
        assert_eq!(given("@bob@quadhost"), Some(full("bob", "quadhost")));
        assert_eq!(given("all"), Some(Mention::Everyone));
        // Anything that is not a whole name names nobody.
        for value in [
            "",
            "@",
            "bob!",
            "bob@",
            "bob@quadhost!",
            "bob bob",
            "types/node",
        ] {
            assert_eq!(given(value), None, "{value:?}");
        }
    }

    fn roster() -> Vec<Candidate> {
        vec![
            Candidate {
                id: 1,
                identity: "alice@macbookpro".to_string(),
                name: "alice".to_string(),
            },
            Candidate {
                id: 2,
                identity: "bob@macbookpro".to_string(),
                name: "bob".to_string(),
            },
            Candidate {
                id: 3,
                identity: "bob@quadhost".to_string(),
                name: "bob".to_string(),
            },
            Candidate {
                id: 4,
                identity: "carol@quadhost".to_string(),
                name: "carol".to_string(),
            },
        ]
    }

    #[test]
    fn a_short_form_resolves_when_only_one_participant_answers_to_it() {
        assert_eq!(resolve(&[short("alice")], &roster()), Ok(vec![1]));
        assert_eq!(resolve(&[full("bob", "quadhost")], &roster()), Ok(vec![3]));
    }

    #[test]
    fn an_ambiguous_short_form_names_the_candidates() {
        let err = resolve(&[short("bob")], &roster()).expect_err("two bobs");
        assert_eq!(
            err,
            Unresolved::Ambiguous {
                written: "@bob".to_string(),
                candidates: vec!["bob@macbookpro".to_string(), "bob@quadhost".to_string()],
            }
        );
    }

    #[test]
    fn a_mention_naming_nobody_is_unresolved() {
        assert_eq!(
            resolve(&[short("dave")], &roster()),
            Err(Unresolved::Unknown {
                written: "@dave".to_string()
            })
        );
        assert_eq!(
            resolve(&[full("alice", "quadhost")], &roster()),
            Err(Unresolved::Unknown {
                written: "@alice@quadhost".to_string()
            })
        );
    }

    #[test]
    fn everyone_is_everyone_in_the_channel() {
        assert_eq!(
            resolve(&[Mention::Everyone], &roster()),
            Ok(vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn recipients_are_listed_once_and_in_roster_order() {
        let written = [short("carol"), Mention::Everyone, short("alice")];
        assert_eq!(resolve(&written, &roster()), Ok(vec![1, 2, 3, 4]));

        let written = [full("bob", "quadhost"), short("alice")];
        assert_eq!(resolve(&written, &roster()), Ok(vec![1, 3]));
    }

    #[test]
    fn no_mentions_is_a_broadcast() {
        assert_eq!(resolve(&[], &roster()), Ok(vec![]));
    }
}
