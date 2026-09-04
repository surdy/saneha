//! Mentions: the `@name` in a message body that names a recipient.
//!
//! A mention counts only at the start of a body or after whitespace, so
//! `mail me at a@b.com` addresses nobody and `@bob` addresses bob. The token
//! after the `@` is a name in the alphabet an identity uses — lowercase
//! letters, digits and hyphens — so it ends at the first character a name
//! cannot hold, and a trailing `.`, `,` or `:` is punctuation rather than part
//! of the name. `@name@host` names an identity outright; `@all` names everyone
//! in the channel.
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
pub fn mentions(body: &str) -> Vec<Mention> {
    let mut found: Vec<Mention> = Vec::new();
    // A mention starts the body or follows whitespace; nothing else counts,
    // which is what leaves an email address alone.
    let mut after_space = true;
    let mut cursor = 0;

    while cursor < body.len() {
        let rest = &body[cursor..];
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
    found
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

    if let Some(after_name) = rest[used..].strip_prefix('@') {
        if let Some(host) = name_prefix(after_name) {
            used += '@'.len_utf8() + host.len();
            return Some((
                Mention::Full {
                    name: name.to_string(),
                    host: host.to_string(),
                },
                used,
            ));
        }
    }
    if name == EVERYONE {
        return Some((Mention::Everyone, used));
    }
    Some((Mention::Short(name.to_string()), used))
}

/// The name at the front of `value`: the run of characters a name may hold,
/// with a trailing hyphen dropped because a name does not end in one. `None`
/// when there is no name there at all, which is what makes `@` on its own,
/// `@ Bob` and `@-bob` text rather than mentions.
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

/// The alphabet an identity is written in. Anything else ends the token, which
/// is what leaves the full stop at the end of a sentence out of the name.
fn is_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
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
        assert_eq!(mentions("@bob@Quadhost"), vec![short("bob")]);
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
        for body in ["@", "@ bob", "@-bob", "@Bob", "@ਸੁਨੇਹਾ", "email@", "@@bob"] {
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
        for value in ["", "@", "bob!", "Bob", "bob@", "bob@quadhost!", "bob bob"] {
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
