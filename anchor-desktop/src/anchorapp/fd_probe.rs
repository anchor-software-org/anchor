//! Periodic file-descriptor accounting for diagnosing "too many open files"
//! (EMFILE) after long uptime.
//!
//! Every [`PROBE_INTERVAL`] we scan `/proc/self/fd/`, count entries, and
//! bucket them by kind so a leak shows up as a specific category growing
//! monotonically rather than a single unhelpful total. Also logs the
//! current soft `RLIMIT_NOFILE` on the first probe so we know the ceiling.
//!
//! The scan is a single `readdir` plus one `readlink` per entry — cheap
//! enough at 60s cadence that it's fine to leave on in release builds.

use std::fs;
use std::io;
use std::thread;
use std::time::Duration;

const PROBE_INTERVAL: Duration = Duration::from_secs(60);

/// Spawns the probe thread. Idempotent from the caller's perspective — call
/// once at startup after the logger is initialized.
pub fn start() {
    thread::Builder::new()
        .name("fd-probe".into())
        .spawn(move || {
            log_soft_limit();
            loop {
                match snapshot() {
                    Ok(s) => log::debug!(
                        "[fd-probe] total={} sockets={} pipes={} anon_inode={} files={} other={}",
                        s.total,
                        s.sockets,
                        s.pipes,
                        s.anon_inode,
                        s.files,
                        s.other,
                    ),
                    Err(e) => log::warn!("[fd-probe] scan failed: {e}"),
                }
                thread::sleep(PROBE_INTERVAL);
            }
        })
        .expect("spawn fd-probe thread");
}

#[derive(Default)]
struct FdCounts {
    total: usize,
    sockets: usize,
    pipes: usize,
    anon_inode: usize,
    files: usize,
    other: usize,
}

fn snapshot() -> io::Result<FdCounts> {
    let mut c = FdCounts::default();
    for entry in fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        c.total += 1;
        // readlink may race with an fd close (target vanishes); count as
        // "other" rather than aborting the whole probe.
        let Ok(target) = fs::read_link(entry.path()) else {
            c.other += 1;
            continue;
        };
        let s = target.to_string_lossy();
        if s.starts_with("socket:") {
            c.sockets += 1;
        } else if s.starts_with("pipe:") {
            c.pipes += 1;
        } else if s.starts_with("anon_inode:") {
            // Covers eventfd, timerfd, epoll, signalfd, and — importantly for
            // this app — dmabuf fds imported by VAAPI.
            c.anon_inode += 1;
        } else if s.starts_with('/') {
            c.files += 1;
        } else {
            c.other += 1;
        }
    }
    Ok(c)
}

fn log_soft_limit() {
    // Best-effort — if the syscall fails we just skip the log line.
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
    if rc == 0 {
        log::debug!("[fd-probe] RLIMIT_NOFILE soft={} hard={}", lim.rlim_cur, lim.rlim_max);
    }
}
