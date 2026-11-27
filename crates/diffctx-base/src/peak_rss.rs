#![allow(unsafe_code)]

#[cfg(unix)]
fn getrusage_max_rss_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return 0;
    }
    let max_rss = unsafe { usage.assume_init() }.ru_maxrss.max(0) as u64;
    // ru_maxrss unit differs by platform: bytes on macOS, kilobytes on Linux.
    if cfg!(target_os = "macos") {
        max_rss
    } else {
        max_rss * 1024
    }
}

#[cfg(target_os = "macos")]
pub fn peak_rss_bytes() -> u64 {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    if rc == 0 {
        let footprint = unsafe { info.assume_init() }.ri_lifetime_max_phys_footprint;
        if footprint > 0 {
            return footprint;
        }
    }
    getrusage_max_rss_bytes()
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn peak_rss_bytes() -> u64 {
    getrusage_max_rss_bytes()
}

#[cfg(not(unix))]
pub fn peak_rss_bytes() -> u64 {
    0
}
