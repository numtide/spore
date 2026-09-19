use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct UserData {
    pub system: String,
    #[serde(default)]
    pub substituters: Vec<String>,
    #[serde(rename = "trusted-public-keys", default)]
    pub trusted_public_keys: Vec<String>,
    /// repart definitions; see Layout::from_json.
    #[serde(default)]
    pub layout: Option<serde_json::Value>,
    /// Boots of the target without a good mark before the bootstrap takes over.
    #[serde(rename = "boot-tries", default = "default_tries")]
    pub boot_tries: u8,
    #[serde(default)]
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

fn default_tries() -> u8 {
    3
}

impl UserData {
    pub fn parse(s: &str) -> Result<UserData, String> {
        let u: UserData = serde_json::from_str(s).map_err(|e| format!("user-data: {e}"))?;
        if u.substituters.is_empty() {
            return Err("user-data names no substituter".into());
        }
        if u.trusted_public_keys.is_empty() {
            return Err("user-data names no trusted-public-keys".into());
        }
        if !(1..=9).contains(&u.boot_tries) {
            return Err("user-data: boot-tries must be 1 to 9".into());
        }
        Ok(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_userdata() {
        let u = UserData::parse(
            r#"{"system": "/nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-x",
                "substituters": ["http://10.0.2.2:8080"],
                "trusted-public-keys": ["k:AAAA"], "extra": 1}"#,
        )
        .unwrap();
        assert_eq!(u.substituters, ["http://10.0.2.2:8080"]);
        assert_eq!((u.boot_tries, u.fallback), (3, Fallback::Provision));
        let u = UserData::parse(
            r#"{"system": "x", "substituters": ["s"], "trusted-public-keys": ["k"],
                "boot-tries": 1, "fallback": "rescue"}"#,
        )
        .unwrap();
        assert_eq!((u.boot_tries, u.fallback), (1, Fallback::Rescue));
        for bad in [
            r#"{"system": "x", "substituters": []}"#,
            r#"{"system": "x", "substituters": ["s"], "trusted-public-keys": ["k"], "boot-tries": 0}"#,
            r#"{"system": "x", "substituters": ["s"], "trusted-public-keys": ["k"], "fallback": "x"}"#,
            "not json",
        ] {
            assert!(UserData::parse(bad).is_err(), "{bad}");
        }
    }
}
