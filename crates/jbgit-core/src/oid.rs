use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// 20-byte SHA-1 object identifier decoupled from `git2::Oid` so `jbgit-core`
/// stays pure and UI-renderable without FFI dependencies.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid([u8; 20]);

#[derive(Debug, Error)]
#[error("invalid SHA-1 hex oid: {0}")]
pub struct ParseOidError(String);

impl Oid {
    pub const ZERO: Self = Self([0u8; 20]);

    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 20]
    }

    /// Returns the classic 7-character abbreviated SHA-1 hex string.
    pub fn short(&self) -> String {
        let full = self.to_string();
        full[..7].to_string()
    }

    /// Returns `true` if the lowercase hex form of this oid starts with `prefix`
    /// (case-insensitive). Empty prefixes never match.
    pub fn matches_hex_prefix(&self, prefix: &str) -> bool {
        if prefix.is_empty() || prefix.len() > 40 {
            return false;
        }
        let full = self.to_string();
        full.as_bytes()
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| *a == b.to_ascii_lowercase())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self)
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Oid {
    type Err = ParseOidError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() != 40 {
            return Err(ParseOidError(s.to_string()));
        }
        let mut bytes = [0u8; 20];
        let (chunks, _) = s.as_bytes().as_chunks::<2>();
        for (i, chunk) in chunks.iter().enumerate() {
            let hi = hex_val(chunk[0]).ok_or_else(|| ParseOidError(s.to_string()))?;
            let lo = hex_val(chunk[1]).ok_or_else(|| ParseOidError(s.to_string()))?;
            bytes[i] = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }
}

const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hex() {
        let hex = "0123456789abcdef0123456789abcdef01234567";
        let oid: Oid = hex.parse().unwrap();
        assert_eq!(oid.to_string(), hex);
        assert_eq!(oid.short(), "0123456");
    }
}
