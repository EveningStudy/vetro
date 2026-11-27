use std::{
    cmp::Ordering,
    env,
    ffi::{OsStr, OsString},
    fs::File,
    future::Future,
    io,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
    ptr::{null, null_mut},
    sync::OnceLock,
};

use windows_sys::Win32::{
    Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0},
    Globalization::{CompareStringOrdinal, CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN},
    System::{
        JobObjects::{
            CreateJobObjectW, JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
            JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
            InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
            LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        },
    },
};

use super::SpawnSpec;

static ROOT_JOB: OnceLock<Result<OwnedHandle, i32>> = OnceLock::new();

pub(super) struct Tree(OwnedHandle);

impl Tree {
    pub(super) fn terminate(&self) -> io::Result<()> {
        checked(unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) })
    }

    pub(super) fn members(&self, _leader: u32) -> Vec<(u32, u64)> {
        const CAPACITY: usize = 1024;
        let header = size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() - size_of::<usize>();
        let mut buffer = vec![0usize; header.div_ceil(size_of::<usize>()) + CAPACITY];
        let queried = unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<usize>()) as u32,
                null_mut(),
            )
        };
        if queried == 0 {
            return Vec::new();
        }
        let list = unsafe { &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
        let count = (list.NumberOfProcessIdsInList as usize).min(CAPACITY);
        let ids = unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count) };
        ids.iter().map(|pid| (*pid as u32, 0)).collect()
    }
}

pub(super) struct Spawned<Wait> {
    pub pid: u32,
    pub tree: Tree,
    pub stdin: tokio::fs::File,
    pub stdout: tokio::fs::File,
    pub stderr: tokio::fs::File,
    pub wait: Wait,
}

pub(super) fn spawn(
    spec: SpawnSpec,
) -> io::Result<Spawned<impl Future<Output = io::Result<ExitStatus>>>> {
    let root = ROOT_JOB
        .get_or_init(|| create_job().map_err(|error| error.raw_os_error().unwrap()))
        .as_ref()
        .map_err(|code| io::Error::from_raw_os_error(*code))?;
    let job = create_job()?;
    let (application, mut command) = if is_batch_script(spec.program.as_ref()) {
        let comspec = resolve_comspec();
        (
            Some(terminated(&comspec)?),
            batch_command_line(&comspec, spec.program.as_os_str(), &spec.args)?,
        )
    } else {
        (None, command_line(spec.program.as_os_str(), &spec.args)?)
    };
    let cwd = spec
        .cwd
        .map(|path| terminated(path.as_os_str()))
        .transpose()?;
    let env = environment(spec.env.into_iter().collect())?;
    let (child_stdin, stdin) = io::pipe()?;
    let (stdout, child_stdout) = io::pipe()?;
    let (stderr, child_stderr) = io::pipe()?;
    let handles = [
        child_stdin.as_raw_handle(),
        child_stdout.as_raw_handle(),
        child_stderr.as_raw_handle(),
    ];
    let jobs = [root.as_raw_handle(), job.as_raw_handle()];
    let mut attrs = Attributes::new()?;
    for handle in handles {
        checked(unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) })?;
    }
    attrs.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
    attrs.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    [
        startup.StartupInfo.hStdInput,
        startup.StartupInfo.hStdOutput,
        startup.StartupInfo.hStdError,
    ] = handles;
    startup.lpAttributeList = attrs.as_ptr();
    let mut info = PROCESS_INFORMATION::default();
    checked(unsafe {
        CreateProcessW(
            application.as_ref().map_or(null(), |app| app.as_ptr()),
            command.as_mut_ptr(),
            null(),
            null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            env.as_ref().map_or(null(), |env| env.as_ptr().cast()),
            cwd.as_ref().map_or(null(), |cwd| cwd.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        )
    })?;
    drop((child_stdin, child_stdout, child_stderr));
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    drop(unsafe { OwnedHandle::from_raw_handle(info.hThread) });

    Ok(Spawned {
        pid: info.dwProcessId,
        tree: Tree(job),
        stdin: tokio::fs::File::from_std(File::from(OwnedHandle::from(stdin))),
        stdout: tokio::fs::File::from_std(File::from(OwnedHandle::from(stdout))),
        stderr: tokio::fs::File::from_std(File::from(OwnedHandle::from(stderr))),
        wait: async move {
            tokio::task::spawn_blocking(move || wait_for_exit(process))
                .await
                .map_err(io::Error::other)?
        },
    })
}

fn checked(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn create_job() -> io::Result<OwnedHandle> {
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    checked(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    })?;
    Ok(job)
}

fn wait_for_exit(process: OwnedHandle) -> io::Result<ExitStatus> {
    let process = process.as_raw_handle();
    if unsafe { WaitForSingleObject(process, INFINITE) } != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }
    let mut code = 0;
    checked(unsafe { GetExitCodeProcess(process, &mut code) })?;
    Ok(ExitStatus::from_raw(code))
}

struct Attributes(Vec<usize>);

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes) };
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        checked(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut bytes)
        })?;
        Ok(Self(storage))
    }

    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }

    fn set(&mut self, attribute: u32, handles: &[HANDLE]) -> io::Result<()> {
        checked(unsafe {
            UpdateProcThreadAttribute(
                self.as_ptr(),
                0,
                attribute as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                null_mut(),
                null(),
            )
        })
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let value = value.encode_wide().collect::<Vec<_>>();
    if value.contains(&0) {
        Err(io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"))
    } else {
        Ok(value)
    }
}

fn terminated(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value = wide(value)?;
    value.push(0);
    Ok(value)
}

fn is_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

fn resolve_comspec() -> OsString {
    env::var_os("ComSpec")
        .filter(|value| {
            !value.is_empty() && Path::new(value).is_absolute() && Path::new(value).is_file()
        })
        .unwrap_or_else(|| OsString::from(r"C:\Windows\System32\cmd.exe"))
}

fn batch_command_line(comspec: &OsStr, program: &OsStr, args: &[OsString]) -> io::Result<Vec<u16>> {
    let mut inner = Vec::new();
    push_batch_quoted(&mut inner, program, false)?;
    for arg in args {
        inner.push(b' ' as u16);
        push_batch_quoted(&mut inner, arg, true)?;
    }

    let mut command = Vec::new();
    push_crt_quoted(&mut command, comspec, false)?;
    for unit in wide(OsStr::new(" /D /V:OFF /S /C "))? {
        command.push(unit);
    }
    command.push(b'"' as u16);
    command.extend(inner);
    command.push(b'"' as u16);
    command.push(0);
    Ok(command)
}

// `cmd.exe /S /C` still interprets `"`, `%`, and `!` even inside double quotes,
// so reject metacharacters that could break out of quoting or expand env vars.
fn push_batch_quoted(out: &mut Vec<u16>, value: &OsStr, allow_empty: bool) -> io::Result<()> {
    const QUOTE: u16 = b'"' as u16;
    const SLASH: u16 = b'\\' as u16;
    const PERCENT: u16 = b'%' as u16;
    const BANG: u16 = b'!' as u16;
    let units = wide(value)?;
    if (!allow_empty && units.is_empty())
        || units.contains(&QUOTE)
        || units.contains(&PERCENT)
        || units.contains(&BANG)
        || units.contains(&(b'\r' as u16))
        || units.contains(&(b'\n' as u16))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid batch command argument",
        ));
    }
    let trailing_slashes = units
        .iter()
        .rev()
        .take_while(|&&unit| unit == SLASH)
        .count();
    out.push(QUOTE);
    out.extend(units);
    out.extend(std::iter::repeat_n(SLASH, trailing_slashes));
    out.push(QUOTE);
    Ok(())
}

fn push_crt_quoted(out: &mut Vec<u16>, value: &OsStr, allow_empty: bool) -> io::Result<()> {
    const QUOTE: u16 = b'"' as u16;
    const SLASH: u16 = b'\\' as u16;
    let units = wide(value)?;
    if (!allow_empty && units.is_empty())
        || units.contains(&(b'\r' as u16))
        || units.contains(&(b'\n' as u16))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid command argument",
        ));
    }
    if !allow_empty && units.contains(&QUOTE) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid executable path",
        ));
    }
    out.push(QUOTE);
    let mut slashes = 0;
    for unit in units {
        if unit == SLASH {
            slashes += 1;
        } else {
            let count = if unit == QUOTE {
                slashes * 2 + 1
            } else {
                slashes
            };
            out.extend(std::iter::repeat_n(SLASH, count));
            out.push(unit);
            slashes = 0;
        }
    }
    out.extend(std::iter::repeat_n(SLASH, slashes * 2));
    out.push(QUOTE);
    Ok(())
}

fn command_line(program: &OsStr, args: &[OsString]) -> io::Result<Vec<u16>> {
    const QUOTE: u16 = b'"' as u16;
    const SLASH: u16 = b'\\' as u16;
    let program = wide(program)?;
    if program.is_empty() || program.contains(&QUOTE) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid executable path",
        ));
    }
    let mut command = vec![QUOTE];
    command.extend(program);
    command.push(QUOTE);
    for arg in args {
        command.extend([b' ' as u16, QUOTE]);
        let mut slashes = 0;
        for unit in wide(arg)? {
            if unit == SLASH {
                slashes += 1;
            } else {
                let count = if unit == QUOTE {
                    slashes * 2 + 1
                } else {
                    slashes
                };
                command.extend(std::iter::repeat_n(SLASH, count));
                command.push(unit);
                slashes = 0;
            }
        }
        command.extend(std::iter::repeat_n(SLASH, slashes * 2));
        command.push(QUOTE);
    }
    command.push(0);
    Ok(command)
}

fn environment(overrides: Vec<(OsString, OsString)>) -> io::Result<Option<Vec<u16>>> {
    if overrides.is_empty() {
        return Ok(None);
    }
    let mut vars = std::env::vars_os()
        .map(|(name, value)| Ok((wide(&name)?, wide(&value)?)))
        .collect::<io::Result<Vec<_>>>()?;
    for (name, value) in overrides {
        let name = wide(&name)?;
        if name.is_empty() || name.contains(&(b'=' as u16)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid environment key",
            ));
        }
        vars.retain(|(key, _)| compare_keys(key, &name) != Ordering::Equal);
        vars.push((name, wide(&value)?));
    }
    vars.sort_by(|(left, _), (right, _)| compare_keys(left, right));
    let mut block = Vec::new();
    for (key, value) in vars {
        block.extend(key);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    Ok(Some(block))
}

fn compare_keys(left: &[u16], right: &[u16]) -> Ordering {
    match unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    } {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_EQUAL => Ordering::Equal,
        CSTR_GREATER_THAN => Ordering::Greater,
        _ => left.cmp(right),
    }
}
