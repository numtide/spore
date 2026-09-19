// Copied from ietsp src/store/hash.rs, cut down to sha256 and the encodings
// that narinfo files and the nix database use.
// ietsp is LGPL-2.1-or-later; its author relicenses this copy under MIT (see LICENSE).

use std::fmt;
use std::io::Write;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashError(pub String);

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HashError {}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256(pub [u8; 32]);

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.to_nix32())
    }
}

impl Sha256 {
    pub fn to_base16(&self) -> String {
        base16_encode(&self.0)
    }

    pub fn to_nix32(&self) -> String {
        nix32_encode(&self.0)
    }

    /// Mirrors hash.cc:Hash::parseAny for sha256: `sha256:<base16|nix32|base64>` or
    /// `sha256-<base64>` (SRI).
    pub fn parse(s: &str) -> Result<Sha256, HashError> {
        let (rest, sri) = if let Some(r) = s.strip_prefix("sha256:") {
            (r, false)
        } else if let Some(r) = s.strip_prefix("sha256-") {
            (r, true)
        } else {
            return Err(HashError(format!("hash '{s}' is not a sha256 hash")));
        };
        let d = if sri {
            base64_decode(rest)?
        } else if rest.len() == 64 {
            base16_decode(rest)?
        } else if rest.len() == nix32_encoded_len(32) {
            nix32_decode(rest)?
        } else if rest.len() == 44 {
            base64_decode(rest)?
        } else {
            return Err(HashError(format!("hash '{s}' has wrong length for sha256")));
        };
        let b: [u8; 32] = d
            .try_into()
            .map_err(|_| HashError(format!("hash '{s}' has wrong length for sha256")))?;
        Ok(Sha256(b))
    }
}

/// Mirrors hash.cc:HashSink: an incremental sha256 that also counts bytes. ring, not the
/// sha2 crate: ring has assembly for CPUs without SHA-NI, like the Skylake of hcloud cx.
pub struct Hasher {
    state: ring::digest::Context,
    bytes: u64,
}

impl Default for Hasher {
    fn default() -> Self {
        Hasher {
            state: ring::digest::Context::new(&ring::digest::SHA256),
            bytes: 0,
        }
    }
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        self.bytes += data.len() as u64;
        self.state.update(data);
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    pub fn finish(self) -> Sha256 {
        Sha256(self.state.finish().as_ref().try_into().unwrap())
    }
}

impl Write for Hasher {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

const NIX32_CHARS: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Mirrors base-nix-32.hh:BaseNix32::encodedLength.
pub fn nix32_encoded_len(n: usize) -> usize {
    (n * 8 - 1) / 5 + 1
}

/// Mirrors base-nix-32.cc:BaseNix32::encode (Nix's reversed-bit-order base32).
pub fn nix32_encode(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let len = nix32_encoded_len(bytes.len());
    let mut s = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let lo = bytes[i] >> j;
        let hi = if i + 1 < bytes.len() {
            bytes[i + 1].checked_shl((8 - j) as u32).unwrap_or(0)
        } else {
            0
        };
        s.push(NIX32_CHARS[((lo | hi) & 0x1f) as usize] as char);
    }
    s
}

/// Mirrors base-nix-32.cc:BaseNix32::decode.
pub fn nix32_decode(s: &str) -> Result<Vec<u8>, HashError> {
    let bytes = s.as_bytes();
    let mut res: Vec<u8> = Vec::with_capacity(bytes.len() * 5 / 8 + 1);
    for n in 0..bytes.len() {
        let c = bytes[bytes.len() - n - 1];
        let digit = NIX32_CHARS.iter().position(|&x| x == c).ok_or_else(|| {
            HashError(format!(
                "invalid character in Nix32 (Nix's Base32 variation) string: '{}'",
                c as char
            ))
        })? as u8;
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        if res.len() < i + 1 {
            res.resize(i + 1, 0);
        }
        res[i] |= digit << j;
        let carry = digit.checked_shr((8 - j) as u32).unwrap_or(0);
        if carry != 0 {
            if res.len() < i + 2 {
                res.resize(i + 2, 0);
            }
            res[i + 1] |= carry;
        }
    }
    Ok(res)
}

/// Mirrors base-n.cc:base16::encode.
pub fn base16_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Mirrors base-n.cc:base16::decode.
pub fn base16_decode(s: &str) -> Result<Vec<u8>, HashError> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(HashError(format!(
            "invalid base16 string '{s}': odd length"
        )));
    }
    let nibble = |c: u8| -> Result<u8, HashError> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(HashError(format!(
                "invalid character in base16 string: '{}'",
                c as char
            ))),
        }
    };
    bytes
        .chunks(2)
        .map(|p| Ok(nibble(p[0])? << 4 | nibble(p[1])?))
        .collect()
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, HashError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| HashError(format!("invalid base64 string '{s}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // printf 'hello world\n' | nix hash file --type sha256 --base16 /dev/stdin
    const HELLO: &str = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";

    #[test]
    fn hasher_matches_sha256sum() {
        let mut h = Hasher::default();
        h.update(b"hello world\n");
        assert_eq!(h.bytes_written(), 12);
        assert_eq!(h.finish().to_base16(), HELLO);
    }

    #[test]
    fn parse_all_encodings() {
        let b16 = Sha256::parse(&format!("sha256:{HELLO}")).unwrap();
        let n32 = Sha256::parse(&format!("sha256:{}", b16.to_nix32())).unwrap();
        assert_eq!(b16, n32);
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b16.0);
        assert_eq!(Sha256::parse(&format!("sha256-{b64}")).unwrap(), b16);
        assert_eq!(Sha256::parse(&format!("sha256:{b64}")).unwrap(), b16);
        assert!(Sha256::parse(HELLO).is_err());
        assert!(Sha256::parse("sha256:abc").is_err());
    }

    // nix hash convert --to nix32 sha256:a948904f...
    #[test]
    fn nix32_known_value() {
        let h = Sha256::parse(&format!("sha256:{HELLO}")).unwrap();
        assert_eq!(
            h.to_nix32(),
            "0ix4jahrkll5zg01wandq78jw3ab30q4nscph67rniqg5x7r0j59"
        );
    }
}
