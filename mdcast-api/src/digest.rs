//! Content digest: the identity of one blob in the asset-negotiation
//! protocol. A caller announces the assets a render needs by digest, the
//! server answers with the ones it does not hold yet.
//!
//! [`Digest`] is a newtype rather than a bare `String` because the server
//! must never trust a caller-supplied digest: it is used as a lookup key for
//! stored blob bytes, so anything that is not exactly 64 lowercase hex
//! characters is rejected at the deserialization boundary instead of
//! reaching storage.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Number of hex characters in a sha256 digest.
pub const DIGEST_HEX_LEN: usize = 64;

const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

/// A validated lowercase-hex sha256 digest.
///
/// Serializes as a plain JSON string; deserializing runs the same validation
/// as [`Digest::parse`], so an invalid digest never materializes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    /// Validate `s` as a lowercase-hex sha256 digest.
    pub fn parse(s: impl Into<String>) -> Result<Self, DigestError> {
        let s = s.into();
        if s.len() != DIGEST_HEX_LEN {
            return Err(DigestError::Length(s.len()));
        }
        if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DigestError::NotLowercaseHex);
        }
        Ok(Self(s))
    }

    /// Build a digest from raw sha256 output — the shape every hasher hands
    /// back — without going through a string that could be malformed.
    pub fn from_sha256(bytes: [u8; 32]) -> Self {
        let mut hex = String::with_capacity(DIGEST_HEX_LEN);
        for b in bytes {
            hex.push(HEX_DIGITS[(b >> 4) as usize] as char);
            hex.push(HEX_DIGITS[(b & 0x0f) as usize] as char);
        }
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Digest {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<Digest> for String {
    fn from(d: Digest) -> Self {
        d.0
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Digest::parse(s)
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Digest::parse(s)
    }
}

/// Why a string is not a valid [`Digest`]. Hand-written rather than derived
/// with `thiserror`, to keep this crate on serde alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestError {
    /// Wrong number of characters (a sha256 digest is exactly 64).
    Length(usize),
    /// Contains something other than `0-9a-f` — uppercase included, since a
    /// digest has exactly one spelling.
    NotLowercaseHex,
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DigestError::Length(len) => {
                write!(f, "expected {DIGEST_HEX_LEN} hex characters, got {len}")
            }
            DigestError::NotLowercaseHex => {
                f.write_str("expected lowercase hex characters (0-9, a-f)")
            }
        }
    }
}

impl std::error::Error for DigestError {}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn parse_accepts_a_lowercase_hex_sha256() {
        let digest = Digest::parse(VALID).unwrap();
        assert_eq!(digest.as_str(), VALID);
        assert_eq!(digest.to_string(), VALID);
    }

    #[test]
    fn parse_rejects_wrong_length() {
        assert_eq!(Digest::parse("abc"), Err(DigestError::Length(3)));
        assert_eq!(
            Digest::parse(format!("{VALID}0")),
            Err(DigestError::Length(DIGEST_HEX_LEN + 1))
        );
        assert_eq!(
            Digest::parse(&VALID[..DIGEST_HEX_LEN - 1]),
            Err(DigestError::Length(DIGEST_HEX_LEN - 1))
        );
    }

    #[test]
    fn parse_rejects_uppercase() {
        assert_eq!(
            Digest::parse(VALID.to_uppercase()),
            Err(DigestError::NotLowercaseHex)
        );
    }

    #[test]
    fn parse_rejects_non_hex() {
        let non_hex = format!("z{}", &VALID[1..]);
        assert_eq!(Digest::parse(non_hex), Err(DigestError::NotLowercaseHex));
    }

    #[test]
    fn from_sha256_hex_encodes_all_nibbles() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x0f;
        bytes[1] = 0xa0;
        bytes[31] = 0xff;

        let digest = Digest::from_sha256(bytes);

        assert_eq!(digest.as_str().len(), DIGEST_HEX_LEN);
        assert!(digest.as_str().starts_with("0fa0"));
        assert!(digest.as_str().ends_with("ff"));
        assert_eq!(Digest::parse(digest.as_str()).unwrap(), digest);
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let digest = Digest::parse(VALID).unwrap();
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(json, format!("\"{VALID}\""));
        assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), digest);
    }

    #[test]
    fn deserialization_rejects_an_invalid_digest() {
        let err = serde_json::from_str::<Digest>("\"NOPE\"").unwrap_err();
        assert!(err.to_string().contains("64 hex characters"), "{err}");
    }

    #[test]
    fn from_str_matches_parse() {
        assert_eq!(
            VALID.parse::<Digest>().unwrap(),
            Digest::parse(VALID).unwrap()
        );
        assert!("".parse::<Digest>().is_err());
    }
}
