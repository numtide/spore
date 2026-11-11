use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

// the libc crate lacks it for aarch64 musl
#[cfg(target_arch = "x86_64")]
const SYS_KEXEC_FILE_LOAD: libc::c_long = libc::SYS_kexec_file_load;
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_FILE_LOAD: libc::c_long = 294;

const KEXEC_FILE_NO_INITRAMFS: libc::c_ulong = 0x4;

/// Loads the next kernel with kexec_file_load(2); needs CONFIG_KEXEC_FILE.
pub fn load(kernel: &Path, initrd: Option<&Path>, cmdline: &str) -> Result<(), String> {
    let k = File::open(kernel).map_err(|e| format!("{}: {e}", kernel.display()))?;
    let i = initrd
        .map(|p| File::open(p).map_err(|e| format!("{}: {e}", p.display())))
        .transpose()?;
    let (ifd, flags) = match &i {
        Some(f) => (f.as_raw_fd(), 0),
        None => (-1, KEXEC_FILE_NO_INITRAMFS),
    };
    let c = CString::new(cmdline).map_err(|e| e.to_string())?;
    // the length counts the NUL: the kernel rejects a cmdline that does not end in one
    let len = c.as_bytes_with_nul().len();
    let r = unsafe {
        libc::syscall(
            SYS_KEXEC_FILE_LOAD,
            k.as_raw_fd(),
            ifd,
            len,
            c.as_ptr(),
            flags,
        )
    };
    if r != 0 {
        return Err(format!("kexec_file_load: {}", io::Error::last_os_error()));
    }
    Ok(())
}

/// Jumps into the loaded kernel. Returns only on failure.
pub fn exec() -> String {
    unsafe { libc::sync() };
    unsafe { libc::reboot(libc::LINUX_REBOOT_CMD_KEXEC) };
    format!("reboot(KEXEC): {}", io::Error::last_os_error())
}
