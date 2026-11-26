use std::{
    fs::DirBuilder,
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt},
    },
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

// Darwin's socket path limit applies to bytes, including names with spaces
// and multibyte characters. Keep sockets short without relocating credentials,
// the instance lock, or status files out of the selected profile.
pub(super) fn socket_directory(data_dir: &Path) -> io::Result<PathBuf> {
    let canonical = data_dir.canonicalize()?;
    let owner = canonical.metadata()?.uid();
    let profile = Sha256::digest(canonical.as_os_str().as_bytes());
    let path = Path::new("/tmp").join(format!("agentpulse-{owner}-{profile:.24x}"));
    match DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = path.symlink_metadata()?;
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("unsafe AgentPulse socket directory: {}", path.display()),
        ));
    }
    Ok(path)
}
