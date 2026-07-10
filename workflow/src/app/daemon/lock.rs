use super::*;

/// Acquire the per-config singleton lock, honoring `--force` recovery.
///
/// Returns the held guard on success, or `None` after printing a clear error
/// when another live daemon owns the lock.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub fn acquire_daemon_lock(
    store: &DaemonStore,
    config_id: &str,
    force: bool,
) -> Option<luther_workflow::monitor::SingletonGuard> {
    use luther_workflow::monitor::{acquire_singleton_lock, process::MonitorError};

    let lock_path = store.lock_path(config_id).to_string_lossy().to_string();
    match acquire_singleton_lock(&lock_path) {
        Ok(guard) => Some(guard),
        Err(MonitorError::LockHeld { pid }) => {
            if force {
                if !daemon_lock_pid_matches_current_executable(pid) {
                    eprintln!(
                        "Error: refusing to replace daemon lock for '{config_id}' because pid {pid} does not appear to be this daemon binary"
                    );
                    return None;
                }
                if !luther_workflow::daemon::terminate_pid(pid) {
                    eprintln!(
                        "Error: failed to confirm daemon pid {pid} exited before replacing lock for '{config_id}'"
                    );
                    return None;
                }
                match std::fs::remove_file(&lock_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        eprintln!(
                            "Error: failed to remove stale daemon lock file '{lock_path}' for '{config_id}': {e}"
                        );
                        return None;
                    }
                }
                match acquire_singleton_lock(&lock_path) {
                    Ok(guard) => Some(guard),
                    Err(e) => {
                        eprintln!("Error: failed to replace daemon lock for '{config_id}': {e}");
                        None
                    }
                }
            } else {
                eprintln!(
                    "Error: daemon already running (config={config_id}, pid={pid}). \
                     Use --force to replace it."
                );
                None
            }
        }
        Err(e) => {
            eprintln!("Error: failed to acquire daemon lock for '{config_id}': {e}");
            None
        }
    }
}

/// Verify that `pid` still refers to this daemon binary before the caller
/// replaces its lock and terminates it.
///
/// On Linux the check compares the canonical executable path of the target
/// process (`/proc/{pid}/exe`) against this process's executable, which is far
/// stronger than a command-line filename-suffix heuristic and cannot be fooled
/// by an unrelated process that merely happens to have a matching argument. If
/// the identity cannot be resolved confidently the function returns `false` so
/// the caller refuses to terminate the process.
#[cfg(target_os = "linux")]
pub fn daemon_lock_pid_matches_current_executable(pid: u32) -> bool {
    let Ok(current_exe) = std::env::current_exe() else {
        return false;
    };
    // Resolve both paths canonically so symlinks (e.g. /proc/{pid}/exe) and
    // relative launch paths compare equal only when they are truly the same
    // on-disk binary.
    let Ok(current_exe) = std::fs::canonicalize(&current_exe) else {
        return false;
    };
    let Ok(target_exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    match std::fs::canonicalize(&target_exe) {
        Ok(target_exe) => target_exe == current_exe,
        Err(_) => target_exe == current_exe,
    }
}

/// Non-Linux fallback that compares just the executable command name reported
/// by `ps -o comm=` against this process's executable file name.
///
/// Using `comm=` (the executable name only) avoids splitting the full argument
/// string on whitespace, which would misparse executables or arguments that
/// contain spaces. When the identity cannot be resolved the function returns
/// `false` so the caller refuses to terminate an ambiguous process.
#[cfg(not(target_os = "linux"))]
pub fn daemon_lock_pid_matches_current_executable(pid: u32) -> bool {
    let Ok(current_exe) = std::env::current_exe() else {
        return false;
    };
    let Some(current_name) = current_exe.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|command| {
            // `comm=` yields a single command-name field; trim surrounding
            // whitespace/newline and compare against the executable name,
            // tolerating a leading path component if present.
            let reported = command.trim();
            let reported_name = std::path::Path::new(reported)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(reported);
            reported_name == current_name
        })
}
