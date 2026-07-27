//! Process-group lifecycle control for llxprt child processes.

use std::process::Command;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
pub(super) fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
pub(super) fn configure_process_group(_command: &mut Command) {}

pub(super) fn terminate_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        signal_process_group(child.id(), rustix::process::Signal::TERM);
        thread::sleep(Duration::from_millis(250));
        signal_process_group(child.id(), rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        thread::sleep(Duration::from_millis(250));
        let _ = child.kill();
    }
}

#[cfg(unix)]
fn signal_process_group(process_group: u32, signal: rustix::process::Signal) {
    use rustix::process::{getpgid, kill_process, kill_process_group, Pid};

    let Ok(raw_pid) = i32::try_from(process_group) else {
        return;
    };
    let Some(pid) = Pid::from_raw(raw_pid) else {
        return;
    };
    if getpgid(Some(pid)).is_ok_and(|pgid| pgid == pid) {
        let _ = kill_process_group(pid, signal);
    } else {
        let _ = kill_process(pid, signal);
    }
}
