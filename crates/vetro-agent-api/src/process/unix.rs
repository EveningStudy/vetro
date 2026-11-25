use std::{
    future::Future,
    io::{self, PipeWriter},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    process::{ExitStatus, Stdio},
    sync::Mutex,
};

use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

use super::SpawnSpec;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod descendants;

pub(super) struct Tree(Mutex<Option<PipeWriter>>);

impl Tree {
    pub(super) fn terminate(&self) -> io::Result<()> {
        self.0.lock().unwrap().take();
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn members(&self, leader: u32) -> Vec<(u32, u64)> {
        unsafe { descendants::snapshot(leader as libc::pid_t) }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn members(&self, _leader: u32) -> Vec<(u32, u64)> {
        Vec::new()
    }
}

pub(super) struct Spawned<Wait> {
    pub pid: u32,
    pub tree: Tree,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
    pub wait: Wait,
}

pub(super) fn spawn(
    spec: SpawnSpec,
) -> io::Result<Spawned<impl Future<Output = io::Result<ExitStatus>>>> {
    let (watchdog, lifetime) = io::pipe()?;
    let watchdog_fd = unsafe { libc::fcntl(watchdog.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if watchdog_fd == -1 {
        return Err(io::Error::last_os_error());
    }
    let watchdog_fd = unsafe { OwnedFd::from_raw_fd(watchdog_fd) };
    let max_fd = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    if max_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut command = Command::new(spec.program);
    command
        .args(spec.args)
        .envs(spec.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    if let Some(cwd) = spec.cwd {
        command.current_dir(cwd);
    }
    unsafe {
        command.pre_exec(move || {
            let leader = libc::getpid();
            match libc::fork() {
                -1 => Err(io::Error::last_os_error()),
                0 => watch_parent(watchdog_fd.as_raw_fd(), max_fd as libc::c_int, leader),
                _ => Ok(()),
            }
        });
    }
    let mut child = command.spawn()?;
    Ok(Spawned {
        pid: child.id().unwrap(),
        tree: Tree(Mutex::new(Some(lifetime))),
        stdin: child.stdin.take().unwrap(),
        stdout: child.stdout.take().unwrap(),
        stderr: child.stderr.take().unwrap(),
        wait: async move { child.wait().await },
    })
}

/// The pre-exec watchdog stays in the child group. EOF also detects SIGKILL of Vetro.
unsafe fn watch_parent(watchdog_fd: libc::c_int, max_fd: libc::c_int, _leader: libc::pid_t) -> ! {
    if libc::dup2(watchdog_fd, libc::STDIN_FILENO) == -1 {
        libc::kill(0, libc::SIGKILL);
        libc::_exit(1);
    }
    close_other_fds(max_fd);
    let mut signals = std::mem::zeroed();
    libc::sigfillset(&mut signals);
    libc::sigprocmask(libc::SIG_SETMASK, &signals, std::ptr::null_mut());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut descendants = descendants::Tracker::new(_leader);
    let mut lifetime = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        descendants.refresh();
        let ready = libc::poll(&mut lifetime, 1, 50);
        if ready > 0
            || (ready < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR))
        {
            break;
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    descendants.terminate();
    libc::kill(0, libc::SIGKILL);
    libc::_exit(1);
}

unsafe fn close_other_fds(max_fd: libc::c_int) {
    #[cfg(target_os = "linux")]
    if libc::syscall(libc::SYS_close_range, 1u32, u32::MAX, 0u32) == 0 {
        return;
    }
    for fd in 1..max_fd {
        libc::close(fd);
    }
}
