use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::narinfo::{NarInfo, PublicKey};
use crate::store::hash::Hasher;
use crate::store::nar::restore_path;
use crate::store::path::StorePath;

pub struct Fetcher {
    agent: ureq::Agent,
    substituters: Vec<String>,
    keys: Vec<PublicKey>,
    store: PathBuf,
}

#[derive(Default)]
struct State {
    infos: VecDeque<StorePath>,
    nars: VecDeque<(String, NarInfo)>,
    seen: HashSet<StorePath>,
    pending: usize,
    done: Vec<NarInfo>,
    bytes: u64,
    error: Option<String>,
}

struct HashReader<R> {
    inner: R,
    hasher: Hasher,
}

impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

impl Fetcher {
    /// `store` is the directory that holds the restored paths, e.g. /mnt/nix/store.
    pub fn new(substituters: Vec<String>, keys: Vec<PublicKey>, store: PathBuf) -> Fetcher {
        let agent = ureq::AgentBuilder::new()
            .max_idle_connections(256)
            .max_idle_connections_per_host(64)
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(60))
            .user_agent("spore")
            .build();
        Fetcher {
            agent,
            substituters: substituters
                .into_iter()
                .map(|s| s.trim_end_matches('/').to_string())
                .collect(),
            keys,
            store,
        }
    }

    pub fn get(&self, url: &str) -> Result<Box<dyn Read + Send + Sync>, ureq::Error> {
        Ok(self.agent.get(url).call()?.into_reader())
    }

    /// Fetches the signed narinfo of `path` from the first substituter that has it.
    fn narinfo(&self, path: &StorePath) -> Result<(String, NarInfo), String> {
        let mut errors = Vec::new();
        for sub in &self.substituters {
            let url = format!("{sub}/{}.narinfo", path.hash_part());
            let text = match self.agent.get(&url).call() {
                Ok(r) => r.into_string().map_err(|e| format!("{url}: {e}"))?,
                Err(ureq::Error::Status(404, _)) => continue,
                Err(e) => {
                    errors.push(format!("{url}: {e}"));
                    continue;
                }
            };
            let ni = NarInfo::parse(&text).map_err(|e| format!("{url}: {e}"))?;
            if ni.path != *path {
                return Err(format!("{url} describes {} instead", ni.path));
            }
            if !ni.verify(&self.keys) {
                return Err(format!("{url}: no signature by a trusted key"));
            }
            return Ok((sub.clone(), ni));
        }
        if errors.is_empty() {
            Err(format!("no substituter has {path}"))
        } else {
            Err(errors.join("; "))
        }
    }

    /// Downloads, decompresses and restores the NAR of `ni`, checking NarHash and NarSize
    /// on the stream.
    fn nar(&self, sub: &str, ni: &NarInfo, store: &Path) -> Result<(), String> {
        let url = format!("{sub}/{}", ni.url);
        let body = self.get(&url).map_err(|e| format!("{url}: {e}"))?;
        let decoded: Box<dyn Read> = match ni.compression.as_str() {
            "none" | "" => Box::new(body),
            "xz" => Box::new(xz2::read::XzDecoder::new(body)),
            "zstd" => Box::new(zstd::stream::read::Decoder::new(body).map_err(|e| e.to_string())?),
            c => return Err(format!("{url}: compression '{c}' is not supported")),
        };
        let mut src = HashReader {
            inner: decoded,
            hasher: Hasher::default(),
        };
        let dest = store.join(ni.path.base_name());
        let tmp = store.join(format!(".tmp-{}", ni.path.hash_part()));
        remove(&tmp);
        remove(&dest);
        restore_path(&mut src, &tmp)
            .and_then(|_| io::copy(&mut src, &mut io::sink()).map(|_| ()))
            .map_err(|e| {
                remove(&tmp);
                format!("{url}: {e}")
            })?;
        let size = src.hasher.bytes_written();
        let hash = src.hasher.finish();
        if size != ni.nar_size || hash != ni.nar_hash {
            remove(&tmp);
            return Err(format!(
                "{}: NAR is {size} bytes with hash {hash:?}, narinfo says {} bytes with {:?}",
                ni.path, ni.nar_size, ni.nar_hash
            ));
        }
        fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))
    }

    /// Fetches the single path `path` (not its closure) into the directory `store`.
    pub fn fetch_one(&self, path: &StorePath, store: &Path) -> Result<(), String> {
        fs::create_dir_all(store).map_err(|e| format!("{}: {e}", store.display()))?;
        let (sub, ni) = self.narinfo(path)?;
        retry(3, || self.nar(&sub, &ni, store))
    }

    /// Pulls the closure of `roots` into the store, skipping `valid` paths (their closures
    /// are complete). Returns the narinfo of every path it added.
    pub fn pull(
        &self,
        roots: &[StorePath],
        valid: &HashSet<String>,
        jobs: usize,
    ) -> Result<(Vec<NarInfo>, u64), String> {
        fs::create_dir_all(&self.store).map_err(|e| format!("{}: {e}", self.store.display()))?;
        let mut st = State::default();
        for r in roots {
            if st.seen.insert(r.clone()) && !valid.contains(&r.to_string()) {
                st.infos.push_back(r.clone());
                st.pending += 1;
            }
        }
        let state = Mutex::new(st);
        let cv = Condvar::new();
        std::thread::scope(|s| {
            for _ in 0..jobs.max(1) {
                s.spawn(|| self.worker(&state, &cv, valid));
            }
        });
        let st = state.into_inner().unwrap();
        match st.error {
            Some(e) => Err(e),
            None => Ok((st.done, st.bytes)),
        }
    }

    fn worker(&self, state: &Mutex<State>, cv: &Condvar, valid: &HashSet<String>) {
        enum Task {
            Info(StorePath),
            Nar(String, NarInfo),
        }
        loop {
            let task = {
                let mut st = state.lock().unwrap();
                loop {
                    if st.error.is_some() || st.pending == 0 {
                        cv.notify_all();
                        return;
                    }
                    if let Some(p) = st.infos.pop_front() {
                        break Task::Info(p);
                    }
                    if let Some((sub, ni)) = st.nars.pop_front() {
                        break Task::Nar(sub, ni);
                    }
                    st = cv.wait(st).unwrap();
                }
            };
            let result = match task {
                Task::Info(p) => self.narinfo(&p).map(|(sub, ni)| {
                    let mut st = state.lock().unwrap();
                    for r in &ni.references {
                        if st.seen.insert(r.clone()) && !valid.contains(&r.to_string()) {
                            st.infos.push_back(r.clone());
                            st.pending += 1;
                        }
                    }
                    st.nars.push_back((sub, ni));
                }),
                Task::Nar(sub, ni) => {
                    let r = retry(3, || self.nar(&sub, &ni, &self.store));
                    r.map(|_| {
                        let mut st = state.lock().unwrap();
                        st.pending -= 1;
                        st.bytes += ni.nar_size;
                        st.done.push(ni);
                    })
                }
            };
            if let Err(e) = result {
                state.lock().unwrap().error.get_or_insert(e);
            }
            cv.notify_all();
        }
    }
}

fn retry(n: usize, mut f: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    let mut last = Ok(());
    for i in 0..n {
        last = f();
        if last.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200 << i));
    }
    last
}

/// Store paths are read-only; make them writable so they can go.
fn remove(p: &Path) {
    let Ok(md) = fs::symlink_metadata(p) else {
        return;
    };
    if md.is_dir() {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                remove(&e.path());
            }
        }
        let _ = fs::remove_dir(p);
    } else {
        let _ = fs::remove_file(p);
    }
}
