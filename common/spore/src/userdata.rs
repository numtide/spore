use std::collections::BTreeMap;

use serde::Deserialize;

/// The `spore` section of the user-data. The rest of the document belongs to
/// the target.
#[derive(Debug)]
pub struct UserData {
    /// The store path for this machine's arch.
    pub system: String,
    pub substituters: Vec<String>,
    pub trusted_public_keys: Vec<String>,
    /// repart definitions; see Layout::from_json.
    pub layout: Option<serde_json::Value>,
    /// Boots of the target without a good mark before the bootstrap takes over.
    pub boot_tries: u8,
    pub fallback: Fallback,
}

/// What the bootstrap does when the target has no tries left.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Fallback {
    #[default]
    Provision,
    Rescue,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum System {
    One(String),
    PerArch(BTreeMap<String, String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Section {
    #[serde(rename = "version")]
    _version: u32,
    system: System,
    substituters: Vec<String>,
    trusted_public_keys: Vec<String>,
    #[serde(default)]
    layout: Option<serde_json::Value>,
    #[serde(default = "default_tries")]
    boot_tries: u8,
    #[serde(default)]
    fallback: Fallback,
}

#[derive(Deserialize)]
struct Document {
    spore: Option<serde_json::Value>,
}

fn default_tries() -> u8 {
    3
}

const VERSION: u32 = 1;
fn arch() -> String {
    format!("{}-linux", std::env::consts::ARCH)
}

impl UserData {
    pub fn parse(s: &str) -> Result<UserData, String> {
        let d: Document = serde_json::from_str(s).map_err(|e| format!("user-data: {e}"))?;
        let v = d.spore.ok_or("user-data has no spore section")?;
        let version = v.get("version").and_then(|v| v.as_u64());
        if version != Some(VERSION as u64) {
            return Err(format!(
                "user-data: spore.version must be {VERSION}, not {}",
                v.get("version").map_or("missing".into(), |v| v.to_string())
            ));
        }
        let u: Section = serde_json::from_value(v).map_err(|e| format!("user-data: spore: {e}"))?;
        let system = match u.system {
            System::One(s) => s,
            System::PerArch(mut m) => m
                .remove(&arch())
                .ok_or(format!("user-data: spore.system has no {} entry", arch()))?,
        };
        if u.substituters.is_empty() {
            return Err("user-data names no substituter".into());
        }
        if u.trusted_public_keys.is_empty() {
            return Err("user-data names no trusted-public-keys".into());
        }
        if !(1..=9).contains(&u.boot_tries) {
            return Err("user-data: boot-tries must be 1 to 9".into());
        }
        Ok(UserData {
            system,
            substituters: u.substituters,
            trusted_public_keys: u.trusted_public_keys,
            layout: u.layout,
            boot_tries: u.boot_tries,
            fallback: u.fallback,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(spore: &str) -> String {
        format!(r#"{{"spore": {spore}, "nix-farm": {{"join-token": "t"}}}}"#)
    }

    #[test]
    fn parse_userdata() {
        let u = UserData::parse(&doc(
            r#"{"version": 1, "system": "/nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-x",
                "substituters": ["http://10.0.2.2:8080"], "trusted-public-keys": ["k:AAAA"]}"#,
        ))
        .unwrap();
        assert_eq!(u.substituters, ["http://10.0.2.2:8080"]);
        assert_eq!((u.boot_tries, u.fallback), (3, Fallback::Provision));
        let u = UserData::parse(&doc(
            r#"{"version": 1, "system": "x", "substituters": ["s"], "trusted-public-keys": ["k"],
                "boot-tries": 1, "fallback": "rescue"}"#,
        ))
        .unwrap();
        assert_eq!((u.boot_tries, u.fallback), (1, Fallback::Rescue));
        for bad in [
            doc(
                r#"{"version": 1, "system": "x", "substituters": [], "trusted-public-keys": ["k"]}"#,
            ),
            doc(
                r#"{"version": 1, "system": "x", "substituters": ["s"], "trusted-public-keys": ["k"], "boot-tries": 0}"#,
            ),
            doc(
                r#"{"version": 1, "system": "x", "substituters": ["s"], "trusted-public-keys": ["k"], "fallback": "x"}"#,
            ),
            doc(
                r#"{"version": 1, "system": "x", "substituters": ["s"], "trusted-public-keys": ["k"], "boot_tries": 1}"#,
            ),
            doc(
                r#"{"version": 2, "system": "x", "substituters": ["s"], "trusted-public-keys": ["k"]}"#,
            ),
            doc(r#"{"system": "x", "substituters": ["s"], "trusted-public-keys": ["k"]}"#),
            r#"{"system": "x", "substituters": ["s"], "trusted-public-keys": ["k"]}"#.into(),
            "not json".into(),
        ] {
            assert!(UserData::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn system_per_arch() {
        let u = UserData::parse(&doc(&format!(
            r#"{{"version": 1, "substituters": ["s"], "trusted-public-keys": ["k"],
                "system": {{"{}": "/nix/store/a", "other-linux": "/nix/store/b"}}}}"#,
            arch()
        )))
        .unwrap();
        assert_eq!(u.system, "/nix/store/a");
        let e = UserData::parse(&doc(
            r#"{"version": 1, "substituters": ["s"], "trusted-public-keys": ["k"],
                "system": {"other-linux": "/nix/store/b"}}"#,
        ))
        .unwrap_err();
        assert!(e.contains("has no"), "{e}");
    }
}
