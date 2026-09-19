// Copied from ietsp src/store/path.rs, cut down to parsing store paths.
// ietsp is LGPL-2.1-or-later; its author relicenses this copy under MIT (see LICENSE).

use std::fmt;

use super::hash::nix32_decode;

pub const STORE_DIR: &str = "/nix/store";

/// Mirrors path.hh:StorePath::HashLen (base32 characters).
pub const HASH_PART_LEN: usize = 32;

/// Mirrors path.hh:StorePath::MaxPathLen.
pub const MAX_NAME_LEN: usize = 211;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorePathError(pub String);

impl fmt::Display for StorePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StorePathError {}

/// A store path stored as its base name `<hash>-<name>`, like path.hh:StorePath.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath {
    base_name: String,
}

impl fmt::Debug for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StorePath({})", self.base_name)
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{STORE_DIR}/{}", self.base_name)
    }
}

/// Mirrors path.cc:checkName.
pub fn check_name(name: &str) -> Result<(), StorePathError> {
    if name.is_empty() {
        return Err(StorePathError("name must not be empty".into()));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(StorePathError(format!(
            "name '{name}' must be no longer than {MAX_NAME_LEN} characters"
        )));
    }
    let b = name.as_bytes();
    if b[0] == b'.' {
        if b.len() == 1 {
            return Err(StorePathError(format!("name '{name}' is not valid")));
        }
        if b[1] == b'-' {
            return Err(StorePathError(format!(
                "name '{name}' is not valid: first dash-separated component must not be '.'"
            )));
        }
        if b[1] == b'.' {
            if b.len() == 2 {
                return Err(StorePathError(format!("name '{name}' is not valid")));
            }
            if b[2] == b'-' {
                return Err(StorePathError(format!(
                    "name '{name}' is not valid: first dash-separated component must not be '..'"
                )));
            }
        }
    }
    for &c in b {
        if !(c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')) {
            return Err(StorePathError(format!(
                "name '{name}' contains illegal character '{}'",
                c as char
            )));
        }
    }
    Ok(())
}

impl StorePath {
    pub fn from_base_name(base_name: &str) -> Result<StorePath, StorePathError> {
        let bad = |why: &str| {
            StorePathError(format!(
                "path '{base_name}' is not a valid store path: {why}"
            ))
        };
        if base_name.len() < HASH_PART_LEN + 1 {
            return Err(bad("path too short"));
        }
        let (hash_part, rest) = base_name.split_at(HASH_PART_LEN);
        if !rest.starts_with('-') {
            return Err(bad("missing dash"));
        }
        let decoded = nix32_decode(hash_part).map_err(|e| bad(&e.0))?;
        if decoded.len() != 20 {
            return Err(bad("invalid hash part"));
        }
        check_name(&rest[1..]).map_err(|e| bad(&e.0))?;
        Ok(StorePath {
            base_name: base_name.to_string(),
        })
    }

    /// Mirrors store-api.cc:StoreDirConfig::parseStorePath: `/nix/store/<hash>-<name>`.
    pub fn parse(path: &str) -> Result<StorePath, StorePathError> {
        let bad = || StorePathError(format!("path '{path}' is not in the Nix store"));
        let rest = path.strip_prefix(STORE_DIR).ok_or_else(bad)?;
        let rest = rest.strip_prefix('/').ok_or_else(bad)?;
        if rest.contains('/') {
            return Err(bad());
        }
        Self::from_base_name(rest)
    }

    pub fn base_name(&self) -> &str {
        &self.base_name
    }

    pub fn hash_part(&self) -> &str {
        &self.base_name[..HASH_PART_LEN]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_parts() {
        let p =
            StorePath::parse("/nix/store/7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-hello-2.12.1").unwrap();
        assert_eq!(p.hash_part(), "7h7qgvs4kgzsn8a6rb273saxyqh4jxlz");
        assert_eq!(
            p.base_name(),
            "7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-hello-2.12.1"
        );
        assert_eq!(
            p.to_string(),
            "/nix/store/7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-hello-2.12.1"
        );
    }

    #[test]
    fn parse_rejects() {
        for bad in [
            "/nix/store/7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-hello/bin",
            "/tmp/7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-hello",
            "/nix/store/7h7qgvs4kgzsn8a6rb273saxyqh4jxle-hello",
            "/nix/store/7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-",
            "/nix/store/7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-a b",
            "/nix/store/7h7qgvs4kgzsn8a6rb273saxyqh4jxlz-..",
        ] {
            assert!(StorePath::parse(bad).is_err(), "{bad}");
        }
    }
}
