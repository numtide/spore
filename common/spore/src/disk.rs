//! The disk step: desired layout -> diff against the GPT -> create what is missing ->
//! format what is new. Planning follows systemd-repart (repart.c of systemd 258):
//!
//! - Each existing partition, in table order, matches the first unmatched definition of
//!   the same type. Unmatched partitions stay as they are.
//! - New partitions go first-fit into the free areas, smallest area first, each with its
//!   minimum size plus padding, in 4 KiB grains.
//! - Each free area is shared by weight in three phases: partitions whose minimum is
//!   above their share get the minimum, those whose maximum is below it get the maximum,
//!   the rest get the share. What is left goes to the first that can take it.
//!
//! Deliberate difference: matched partitions never grow, since that would need a file
//! system resize.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::gpt::{self, Entry, Gpt, Guid};
use crate::repart::{Format, Layout};

const GRAIN: u64 = 4096;

fn up(v: u64) -> u64 {
    v.div_ceil(GRAIN) * GRAIN
}

fn down(v: u64) -> u64 {
    v / GRAIN * GRAIN
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    /// Per definition: the partition number, and whether it is new.
    pub numbers: Vec<(u32, bool)>,
    /// Partition numbers removed by `wipe`.
    pub wiped: Vec<u32>,
    /// New entries, by definition index.
    pub create: Vec<(usize, Entry)>,
}

struct Area {
    start: u64,
    size: u64,
    allocated: u64,
}

/// repart.c scale_by_weight, without the overflow dance.
fn scale(value: u64, weight: u64, sum: u64) -> u64 {
    if weight == 0 {
        return 0;
    }
    (value as u128 * weight as u128 / sum as u128) as u64
}

/// Diffs `layout` against `table`. Entries are placed in bytes on 4 KiB grains.
pub fn plan(layout: &Layout, table: &Gpt) -> Result<Plan, String> {
    let sector = table.sector;
    let mut entries: Vec<(u32, Entry)> = table
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.clone().map(|e| (i as u32 + 1, e)))
        .collect();
    let mut wiped = Vec::new();
    if layout.wipe {
        entries.retain(|(n, e)| {
            let keep = e.type_guid == gpt::ESP || e.type_guid == gpt::BIOS_BOOT;
            if !keep {
                wiped.push(*n);
            }
            keep
        });
    }

    let defs = &layout.partitions;
    let mut numbers: Vec<Option<u32>> = vec![None; defs.len()];
    for (n, e) in &entries {
        if let Some(i) =
            (0..defs.len()).find(|&i| numbers[i].is_none() && defs[i].type_guid == e.type_guid)
        {
            numbers[i] = Some(*n);
        }
    }
    let new: Vec<usize> = (0..defs.len()).filter(|&i| numbers[i].is_none()).collect();
    for &i in &new {
        defs[i]
            .creatable()
            .map_err(|e| format!("partition {} ({}): {e}", i + 1, defs[i].type_name))?;
    }

    let mut used: Vec<(u64, u64)> = entries
        .iter()
        .map(|(_, e)| (e.first_lba * sector, (e.last_lba + 1) * sector))
        .collect();
    used.sort();
    let first = up(table.first_usable * sector);
    let end = down((table.max_last_usable() + 1) * sector);
    let mut areas = Vec::new();
    let mut at = first;
    for &(a, b) in used.iter().chain([(end, end)].iter()) {
        let start = up(at);
        let stop = down(a.min(end));
        if stop > start {
            areas.push(Area {
                start,
                size: stop - start,
                allocated: 0,
            });
        }
        at = at.max(b);
    }

    let min: Vec<u64> = defs.iter().map(|p| up(p.min_size())).collect();
    let max: Vec<u64> = defs
        .iter()
        .zip(&min)
        .map(|(p, &m)| p.size_max.map_or(u64::MAX, |x| down(x).max(m)))
        .collect();
    let pmax: Vec<u64> = defs
        .iter()
        .map(|p| p.padding_max.unwrap_or(u64::MAX))
        .collect();

    let mut order: Vec<usize> = (0..areas.len()).collect();
    order.sort_by_key(|&a| areas[a].size);
    let mut area_of = vec![usize::MAX; defs.len()];
    for &i in &new {
        let required = up(min[i] + defs[i].padding_min);
        let a = order
            .iter()
            .copied()
            .find(|&a| areas[a].size - areas[a].allocated >= required)
            .ok_or_else(|| {
                format!(
                    "no free area fits partition {} ({}, at least {} MiB)",
                    i + 1,
                    defs[i].type_name,
                    required >> 20
                )
            })?;
        areas[a].allocated += required;
        area_of[i] = a;
    }

    let mut size = vec![None::<u64>; defs.len()];
    let mut pad = vec![None::<u64>; defs.len()];
    for (a, area) in areas.iter().enumerate() {
        let members: Vec<usize> = new.iter().copied().filter(|&i| area_of[i] == a).collect();
        if members.is_empty() {
            continue;
        }
        let mut span = area.size;
        let mut wsum: u64 = members
            .iter()
            .map(|&i| defs[i].weight + defs[i].padding_weight)
            .sum();
        let mut phase = 0;
        while phase < 3 {
            let mut again = false;
            for &i in &members {
                let d = &defs[i];
                for (slot, w, lo, hi) in [
                    (&mut size[i], d.weight, min[i], max[i]),
                    (&mut pad[i], d.padding_weight, d.padding_min, pmax[i]),
                ] {
                    if slot.is_some() {
                        continue;
                    }
                    let share = scale(span, w, wsum);
                    let v = match phase {
                        0 if lo > share => {
                            again = true;
                            lo
                        }
                        1 if hi < share => {
                            again = true;
                            hi
                        }
                        2 => down(share).clamp(lo, hi),
                        _ => continue,
                    };
                    *slot = Some(v);
                    span = span.saturating_sub(up(v));
                    wsum -= w;
                }
            }
            if !again {
                phase += 1;
            }
        }
        for &i in &members {
            if span == 0 {
                break;
            }
            let s = size[i].unwrap();
            let m = down(s + span).max(s).min(max[i]);
            span = span.saturating_sub(up(m - s));
            size[i] = Some(m);
        }
    }

    let mut names: Vec<String> = entries.iter().map(|(_, e)| e.name.clone()).collect();
    let mut free_slots = (0..table.entries.len() as u32)
        .map(|s| s + 1)
        .filter(|n| !entries.iter().any(|(m, _)| m == n));
    let mut cursor: Vec<u64> = areas.iter().map(|a| a.start).collect();
    let mut create = Vec::new();
    let mut out = Vec::new();
    for (i, d) in defs.iter().enumerate() {
        if let Some(n) = numbers[i] {
            out.push((n, false));
            continue;
        }
        let a = area_of[i];
        let (s, p) = (size[i].unwrap(), pad[i].unwrap());
        let label = match &d.label {
            Some(l) => l.clone(),
            None => {
                let base = d.type_name.clone();
                let mut l = base.clone();
                let mut k = 2;
                while names.contains(&l) {
                    l = format!("{base}-{k}");
                    k += 1;
                }
                l
            }
        };
        names.push(label.clone());
        let n = free_slots.next().ok_or("no free GPT entry")?;
        create.push((
            i,
            Entry {
                type_guid: d.type_guid,
                unique: match d.uuid {
                    Some(u) => u,
                    None => Guid::random().map_err(|e| e.to_string())?,
                },
                first_lba: cursor[a] / sector,
                last_lba: (cursor[a] + s) / sector - 1,
                attrs: 0,
                name: label,
            },
        ));
        cursor[a] += up(s) + up(p);
        out.push((n, true));
    }
    Ok(Plan {
        numbers: out,
        wiped,
        create,
    })
}

fn sys_read(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

pub struct Disk {
    pub name: String,
    pub dev: PathBuf,
    pub bytes: u64,
}

impl Disk {
    /// The first whole disk whose GPT has an ESP: the one the firmware booted.
    pub fn find_boot() -> Result<(Disk, Gpt), String> {
        let mut names: Vec<String> = fs::read_dir("/sys/block")
            .map_err(|e| format!("/sys/block: {e}"))?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| {
                !["loop", "ram", "zram", "sr", "fd"]
                    .iter()
                    .any(|p| n.starts_with(p))
            })
            .collect();
        names.sort();
        for name in names {
            let sys = Path::new("/sys/block").join(&name);
            let sector = sys_read(sys.join("queue/logical_block_size"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(512);
            let Some(bytes) = sys_read(sys.join("size")).and_then(|s| s.parse::<u64>().ok()) else {
                continue;
            };
            let disk = Disk {
                dev: Path::new("/dev").join(&name),
                name,
                bytes: bytes * 512,
            };
            let Ok(mut f) = File::open(&disk.dev) else {
                continue;
            };
            if let Ok(t) = Gpt::read(&mut f, sector, disk.bytes)
                && t.entries.iter().flatten().any(|e| e.type_guid == gpt::ESP)
            {
                return Ok((disk, t));
            }
        }
        Err("no disk with an ESP".into())
    }

    pub fn partition(&self, n: u32) -> PathBuf {
        let sep = if self.name.ends_with(|c: char| c.is_ascii_digit()) {
            "p"
        } else {
            ""
        };
        Path::new("/dev").join(format!("{}{sep}{n}", self.name))
    }

    /// Returns the number of retries (EBUSY while a partition is still open).
    fn reread(&self) -> Result<u32, String> {
        const BLKRRPART: libc::c_ulong = 0x125f;
        let f = File::open(&self.dev).map_err(|e| e.to_string())?;
        let mut last = io::Error::from_raw_os_error(0);
        for i in 0..50 {
            if unsafe { libc::ioctl(f.as_raw_fd(), BLKRRPART as _) } == 0 {
                return Ok(i);
            }
            last = io::Error::last_os_error();
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("BLKRRPART {}: {last}", self.dev.display()))
    }
}

fn wait_for(p: &Path) -> Result<(), String> {
    let t = Instant::now();
    while !p.exists() {
        if t.elapsed() > Duration::from_secs(10) {
            return Err(format!("{} did not appear", p.display()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn read_at(dev: &Path, off: u64, buf: &mut [u8]) -> io::Result<()> {
    let mut f = File::open(dev)?;
    f.seek(SeekFrom::Start(off))?;
    f.read_exact(buf)
}

pub fn has_ext4(dev: &Path) -> bool {
    let mut magic = [0u8; 2];
    read_at(dev, 1024 + 56, &mut magic).is_ok() && magic == [0x53, 0xEF]
}

/// No file system signature in the first 64 KiB: a partition made by a run that
/// stopped before mkfs.
fn blank(dev: &Path) -> bool {
    let mut b = vec![0u8; 64 * 1024];
    read_at(dev, 0, &mut b).is_ok() && b.iter().all(|&x| x == 0)
}

fn zero_start(dev: &Path) -> Result<(), String> {
    let mut f = OpenOptions::new()
        .write(true)
        .open(dev)
        .map_err(|e| format!("{}: {e}", dev.display()))?;
    f.write_all(&vec![0u8; 1024 * 1024])
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("{}: {e}", dev.display()))
}

fn mkfs(fmt: Format, dev: &Path, label: &str) -> Result<(), String> {
    match fmt {
        Format::Ext4 => {
            let label: String = label.chars().take(16).collect();
            // mke2fs discards the whole partition by default: 1.3-1.8 s on hcloud cax,
            // for nothing, since a disk made from a snapshot is sparse
            let st = Command::new("/bin/mke2fs")
                .args(["-q", "-F", "-t", "ext4", "-L", &label])
                .args(["-E", "nodiscard,lazy_itable_init=1,lazy_journal_init=1"])
                .arg(dev)
                .status()
                .map_err(|e| format!("/bin/mke2fs: {e}"))?;
            if !st.success() {
                return Err(format!("mke2fs {} failed: {st}", dev.display()));
            }
        }
    }
    Ok(())
}

pub struct Prepared {
    pub esp: PathBuf,
    /// (mount point, device), shortest path first; "/" is the first.
    pub mounts: Vec<(String, PathBuf)>,
    pub created: usize,
    /// Mount points whose partition has no ext4; the target mounts those itself.
    pub skipped: Vec<String>,
}

/// Brings the boot disk to `layout`: wipes if asked, creates the missing partitions,
/// formats the new ones and blank matched ones.
pub fn prepare(layout: &Layout) -> Result<Prepared, String> {
    let t = Instant::now();
    let (disk, mut table) = Disk::find_boot()?;
    log!(
        "disk: {} found in {} ms",
        disk.dev.display(),
        t.elapsed().as_millis()
    );
    let p = plan(layout, &table)?;
    for n in &p.wiped {
        table.entries[*n as usize - 1] = None;
    }
    for (_, e) in &p.create {
        table.add(e.clone()).map_err(|e| e.to_string())?;
    }
    let ms = |t: Instant| t.elapsed().as_millis();
    let grown = table.last_usable != table.max_last_usable();
    if !p.create.is_empty() || !p.wiped.is_empty() || grown {
        let t = Instant::now();
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&disk.dev)
            .map_err(|e| format!("{}: {e}", disk.dev.display()))?;
        table.write(&mut f).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        drop(f);
        let w = ms(t);
        let t = Instant::now();
        let retries = disk.reread()?;
        log!(
            "disk: GPT written in {w} ms, re-read in {} ms ({retries} retries)",
            ms(t)
        );
    }
    for (i, d) in layout.partitions.iter().enumerate() {
        let (n, new) = p.numbers[i];
        let dev = disk.partition(n);
        let t = Instant::now();
        wait_for(&dev)?;
        let waited = ms(t);
        let label = table.entries[n as usize - 1]
            .as_ref()
            .map(|e| e.name.clone())
            .unwrap_or_default();
        let t = Instant::now();
        if new || blank(&dev) {
            match d.fs()? {
                Some(fmt) => {
                    mkfs(fmt, &dev, &label)?;
                    log!(
                        "disk: {} appeared after {waited} ms, mkfs took {} ms",
                        dev.display(),
                        ms(t)
                    );
                }
                None if new => zero_start(&dev)?,
                None => {}
            }
        }
    }
    let mut mounts = Vec::new();
    let mut skipped = Vec::new();
    for (m, i) in layout.mounts() {
        let dev = disk.partition(p.numbers[i].0);
        if has_ext4(&dev) {
            mounts.push((m, dev));
        } else if m == "/" {
            return Err(format!("{} for / has no ext4 file system", dev.display()));
        } else {
            skipped.push(m);
        }
    }
    if mounts.first().map(|(m, _)| m.as_str()) != Some("/") {
        return Err(format!("{} has no partition for /", layout.source));
    }
    let esp = table
        .entries
        .iter()
        .position(|e| e.as_ref().is_some_and(|e| e.type_guid == gpt::ESP))
        .ok_or("the ESP is gone")?;
    Ok(Prepared {
        esp: disk.partition(esp as u32 + 1),
        mounts,
        created: p.create.len(),
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpt::tests::image;
    use crate::repart::{MIB, Partition};
    use std::io::Cursor;

    fn table(size: u64) -> Gpt {
        let mut disk = image(131 * MIB);
        disk.resize(size as usize, 0);
        Gpt::read(&mut Cursor::new(&mut disk), 512, size).unwrap()
    }

    fn part(conf: &str) -> Partition {
        Partition::parse_conf(&format!("[Partition]\n{conf}")).unwrap()
    }

    fn layout(parts: Vec<Partition>) -> Layout {
        Layout {
            partitions: parts,
            wipe: false,
            source: "test",
        }
    }

    fn bytes(e: &Entry) -> u64 {
        (e.last_lba + 1 - e.first_lba) * 512
    }

    const END: u64 = 2048 * MIB / 512 - 34;

    fn free() -> u64 {
        down((END + 1) * 512) - 266240 * 512
    }

    #[test]
    fn default_on_fresh_image() {
        let t = table(2048 * MIB);
        let p = plan(&Layout::default_convention(), &t).unwrap();
        assert_eq!(p.numbers, [(3, true)]);
        let e = &p.create[0].1;
        assert_eq!(e.first_lba, 266240);
        assert_eq!(bytes(e), free());
        assert_eq!(e.name, "nixos");
        assert_eq!(e.type_guid, gpt::LINUX_FS);
    }

    #[test]
    fn matching_is_by_type_in_table_order() {
        let mut t = table(2048 * MIB);
        let l = layout(vec![
            part("Type=linux-generic\nLabel=a\nFormat=ext4\nSizeMaxBytes=100M"),
            part("Type=linux-generic\nLabel=b\nFormat=ext4"),
        ]);
        let p = plan(&l, &t).unwrap();
        assert_eq!(p.numbers, [(3, true), (4, true)]);
        for (_, e) in p.create {
            t.add(e).unwrap();
        }
        let again = plan(&l, &t).unwrap();
        assert_eq!(again.numbers, [(3, false), (4, false)]);
        assert!(again.create.is_empty());

        let swapped = layout(vec![
            part("Type=linux-generic\nLabel=b"),
            part("Type=esp\nLabel=x"),
        ]);
        assert_eq!(
            plan(&swapped, &t).unwrap().numbers,
            [(3, false), (1, false)]
        );
    }

    #[test]
    fn weights_minimums_and_maximums() {
        let t = table(2048 * MIB);
        let p = plan(
            &layout(vec![
                part("Type=root\nFormat=ext4\nSizeMinBytes=1G"),
                part("Type=swap\nSizeMaxBytes=100M\nWeight=1000"),
                part("Type=var\nFormat=ext4\nWeight=3000"),
            ]),
            &t,
        )
        .unwrap();
        let s: Vec<u64> = p.create.iter().map(|(_, e)| bytes(e)).collect();
        assert_eq!(s[0], 1024 * MIB, "root needs more than its share");
        assert_eq!(s[1], 100 * MIB, "swap takes less than its share");
        assert_eq!(s[2], free() - 1124 * MIB, "var gets the rest");
        assert_eq!(p.create[1].1.first_lba, p.create[0].1.last_lba + 1);
        assert_eq!(p.create[0].1.name, "root");
        assert_eq!(p.create[1].1.name, "swap");
    }

    #[test]
    fn equal_weights_share_equally_and_padding() {
        let t = table(2048 * MIB);
        let p = plan(
            &layout(vec![
                part("Type=linux-generic\nPaddingWeight=1000"),
                part("Type=linux-generic"),
            ]),
            &t,
        )
        .unwrap();
        let (a, b) = (&p.create[0].1, &p.create[1].1);
        assert_eq!(bytes(a), down(free() / 3));
        assert!(b.first_lba > a.last_lba + 1, "padding leaves a gap");
        assert_eq!(a.name, "linux-generic");
        assert_eq!(b.name, "linux-generic-2");
    }

    #[test]
    fn too_big_and_unsupported() {
        let t = table(1024 * MIB);
        let big = plan(&layout(vec![part("Type=root\nSizeMinBytes=4G")]), &t).unwrap_err();
        assert!(big.contains("no free area"), "{big}");
        let e = plan(&layout(vec![part("Type=root\nEncrypt=tpm2")]), &t).unwrap_err();
        assert!(e.contains("Encrypt=tpm2 is not supported yet"), "{e}");
        assert!(plan(&layout(vec![part("Type=esp\nFormat=vfat")]), &t).is_ok());
    }

    #[test]
    fn wipe_keeps_esp_and_bios_boot() {
        let mut t = table(2048 * MIB);
        for (_, e) in plan(&Layout::default_convention(), &t).unwrap().create {
            t.add(e).unwrap();
        }
        let mut l = layout(vec![part("Type=root\nFormat=ext4\nMountPoint=/")]);
        assert!(plan(&l, &t).unwrap_err().contains("no free area"));
        l.wipe = true;
        let p = plan(&l, &t).unwrap();
        assert_eq!(p.wiped, [3]);
        assert_eq!(p.numbers, [(3, true)]);
        assert_eq!(p.create[0].1.first_lba, 266240);
    }
}
