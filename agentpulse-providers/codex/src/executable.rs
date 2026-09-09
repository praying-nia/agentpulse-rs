//! Resolve native Codex binaries without invoking Windows shell wrappers.
use std::{
    io,
    path::{Path, PathBuf},
};

/// Resolves a Codex executable for both Host and Provider.
/// On Windows, supports native binaries on PATH and the official npm package's
/// native vendor binary next to a codex.cmd/ps1 shim. Shell scripts are not run.
/// Unix preserves the caller's executable and normal PATH lookup semantics.
pub fn resolve_codex_executable(executable: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        Ok(executable.to_path_buf())
    }
    #[cfg(windows)]
    {
        let explicit = executable.components().count() > 1 || executable.is_absolute();
        if explicit {
            if executable
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                && executable.is_file()
            {
                return executable.canonicalize();
            }
            if executable
                .file_stem()
                .is_some_and(|stem| stem.eq_ignore_ascii_case("codex"))
                && let Some(parent) = executable.parent()
                && let Some(binary) = npm_binary(parent)
            {
                return binary.canonicalize();
            }
        } else {
            for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                let binary = directory.join(executable).with_extension("exe");
                if binary.is_file() {
                    return binary.canonicalize();
                }
                if executable
                    .file_stem()
                    .is_some_and(|stem| stem.eq_ignore_ascii_case("codex"))
                    && let Some(binary) = npm_binary(&directory)
                {
                    return binary.canonicalize();
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "native Codex executable not found for {}; pass --codex <path-to-codex.exe>",
                executable.display()
            ),
        ))
    }
}

#[cfg(windows)]
fn npm_binary(directory: &Path) -> Option<PathBuf> {
    #[cfg(target_arch = "x86_64")]
    let (package, target) = ("codex-win32-x64", "x86_64-pc-windows-msvc");
    #[cfg(target_arch = "aarch64")]
    let (package, target) = ("codex-win32-arm64", "aarch64-pc-windows-msvc");
    let root = directory.join("node_modules/@openai/codex");
    for vendor in [
        root.join(format!("node_modules/@openai/{package}/vendor/{target}")),
        root.join(format!("vendor/{target}")),
    ] {
        for subdirectory in ["bin", "codex"] {
            let binary = vendor.join(subdirectory).join("codex.exe");
            if binary.is_file() {
                return Some(binary);
            }
        }
    }
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn explicit_native_path_handles_spaces_and_npm_shims() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!(
            "ap executable {}",
            agentpulse_core::ProviderId::new()
        ));
        let vendor = root.join("node_modules/@openai/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin");
        #[cfg(target_arch = "aarch64")]
        let vendor = root.join("node_modules/@openai/codex/node_modules/@openai/codex-win32-arm64/vendor/aarch64-pc-windows-msvc/bin");
        std::fs::create_dir_all(&vendor)?;
        let exe = vendor.join("codex.exe");
        std::fs::write(&exe, b"resolver fixture; never executed")?;
        let result = (|| {
            assert_eq!(resolve_codex_executable(&exe)?, exe.canonicalize()?);
            assert_eq!(
                resolve_codex_executable(&root.join("codex.cmd"))?,
                exe.canonicalize()?
            );
            assert!(resolve_codex_executable(&root.join("custom.ps1")).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
    }
}
