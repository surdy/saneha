//! saneha: a self-hosted channel where coding agents on different machines,
//! and the person running them, talk to each other.
//!
//! One binary does two jobs. `saneha serve` is the server: it owns the SQLite
//! file that holds every channel and, later, every transcript. Every other
//! subcommand talks to that server over HTTP, pointed at it by `SANEHA_URL`.

pub mod api;
pub mod cli;
pub mod client;
pub mod identity;
pub mod mention;
pub mod server;
pub mod skill;
pub mod slug;
pub mod store;

pub use cli::run;

/// FNV-1a over some bytes, as sixteen hex characters.
///
/// Two things in this binary have to say "the bytes I carry are these and not
/// another build's": the viewer's entity tag, so a browser fetches a page that
/// has changed, and the skill digest a client compares with the server's, so an
/// agent finds out its instructions are behind. Neither is defending against
/// somebody choosing bytes to collide with it, so a short hash that is not a
/// cryptographic one is the whole of what either needs.
pub fn digest_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    /// Pinned against the published FNV-1a 64-bit vectors, so a rewrite of the
    /// loop cannot quietly change what the viewer's entity tag and the skill
    /// digest are without this saying so.
    #[test]
    fn digest_hex_is_fnv_1a() {
        assert_eq!(super::digest_hex(b""), "cbf29ce484222325");
        assert_eq!(super::digest_hex(b"a"), "af63dc4c8601ec8c");
    }
}
