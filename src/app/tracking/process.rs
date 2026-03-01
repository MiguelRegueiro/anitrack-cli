use std::process::{Command as ProcessCommand, ExitStatus};

use anyhow::{Context, Result, anyhow};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
struct ScopedSigaction {
    signum: libc::c_int,
    old_action: libc::sigaction,
}

#[cfg(unix)]
impl ScopedSigaction {
    fn ignore(signum: libc::c_int) -> Result<Self> {
        unsafe {
            let mut new_action: libc::sigaction = std::mem::zeroed();
            new_action.sa_sigaction = libc::SIG_IGN;
            libc::sigemptyset(&mut new_action.sa_mask);
            new_action.sa_flags = 0;

            let mut old_action: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signum, &new_action, &mut old_action) != 0 {
                return Err(anyhow!("failed to update signal action for {signum}"));
            }

            Ok(Self { signum, old_action })
        }
    }
}

#[cfg(unix)]
impl Drop for ScopedSigaction {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::sigaction(self.signum, &self.old_action, std::ptr::null_mut());
        }
    }
}

#[cfg(unix)]
struct TerminalForegroundGuard {
    stdin_fd: libc::c_int,
    parent_pgrp: libc::pid_t,
    child_foreground: bool,
}

#[cfg(unix)]
impl TerminalForegroundGuard {
    fn new(stdin_fd: libc::c_int, parent_pgrp: libc::pid_t) -> Self {
        Self {
            stdin_fd,
            parent_pgrp,
            child_foreground: false,
        }
    }

    fn handoff_to_child(&mut self, child_pgrp: libc::pid_t) {
        self.child_foreground = unsafe { libc::tcsetpgrp(self.stdin_fd, child_pgrp) == 0 };
    }
}

#[cfg(unix)]
impl Drop for TerminalForegroundGuard {
    fn drop(&mut self) {
        if !self.child_foreground {
            return;
        }
        unsafe {
            let _ = libc::tcsetpgrp(self.stdin_fd, self.parent_pgrp);
        }
    }
}

#[cfg(unix)]
pub(crate) fn with_sigint_ignored<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    let _sigint_guard = ScopedSigaction::ignore(libc::SIGINT)?;
    f()
}

#[cfg(not(unix))]
pub(crate) fn with_sigint_ignored<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    f()
}

#[cfg(unix)]
pub(crate) fn run_interactive_cmd(mut cmd: ProcessCommand) -> Result<ExitStatus> {
    let stdin_fd = libc::STDIN_FILENO;
    let parent_pgrp = unsafe { libc::tcgetpgrp(stdin_fd) };
    if parent_pgrp == -1 {
        return cmd.status().context("failed to launch ani-cli");
    }

    let _sigttou_guard = ScopedSigaction::ignore(libc::SIGTTOU)?;
    let mut terminal_guard = TerminalForegroundGuard::new(stdin_fd, parent_pgrp);

    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            libc::signal(libc::SIGTSTP, libc::SIG_DFL);
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().context("failed to spawn ani-cli")?;
    let child_pgid = child.id() as libc::pid_t;
    terminal_guard.handoff_to_child(child_pgid);
    child.wait().context("failed waiting on ani-cli")
}

#[cfg(not(unix))]
pub(crate) fn run_interactive_cmd(mut cmd: ProcessCommand) -> Result<ExitStatus> {
    cmd.status().context("failed to launch ani-cli")
}
