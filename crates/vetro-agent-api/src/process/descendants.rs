use libc::pid_t;

const CAPACITY: usize = 8192;

#[derive(Clone, Copy, PartialEq)]
struct Identity {
    pid: pid_t,
    birth: u64,
}

pub(super) struct Tracker {
    processes: [Option<Identity>; CAPACITY],
    count: usize,
    watchdog: pid_t,
}

/// Lists the live descendants of `leader`, for callers outside the watchdog.
pub(super) unsafe fn snapshot(leader: pid_t) -> Vec<(u32, u64)> {
    let mut tracker = Box::new(Tracker::new(leader));
    tracker.refresh();
    tracker.processes[..tracker.count]
        .iter()
        .flatten()
        .map(|identity| (identity.pid as u32, identity.birth))
        .collect()
}

impl Tracker {
    pub(super) unsafe fn new(leader: pid_t) -> Self {
        let mut tracker = Self {
            processes: [None; CAPACITY],
            count: 0,
            watchdog: libc::getpid(),
        };
        if let Some((identity, _)) = process(leader) {
            tracker.push(identity);
        }
        tracker
    }

    fn push(&mut self, identity: Identity) {
        if self.count < self.processes.len() {
            self.processes[self.count] = Some(identity);
            self.count += 1;
        }
    }

    pub(super) unsafe fn refresh(&mut self) {
        let mut keep = 0;
        for index in 0..self.count {
            let identity = self.processes[index].unwrap();
            if process(identity.pid).is_some_and(|(current, _)| current == identity) {
                self.processes[keep] = Some(identity);
                keep += 1;
            }
        }
        self.count = keep;
        if self.count == 0 {
            return;
        }
        loop {
            let previous = self.count;
            visit_processes(|pid| {
                if pid == self.watchdog || self.count == CAPACITY {
                    return;
                }
                let Some((child, parent)) = process(pid) else {
                    return;
                };
                let known = &self.processes[..self.count];
                if !known.contains(&Some(child))
                    && known.iter().flatten().any(|identity| {
                        identity.pid == parent
                            && process(parent).is_some_and(|(current, _)| current == *identity)
                    })
                {
                    self.push(child);
                }
            });
            if previous == self.count || self.count == CAPACITY {
                break;
            }
        }
    }

    pub(super) unsafe fn terminate(&mut self) {
        self.refresh();
        for identity in self.processes[..self.count].iter().flatten().rev() {
            if process(identity.pid).is_some_and(|(current, _)| current == *identity) {
                libc::kill(identity.pid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn number(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u64, |value, digit| {
        digit
            .is_ascii_digit()
            .then(|| value * 10 + u64::from(*digit - b'0'))
    })
}

#[cfg(target_os = "linux")]
unsafe fn process(pid: pid_t) -> Option<(Identity, pid_t)> {
    let mut digits = [0; 10];
    let mut value = pid as u32;
    let mut start = digits.len();
    while value != 0 {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let digits = &digits[start..];
    let mut path = *b"/proc/0000000000/stat\0";
    path[6..6 + digits.len()].copy_from_slice(digits);
    path[6 + digits.len()..12 + digits.len()].copy_from_slice(b"/stat\0");
    let fd = libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC);
    if fd < 0 {
        return None;
    }
    let mut buffer = [0; 1024];
    let read = libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len());
    libc::close(fd);
    let bytes = buffer.get(..usize::try_from(read).ok()?)?;
    let end = bytes.iter().rposition(|byte| *byte == b')')?;
    let mut fields = bytes.get(end + 2..)?.split(|byte| *byte == b' ');
    fields.next()?;
    let parent = number(fields.next()?)? as pid_t;
    let birth = number(fields.nth(17)?)?;
    Some((Identity { pid, birth }, parent))
}

#[cfg(target_os = "linux")]
unsafe fn visit_processes(mut visit: impl FnMut(pid_t)) {
    let fd = libc::open(
        c"/proc".as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
    );
    if fd < 0 {
        return;
    }
    let mut buffer = [0u8; 8192];
    loop {
        let read = libc::syscall(libc::SYS_getdents64, fd, buffer.as_mut_ptr(), buffer.len());
        if read <= 0 {
            break;
        }
        let mut offset = 0;
        while offset + 19 < read as usize {
            let len = u16::from_ne_bytes([buffer[offset + 16], buffer[offset + 17]]) as usize;
            if len <= 19 || offset + len > read as usize {
                break;
            }
            let name = &buffer[offset + 19..offset + len];
            if let Some(pid) = name
                .iter()
                .position(|byte| *byte == 0)
                .and_then(|end| number(&name[..end]))
                .and_then(|pid| pid_t::try_from(pid).ok())
            {
                visit(pid);
            }
            offset += len;
        }
    }
    libc::close(fd);
}

#[cfg(target_os = "macos")]
unsafe fn process(pid: pid_t) -> Option<(Identity, pid_t)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    if libc::proc_pidinfo(
        pid,
        libc::PROC_PIDTBSDINFO,
        0,
        info.as_mut_ptr().cast(),
        size,
    ) != size
    {
        return None;
    }
    let info = info.assume_init();
    let birth = info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec;
    Some((Identity { pid, birth }, info.pbi_ppid as pid_t))
}

#[cfg(target_os = "macos")]
unsafe fn visit_processes(mut visit: impl FnMut(pid_t)) {
    let mut pids = [0; CAPACITY];
    let size = std::mem::size_of_val(&pids) as libc::c_int;
    let count = libc::proc_listallpids(pids.as_mut_ptr().cast(), size).max(0) as usize;
    for pid in pids.into_iter().take(count).filter(|pid| *pid > 0) {
        visit(pid);
    }
}
