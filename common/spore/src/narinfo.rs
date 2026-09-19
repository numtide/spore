use std::collections::BTreeSet;

use ed25519_dalek::{Signature, VerifyingKey};

use crate::store::hash::{Sha256, base64_decode};
use crate::store::path::StorePath;

#[derive(Debug, Clone)]
pub struct NarInfo {
    pub path: StorePath,
    pub url: String,
    pub compression: String,
    pub nar_hash: Sha256,
    pub nar_size: u64,
    pub references: BTreeSet<StorePath>,
    pub deriver: Option<StorePath>,
    pub sigs: Vec<String>,
    pub ca: Option<String>,
}

fn base_name(s: &str) -> Result<StorePath, String> {
    StorePath::from_base_name(s).map_err(|e| e.0)
}

impl NarInfo {
    /// Mirrors nar-info.cc:NarInfo::NarInfo(store, s, whence).
    pub fn parse(text: &str) -> Result<NarInfo, String> {
        let mut path = None;
        let mut url = None;
        let mut compression = None;
        let mut nar_hash = None;
        let mut nar_size = None;
        let mut references = BTreeSet::new();
        let mut deriver = None;
        let mut sigs = Vec::new();
        let mut ca = None;
        for line in text.lines() {
            let Some((k, v)) = line.split_once(": ") else {
                continue;
            };
            match k {
                "StorePath" => path = Some(StorePath::parse(v).map_err(|e| e.0)?),
                "URL" => url = Some(v.to_string()),
                "Compression" => compression = Some(v.to_string()),
                "NarHash" => nar_hash = Some(Sha256::parse(v).map_err(|e| e.0)?),
                "NarSize" => nar_size = Some(v.parse().map_err(|_| format!("bad NarSize '{v}'"))?),
                "References" => {
                    for r in v.split_whitespace() {
                        references.insert(base_name(r)?);
                    }
                }
                "Deriver" if v != "unknown-deriver" => deriver = Some(base_name(v)?),
                "Sig" => sigs.push(v.to_string()),
                "CA" => ca = Some(v.to_string()),
                _ => {}
            }
        }
        let missing = |f: &str| format!("narinfo has no {f}");
        Ok(NarInfo {
            path: path.ok_or_else(|| missing("StorePath"))?,
            url: url.ok_or_else(|| missing("URL"))?,
            compression: compression.unwrap_or_else(|| "bzip2".into()),
            nar_hash: nar_hash.ok_or_else(|| missing("NarHash"))?,
            nar_size: nar_size
                .filter(|&n| n > 0)
                .ok_or_else(|| missing("NarSize"))?,
            references,
            deriver,
            sigs,
            ca,
        })
    }

    /// Mirrors path-info.cc:ValidPathInfo::fingerprint.
    pub fn fingerprint(&self) -> String {
        let refs: Vec<String> = self.references.iter().map(|r| r.to_string()).collect();
        format!(
            "1;{};sha256:{};{};{}",
            self.path,
            self.nar_hash.to_nix32(),
            self.nar_size,
            refs.join(",")
        )
    }

    /// True if one signature verifies against one of `keys` (names must match).
    pub fn verify(&self, keys: &[PublicKey]) -> bool {
        let fp = self.fingerprint();
        self.sigs.iter().any(|sig| {
            let Some((name, b64)) = sig.split_once(':') else {
                return false;
            };
            let Ok(bytes) = base64_decode(b64) else {
                return false;
            };
            let Ok(sig) = Signature::from_slice(&bytes) else {
                return false;
            };
            keys.iter()
                .filter(|k| k.name == name)
                .any(|k| k.key.verify_strict(fp.as_bytes(), &sig).is_ok())
        })
    }
}

#[derive(Debug, Clone)]
pub struct PublicKey {
    pub name: String,
    key: VerifyingKey,
}

impl PublicKey {
    /// Parses `name:base64(32-byte ed25519 key)`, as in trusted-public-keys.
    pub fn parse(s: &str) -> Result<PublicKey, String> {
        let bad = || format!("bad public key '{s}'");
        let (name, b64) = s.split_once(':').ok_or_else(bad)?;
        let bytes: [u8; 32] = base64_decode(b64)
            .map_err(|_| bad())?
            .try_into()
            .map_err(|_| bad())?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|_| bad())?;
        Ok(PublicKey {
            name: name.to_string(),
            key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // curl https://cache.nixos.org/cp7wjv1pl4wapfk48svvizxd089v9h0a.narinfo
    const COREUTILS: &str = "StorePath: /nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-coreutils-9.11
URL: nar/0fhwlki24hfp98gljrnxnc0cs64rklvwm2zjjj0ck6r6vgml2nar.nar.xz
Compression: xz
FileHash: sha256:0fhwlki24hfp98gljrnxnc0cs64rklvwm2zjjj0ck6r6vgml2nar
FileSize: 648508
NarHash: sha256:0jhjl0br7781mmmzr6rza9acqp9bpjbvi1kx1d4v20l2xk8jybmh
NarSize: 1806736
References: 23k009x6ahbn9whivq79llcm6207m0fb-attr-2.5.2 6yxih2q7hd8z4ibf2zwbgggy6hgad8gl-gmp-with-cxx-6.3.0 cp7wjv1pl4wapfk48svvizxd089v9h0a-coreutils-9.11 g0iqacr2c3q64lhb3zq7w0rcxxyz4p8a-acl-2.3.2 ias8xacs1h3jy7xgwi2awvim61k2ji6c-glibc-2.42-67
Deriver: nvmwza7ls167mw3j9fd60h009nwb5c17-coreutils-9.11.drv
Sig: cache.nixos.org-1:0tgD75hWhPgRIsa/27JtNOY0h5QZjMto+v010CUJ2a8vWiFCjQcglpZjk19o2HyYrpKrbhlRVU41dvdMo6+qDw==
";
    const CACHE_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

    #[test]
    fn parse_real_narinfo() {
        let ni = NarInfo::parse(COREUTILS).unwrap();
        assert_eq!(
            ni.url,
            "nar/0fhwlki24hfp98gljrnxnc0cs64rklvwm2zjjj0ck6r6vgml2nar.nar.xz"
        );
        assert_eq!(ni.compression, "xz");
        assert_eq!(ni.nar_size, 1806736);
        assert_eq!(ni.references.len(), 5);
        assert!(ni.references.contains(&ni.path));
        assert_eq!(
            ni.deriver.unwrap().base_name(),
            "nvmwza7ls167mw3j9fd60h009nwb5c17-coreutils-9.11.drv"
        );
    }

    #[test]
    fn fingerprint_format() {
        let ni = NarInfo::parse(COREUTILS).unwrap();
        assert_eq!(
            ni.fingerprint(),
            "1;/nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-coreutils-9.11;\
             sha256:0jhjl0br7781mmmzr6rza9acqp9bpjbvi1kx1d4v20l2xk8jybmh;1806736;\
             /nix/store/23k009x6ahbn9whivq79llcm6207m0fb-attr-2.5.2,\
             /nix/store/6yxih2q7hd8z4ibf2zwbgggy6hgad8gl-gmp-with-cxx-6.3.0,\
             /nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-coreutils-9.11,\
             /nix/store/g0iqacr2c3q64lhb3zq7w0rcxxyz4p8a-acl-2.3.2,\
             /nix/store/ias8xacs1h3jy7xgwi2awvim61k2ji6c-glibc-2.42-67"
        );
    }

    #[test]
    fn verify_real_signature() {
        let ni = NarInfo::parse(COREUTILS).unwrap();
        let key = PublicKey::parse(CACHE_KEY).unwrap();
        assert!(ni.verify(std::slice::from_ref(&key)));

        let other = PublicKey::parse("other:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=").unwrap();
        assert!(!ni.verify(&[other]));

        let mut forged = ni.clone();
        forged.nar_size += 1;
        assert!(!forged.verify(&[key.clone()]));

        let mut dropped = ni;
        dropped.references.pop_first();
        assert!(!dropped.verify(&[key]));
    }

    #[test]
    fn missing_fields() {
        assert!(
            NarInfo::parse("StorePath: /nix/store/cp7wjv1pl4wapfk48svvizxd089v9h0a-x\n").is_err()
        );
        let no_comp = COREUTILS.replace("Compression: xz\n", "");
        assert_eq!(NarInfo::parse(&no_comp).unwrap().compression, "bzip2");
    }
}
