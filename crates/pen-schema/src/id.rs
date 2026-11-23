//! Node id generation compatible with the legacy app.
//!
//! Legacy rule: 5 random characters from `[0-9A-Za-z]`; when the first one is
//! a digit, a random letter is prepended (6 characters) so the id is a valid
//! JavaScript identifier. Older files may contain ids starting with a digit and
//! those must still be accepted.

use std::cell::Cell;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

const ALPHANUMERIC: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const LETTERS: &[u8; 52] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Small, fast PRNG (SplitMix64). Not cryptographic; ids only need to be unique.
#[derive(Debug, Clone)]
pub struct IdGenerator {
    state: u64,
}

impl IdGenerator {
    /// Seeded from the process-random `RandomState` keys and the clock.
    pub fn new() -> Self {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        IdGenerator { state: h.finish() }
    }

    /// Deterministic generator (tests, reproducible migrations).
    pub fn with_seed(seed: u64) -> Self {
        IdGenerator { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn pick(&mut self, set: &[u8]) -> char {
        // Rejection-free modulo; bias is negligible for 62/52-symbol sets.
        set[(self.next_u64() % set.len() as u64) as usize] as char
    }

    /// A new id following the legacy rule.
    pub fn next_id(&mut self) -> String {
        let mut id = String::with_capacity(6);
        for _ in 0..5 {
            id.push(self.pick(ALPHANUMERIC));
        }
        if id.as_bytes()[0].is_ascii_digit() {
            id.insert(0, self.pick(LETTERS));
        }
        id
    }

    /// A new id for which `taken` returns false.
    pub fn next_unique(&mut self, taken: impl Fn(&str) -> bool) -> String {
        loop {
            let id = self.next_id();
            if !taken(&id) {
                return id;
            }
        }
    }

    /// Random UUID v4 (`8-4-4-4-12` lowercase hex), as used by the 1.0 format.
    pub fn next_uuid(&mut self) -> String {
        let a = self.next_u64();
        let b = self.next_u64();
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&a.to_be_bytes());
        bytes[8..].copy_from_slice(&b.to_be_bytes());
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

impl Default for IdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static SEED: Cell<Option<u64>> = const { Cell::new(None) };
}

fn with_thread_rng<R>(f: impl FnOnce(&mut IdGenerator) -> R) -> R {
    SEED.with(|cell| {
        let mut g = match cell.get() {
            Some(state) => IdGenerator::with_seed(state),
            None => IdGenerator::new(),
        };
        let r = f(&mut g);
        cell.set(Some(g.state));
        r
    })
}

/// New id from a thread-local generator.
pub fn new_id() -> String {
    with_thread_rng(IdGenerator::next_id)
}

/// New UUID v4 from a thread-local generator.
pub fn new_uuid() -> String {
    with_thread_rng(IdGenerator::next_uuid)
}

/// Whether `id` can be used as a node id (non-empty, no `/`).
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/')
}

/// Whether `id` follows the rule used for newly generated ids.
pub fn is_generated_style(id: &str) -> bool {
    let b = id.as_bytes();
    match b.len() {
        5 => b[0].is_ascii_alphabetic() && b.iter().all(u8::is_ascii_alphanumeric),
        6 => {
            b[0].is_ascii_alphabetic()
                && b[1].is_ascii_digit()
                && b.iter().all(u8::is_ascii_alphanumeric)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_follow_legacy_rule() {
        let mut g = IdGenerator::with_seed(42);
        let mut seen = HashSet::new();
        for _ in 0..10_000 {
            let id = g.next_id();
            assert!(is_generated_style(&id), "{id}");
            assert!(id.as_bytes()[0].is_ascii_alphabetic());
            seen.insert(id);
        }
        assert!(seen.len() > 9_990);
        assert!(
            seen.iter().any(|id| id.len() == 6),
            "digit-first ids get a letter prefix"
        );
    }

    #[test]
    fn unique_and_uuid() {
        let mut g = IdGenerator::with_seed(1);
        let first = g.clone().next_id();
        let id = g.next_unique(|c| c == first);
        assert_ne!(id, first);
        let u = g.next_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(is_valid_id("1dsjR"));
        assert!(!is_valid_id("a/b"));
        assert!(!new_id().is_empty());
    }
}
