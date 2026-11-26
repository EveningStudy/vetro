use std::{
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

const LAUNCHER_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) fn open_in_system(path: &Path) -> io::Result<()> {
    let mut failed_launcher = None;
    let mut spawn_error = None;
    for mut command in open::commands(path) {
        vetro_agent_api::process::remove_appimage_environment(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let (child_sender, child_receiver) = mpsc::channel::<Child>();
        let (status_sender, status_receiver) = mpsc::channel();
        thread::Builder::new().spawn(move || {
            if let Ok(mut child) = child_receiver.recv() {
                let _ = status_sender.send(child.wait());
            }
        })?;
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                spawn_error = Some(error);
                continue;
            }
        };
        child_sender
            .send(child)
            .map_err(|_| io::Error::other("System launcher observer stopped"))?;
        match status_receiver.recv_timeout(LAUNCHER_TIMEOUT) {
            Err(RecvTimeoutError::Timeout) => return Ok(()),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("System launcher observer stopped"));
            }
            Ok(status) => {
                let status = status?;
                if status.success() {
                    return Ok(());
                }
                failed_launcher.get_or_insert_with(|| {
                    io::Error::other(format!(
                        "System launcher {} exited with {status}",
                        command.get_program().to_string_lossy()
                    ))
                });
            }
        }
    }
    Err(failed_launcher.or(spawn_error).unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "No system file launcher found")
    }))
}
