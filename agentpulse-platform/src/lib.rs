//! Private files, bounded current-user administrative IPC, and Windows process trees.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{AdminListener, AdminStream};
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub use windows::{AdminListener, AdminStream};

#[cfg(windows)]
#[allow(unsafe_code)]
mod process;
#[cfg(windows)]
pub use process::ProcessJob;

/// Maximum administrative request or response size, before allocation.
pub const MAX_ADMIN_MESSAGE: usize = 64 * 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Creates a private directory and removes inherited access by other users.
/// The path must designate an application-owned directory, not a shared root.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    let existed = fs::symlink_metadata(path).is_ok();
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(invalid("private directory is not a real directory"));
    }
    #[cfg(unix)]
    {
        protect(path, true)
    }
    #[cfg(windows)]
    {
        if existed {
            windows::protect(path)
        } else {
            windows::protect_new(path, true)
        }
    }
}

/// Protects an existing private file, rejecting links and special files.
pub fn protect_file(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("private file is not a regular file"));
    }
    protect(path, false)
}

fn protect(path: &Path, directory: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )
    }
    #[cfg(windows)]
    {
        let _ = directory;
        windows::protect(path)
    }
}

/// Opens a persistent lock file inside a private application directory.
pub fn open_private_lock(path: &Path) -> io::Result<File> {
    let existed = path.symlink_metadata().is_ok();
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if existed {
        protect_file(path)?;
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    if !existed {
        windows::protect_new(path, false)?;
    } else {
        protect_file(path)?;
    }
    #[cfg(unix)]
    protect_file(path)?;
    Ok(file)
}

/// Replaces a private file using a unique same-directory temporary file.
/// Failed writes leave the old destination intact; temporary files are removed.
pub fn atomic_write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| invalid("private file requires a parent directory"))?;
    ensure_private_dir(parent)?;
    if path.symlink_metadata().is_ok() {
        protect_file(path)?;
    }
    let temporary = parent.join(format!(".agentpulse-{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        protect_file(&temporary)?;
        #[cfg(windows)]
        windows::protect_new(&temporary, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
