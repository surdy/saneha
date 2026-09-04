//! Minting readable channel names.
//!
//! A channel created without a name gets an adjective-noun slug such as
//! `brisk-otter`: short enough to type, distinct enough to say out loud when
//! handing it to the person at the other machine.

use rand::RngExt;

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brave", "brisk", "calm", "clear", "clever", "crisp", "curious", "daring",
    "deft", "eager", "early", "easy", "fair", "fleet", "fond", "frank", "gentle", "glad", "grand",
    "happy", "hardy", "keen", "kind", "level", "lively", "loyal", "lucid", "mellow", "merry",
    "mild", "neat", "nimble", "noble", "patient", "placid", "plucky", "prime", "proud", "quick",
    "quiet", "rapid", "ready", "rustic", "sharp", "silent", "sleek", "smooth", "snug", "solid",
    "spry", "stark", "steady", "sunny", "swift", "tidy", "tranquil", "trusty", "upbeat", "vivid",
    "warm", "wise", "witty",
];

const NOUNS: &[&str] = &[
    "acorn",
    "alder",
    "anchor",
    "aspen",
    "badger",
    "beacon",
    "birch",
    "bison",
    "cedar",
    "cobalt",
    "comet",
    "coral",
    "cypress",
    "delta",
    "ember",
    "falcon",
    "fathom",
    "fennel",
    "ferry",
    "finch",
    "fjord",
    "garnet",
    "harbor",
    "hazel",
    "heron",
    "indigo",
    "ivory",
    "juniper",
    "kestrel",
    "lagoon",
    "lantern",
    "larch",
    "lichen",
    "lynx",
    "maple",
    "marlin",
    "meadow",
    "mesa",
    "nectar",
    "onyx",
    "orchard",
    "osprey",
    "otter",
    "pebble",
    "pine",
    "prairie",
    "quartz",
    "quill",
    "raven",
    "reef",
    "ridge",
    "rowan",
    "sable",
    "sage",
    "sandpiper",
    "sequoia",
    "sparrow",
    "spruce",
    "summit",
    "thistle",
    "tundra",
    "walnut",
    "willow",
    "wren",
];

/// A fresh two-word slug. Callers retry on collision; the pairs number in the
/// thousands, so a retry is rare.
pub fn mint() -> String {
    let mut rng = rand::rng();
    let adjective = ADJECTIVES[rng.random_range(0..ADJECTIVES.len())];
    let noun = NOUNS[rng.random_range(0..NOUNS.len())];
    format!("{adjective}-{noun}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::validate_channel_name;

    #[test]
    fn minted_names_are_valid_channel_names() {
        for _ in 0..200 {
            let name = mint();
            validate_channel_name(&name).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(name.matches('-').count(), 1, "{name}");
        }
    }

    #[test]
    fn word_lists_are_clean_and_roomy() {
        for word in ADJECTIVES.iter().chain(NOUNS) {
            assert!(
                word.chars().all(|c| c.is_ascii_lowercase()),
                "{word} is not plain lowercase"
            );
        }
        let combinations = ADJECTIVES.len() * NOUNS.len();
        assert!(
            combinations > 1000,
            "only {combinations} slugs to mint from"
        );
    }

    #[test]
    fn mints_more_than_one_name() {
        let first = mint();
        let varied = (0..50).any(|_| mint() != first);
        assert!(varied, "mint kept returning {first}");
    }
}
