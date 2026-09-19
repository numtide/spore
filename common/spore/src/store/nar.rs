// Copied from ietsp src/store/nar.rs. restore_path is changed to write the
// canonical store metadata (mtime 1, no write bits) as it goes, so no second
// pass over the tree is needed.
// ietsp is LGPL-2.1-or-later; its author relicenses this copy under MIT (see LICENSE).

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

const NAR_MAX_DEPTH: usize = 64;

/// The reader window. The fetch runs one restore per download thread, so this
/// is per thread; a token must fit, and a file body goes through it in chunks.
const WINDOW: usize = 256 * 1024;

/// The encoding of a run of tokens, so a run is one compare. `N` is checked
/// against the encoded length at compile time.
const fn framed<const N: usize>(toks: &[&[u8]]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut o = 0;
    let mut t = 0;
    while t < toks.len() {
        let s = toks[t];
        let mut i = 0;
        while i < 8 {
            out[o + i] = (s.len() >> (8 * i)) as u8;
            i += 1;
        }
        o += 8;
        let mut j = 0;
        while j < s.len() {
            out[o + j] = s[j];
            j += 1;
        }
        o += s.len().next_multiple_of(8);
        t += 1;
    }
    assert!(o == N, "framed: wrong N");
    out
}

// The groups of nix-community/go-nix PR #123 (pkg/narv2). Byte 16 of the 32
// bytes at a node's type token is the length prefix of the token after the
// type: `target` (6) after `symlink`, `contents` (8) after `regular`,
// `executable` (10) after an executable `regular`. For `directory` it is the
// last letter, `y`.
const TOK_NAR: [u8; 56] = framed(&[b"nix-archive-1", b"(", b"type"]);
const TOK_REG: [u8; 32] = framed(&[b"regular", b"contents"]);
const TOK_EXE: [u8; 64] = framed(&[b"regular", b"executable", b"", b"contents"]);
const TOK_SYM: [u8; 32] = framed(&[b"symlink", b"target"]);
const TOK_DIR: [u8; 24] = framed(&[b"directory"]);
const TOK_ENT: [u8; 48] = framed(&[b"entry", b"(", b"name"]);
const TOK_NOD: [u8; 48] = framed(&[b"node", b"(", b"type"]);
const TOK_PAR: [u8; 16] = framed(&[b")"]);

const TAG_SYM: u8 = 6;
const TAG_REG: u8 = 8;
const TAG_EXE: u8 = 10;
const TAG_DIR: u8 = b'y';

/// `NAME_MAX`: a name the reader accepts is a name the file system can hold.
const MAX_NAME: usize = 255;
/// `PATH_MAX` less its NUL.
const MAX_TARGET: usize = 4095;

fn bad_nar(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("bad NAR: {why}"))
}

/// A NAR stream read through one refillable window: token runs matched against
/// the pre-framed constants, the node shape from one peeked byte, contents
/// streamed by their declared length, nothing allocated per token.
struct Reader<'a> {
    src: &'a mut dyn Read,
    buf: Box<[u8]>,
    start: usize,
    end: usize,
    eof: bool,
}

impl<'a> Reader<'a> {
    fn new(src: &'a mut dyn Read) -> Reader<'a> {
        Reader {
            src,
            buf: vec![0u8; WINDOW].into_boxed_slice(),
            start: 0,
            end: 0,
            eof: false,
        }
    }

    fn refill(&mut self) -> io::Result<()> {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        if self.end == self.buf.len() {
            return Ok(());
        }
        let got = self.src.read(&mut self.buf[self.end..])?;
        self.eof = got == 0;
        self.end += got;
        Ok(())
    }

    /// Makes the window hold at least `n` bytes.
    fn want(&mut self, n: usize) -> io::Result<()> {
        while self.end - self.start < n {
            self.refill()?;
            if self.eof && self.end - self.start < n {
                return Err(bad_nar("stream ended early"));
            }
        }
        Ok(())
    }

    fn advance(&mut self, n: usize) {
        self.start += n;
    }

    fn peek(&mut self, n: usize) -> io::Result<&[u8]> {
        self.want(n)?;
        Ok(&self.buf[self.start..self.start + n])
    }

    /// Fills the window to `n` bytes if the source has them; returns how many it holds.
    fn avail(&mut self, n: usize) -> io::Result<usize> {
        while self.end - self.start < n {
            self.refill()?;
            if self.eof {
                break;
            }
        }
        Ok((self.end - self.start).min(n))
    }

    fn consume<const N: usize>(&mut self, run: &[u8; N], what: &str) -> io::Result<()> {
        if self.avail(N)? < N || self.buf[self.start..self.start + N] != *run {
            return Err(bad_nar(&format!("expected {what}")));
        }
        self.advance(N);
        Ok(())
    }

    fn u64(&mut self) -> io::Result<u64> {
        let n = u64::from_le_bytes(self.peek(8)?.try_into().unwrap());
        self.advance(8);
        Ok(n)
    }

    /// Up to `n` bytes, however many the window holds.
    fn some(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.start == self.end {
            self.refill()?;
            if self.eof {
                return Err(bad_nar("stream ended early"));
            }
        }
        let take = n.min(self.end - self.start);
        self.advance(take);
        Ok(&self.buf[self.start - take..self.start])
    }

    fn padding(&mut self, len: u64) -> io::Result<()> {
        let rem = (len % 8) as usize;
        if rem != 0 {
            if self.peek(8 - rem)?.iter().any(|&b| b != 0) {
                return Err(bad_nar("non-zero padding"));
            }
            self.advance(8 - rem);
        }
        Ok(())
    }

    /// Mirrors serialise.hh:readString with a size cap, into `out`.
    fn token(&mut self, max: usize, out: &mut Vec<u8>) -> io::Result<()> {
        let len = self.u64()?;
        if len > max as u64 {
            return Err(bad_nar("string too long"));
        }
        let len = len as usize;
        let need = len.next_multiple_of(8);
        let t = self.peek(need)?;
        if t[len..].iter().any(|&b| b != 0) {
            return Err(bad_nar("non-zero padding"));
        }
        out.clear();
        out.extend_from_slice(&t[..len]);
        self.advance(need);
        Ok(())
    }

    fn node_tag(&mut self) -> io::Result<u8> {
        match self.peek(32)?[16] {
            tag @ (TAG_SYM | TAG_REG | TAG_EXE | TAG_DIR) => Ok(tag),
            _ => Err(bad_nar("unknown node type")),
        }
    }

    fn contents_to(&mut self, out: &mut fs::File, len: u64) -> io::Result<()> {
        let mut left = len;
        while left > 0 {
            let chunk = self
                .some(left.min(WINDOW as u64) as usize)
                .map_err(|_| bad_nar("truncated file contents"))?;
            out.write_all(chunk)?;
            left -= chunk.len() as u64;
        }
        self.padding(len)
    }
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
    let mut r = Restorer {
        r: Reader::new(source),
        path: dest.as_os_str().as_bytes().to_vec(),
        name: Vec::with_capacity(MAX_NAME),
        prev: Vec::new(),
    };
    r.r.consume(&TOK_NAR, "a NAR header")?;
    r.node(0)
}

/// One path buffer, one name buffer, and the previous entry name per depth.
struct Restorer<'a> {
    r: Reader<'a>,
    path: Vec<u8>,
    name: Vec<u8>,
    prev: Vec<Vec<u8>>,
}

impl Restorer<'_> {
    fn cur(&self) -> &Path {
        Path::new(OsStr::from_bytes(&self.path))
    }

    fn node(&mut self, depth: usize) -> io::Result<()> {
        if depth >= NAR_MAX_DEPTH {
            return Err(bad_nar("directory nesting too deep"));
        }
        match self.r.node_tag()? {
            TAG_REG => {
                self.r.consume(&TOK_REG, "'regular' 'contents'")?;
                self.regular(false)?;
            }
            TAG_EXE => {
                self.r
                    .consume(&TOK_EXE, "'regular' 'executable' '' 'contents'")?;
                self.regular(true)?;
            }
            TAG_SYM => {
                self.r.consume(&TOK_SYM, "'symlink' 'target'")?;
                self.r.token(MAX_TARGET, &mut self.name)?;
                std::os::unix::fs::symlink(OsStr::from_bytes(&self.name), self.cur())?;
                canonical_mtime(self.cur())?;
            }
            // the `)` after the entries is the directory node's own
            _ => {
                self.r.consume(&TOK_DIR, "'directory'")?;
                return self.directory(depth);
            }
        }
        self.r.consume(&TOK_PAR, "')'")
    }

    fn regular(&mut self, executable: bool) -> io::Result<()> {
        let len = self.r.u64()?;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if executable { 0o555 } else { 0o444 })
            .open(self.cur())?;
        self.r.contents_to(&mut f, len)?;
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
    }

    fn directory(&mut self, depth: usize) -> io::Result<()> {
        fs::create_dir(self.cur())?;
        if self.prev.len() <= depth {
            self.prev.resize(depth + 1, Vec::new());
        }
        self.prev[depth].clear();
        let base = self.path.len();
        loop {
            // the length prefix: 1 for ")", 5 for "entry"
            match self.r.peek(16)?[0] {
                1 => {
                    self.r.consume(&TOK_PAR, "')'")?;
                    fs::set_permissions(self.cur(), fs::Permissions::from_mode(0o555))?;
                    return canonical_mtime(self.cur());
                }
                5 => {}
                _ => return Err(bad_nar("expected 'entry' or ')'")),
            }
            self.r.consume(&TOK_ENT, "'entry' '(' 'name'")?;
            self.r.token(MAX_NAME, &mut self.name)?;
            let name = &self.name;
            if name.is_empty()
                || name.as_slice() == b"."
                || name.as_slice() == b".."
                || name.contains(&b'/')
                || name.contains(&0)
            {
                return Err(bad_nar("bad entry name"));
            }
            if *name <= self.prev[depth] {
                return Err(bad_nar("directory entries out of order"));
            }
            self.prev[depth].clear();
            self.prev[depth].extend_from_slice(name);
            self.r.consume(&TOK_NOD, "'node' '(' 'type'")?;
            self.path.push(b'/');
            self.path.extend_from_slice(&self.prev[depth]);
            let res = self.node(depth + 1);
            self.path.truncate(base);
            res?;
            self.r.consume(&TOK_PAR, "')'")?;
        }
    }
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

    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(3).min(self.0.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn restore_short_reads_and_a_file_larger_than_the_window() {
        let tree = fixture_tree("window");
        let big: Vec<u8> = (0..2 * WINDOW + 5).map(|i| (i % 251) as u8).collect();
        fs::write(tree.join("big"), &big).unwrap();
        let nar = nar_of(&tree);
        let out = tree.with_file_name("restored");
        restore_path(&mut Trickle(&nar), &out).unwrap();
        assert_eq!(nar_of(&out), nar);
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
