//! Secrets: user tokens, the operator token and browser session ids.
//!
//! Every secret is 256 random bits from the OS's generator ([`Secret::generate`]), written as
//! 64 lower-case hex digits. At rest only its SHA-256 ([`TokenHash`]) is kept; hashes are
//! compared in constant time ([`TokenHash::matches`]). A [`Secret`] never shows in `Debug`
//! output or logs: only the code that hands it to its owner reads it ([`Secret::expose`]).

use std::fmt;

use ring::rand::{SecureRandom, SystemRandom};

/// The bytes of a secret (256 bits).
const SECRET_BYTES: usize = 32;

/// A secret (token or session id) in clear text, as handed to its owner once.
///
/// `Debug` shows `Secret(…<last 4>)` only; equality is constant time.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// A new secret: 256 bits from the OS's random generator. `Err` (the OS's reason) when the
    /// generator fails.
    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0u8; SECRET_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "the operating system's random generator failed".to_string())?;
        Ok(Self(hex(&bytes)))
    }

    /// A secret given as text (a token from a request or a file).
    pub fn from_text(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The clear text: only for handing it to its owner (a print, a cookie, `server.run`).
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Its SHA-256.
    pub fn hash(&self) -> TokenHash {
        TokenHash::of(&self.0)
    }

    /// Its last four characters (shown in listings so an admin can tell tokens apart).
    pub fn last4(&self) -> String {
        last4(&self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(…{})", self.last4())
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.hash().matches(&other.hash())
    }
}

impl Eq for Secret {}

impl serde::Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

/// The last four characters of `text` (all of it when shorter).
pub(crate) fn last4(text: &str) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(4)).collect()
}

/// The SHA-256 of a secret: what is kept at rest.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenHash([u8; 32]);

impl TokenHash {
    /// The SHA-256 of `text` (its UTF-8 bytes).
    pub fn of(text: &str) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(digest.as_ref());
        Self(bytes)
    }

    /// A hash written as 64 hex digits (`access.toml`): `None` for anything else.
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// The 64 lower-case hex digits.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Whether the two hashes are equal, in constant time (no early exit on the first
    /// difference).
    pub fn matches(&self, other: &TokenHash) -> bool {
        let mut difference = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            difference |= std::hint::black_box(a ^ b);
        }
        std::hint::black_box(difference) == 0
    }
}

impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TokenHash({})", self.to_hex())
    }
}

/// `bytes` as lower-case hex digits.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_random_256_bit_hex() {
        let a = Secret::generate().unwrap();
        let b = Secret::generate().unwrap();
        assert_eq!(a.expose().len(), 64);
        assert!(a.expose().bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(a, Secret::from_text(a.expose()));
        assert_eq!(a.last4(), a.expose()[60..]);
    }

    #[test]
    fn debug_never_shows_the_secret() {
        let secret = Secret::generate().unwrap();
        let shown = format!("{secret:?}");
        assert!(!shown.contains(secret.expose()), "{shown}");
        assert!(shown.ends_with(&format!("…{})", secret.last4())), "{shown}");
    }

    #[test]
    fn hashes_round_trip_and_compare() {
        // The SHA-256 of "abc" (FIPS 180-2 test vector).
        let hash = TokenHash::of("abc");
        assert_eq!(
            hash.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(TokenHash::parse_hex(&hash.to_hex()), Some(hash));
        assert!(hash.matches(&TokenHash::of("abc")));
        assert!(!hash.matches(&TokenHash::of("abd")));
        assert_eq!(TokenHash::parse_hex("xyz"), None);
        assert_eq!(TokenHash::parse_hex(&"g".repeat(64)), None);
        assert_eq!(last4("ab"), "ab");
    }
}
