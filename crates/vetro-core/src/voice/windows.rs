use std::{
    fs, io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use tempfile::TempDir;
use windows_sys::Win32::System::LibraryLoader::{
    BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
};

const UTF8_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
    <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
        <security><requestedPrivileges>
            <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
        </requestedPrivileges></security>
    </trustInfo>
    <application xmlns="urn:schemas-microsoft-com:asm.v3">
        <windowsSettings>
            <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
        </windowsSettings>
    </application>
</assembly>"#;

pub(super) fn utf8_punctuation(root: &Path, original: &Path) -> io::Result<(TempDir, PathBuf)> {
    let directory = tempfile::tempdir_in(root)?;
    let executable = directory
        .path()
        .join(super::executable("sherpa-onnx-offline-punctuation"));
    fs::copy(original, &executable)?;
    let wide_path: Vec<_> = executable.as_os_str().encode_wide().chain([0]).collect();
    let manifest_resource = std::ptr::without_provenance(24);
    let process_manifest = std::ptr::without_provenance(1);
    unsafe {
        let update = BeginUpdateResourceW(wide_path.as_ptr(), 1);
        if update.is_null() {
            return Err(io::Error::last_os_error());
        }
        if UpdateResourceW(
            update,
            manifest_resource,
            process_manifest,
            0,
            UTF8_MANIFEST.as_ptr().cast(),
            UTF8_MANIFEST.len() as u32,
        ) == 0
        {
            let error = io::Error::last_os_error();
            EndUpdateResourceW(update, 1);
            return Err(error);
        }
        if EndUpdateResourceW(update, 0) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((directory, executable))
}
