//! Partition definitions with systemd-repart semantics (repart.d(5)): the `[Partition]`
//! keys, parsed from `.conf` text or from the user-data JSON.

use crate::gpt::{self, Guid};

pub const MIB: u64 = 1024 * 1024;
/// repart.c DEFAULT_MIN_SIZE.
pub const DEFAULT_MIN_SIZE: u64 = 10 * MIB;
/// resize-fs.h EXT4_MINIMAL_SIZE.
pub const EXT4_MIN_SIZE: u64 = 32 * MIB;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Ext4,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Partition {
    pub type_guid: Guid,
    pub type_name: String,
    pub label: Option<String>,
    pub uuid: Option<Guid>,
    /// The Format= value; only ext4 can be made.
    pub format: Option<String>,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    pub weight: u64,
    pub padding_min: u64,
    pub padding_max: Option<u64>,
    pub padding_weight: u64,
    /// MountPoint=: where the bootstrap mounts it below /mnt.
    pub mount: Option<String>,
    /// Keys that change what a new partition holds, which this does not do yet
    /// (Encrypt=, CopyFiles=, ...). They matter only when the partition is created.
    pub unsupported: Vec<String>,
}

impl Partition {
    pub fn new(type_name: &str) -> Result<Partition, String> {
        Ok(Partition {
            type_guid: type_guid(type_name)?,
            type_name: type_name.to_string(),
            label: None,
            uuid: None,
            format: None,
            size_min: None,
            size_max: None,
            weight: 1000,
            padding_min: 0,
            padding_max: None,
            padding_weight: 0,
            mount: None,
            unsupported: Vec::new(),
        })
    }

    pub fn fs(&self) -> Result<Option<Format>, String> {
        match self.format.as_deref() {
            None => Ok(None),
            Some("ext4") => Ok(Some(Format::Ext4)),
            Some(f) => Err(format!("Format={f} is not supported yet; only ext4")),
        }
    }

    /// Checks what creating this partition needs.
    pub fn creatable(&self) -> Result<(), String> {
        self.fs()?;
        match self.unsupported.first() {
            Some(k) => Err(format!("{k} is not supported yet")),
            None => Ok(()),
        }
    }

    /// repart.c partition_min_size for a new partition, before rounding to the grain.
    pub fn min_size(&self) -> u64 {
        let fs = if self.format.as_deref() == Some("ext4") {
            EXT4_MIN_SIZE
        } else {
            0
        };
        self.size_min.unwrap_or(DEFAULT_MIN_SIZE).max(fs).max(4096)
    }

    fn set(&mut self, key: &str, v: &str) -> Result<(), String> {
        let bad = || format!("{key}={v}: bad value");
        match key {
            "Type" => {
                self.type_guid = type_guid(v)?;
                self.type_name = v.to_string();
            }
            "Label" => self.label = Some(v.to_string()),
            "UUID" => self.uuid = Some(Guid::try_parse(v).ok_or_else(bad)?),
            "Format" => self.format = Some(v.to_string()),
            "SizeMinBytes" => self.size_min = Some(parse_size(v).ok_or_else(bad)?),
            "SizeMaxBytes" => self.size_max = Some(parse_size(v).ok_or_else(bad)?),
            "PaddingMinBytes" => self.padding_min = parse_size(v).ok_or_else(bad)?,
            "PaddingMaxBytes" => self.padding_max = Some(parse_size(v).ok_or_else(bad)?),
            "Weight" => self.weight = parse_weight(v).ok_or_else(bad)?,
            "PaddingWeight" => self.padding_weight = parse_weight(v).ok_or_else(bad)?,
            "MountPoint" => {
                let p = v.split(':').next().unwrap_or_default();
                if !p.starts_with('/') {
                    return Err(bad());
                }
                self.mount = Some(p.to_string());
            }
            "Encrypt" if v == "off" || v == "no" || v == "false" => {}
            "Verity" if v == "off" => {}
            "Encrypt" | "Verity" | "CopyBlocks" | "CopyFiles" | "MakeDirectories"
            | "MakeSymlinks" | "Subvolumes" | "DefaultSubvolume" | "Minimize" | "SupplementFor"
            | "ExcludeFiles" | "ExcludeFilesTarget" | "Compression" | "CompressionLevel"
            | "EncryptedVolume" => self.unsupported.push(format!("{key}={v}")),
            _ => eprintln!("spore: ignoring {key}={v}"),
        }
        Ok(())
    }

    /// Parses one repart.d `.conf` file; only the `[Partition]` section counts.
    pub fn parse_conf(text: &str) -> Result<Partition, String> {
        let mut p: Option<Partition> = None;
        let mut pending = Vec::new();
        let mut in_partition = false;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') {
                in_partition = line == "[Partition]";
                continue;
            }
            if !in_partition {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("bad line '{line}'"))?;
            pending.push((k.trim().to_string(), v.trim().to_string()));
        }
        for (k, v) in &pending {
            if k == "Type" {
                p = Some(Partition::new(v)?);
            }
        }
        let mut p = p.ok_or("definition has no Type=")?;
        for (k, v) in pending.iter().filter(|(k, _)| k != "Type") {
            p.set(k, v)?;
        }
        Ok(p)
    }

    /// Parses a user-data definition: an object with the repart key names, values as
    /// strings or numbers.
    pub fn from_json(v: &serde_json::Value) -> Result<Partition, String> {
        let o = v
            .as_object()
            .ok_or("a layout partition must be an object")?;
        let s = |v: &serde_json::Value| match v {
            serde_json::Value::String(s) => Ok(s.clone()),
            serde_json::Value::Number(n) => Ok(n.to_string()),
            serde_json::Value::Bool(b) => Ok(if *b { "yes" } else { "no" }.to_string()),
            _ => Err(format!("bad value {v}")),
        };
        let ty = o.get("Type").ok_or("layout partition has no Type")?;
        let mut p = Partition::new(&s(ty)?)?;
        for (k, v) in o.iter().filter(|(k, _)| *k != "Type") {
            p.set(k, &s(v)?)?;
        }
        Ok(p)
    }
}

impl Guid {
    pub fn try_parse(s: &str) -> Option<Guid> {
        let hex: String = s.chars().filter(|&c| c != '-').collect();
        if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some(Guid::parse(s))
    }
}

/// The well-known names of gpt.c:gpt_partition_type_table that make sense here, or a
/// raw type GUID. `root` and `usr` are the native (x86-64 or arm64) ones.
pub fn type_guid(name: &str) -> Result<Guid, String> {
    let native = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86-64"
    };
    let name = match name {
        "root" => format!("root-{native}"),
        "usr" => format!("usr-{native}"),
        n => n.replace('_', "-"),
    };
    let g = match name.as_str() {
        "esp" => gpt::ESP,
        "xbootldr" => Guid::parse("BC13C2FF-59E6-4262-A352-B275FD6F7172"),
        "swap" => Guid::parse("0657FD6D-A4AB-43C4-84E5-0933C84B4F4F"),
        "home" => Guid::parse("933AC7E1-2EB4-4F13-B844-0E14E2AEF915"),
        "srv" => Guid::parse("3B8F8425-20E0-4F3B-907F-1A25A76F98E8"),
        "var" => Guid::parse("4D21B016-B534-45C2-A9FB-5C16E091FD2D"),
        "tmp" => Guid::parse("7EC6F557-3BC5-4ACA-B293-16EF5DF639D1"),
        "user-home" => Guid::parse("773F91EF-66D4-49B5-BD83-D683BF40AD16"),
        "linux-generic" => gpt::LINUX_FS,
        "root-x86-64" => Guid::parse("4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709"),
        "root-arm64" => Guid::parse("B921B045-1DF0-41C3-AF44-4C6F280D3FAE"),
        "usr-x86-64" => Guid::parse("8484680C-9521-48C6-9C11-B0720656F69E"),
        "usr-arm64" => Guid::parse("B0E01050-EE5F-4390-949A-9101B17104E9"),
        n => Guid::try_parse(n).ok_or_else(|| format!("unknown partition type '{n}'"))?,
    };
    Ok(g)
}

/// Mirrors parse-util.c:parse_size with base 1024: `512M`, `1.5G`, `4096`.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mul) = match s.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => {
            let m: u64 = match &s[i..] {
                "B" => 1,
                "K" => 1 << 10,
                "M" => 1 << 20,
                "G" => 1 << 30,
                "T" => 1 << 40,
                "P" => 1 << 50,
                "E" => 1 << 60,
                _ => return None,
            };
            (&s[..i], m)
        }
        None => (s, 1),
    };
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    let int: u64 = int.parse().ok()?;
    let mut v = int.checked_mul(mul)?;
    if !frac.is_empty() {
        let digits: u32 = frac.len() as u32;
        let f: u64 = frac.parse().ok()?;
        v = v.checked_add((f as u128 * mul as u128 / 10u128.pow(digits)) as u64)?;
    }
    Some(v)
}

/// repart.c accepts 0..=1000000.
fn parse_weight(s: &str) -> Option<u64> {
    s.parse().ok().filter(|&w| w <= 1_000_000)
}

/// Where the partitions come from, highest precedence first.
#[derive(Clone, Debug)]
pub struct Layout {
    pub partitions: Vec<Partition>,
    /// Remove every partition but the ESP and the BIOS boot partition first.
    pub wipe: bool,
    pub source: &'static str,
}

impl Layout {
    /// ext4 `nixos` over the free space; the ESP and BIOS boot partition stay as
    /// unmatched partitions.
    pub fn default_convention() -> Layout {
        let mut root = Partition::new("linux-generic").unwrap();
        root.label = Some("nixos".into());
        root.format = Some("ext4".into());
        root.size_min = Some(512 * MIB);
        root.mount = Some("/".into());
        Layout {
            partitions: vec![root],
            wipe: false,
            source: "the default convention",
        }
    }

    /// The user-data `layout`: `{"wipe": bool, "partitions": [{...}, ...]}`.
    pub fn from_json(v: &serde_json::Value) -> Result<Layout, String> {
        let wipe = v.get("wipe").and_then(|w| w.as_bool()).unwrap_or(false);
        let parts = v
            .get("partitions")
            .and_then(|p| p.as_array())
            .ok_or("layout has no partitions array")?;
        Ok(Layout {
            partitions: parts
                .iter()
                .map(Partition::from_json)
                .collect::<Result<_, _>>()
                .map_err(|e| format!("user-data layout: {e}"))?,
            wipe,
            source: "the user-data layout",
        })
    }

    /// repart.d files, already sorted by file name.
    pub fn from_confs(files: &[(String, String)]) -> Result<Layout, String> {
        Ok(Layout {
            partitions: files
                .iter()
                .map(|(name, text)| Partition::parse_conf(text).map_err(|e| format!("{name}: {e}")))
                .collect::<Result<_, _>>()?,
            wipe: false,
            source: "the repart.d of the system",
        })
    }

    /// The partition that holds /: MountPoint=/ if any definition has a MountPoint,
    /// else the first root type, else the first ext4 definition.
    pub fn root_index(&self) -> Option<usize> {
        let ps = &self.partitions;
        if ps.iter().any(|p| p.mount.is_some()) {
            return ps.iter().position(|p| p.mount.as_deref() == Some("/"));
        }
        let root = [
            type_guid("root-x86-64").unwrap(),
            type_guid("root-arm64").unwrap(),
        ];
        ps.iter()
            .position(|p| root.contains(&p.type_guid))
            .or_else(|| ps.iter().position(|p| p.format.as_deref() == Some("ext4")))
    }

    /// (mount point, definition index), shortest path first.
    pub fn mounts(&self) -> Vec<(String, usize)> {
        let mut m: Vec<(String, usize)> = self
            .partitions
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.mount.clone().map(|m| (m, i)))
            .collect();
        if m.is_empty()
            && let Some(i) = self.root_index()
        {
            m.push(("/".into(), i));
        }
        m.sort_by_key(|(p, _)| p.len());
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("512M"), Some(512 * MIB));
        assert_eq!(parse_size("1.5G"), Some(1536 * MIB));
        assert_eq!(parse_size("2T"), Some(2 << 40));
        assert_eq!(parse_size("1X"), None);
        assert_eq!(parse_size("M"), None);
    }

    #[test]
    fn conf_file() {
        let p = Partition::parse_conf(
            "# NixOS systemd.repart\n[Partition]\nType=root\nLabel=nixos\nFormat=ext4\n\
             SizeMinBytes=2G\nWeight=500\nMountPoint=/:x-systemd.growfs\n[Other]\nType=swap\n",
        )
        .unwrap();
        let native = if cfg!(target_arch = "aarch64") {
            "root-arm64"
        } else {
            "root-x86-64"
        };
        assert_eq!(p.type_guid, type_guid(native).unwrap());
        assert_eq!(p.label.as_deref(), Some("nixos"));
        assert_eq!(p.size_min, Some(2 << 30));
        assert_eq!(p.weight, 500);
        assert_eq!(p.mount.as_deref(), Some("/"));
        assert_eq!(p.fs(), Ok(Some(Format::Ext4)));
        assert!(p.creatable().is_ok());
        assert!(Partition::parse_conf("[Partition]\nLabel=x\n").is_err());
        assert!(Partition::parse_conf("[Partition]\nType=nope\n").is_err());
    }

    #[test]
    fn raw_guid_and_unsupported() {
        let p = Partition::parse_conf(
            "[Partition]\nType=21686148-6449-6E6F-744E-656564454649\nEncrypt=tpm2\nFormat=xfs\n",
        )
        .unwrap();
        assert_eq!(p.type_guid, gpt::BIOS_BOOT);
        assert_eq!(p.unsupported, ["Encrypt=tpm2"]);
        assert!(p.creatable().unwrap_err().contains("xfs"));
        let mut q = p.clone();
        q.format = None;
        assert_eq!(
            q.creatable().unwrap_err(),
            "Encrypt=tpm2 is not supported yet"
        );
    }

    #[test]
    fn json_layout() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"wipe": true, "partitions": [
                {"Type": "root", "Label": "nixos", "Format": "ext4", "SizeMaxBytes": "20G", "MountPoint": "/"},
                {"Type": "var", "Label": "data", "Format": "ext4", "Weight": 2000, "MountPoint": "/var"}]}"#,
        )
        .unwrap();
        let l = Layout::from_json(&v).unwrap();
        assert!(l.wipe);
        assert_eq!(l.partitions[1].weight, 2000);
        assert_eq!(l.partitions[0].size_max, Some(20 << 30));
        assert_eq!(l.mounts(), [("/".to_string(), 0), ("/var".to_string(), 1)]);
    }

    #[test]
    fn root_without_mount_points() {
        let l = Layout::from_confs(&[
            (
                "00-esp.conf".into(),
                "[Partition]\nType=esp\nFormat=vfat\n".into(),
            ),
            (
                "10-root.conf".into(),
                "[Partition]\nType=root\nFormat=ext4\n".into(),
            ),
        ])
        .unwrap();
        assert_eq!(l.root_index(), Some(1));
        assert_eq!(l.mounts(), [("/".to_string(), 1)]);
        assert_eq!(Layout::default_convention().root_index(), Some(0));
    }
}
