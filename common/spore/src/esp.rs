//! The target on the ESP: `target/kernel`, `target/initrd`, `target/cmdline`, and the
//! boot counter `target/tries`. Limine only starts the bootstrap; the bootstrap reads
//! these and kexecs the target. The kernel has no VFAT driver, so this edits the FAT
//! file system on the raw partition.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

pub struct Target<'a> {
    pub kernel: &'a Path,
    pub initrd: &'a Path,
    pub cmdline: &'a str,
}

/// `target/tries` holds "LEFT TOTAL". The bootstrap takes one try before each start of
/// the target. The boot-good script of the target (targets/boot-good.sh) writes
/// "TOTAL TOTAL".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tries {
    pub left: u8,
    pub total: u8,
}

impl Tries {
    pub fn parse(s: &str) -> Option<Tries> {
        let w: Vec<u8> = s
            .split_whitespace()
            .map(|w| w.parse().ok())
            .collect::<Option<_>>()?;
        match w[..] {
            [left, total] if left <= total && total > 0 => Some(Tries { left, total }),
            _ => None,
        }
    }
}

type Dir<'a, 'c, D> = fatfs::Dir<'a, &'c mut ChunkCache<D>>;

fn copy_in<D: Read + Write + Seek>(dir: &Dir<'_, '_, D>, name: &str, src: &Path) -> io::Result<()> {
    let mut f = dir.create_file(name)?;
    f.truncate()?;
    io::copy(&mut File::open(src)?, &mut f)?;
    f.flush()
}

/// Rewrites a small file in place: the same length touches one data sector.
fn write_small<D: Read + Write + Seek>(
    dir: &Dir<'_, '_, D>,
    name: &str,
    data: &[u8],
) -> io::Result<()> {
    let mut f = dir.create_file(name)?;
    f.write_all(data)?;
    f.truncate()?;
    f.flush()
}

fn read_small<D: Read + Write + Seek>(dir: &Dir<'_, '_, D>, name: &str) -> io::Result<String> {
    let mut s = String::new();
    dir.open_file(name)?.take(4096).read_to_string(&mut s)?;
    Ok(s)
}

pub struct Esp<D> {
    c: ChunkCache<D>,
    sync: fn(&D) -> io::Result<()>,
}

impl Esp<File> {
    pub fn open(path: &Path) -> Result<Self, String> {
        let dev = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Esp::new(dev, File::sync_all).map_err(|e| format!("ESP {}: {e}", path.display()))
    }
}

impl<D: Read + Write + Seek> Esp<D> {
    pub fn new(dev: D, sync: fn(&D) -> io::Result<()>) -> io::Result<Self> {
        Ok(Esp {
            c: ChunkCache::new(dev)?,
            sync,
        })
    }

    fn with<R>(&mut self, f: impl FnOnce(&Dir<'_, '_, D>) -> io::Result<R>) -> io::Result<R> {
        self.c.seek(io::SeekFrom::Start(0))?;
        let fs = fatfs::FileSystem::new(&mut self.c, fatfs::FsOptions::new())?;
        let r = f(&fs.root_dir())?;
        fs.unmount()?;
        Ok(r)
    }

    fn commit(&mut self) -> io::Result<()> {
        self.c.flush()?;
        (self.sync)(&self.c.dev)
    }

    /// None if there is no complete target.
    pub fn tries(&mut self) -> io::Result<Option<Tries>> {
        self.with(|root| {
            match root
                .open_dir("target")
                .and_then(|d| read_small(&d, "tries"))
            {
                Ok(s) => Ok(Tries::parse(&s)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    pub fn set_tries(&mut self, t: Tries) -> io::Result<()> {
        let line = format!("{} {}\n", t.left, t.total);
        self.with(|root| write_small(&root.open_dir("target")?, "tries", line.as_bytes()))?;
        self.commit()
    }

    /// Writes the target files, then `tries`. The old counter goes first, so a target
    /// with files from two systems never starts.
    pub fn write_target(&mut self, t: &Target<'_>, tries: Tries) -> io::Result<()> {
        self.with(|root| match root.create_dir("target")?.remove("tries") {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        })?;
        self.commit()?;
        self.with(|root| {
            let dir = root.open_dir("target")?;
            copy_in(&dir, "kernel", t.kernel)?;
            copy_in(&dir, "initrd", t.initrd)?;
            write_small(&dir, "cmdline", t.cmdline.as_bytes())
        })?;
        self.commit()?;
        self.set_tries(tries)
    }

    /// Copies the target kernel and initrd to `out/kernel` and `out/initrd`; returns the
    /// command line.
    pub fn read_target(&mut self, out: &Path) -> io::Result<String> {
        self.with(|root| {
            let dir = root.open_dir("target")?;
            for n in ["kernel", "initrd"] {
                io::copy(&mut dir.open_file(n)?, &mut File::create(out.join(n))?)?;
            }
            Ok(read_small(&dir, "cmdline")?.trim().to_string())
        })
    }
}

const CHUNK: u64 = 1024 * 1024;

/// fatfs does sector-sized I/O. On a network disk each write into an uncached page
/// costs a synchronous read (3.8 s for a NixOS kernel and initrd on hcloud cx23), so
/// whole chunks are read once and written back once.
pub struct ChunkCache<D> {
    dev: D,
    len: u64,
    pos: u64,
    chunks: std::collections::BTreeMap<u64, (Vec<u8>, bool)>,
}

impl<D: Read + Write + Seek> ChunkCache<D> {
    pub fn new(mut dev: D) -> io::Result<Self> {
        let len = dev.seek(io::SeekFrom::End(0))?;
        Ok(ChunkCache {
            dev,
            len,
            pos: 0,
            chunks: Default::default(),
        })
    }

    fn chunk(&mut self, i: u64) -> io::Result<&mut (Vec<u8>, bool)> {
        if !self.chunks.contains_key(&i) {
            let n = CHUNK.min(self.len - i * CHUNK) as usize;
            let mut b = vec![0u8; n];
            self.dev.seek(io::SeekFrom::Start(i * CHUNK))?;
            self.dev.read_exact(&mut b)?;
            self.chunks.insert(i, (b, false));
        }
        Ok(self.chunks.get_mut(&i).unwrap())
    }

    fn io(
        &mut self,
        len: usize,
        mut f: impl FnMut(&mut (Vec<u8>, bool), usize, usize, usize),
    ) -> io::Result<usize> {
        let len = len.min(self.len.saturating_sub(self.pos) as usize);
        let mut done = 0;
        while done < len {
            let (i, off) = (self.pos / CHUNK, (self.pos % CHUNK) as usize);
            let c = self.chunk(i)?;
            let n = (c.0.len() - off).min(len - done);
            f(c, off, done, n);
            done += n;
            self.pos += n as u64;
        }
        Ok(done)
    }
}

impl<D: Read + Write + Seek> Read for ChunkCache<D> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.io(buf.len(), |c, off, done, n| {
            buf[done..done + n].copy_from_slice(&c.0[off..off + n])
        })
    }
}

impl<D: Read + Write + Seek> Write for ChunkCache<D> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.pos >= self.len && !buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write past the end of the ESP",
            ));
        }
        self.io(buf.len(), |c, off, done, n| {
            c.0[off..off + n].copy_from_slice(&buf[done..done + n]);
            c.1 = true;
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        for (i, (b, dirty)) in self.chunks.iter_mut() {
            if *dirty {
                self.dev.seek(io::SeekFrom::Start(i * CHUNK))?;
                self.dev.write_all(b)?;
                *dirty = false;
            }
        }
        self.dev.flush()
    }
}

impl<D> Seek for ChunkCache<D> {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        let p = match to {
            io::SeekFrom::Start(p) => p as i128,
            io::SeekFrom::End(d) => self.len as i128 + d as i128,
            io::SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if p < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before 0"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn image() -> Cursor<Vec<u8>> {
        let mut img = Cursor::new(vec![0u8; 64 * 1024 * 1024]);
        fatfs::format_volume(
            &mut img,
            fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32),
        )
        .unwrap();
        img
    }

    #[test]
    fn parse_tries() {
        assert_eq!(Tries::parse("2 3\n"), Some(Tries { left: 2, total: 3 }));
        assert_eq!(Tries::parse("0 1"), Some(Tries { left: 0, total: 1 }));
        for bad in ["", "3", "4 3", "0 0", "1 2 3", "a 3", "-1 3"] {
            assert_eq!(Tries::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn target_round_trip() {
        let dir = std::env::temp_dir().join(format!("spore-esp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let kernel: Vec<u8> = (0..3_000_000u32).map(|i| i as u8).collect();
        std::fs::write(dir.join("k"), &kernel).unwrap();
        std::fs::write(dir.join("i"), b"initrd").unwrap();
        let mut esp = Esp::new(image(), |_| Ok(())).unwrap();
        assert_eq!(esp.tries().unwrap(), None);
        for cmdline in ["init=/nix/store/x/init panic=10", "init=/nix/store/y/init"] {
            let t = Target {
                kernel: &dir.join("k"),
                initrd: &dir.join("i"),
                cmdline,
            };
            esp.write_target(&t, Tries { left: 2, total: 3 }).unwrap();
            assert_eq!(esp.tries().unwrap(), Some(Tries { left: 2, total: 3 }));
            let out = dir.join("out");
            std::fs::create_dir_all(&out).unwrap();
            assert_eq!(esp.read_target(&out).unwrap(), cmdline);
            assert!(std::fs::read(out.join("kernel")).unwrap() == kernel);
            assert_eq!(std::fs::read(out.join("initrd")).unwrap(), b"initrd");
        }
        esp.set_tries(Tries { left: 0, total: 3 }).unwrap();
        let mut again = Esp::new(esp.c.dev, |_| Ok(())).unwrap();
        assert_eq!(again.tries().unwrap(), Some(Tries { left: 0, total: 3 }));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
