//! UEFI 2.10 section 5.3: GUID partition table.

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    /// Parses the text form; the first three fields are stored little-endian.
    pub const fn parse(s: &str) -> Guid {
        let s = s.as_bytes();
        let mut hex = [0u8; 32];
        let (mut i, mut n) = (0, 0);
        while i < s.len() {
            let c = s[i];
            i += 1;
            if c == b'-' {
                continue;
            }
            hex[n] = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("bad GUID"),
            };
            n += 1;
        }
        assert!(n == 32);
        let mut be = [0u8; 16];
        let mut k = 0;
        while k < 16 {
            be[k] = hex[2 * k] << 4 | hex[2 * k + 1];
            k += 1;
        }
        Guid([
            be[3], be[2], be[1], be[0], be[5], be[4], be[7], be[6], be[8], be[9], be[10], be[11],
            be[12], be[13], be[14], be[15],
        ])
    }

    pub fn random() -> io::Result<Guid> {
        let mut b = [0u8; 16];
        let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), 16, 0) };
        if n != 16 {
            return Err(io::Error::last_os_error());
        }
        b[7] = b[7] & 0x0f | 0x40;
        b[8] = b[8] & 0x3f | 0x80;
        Ok(Guid(b))
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 16]
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9]
        )?;
        b[10..].iter().try_for_each(|x| write!(f, "{x:02X}"))
    }
}

pub const ESP: Guid = Guid::parse("C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
pub const BIOS_BOOT: Guid = Guid::parse("21686148-6449-6E6F-744E-656564454649");
pub const LINUX_FS: Guid = Guid::parse("0FC63DAF-8483-4772-8E79-3D69D8477DE4");

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub type_guid: Guid,
    pub unique: Guid,
    pub first_lba: u64,
    pub last_lba: u64,
    pub attrs: u64,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Gpt {
    pub sector: u64,
    pub disk_sectors: u64,
    pub disk_guid: Guid,
    pub first_usable: u64,
    pub last_usable: u64,
    entry_size: u32,
    /// Slot i is partition number i + 1.
    pub entries: Vec<Option<Entry>>,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("GPT: {why}"))
}

impl Gpt {
    /// Reads the primary header and entries. `disk_bytes` is the current device size,
    /// which may be larger than when the table was written.
    pub fn read<D: Read + Seek>(dev: &mut D, sector: u64, disk_bytes: u64) -> io::Result<Gpt> {
        let mut h = vec![0u8; sector as usize];
        dev.seek(SeekFrom::Start(sector))?;
        dev.read_exact(&mut h)?;
        if &h[0..8] != b"EFI PART" {
            return Err(bad("no signature"));
        }
        let hsize = le32(&h, 12) as usize;
        if !(92..=sector as usize).contains(&hsize) {
            return Err(bad("bad header size"));
        }
        let mut hc = h[..hsize].to_vec();
        hc[16..20].fill(0);
        if crc32(&hc) != le32(&h, 16) {
            return Err(bad("header CRC mismatch"));
        }
        let entries_lba = le64(&h, 72);
        let num = le32(&h, 80) as usize;
        let entry_size = le32(&h, 84);
        if entry_size < 128 || num > 1024 {
            return Err(bad("bad entry array"));
        }
        let mut raw = vec![0u8; num * entry_size as usize];
        dev.seek(SeekFrom::Start(entries_lba * sector))?;
        dev.read_exact(&mut raw)?;
        if crc32(&raw) != le32(&h, 88) {
            return Err(bad("entry array CRC mismatch"));
        }
        let entries = raw
            .chunks(entry_size as usize)
            .map(|e| {
                let type_guid = Guid(e[0..16].try_into().unwrap());
                if type_guid.is_zero() {
                    return None;
                }
                let name: Vec<u16> = e[56..128]
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&c| c != 0)
                    .collect();
                Some(Entry {
                    type_guid,
                    unique: Guid(e[16..32].try_into().unwrap()),
                    first_lba: le64(e, 32),
                    last_lba: le64(e, 40),
                    attrs: le64(e, 48),
                    name: String::from_utf16_lossy(&name),
                })
            })
            .collect();
        Ok(Gpt {
            sector,
            disk_sectors: disk_bytes / sector,
            disk_guid: Guid(h[56..72].try_into().unwrap()),
            first_usable: le64(&h, 40),
            last_usable: le64(&h, 48),
            entry_size,
            entries,
        })
    }

    fn entry_sectors(&self) -> u64 {
        (self.entries.len() as u64 * self.entry_size as u64).div_ceil(self.sector)
    }

    /// The last usable LBA once the backup table moves to the end of the disk.
    pub fn max_last_usable(&self) -> u64 {
        self.disk_sectors - 2 - self.entry_sectors()
    }

    /// Free extents inside the usable area, in LBAs, as the table would have them after a
    /// write (backup table at the end of the disk).
    pub fn free(&self) -> Vec<(u64, u64)> {
        let mut used: Vec<(u64, u64)> = self
            .entries
            .iter()
            .flatten()
            .map(|e| (e.first_lba, e.last_lba))
            .collect();
        used.sort();
        let mut free = Vec::new();
        let mut next = self.first_usable;
        for (a, b) in used {
            if a > next {
                free.push((next, a - 1));
            }
            next = next.max(b + 1);
        }
        let last = self.max_last_usable();
        if next <= last {
            free.push((next, last));
        }
        free
    }

    /// Adds `e` in the first empty slot and returns its partition number.
    pub fn add(&mut self, e: Entry) -> io::Result<u32> {
        if e.first_lba < self.first_usable || e.last_lba > self.max_last_usable() {
            return Err(bad("partition outside the usable area"));
        }
        let overlap = self
            .entries
            .iter()
            .flatten()
            .any(|o| e.first_lba <= o.last_lba && o.first_lba <= e.last_lba);
        if overlap || e.first_lba > e.last_lba {
            return Err(bad("partition overlaps another"));
        }
        let slot = self
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| bad("no free entry"))?;
        self.entries[slot] = Some(e);
        Ok(slot as u32 + 1)
    }

    fn entries_raw(&self) -> Vec<u8> {
        let es = self.entry_size as usize;
        let mut raw = vec![0u8; (self.entry_sectors() * self.sector) as usize];
        for (i, e) in self.entries.iter().enumerate() {
            let Some(e) = e else { continue };
            let o = &mut raw[i * es..(i + 1) * es];
            o[0..16].copy_from_slice(&e.type_guid.0);
            o[16..32].copy_from_slice(&e.unique.0);
            o[32..40].copy_from_slice(&e.first_lba.to_le_bytes());
            o[40..48].copy_from_slice(&e.last_lba.to_le_bytes());
            o[48..56].copy_from_slice(&e.attrs.to_le_bytes());
            for (k, c) in e.name.encode_utf16().take(36).enumerate() {
                o[56 + 2 * k..58 + 2 * k].copy_from_slice(&c.to_le_bytes());
            }
        }
        raw
    }

    fn header(&self, my: u64, alt: u64, entries_lba: u64, entries_crc: u32) -> Vec<u8> {
        let mut h = vec![0u8; self.sector as usize];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&self.first_usable.to_le_bytes());
        h[48..56].copy_from_slice(&self.last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&self.disk_guid.0);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        h[84..88].copy_from_slice(&self.entry_size.to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    }

    /// Writes both tables, with the backup at the end of the disk, and fixes the size in
    /// the protective MBR. The MBR boot code (Limine's stage 1) stays untouched.
    pub fn write<D: Read + Write + Seek>(&mut self, dev: &mut D) -> io::Result<()> {
        self.last_usable = self.max_last_usable();
        let last = self.disk_sectors - 1;
        let raw = self.entries_raw();
        let crc = crc32(&raw[..self.entries.len() * self.entry_size as usize]);
        let backup_entries = last - self.entry_sectors();

        let mut mbr = [0u8; 512];
        dev.seek(SeekFrom::Start(0))?;
        dev.read_exact(&mut mbr)?;
        if mbr[450] == 0xEE {
            let size = (self.disk_sectors - 1).min(u32::MAX as u64) as u32;
            mbr[458..462].copy_from_slice(&size.to_le_bytes());
            dev.seek(SeekFrom::Start(0))?;
            dev.write_all(&mbr)?;
        }
        dev.seek(SeekFrom::Start(backup_entries * self.sector))?;
        dev.write_all(&raw)?;
        dev.write_all(&self.header(last, 1, backup_entries, crc))?;
        dev.seek(SeekFrom::Start(2 * self.sector))?;
        dev.write_all(&raw)?;
        dev.seek(SeekFrom::Start(self.sector))?;
        dev.write_all(&self.header(1, last, 2, crc))?;
        dev.flush()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    const MIB: u64 = 1024 * 1024;

    /// A disk laid out like disk.nix: BIOS boot at 2048, ESP at 4096, 131 MiB.
    pub(crate) fn image(size: u64) -> Vec<u8> {
        let mut disk = vec![0u8; size as usize];
        disk[446 + 4] = 0xEE;
        disk[510] = 0x55;
        disk[511] = 0xAA;
        let mut g = Gpt {
            sector: 512,
            disk_sectors: size / 512,
            disk_guid: Guid::parse("11111111-2222-3333-4444-555555555555"),
            first_usable: 34,
            last_usable: 0,
            entry_size: 128,
            entries: vec![None; 128],
        };
        g.add(Entry {
            type_guid: ESP,
            unique: Guid::parse("AAAAAAAA-2222-3333-4444-555555555555"),
            first_lba: 4096,
            last_lba: 4096 + 262144 - 1,
            attrs: 0,
            name: String::new(),
        })
        .unwrap();
        g.add(Entry {
            type_guid: BIOS_BOOT,
            unique: Guid::parse("BBBBBBBB-2222-3333-4444-555555555555"),
            first_lba: 2048,
            last_lba: 4095,
            attrs: 0,
            name: String::new(),
        })
        .unwrap();
        g.write(&mut Cursor::new(&mut disk)).unwrap();
        disk
    }

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn guid_text_form() {
        assert_eq!(format!("{ESP:?}"), "C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
        assert_eq!(&ESP.0[..4], &[0x28, 0x73, 0x2A, 0xC1]);
        let r = Guid::random().unwrap();
        assert_eq!(r.0[7] >> 4, 4);
    }

    #[test]
    fn grow_and_append() {
        let mut disk = image(131 * MIB);
        let boot_code = disk[..440].to_vec();
        disk.resize((512 * MIB) as usize, 0);
        let mut dev = Cursor::new(&mut disk);
        let mut g = Gpt::read(&mut dev, 512, 512 * MIB).unwrap();
        assert_eq!(g.last_usable, 131 * MIB / 512 - 34);
        let free = g.free();
        assert_eq!(free, vec![(34, 2047), (266240, 512 * MIB / 512 - 34)]);
        let n = g
            .add(Entry {
                type_guid: LINUX_FS,
                unique: Guid::random().unwrap(),
                first_lba: 266240,
                last_lba: free[1].1,
                attrs: 0,
                name: "nixos".into(),
            })
            .unwrap();
        assert_eq!(n, 3);
        g.write(&mut dev).unwrap();

        let again = Gpt::read(&mut dev, 512, 512 * MIB).unwrap();
        assert_eq!(again.entries[2].as_ref().unwrap().name, "nixos");
        assert_eq!(again.last_usable, 512 * MIB / 512 - 34);
        drop(dev);

        let last = (512 * MIB / 512 - 1) as usize;
        let backup = &disk[last * 512..last * 512 + 512];
        assert_eq!(&backup[0..8], b"EFI PART");
        assert_eq!(le64(backup, 24), last as u64);
        assert_eq!(le64(backup, 32), 1);
        let mut hc = backup[..92].to_vec();
        hc[16..20].fill(0);
        assert_eq!(crc32(&hc), le32(backup, 16));
        assert_eq!(le32(&disk, 458), (512 * MIB / 512 - 1) as u32);
        assert_eq!(&disk[..440], &boot_code[..]);
    }

    #[test]
    fn rejects_overlap_and_corruption() {
        let mut disk = image(131 * MIB);
        let mut g = Gpt::read(&mut Cursor::new(&mut disk), 512, 131 * MIB).unwrap();
        let e = Entry {
            type_guid: LINUX_FS,
            unique: Guid::random().unwrap(),
            first_lba: 4000,
            last_lba: 5000,
            attrs: 0,
            name: String::new(),
        };
        assert!(g.add(e).is_err());
        disk[512 + 40] ^= 1;
        assert!(Gpt::read(&mut Cursor::new(&mut disk), 512, 131 * MIB).is_err());
    }
}
