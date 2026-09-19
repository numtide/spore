// Copied from ietsp src/store/nar.rs. restore_path is changed to write the
// canonical store metadata (mtime 1, no write bits) as it goes, so no second
// pass over the tree is needed.

use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

const NAR_MAX_DEPTH: usize = 64;

fn read_u64(source: &mut dyn Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    source.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_padding(source: &mut dyn Read, len: u64) -> io::Result<()> {
    let rem = (len % 8) as usize;
    if rem != 0 {
        let mut pad = [0u8; 8];
        source.read_exact(&mut pad[..8 - rem])?;
        if pad.iter().any(|&b| b != 0) {
            return Err(bad_nar("non-zero padding"));
        }
    }
    Ok(())
}

fn bad_nar(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("bad NAR: {why}"))
}

/// Mirrors serialise.hh:readString with a size cap: length-prefixed, zero-padded to 8.
fn read_str(source: &mut dyn Read, max: u64) -> io::Result<Vec<u8>> {
    let len = read_u64(source)?;
    if len > max {
        return Err(bad_nar("string too long"));
    }
    let mut buf = vec![0u8; len as usize];
    source.read_exact(&mut buf)?;
    read_padding(source, len)?;
    Ok(buf)
}

fn expect_str(source: &mut dyn Read, want: &[u8]) -> io::Result<()> {
    let got = read_str(source, 64)?;
    if got != want {
        return Err(bad_nar(&format!(
            "expected '{}', got '{}'",
            String::from_utf8_lossy(want),
            String::from_utf8_lossy(&got)
        )));
    }
    Ok(())
}

/// Sets mtime to 1 without following symlinks; atime stays.
fn canonical_mtime(path: &Path) -> io::Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: 1,
            tv_nsec: 0,
        },
    ];
    let r = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Mirrors archive.cc:parseDump with RestoreSink: restores the NAR read from `source` at
/// `dest`, which must not exist. Files get mode 0444 or 0555, directories 0555, and
/// everything mtime 1, like local-store.cc:canonicalisePathMetaData.
pub fn restore_path(source: &mut dyn Read, dest: &Path) -> io::Result<()> {
    expect_str(source, b"nix-archive-1")?;
    let mut buf = vec![0u8; 256 * 1024];
    restore(source, dest, 0, &mut buf)
}

fn restore(source: &mut dyn Read, dest: &Path, depth: usize, buf: &mut [u8]) -> io::Result<()> {
    if depth >= NAR_MAX_DEPTH {
        return Err(bad_nar("directory nesting too deep"));
    }
    expect_str(source, b"(")?;
    expect_str(source, b"type")?;
    let ty = read_str(source, 64)?;
    match ty.as_slice() {
        b"regular" => {
            let mut tag = read_str(source, 64)?;
            let mut executable = false;
            if tag == b"executable" {
                expect_str(source, b"")?;
                executable = true;
                tag = read_str(source, 64)?;
            }
            if tag != b"contents" {
                return Err(bad_nar("expected 'contents'"));
            }
            let len = read_u64(source)?;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(if executable { 0o555 } else { 0o444 })
                .open(dest)?;
            let mut left = len;
            while left > 0 {
                let n = buf.len().min(left as usize);
                source
                    .read_exact(&mut buf[..n])
                    .map_err(|_| bad_nar("truncated file contents"))?;
                io::Write::write_all(&mut f, &buf[..n])?;
                left -= n as u64;
            }
            read_padding(source, len)?;
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))?;
        }
        b"directory" => {
            fs::create_dir(dest)?;
            let mut prev: Vec<u8> = Vec::new();
            loop {
                let tag = read_str(source, 64)?;
                if tag == b")" {
                    fs::set_permissions(dest, fs::Permissions::from_mode(0o555))?;
                    return canonical_mtime(dest);
                }
                if tag != b"entry" {
                    return Err(bad_nar("expected 'entry' or ')'"));
                }
                expect_str(source, b"(")?;
                expect_str(source, b"name")?;
                let name = read_str(source, 4096)?;
                if name.is_empty()
                    || name == b"."
                    || name == b".."
                    || name.contains(&b'/')
                    || name.contains(&0)
                {
                    return Err(bad_nar("bad entry name"));
                }
                if name <= prev {
                    return Err(bad_nar("directory entries out of order"));
                }
                prev = name.clone();
                expect_str(source, b"node")?;
                restore(
                    source,
                    &dest.join(std::ffi::OsStr::from_bytes(&name)),
                    depth + 1,
                    buf,
                )?;
                expect_str(source, b")")?;
            }
        }
        b"symlink" => {
            expect_str(source, b"target")?;
            let target = read_str(source, 4096)?;
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), dest)?;
            canonical_mtime(dest)?;
        }
        _ => return Err(bad_nar("unknown node type")),
    }
    expect_str(source, b")")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::hash::Hasher;
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::PathBuf;

    fn write_u64(sink: &mut dyn Write, n: u64) -> io::Result<()> {
        sink.write_all(&n.to_le_bytes())
    }

    fn write_padding(sink: &mut dyn Write, len: u64) -> io::Result<()> {
        let rem = (len % 8) as usize;
        if rem != 0 {
            sink.write_all(&[0u8; 8][..8 - rem])?;
        }
        Ok(())
    }

    fn write_str(sink: &mut dyn Write, s: &[u8]) -> io::Result<()> {
        write_u64(sink, s.len() as u64)?;
        sink.write_all(s)?;
        write_padding(sink, s.len() as u64)
    }

    /// ietsp's archive.cc:dumpPath port, kept to build NARs for the tests.
    pub(crate) fn dump_path(path: &Path, sink: &mut dyn Write) -> io::Result<()> {
        write_str(sink, b"nix-archive-1")?;
        dump(path, sink)
    }

    fn dump(path: &Path, sink: &mut dyn Write) -> io::Result<()> {
        let st = fs::symlink_metadata(path)?;
        let ft = st.file_type();
        write_str(sink, b"(")?;
        write_str(sink, b"type")?;
        if ft.is_file() {
            write_str(sink, b"regular")?;
            if st.permissions().mode() & 0o100 != 0 {
                write_str(sink, b"executable")?;
                write_str(sink, b"")?;
            }
            write_str(sink, b"contents")?;
            write_u64(sink, st.len())?;
            io::copy(&mut fs::File::open(path)?, sink)?;
            write_padding(sink, st.len())?;
        } else if ft.is_dir() {
            write_str(sink, b"directory")?;
            let mut names: Vec<Vec<u8>> = fs::read_dir(path)?
                .map(|e| Ok(e?.file_name().as_bytes().to_vec()))
                .collect::<io::Result<_>>()?;
            names.sort();
            for name in names {
                write_str(sink, b"entry")?;
                write_str(sink, b"(")?;
                write_str(sink, b"name")?;
                write_str(sink, &name)?;
                write_str(sink, b"node")?;
                dump(&path.join(std::ffi::OsStr::from_bytes(&name)), sink)?;
                write_str(sink, b")")?;
            }
        } else {
            write_str(sink, b"symlink")?;
            write_str(sink, b"target")?;
            write_str(sink, fs::read_link(path)?.as_os_str().as_bytes())?;
        }
        write_str(sink, b")")
    }

    /// mkdir -p tree/sub; printf 'hello world\n' > tree/a.txt;
    /// printf '#!/bin/sh\necho hi\n' > tree/run.sh; chmod +x tree/run.sh;
    /// ln -s a.txt tree/link; : > tree/sub/empty; printf zzz > tree/sub/z
    pub(crate) fn fixture_tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spore-nar-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        fs::create_dir_all(tree.join("sub")).unwrap();
        fs::write(tree.join("a.txt"), b"hello world\n").unwrap();
        fs::write(tree.join("run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(tree.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("a.txt", tree.join("link")).unwrap();
        fs::write(tree.join("sub/empty"), b"").unwrap();
        fs::write(tree.join("sub/z"), b"zzz").unwrap();
        tree
    }

    fn nar_of(path: &Path) -> Vec<u8> {
        let mut nar = Vec::new();
        dump_path(path, &mut nar).unwrap();
        nar
    }

    // nix-store --dump tree | sha256sum ; nix-store --dump tree | wc -c
    #[test]
    fn directory_nar() {
        let nar = nar_of(&fixture_tree("dir"));
        let mut h = Hasher::default();
        h.update(&nar);
        assert_eq!(h.bytes_written(), 1272);
        assert_eq!(
            h.finish().to_base16(),
            "9db4d4d873838f1fbb8f363841597d61244beebb19bd10b606ae2a6d4b940ede"
        );
    }

    #[test]
    fn restore_roundtrip_is_canonical() {
        let tree = fixture_tree("restore");
        let nar = nar_of(&tree);
        let out = tree.with_file_name("restored");
        restore_path(&mut nar.as_slice(), &out).unwrap();
        assert_eq!(nar_of(&out), nar);
        assert_eq!(
            fs::read_link(out.join("link")).unwrap().as_os_str(),
            "a.txt"
        );
        let mode = |p: &str| fs::symlink_metadata(out.join(p)).unwrap().mode() & 0o7777;
        assert_eq!(mode("run.sh"), 0o555);
        assert_eq!(mode("a.txt"), 0o444);
        assert_eq!(mode("sub"), 0o555);
        assert_eq!(mode(""), 0o555);
        for p in ["", "a.txt", "run.sh", "link", "sub", "sub/z"] {
            assert_eq!(fs::symlink_metadata(out.join(p)).unwrap().mtime(), 1, "{p}");
        }
        for p in ["sub", ""] {
            fs::set_permissions(out.join(p), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn restore_rejects_garbage_and_truncation() {
        let out = std::env::temp_dir().join(format!("spore-nar-bad-{}", std::process::id()));
        assert!(restore_path(&mut &b"nix-archive-2"[..], &out).is_err());
        let nar = nar_of(&fixture_tree("trunc").join("a.txt"));
        let out2 = out.with_extension("2");
        assert!(restore_path(&mut &nar[..nar.len() - 20], &out2).is_err());
    }
}
