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
                let _ = std::fs::remove_file(&lock_path);
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

#[cfg(target_os = "linux")]
pub fn daemon_lock_pid_matches_current_executable(pid: u32) -> bool {
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let Ok(current_exe) = std::env::current_exe() else {
        return false;
    };
    let Some(current_name) = current_exe.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    cmdline
        .split(|byte| *byte == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .any(|arg| arg.ends_with(current_name))
}

#[cfg(not(target_os = "linux"))]
pub fn daemon_lock_pid_matches_current_executable(pid: u32) -> bool {
    let Ok(current_exe) = std::env::current_exe() else {
        return false;
    };
    let Some(current_name) = current_exe.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|command| {
            command
                .split_whitespace()
                .any(|arg| arg.ends_with(current_name))
        })
}
