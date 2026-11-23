//! Opening Antigravity's SQLite files without leaving a trace.
//!
//! SQLite's `mode=ro` still creates `-wal` and `-shm` files next to a WAL database when they are
//! missing. A small VFS shim on top of the platform VFS refuses to create, write or delete any
//! named file, so the worst case of a race with the app is a failed open, never a new file.
//! The app's own `-shm` is opened read-only (`readonly_shm=1`), so its contents are never touched.

use std::{
    ffi::{c_char, c_int, CStr},
    path::Path,
    sync::OnceLock,
};

use rusqlite::{ffi, Connection, OpenFlags};

const VFS_NAME: &CStr = c"vetro-readonly";

static BASE_VFS: OnceLock<usize> = OnceLock::new();

fn base() -> *mut ffi::sqlite3_vfs {
    *BASE_VFS.get().expect("vfs registered") as *mut ffi::sqlite3_vfs
}

fn exists(name: &CStr) -> bool {
    name.to_str().is_ok_and(|p| Path::new(p).is_file())
}

unsafe extern "C" fn x_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    mut flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let base = base();
    if !name.is_null() && flags & ffi::SQLITE_OPEN_DELETEONCLOSE == 0 {
        // SAFETY: SQLite passes a NUL-terminated file name.
        let path = unsafe { CStr::from_ptr(name) };
        if !exists(path) {
            return ffi::SQLITE_CANTOPEN;
        }
        if flags & ffi::SQLITE_OPEN_WAL != 0 {
            let Some(shm) = path
                .to_str()
                .ok()
                .and_then(|p| p.strip_suffix("-wal"))
                .map(|p| format!("{p}-shm"))
            else {
                return ffi::SQLITE_CANTOPEN;
            };
            if !Path::new(&shm).is_file() {
                return ffi::SQLITE_CANTOPEN;
            }
        }
        flags &=
            !(ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_EXCLUSIVE);
        flags |= ffi::SQLITE_OPEN_READONLY;
    }
    // SAFETY: `base` is the registered default VFS, which outlives the process.
    unsafe {
        match (*base).xOpen {
            Some(open) => open(base, name, file, flags, out_flags),
            None => ffi::SQLITE_CANTOPEN,
        }
    }
}

unsafe extern "C" fn x_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    _name: *const c_char,
    _sync: c_int,
) -> c_int {
    ffi::SQLITE_IOERR_DELETE
}

fn register() -> rusqlite::Result<()> {
    static REGISTERED: OnceLock<c_int> = OnceLock::new();
    let rc = *REGISTERED.get_or_init(|| {
        // SAFETY: plain FFI calls; the copied struct is leaked so it lives as long as SQLite.
        unsafe {
            let rc = ffi::sqlite3_initialize();
            if rc != ffi::SQLITE_OK {
                return rc;
            }
            let base = ffi::sqlite3_vfs_find(std::ptr::null());
            if base.is_null() {
                return ffi::SQLITE_ERROR;
            }
            BASE_VFS.get_or_init(|| base as usize);
            let mut vfs = *base;
            vfs.pNext = std::ptr::null_mut();
            vfs.zName = VFS_NAME.as_ptr();
            vfs.xOpen = Some(x_open);
            vfs.xDelete = Some(x_delete);
            ffi::sqlite3_vfs_register(Box::into_raw(Box::new(vfs)), 0)
        }
    });
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None))
    }
}

fn uri(path: &Path, query: &str) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let raw = abs.to_string_lossy();
    #[cfg(windows)]
    let raw = raw.replace('\\', "/");
    let mut out = String::from("file:");
    if !raw.starts_with('/') {
        out.push_str("///");
    }
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || b"/:-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push('?');
    out.push_str(query);
    out
}

/// Open a foreign SQLite database strictly read-only.
///
/// A database with a `-wal` file is opened as a normal reader so committed frames that have
/// not been checkpointed are visible. A WAL that cannot be read safely produces a busy error.
/// A database without a WAL can be opened immutable.
pub fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    register()?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let open = |query: &str| -> rusqlite::Result<Connection> {
        let conn = Connection::open_with_flags_and_vfs(uri(path, query), flags, VFS_NAME)?;
        conn.pragma_update(None, "query_only", true)?;
        conn.pragma_update(None, "temp_store", "memory")?;
        conn.busy_timeout(std::time::Duration::from_millis(10))?;
        // Force the schema read now so a refused `-wal` shows up here, not mid-query.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        Ok(conn)
    };
    if !path.is_file() {
        return Err(rusqlite::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_CANTOPEN),
            Some(format!("{} does not exist", path.display())),
        ));
    }
    let wal = super::sibling(path, "-wal");
    if !wal.try_exists().unwrap_or(true) {
        let immutable = open("immutable=1");
        if !wal.try_exists().unwrap_or(true) {
            return immutable;
        }
    }
    let busy = |detail: &str| {
        rusqlite::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_BUSY),
            Some(format!("{}: WAL unavailable: {detail}", path.display())),
        )
    };
    if !super::sibling(path, "-shm").is_file() {
        return Err(busy("shared memory file missing"));
    }
    open("mode=ro&readonly_shm=1").map_err(|error| busy(&error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_escapes_reserved_characters() {
        #[cfg(windows)]
        assert_eq!(
            uri(Path::new(r"C:\a b\c#d?.db"), "immutable=1"),
            "file:///C:/a%20b/c%23d%3F.db?immutable=1"
        );
        #[cfg(unix)]
        assert_eq!(
            uri(Path::new(r"/tmp/x\y.db"), "mode=ro"),
            "file:/tmp/x%5Cy.db?mode=ro"
        );
    }
}
