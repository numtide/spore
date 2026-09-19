//! PID 1 of the bootstrap. Starts the target on the ESP while it has boot tries left.
//! Otherwise reads what to run from the cloud user-data, puts it on the disk and the
//! ESP, and kexecs into it.

static LOG: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

macro_rules! log {
    ($($a:tt)*) => {{
        let line = format!("spore: {}", format!($($a)*));
        println!("{line}");
        if let Ok(mut l) = $crate::LOG.lock() {
            l.push_str(&line);
            l.push('\n');
        }
    }};
}

mod db;
mod disk;
mod esp;
mod fetch;
mod gpt;
mod kexec;
mod narinfo;
mod net;
mod repart;
mod store;
mod userdata;

use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use esp::Tries;
use narinfo::PublicKey;
use repart::Layout;
use store::path::StorePath;
use userdata::{Fallback, UserData};

const ROOT: &str = "/mnt";
const JOBS: usize = 32;

fn mount(src: &str, dst: &str, fstype: &str) -> Result<(), String> {
    let _ = fs::create_dir_all(dst);
    let c = |s: &str| CString::new(s).unwrap();
    let r = unsafe {
        libc::mount(
            c(src).as_ptr(),
            c(dst).as_ptr(),
            c(fstype).as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if r != 0 {
        return Err(format!(
            "mount {src} on {dst}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn umount(dst: &str) {
    let c = CString::new(dst).unwrap();
    unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) };
}

fn early_init() {
    for (src, dst, fstype) in [
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
        ("devtmpfs", "/dev", "devtmpfs"),
        ("tmpfs", "/run", "tmpfs"),
        ("tmpfs", "/tmp", "tmpfs"),
    ] {
        let _ = mount(src, dst, fstype);
    }
    let console = CString::new("/dev/console").unwrap();
    let fd = unsafe { libc::open(console.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if fd >= 0 {
        for i in 0..3 {
            unsafe { libc::dup2(fd, i) };
        }
        if fd > 2 {
            unsafe { libc::close(fd) };
        }
    }
    unsafe { std::env::set_var("PATH", "/bin") };
}

/// The CPU counter (TSC, CNTVCT). KVM starts it at 0 with the VM, so its value
/// at /init is the time since the reset, firmware and boot loader included.
fn counter() -> u64 {
    #[cfg(target_arch = "x86_64")]
    return unsafe { core::arch::x86_64::_rdtsc() };
    #[cfg(target_arch = "aarch64")]
    {
        let c: u64;
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) c) };
        c
    }
}

/// `c0` is the counter at the start of /init. The rate comes from the counter and the
/// monotonic clock since then, so the start costs no calibration time.
fn log_reset(t0: Instant, c0: u64) {
    let rate = (counter() - c0) as f64 / t0.elapsed().as_secs_f64();
    log!("started {:.2} s after the reset", c0 as f64 / rate);
}

/// The peak resident set of this process (VmHWM) in MiB.
fn peak_mib() -> f64 {
    let kib: u64 = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0);
    kib as f64 / 1024.0
}

fn emergency() -> ! {
    if Path::new("/bin/sh").exists() {
        log!("dropping to a shell");
        let e = Command::new("/bin/setsid").args(["cttyhack", "sh"]).exec();
        log!("cannot start a shell: {e}");
    } else {
        log!("no shell in this build; the debug disk has one");
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Resolves `path` inside `root` as if `root` were `/`, following symlinks.
fn resolve_in(root: &Path, path: &str) -> Result<PathBuf, String> {
    let logical = resolve_with(root, path, &mut |_| Ok(()))?;
    Ok(root.join(logical.strip_prefix("/").unwrap()))
}

/// Like resolve_in, but returns the path as seen from inside `root`, and calls `visit`
/// on each path before it looks at it.
fn resolve_with(
    root: &Path,
    path: &str,
    visit: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let mut todo: Vec<String> = path.split('/').rev().map(String::from).collect();
    let mut cur = PathBuf::from("/");
    let mut hops = 0;
    while let Some(c) = todo.pop() {
        match c.as_str() {
            "" | "." => continue,
            ".." => {
                cur.pop();
                continue;
            }
            _ => {}
        }
        let next = cur.join(&c);
        visit(&next)?;
        match fs::read_link(root.join(next.strip_prefix("/").unwrap())) {
            Ok(t) => {
                hops += 1;
                if hops > 40 {
                    return Err(format!("{path}: too many symlinks"));
                }
                let t = t.to_string_lossy().into_owned();
                if t.starts_with('/') {
                    cur = PathBuf::from("/");
                }
                todo.extend(t.split('/').rev().map(String::from));
            }
            Err(_) => cur = next,
        }
    }
    Ok(cur)
}

struct Args {
    userdata: Option<String>,
    shell: bool,
    kernel_ip: bool,
}

fn args() -> Args {
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let mut a = Args {
        userdata: None,
        shell: false,
        kernel_ip: false,
    };
    for w in cmdline.split_whitespace() {
        if let Some(u) = w.strip_prefix("spore.userdata=") {
            a.userdata = Some(u.to_string());
        }
        a.shell |= w == "spore.shell";
        a.kernel_ip |= w.starts_with("ip=");
    }
    a
}

fn fetch_userdata(url: &str) -> Result<String, String> {
    let mut last = String::new();
    for _ in 0..10 {
        match ureq::get(url).timeout(Duration::from_secs(5)).call() {
            Ok(r) => {
                let mut s = String::new();
                r.into_reader()
                    .take(1 << 20)
                    .read_to_string(&mut s)
                    .map_err(|e| format!("{url}: {e}"))?;
                return Ok(s);
            }
            Err(e) => last = format!("{url}: {e}"),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(last)
}

fn link(target: &str, at: &Path) -> Result<(), String> {
    let _ = fs::remove_file(at);
    std::os::unix::fs::symlink(target, at).map_err(|e| format!("{}: {e}", at.display()))
}

fn fetcher(ud: &UserData, root: &Path) -> Result<(StorePath, fetch::Fetcher), String> {
    let system = StorePath::parse(&ud.system).map_err(|e| e.0)?;
    let keys = ud
        .trusted_public_keys
        .iter()
        .map(|k| PublicKey::parse(k))
        .collect::<Result<Vec<_>, _>>()?;
    let f = fetch::Fetcher::new(ud.substituters.clone(), keys, root.join("nix/store"));
    Ok((system, f))
}

/// The repart.d definitions of `system`, fetching only the store paths on the way into
/// `ram`. None if the system has none.
fn target_layout(
    f: &fetch::Fetcher,
    system: &StorePath,
    ram: &Path,
) -> Result<Option<Layout>, String> {
    let store = ram.join("nix/store");
    let mut visit = |p: &Path| -> Result<(), String> {
        let mut c = p.components().skip(3);
        if p.starts_with("/nix/store")
            && let Some(base) = c.next()
            && c.next().is_none()
        {
            let base = base.as_os_str().to_string_lossy();
            if !store.join(&*base).exists() {
                let sp = StorePath::from_base_name(&base).map_err(|e| e.0)?;
                f.fetch_one(&sp, &store)?;
            }
        }
        Ok(())
    };
    let dir = resolve_with(ram, &format!("{system}/etc/repart.d"), &mut visit)?;
    let Ok(rd) = fs::read_dir(ram.join(dir.strip_prefix("/").unwrap())) else {
        return Ok(None);
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".conf"))
        .collect();
    names.sort();
    let mut files = Vec::new();
    for n in names {
        let p = resolve_with(ram, &format!("{}/{n}", dir.display()), &mut visit)?;
        let text = fs::read_to_string(ram.join(p.strip_prefix("/").unwrap()))
            .map_err(|e| format!("repart.d/{n}: {e}"))?;
        files.push((n, text));
    }
    if files.is_empty() {
        return Ok(None);
    }
    Layout::from_confs(&files).map(Some)
}

/// Pulls the closure of `system` into `root` and registers it. Returns the number of
/// paths added and their NAR bytes.
fn pull(root: &Path, system: &StorePath, f: &fetch::Fetcher) -> Result<(usize, u64), String> {
    let mut db = db::open(root)?;
    let valid: HashSet<String> = {
        let mut q = db
            .prepare("select path from ValidPaths")
            .map_err(|e| e.to_string())?;
        q.query_map([], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    };
    let (infos, bytes) = f.pull(std::slice::from_ref(system), &valid, JOBS)?;
    db::register(&mut db, &infos)?;
    Ok((infos.len(), bytes))
}

/// `init=` and the target's kernel-params. A panic resets the machine, so the next
/// bootstrap counts the boot as failed.
fn target_cmdline(system: &str, params: &str) -> String {
    let mut w = vec![format!("init={system}/init")];
    w.extend(params.split_whitespace().map(String::from));
    if !w.iter().any(|w| w.starts_with("panic=")) {
        w.push("panic=10".into());
    }
    w.join(" ")
}

fn find_esp() -> Result<PathBuf, String> {
    let (disk, table) = disk::Disk::find_boot()?;
    let n = table
        .entries
        .iter()
        .position(|e| e.as_ref().is_some_and(|e| e.type_guid == gpt::ESP))
        .ok_or("no ESP")?;
    let p = disk.partition(n as u32 + 1);
    for _ in 0..100 {
        if p.exists() {
            return Ok(p);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("{} did not appear", p.display()))
}

/// Starts the target on the ESP if it has tries left, after it takes one. Returns the
/// counter when there is none to start: None for no complete target.
fn boot_target(t0: Instant, c0: u64) -> Result<Option<Tries>, String> {
    let s = || format!("{:.2}", t0.elapsed().as_secs_f64());
    let path = find_esp()?;
    let err = |e: std::io::Error| format!("ESP {}: {e}", path.display());
    let mut esp = esp::Esp::open(&path)?;
    let tries = match esp.tries().map_err(err)? {
        Some(t) if t.left > 0 => t,
        t => return Ok(t),
    };
    let out = Path::new("/run/spore/target");
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let cmdline = esp.read_target(out).map_err(err)?;
    let left = tries.left - 1;
    esp.set_tries(Tries { left, ..tries }).map_err(err)?;
    kexec::load(&out.join("kernel"), &out.join("initrd"), &cmdline)?;
    log_reset(t0, c0);
    log!(
        "kexec into the target on the ESP after {} s: {left} of {} tries left",
        s(),
        tries.total
    );
    Err(kexec::exec())
}

fn run(t0: Instant, c0: u64, a: &Args, net_up: &mut bool, failed: bool) -> Result<(), String> {
    let s = || format!("{:.2}", t0.elapsed().as_secs_f64());
    if !*net_up {
        let iface = if a.kernel_ip {
            let pnp = fs::read_to_string("/proc/net/pnp").unwrap_or_default();
            fs::write("/etc/resolv.conf", pnp).map_err(|e| e.to_string())?;
            "the kernel ip= setting".to_string()
        } else {
            net::up()?
        };
        *net_up = true;
        log!("network up on {iface} after {} s", s());
    }

    let url = match &a.userdata {
        Some(u) => u.clone(),
        None => fs::read_to_string("/etc/spore/userdata-url")
            .map(|u| u.trim().to_string())
            .map_err(|_| "no user-data URL for this platform")?,
    };
    let ud = UserData::parse(&fetch_userdata(&url)?)?;
    if failed && ud.fallback == Fallback::Rescue {
        log!("rescue: the user-data asks for it; the disk stays as it is");
        emergency();
    }
    let root = Path::new(ROOT);
    let (system, f) = fetcher(&ud, root)?;

    let layout = match &ud.layout {
        Some(l) => Layout::from_json(l)?,
        None => target_layout(&f, &system, Path::new("/run/spore"))?
            .unwrap_or_else(Layout::default_convention),
    };
    log!(
        "layout from {} after {} s: {} partitions",
        layout.source,
        s(),
        layout.partitions.len()
    );

    umount(ROOT);
    let prep = disk::prepare(&layout)?;
    let t = Instant::now();
    for (m, dev) in &prep.mounts {
        mount(
            &dev.to_string_lossy(),
            &format!("{ROOT}{}", m.trim_end_matches('/')),
            "ext4",
        )?;
    }
    log!("disk: mounted in {} ms", t.elapsed().as_millis());
    log!(
        "disk ready after {} s: {} new partitions",
        s(),
        prep.created
    );
    for m in &prep.skipped {
        log!("not mounting {m}: no ext4 on it");
    }

    let (n, bytes) = pull(root, &system, &f)?;
    log!(
        "fetched {system} after {} s: {} paths, {} MiB",
        s(),
        n,
        bytes >> 20
    );

    let profiles = root.join("nix/var/nix/profiles");
    fs::create_dir_all(&profiles).map_err(|e| e.to_string())?;
    link(&system.to_string(), &profiles.join("system-1-link"))?;
    link("system-1-link", &profiles.join("system"))?;

    let sys = system.to_string();
    let kernel = resolve_in(root, &format!("{sys}/kernel"))?;
    let initrd = resolve_in(root, &format!("{sys}/initrd"))?;
    let params =
        fs::read_to_string(resolve_in(root, &format!("{sys}/kernel-params"))?).unwrap_or_default();
    let cmdline = target_cmdline(&sys, &params);
    let total = ud.boot_tries;
    esp::Esp::open(&prep.esp)?
        .write_target(
            &esp::Target {
                kernel: &kernel,
                initrd: &initrd,
                cmdline: &cmdline,
            },
            Tries {
                left: total - 1,
                total,
            },
        )
        .map_err(|e| format!("ESP {}: {e}", prep.esp.display()))?;
    log!("boot entry written after {} s: {total} tries", s());

    kexec::load(&kernel, &initrd, &cmdline)?;
    log_reset(t0, c0);
    log!(
        "kexec into {system} after {} s, peak memory {:.1} MiB",
        s(),
        peak_mib()
    );
    // the console is gone after kexec; keep the timings on the target
    let _ = fs::create_dir_all(root.join("var/log"));
    let _ = fs::write(root.join("var/log/spore.log"), &*LOG.lock().unwrap());
    unsafe { libc::sync() };
    for (m, _) in prep.mounts.iter().rev() {
        umount(&format!("{ROOT}{}", m.trim_end_matches('/')));
    }
    Err(kexec::exec())
}

fn main() {
    let t0 = Instant::now();
    let c0 = counter();
    let argv: Vec<String> = std::env::args().collect();
    if let [_, cmd, root, file] = argv.as_slice()
        && cmd == "pull"
    {
        let r = fs::read_to_string(file)
            .map_err(|e| format!("{file}: {e}"))
            .and_then(|s| UserData::parse(&s))
            .and_then(|ud| {
                let (system, f) = fetcher(&ud, Path::new(root))?;
                pull(Path::new(root), &system, &f).map(|(n, b)| (system, n, b))
            });
        match r {
            Ok((system, n, bytes)) => log!(
                "fetched {system} after {:.2} s: {n} paths, {} MiB",
                t0.elapsed().as_secs_f64(),
                bytes >> 20
            ),
            Err(e) => {
                log!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::process::id() == 1 {
        early_init();
    }
    if let Some(up) = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|u| u.split_whitespace().next().map(String::from))
    {
        log!("started {up} s after the kernel");
    }
    let a = args();
    if a.shell {
        log!("spore.shell on the kernel command line");
        emergency();
    }
    let failed = match boot_target(t0, c0) {
        Ok(None) => false,
        Ok(Some(t)) => {
            log!("the target used all {} tries without a good boot", t.total);
            true
        }
        Err(e) => {
            log!("cannot start the target on the ESP: {e}");
            false
        }
    };
    let mut net_up = false;
    loop {
        if let Err(e) = run(t0, c0, &a, &mut net_up, failed) {
            log!("error: {e}");
            if Path::new("/bin/sh").exists() {
                emergency();
            }
            log!("retrying in 10 s");
            std::thread::sleep(Duration::from_secs(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_resets_on_panic() {
        assert_eq!(
            target_cmdline("/nix/store/s", " quiet\n"),
            "init=/nix/store/s/init quiet panic=10"
        );
        assert_eq!(
            target_cmdline("/nix/store/s", "panic=-1"),
            "init=/nix/store/s/init panic=-1"
        );
    }

    #[test]
    fn resolve_follows_links_inside_root() {
        let root = std::env::temp_dir().join(format!("spore-resolve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nix/store/k/sub")).unwrap();
        fs::create_dir_all(root.join("nix/store/sys")).unwrap();
        fs::write(root.join("nix/store/k/sub/bzImage"), b"k").unwrap();
        std::os::unix::fs::symlink("sub/bzImage", root.join("nix/store/k/bzImage")).unwrap();
        std::os::unix::fs::symlink("/nix/store/k/bzImage", root.join("nix/store/sys/kernel"))
            .unwrap();
        std::os::unix::fs::symlink("../k/../sys/kernel", root.join("nix/store/sys/k2")).unwrap();
        let want = root.join("nix/store/k/sub/bzImage");
        assert_eq!(resolve_in(&root, "/nix/store/sys/kernel").unwrap(), want);
        assert_eq!(resolve_in(&root, "/nix/store/sys/k2").unwrap(), want);
        std::os::unix::fs::symlink("loop", root.join("loop")).unwrap();
        assert!(resolve_in(&root, "/loop").is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
