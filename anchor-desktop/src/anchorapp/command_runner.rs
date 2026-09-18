//! Command execution engine.
//!
//! Runs user-defined commands through `sh -c` in their own process group so
//! children survive Anchor exiting and can be killed as a group. Two modes:
//!
//! * **detached** (`detach = true`): fire-and-forget. We report only [`RunStatus::Started`].
//!   The process is reaped silently in the background so it doesn't become a zombie,
//!   but its lifetime is otherwise the user's responsibility.
//! * **attached** (`detach = false`): we wait for completion and report
//!   [`RunStatus::Finished`] with the exit code (or [`RunStatus::Failed`] if the
//!   process could not be spawned at all).
//!
//! Either way the spawned process is registered by `exec_id` so it can be killed
//! via [`CommandRunner::kill`], which signals the whole process group.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};

/// Cap on how much trailing stderr we keep from a failed command — enough to
/// surface the reason ("command not found", the last lines of a trace) without
/// letting a noisy command balloon memory.
const STDERR_TAIL_CAP: usize = 4096;

/// Read `r` to EOF, retaining only the last [`STDERR_TAIL_CAP`] bytes.
fn drain_tail<R: Read>(mut r: R) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > STDERR_TAIL_CAP {
                    buf.drain(0..buf.len() - STDERR_TAIL_CAP);
                }
            }
            Err(_) => break,
        }
    }
    buf
}

/// Lifecycle status reported for a single execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    /// The process was spawned successfully.
    Started,
    /// The process exited (attached mode only). `exit_code` is the shell exit
    /// status; processes killed by a signal report `128 + signal`.
    Finished { exit_code: i32 },
    /// The process could not be spawned (e.g. empty command, fork failure).
    Failed { error: String },
}

/// Spawns commands and tracks running children so they can be killed.
#[derive(Clone)]
pub struct CommandRunner {
    /// exec_id → child PID. PID equals the process-group id because every child
    /// is placed in a fresh group via `process_group(0)`.
    running: Arc<Mutex<HashMap<String, u32>>>,
}

impl Default for CommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRunner {
    pub fn new() -> Self {
        CommandRunner { running: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Number of currently-tracked running processes (for diagnostics/tests).
    pub fn running_count(&self) -> usize {
        self.running.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Execute `command`. `on_status` is invoked (possibly from a background
    /// thread) with each lifecycle transition, tagged with `exec_id`. Returns
    /// the generated `exec_id`.
    pub fn run<F>(&self, command: &str, detach: bool, on_status: F) -> String
    where
        F: Fn(&str, RunStatus) + Send + 'static,
    {
        let exec_id = uuid::Uuid::new_v4().to_string();
        self.run_with_id(exec_id.clone(), command, detach, on_status);
        exec_id
    }

    fn run_with_id<F>(&self, exec_id: String, command: &str, detach: bool, on_status: F)
    where
        F: Fn(&str, RunStatus) + Send + 'static,
    {
        let command = command.trim();
        if command.is_empty() {
            on_status(&exec_id, RunStatus::Failed { error: "empty command".to_string() });
            return;
        }

        let mut proc = ProcessCommand::new("sh");
        proc.arg("-c").arg(command);
        // New process group: pgid == child pid. Lets us kill the whole group
        // (the shell plus anything it forked) and keeps signals from leaking
        // back into Anchor's own group.
        proc.process_group(0);
        // Never let a command read Anchor's stdin or block us on a full stdout
        // pipe — both are nulled. stderr is captured for attached runs so we can
        // log *why* a command failed; detached runs keep it nulled so they stay
        // truly fire-and-forget and survive Anchor exiting (a piped stderr would
        // break once our end closed).
        proc.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null());
        if detach {
            proc.stderr(std::process::Stdio::null());
        } else {
            proc.stderr(std::process::Stdio::piped());
        }

        let child = match proc.spawn() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Commands: spawn failed for {:?}: {}", command, e);
                on_status(&exec_id, RunStatus::Failed { error: format!("spawn failed: {}", e) });
                return;
            }
        };

        let pid = child.id();
        if let Ok(mut map) = self.running.lock() {
            map.insert(exec_id.clone(), pid);
        }
        log::info!("Commands: started exec {} (pid {}, detach={})", exec_id, pid, detach);
        on_status(&exec_id, RunStatus::Started);

        let running = self.running.clone();
        let thread_name = format!("cmd-wait-{}", pid);
        let thread_exec_id = exec_id.clone();
        let spawn_result = std::thread::Builder::new().name(thread_name).spawn(move || {
            let mut child = child;
            // Drain stderr on its own thread so a chatty command can't block on a
            // full pipe while we wait. Detached runs have stderr nulled, so this
            // is None for them.
            let stderr_reader =
                child.stderr.take().map(|err| std::thread::spawn(move || drain_tail(err)));
            let wait = child.wait();
            // Always unregister once the process is gone.
            if let Ok(mut map) = running.lock() {
                map.remove(&thread_exec_id);
            }
            let stderr_tail = stderr_reader
                .and_then(|h| h.join().ok())
                .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
                .filter(|s| !s.is_empty());

            if detach {
                // Fire-and-forget: we only reaped to avoid a zombie. Still record
                // the exit code at debug level so a silently-failing detached
                // command leaves some trace.
                if let Ok(status) = &wait {
                    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                    log::debug!(
                        "Commands: detached exec {} exited (code {})",
                        thread_exec_id,
                        code
                    );
                }
                return;
            }
            match wait {
                Ok(status) => {
                    let exit_code =
                        status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                    if exit_code == 0 {
                        log::info!("Commands: exec {} finished (code 0)", thread_exec_id);
                    } else if let Some(tail) = &stderr_tail {
                        log::warn!(
                            "Commands: exec {} failed (code {}): {}",
                            thread_exec_id,
                            exit_code,
                            tail
                        );
                    } else {
                        log::warn!("Commands: exec {} failed (code {})", thread_exec_id, exit_code);
                    }
                    on_status(&thread_exec_id, RunStatus::Finished { exit_code });
                }
                Err(e) => {
                    log::warn!("Commands: wait failed for exec {}: {}", thread_exec_id, e);
                    on_status(
                        &thread_exec_id,
                        RunStatus::Failed { error: format!("wait failed: {}", e) },
                    );
                }
            }
        });

        if spawn_result.is_err() {
            // Couldn't spawn the reaper thread; drop the registry entry so we
            // don't leak a never-cleared PID. The child keeps running.
            if let Ok(mut map) = self.running.lock() {
                map.remove(&exec_id);
            }
            log::warn!("Commands: failed to spawn wait thread for exec {}", exec_id);
        }
    }

    /// Send SIGTERM to the process group of a running execution. Returns true if
    /// the `exec_id` was known (a signal was attempted), false otherwise.
    pub fn kill(&self, exec_id: &str) -> bool {
        let pid = match self.running.lock().ok().and_then(|m| m.get(exec_id).copied()) {
            Some(p) => p,
            None => return false,
        };
        // Negative pid → the whole process group (pgid == pid via process_group(0)).
        let ret = unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
        if ret != 0 {
            log::warn!("Commands: kill(-{}) failed: {}", pid, std::io::Error::last_os_error());
        } else {
            log::info!("Commands: sent SIGTERM to group {} (exec {})", pid, exec_id);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Run a command and collect every status it emits until the channel goes
    /// quiet for `quiet_for`.
    fn collect(command: &str, detach: bool) -> Vec<RunStatus> {
        let runner = CommandRunner::new();
        let (tx, rx) = mpsc::channel();
        runner.run(command, detach, move |_id, status| {
            let _ = tx.send(status);
        });
        let mut out = Vec::new();
        while let Ok(s) = rx.recv_timeout(Duration::from_secs(5)) {
            out.push(s);
        }
        out
    }

    #[test]
    fn true_reports_started_then_finished_zero() {
        let statuses = collect("true", false);
        assert_eq!(statuses, vec![RunStatus::Started, RunStatus::Finished { exit_code: 0 }]);
    }

    #[test]
    fn false_reports_exit_one() {
        let statuses = collect("false", false);
        assert_eq!(statuses.last(), Some(&RunStatus::Finished { exit_code: 1 }));
    }

    #[test]
    fn explicit_exit_code_propagates() {
        let statuses = collect("exit 3", false);
        assert_eq!(statuses.last(), Some(&RunStatus::Finished { exit_code: 3 }));
    }

    #[test]
    fn empty_command_fails_without_spawning() {
        let statuses = collect("   ", false);
        assert_eq!(statuses.len(), 1);
        assert!(matches!(statuses[0], RunStatus::Failed { .. }));
    }

    #[test]
    fn detached_reports_only_started() {
        let statuses = collect("true", true);
        assert_eq!(statuses, vec![RunStatus::Started]);
    }

    #[test]
    fn kill_terminates_running_process() {
        let runner = CommandRunner::new();
        let (tx, rx) = mpsc::channel();
        let exec_id = runner.run("sleep 30", false, move |_id, status| {
            let _ = tx.send(status);
        });

        // Wait for Started, then kill.
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), RunStatus::Started);
        assert!(runner.kill(&exec_id), "exec_id should be known");

        // SIGTERM → process exits; we should see a Finished promptly.
        let finished = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(finished, RunStatus::Finished { .. }), "got {:?}", finished);
        // And the registry is cleared.
        assert_eq!(runner.running_count(), 0);
    }

    #[test]
    fn kill_unknown_exec_id_returns_false() {
        let runner = CommandRunner::new();
        assert!(!runner.kill("does-not-exist"));
    }

    #[test]
    fn drain_tail_keeps_only_the_last_bytes() {
        let data = vec![b'x'; STDERR_TAIL_CAP + 500];
        let tail = drain_tail(std::io::Cursor::new(data));
        assert_eq!(tail.len(), STDERR_TAIL_CAP);
    }

    #[test]
    fn drain_tail_passes_through_small_input() {
        let tail = drain_tail(std::io::Cursor::new(b"boom".to_vec()));
        assert_eq!(tail, b"boom");
    }

    #[test]
    fn command_writing_stderr_still_reports_exit_code() {
        // Capturing stderr must not deadlock or change the reported exit code.
        let statuses = collect("echo boom 1>&2; exit 5", false);
        assert_eq!(statuses.last(), Some(&RunStatus::Finished { exit_code: 5 }));
    }
}
