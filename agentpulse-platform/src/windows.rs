//! Windows FFI is contained here; all exposed operations are safe and bounded.
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{
    ffi::{OsStr, c_void},
    fs::{self, OpenOptions},
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut text: Vec<u16> = value.encode_wide().collect();
    if text.contains(&0) {
        return Err(super::invalid("embedded NUL in Windows name"));
    }
    text.push(0);
    Ok(text)
}

struct LocalMemory(*mut c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: this pointer was returned by a documented LocalAlloc-backed API.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful API call transferred a unique owned handle to us.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn current_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: valid pseudo process handle and writable output pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut length = 0;
    // SAFETY: null buffer with zero length is the documented size query.
    unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut length);
    }
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    // SAFETY: usize-aligned allocation has at least length bytes, valid token.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful TokenUser query initialized an aligned TOKEN_USER.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = null_mut();
    // SAFETY: SID lies in the live token buffer, output is owned LocalAlloc memory.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = LocalMemory(text.cast());
    let mut length = 0;
    // SAFETY: API returns a NUL-terminated UTF-16 SID string.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16(std::slice::from_raw_parts(text, length))
            .map_err(|_| super::invalid("invalid SID encoding"))
    }
}

fn descriptor() -> io::Result<LocalMemory> {
    let sid = current_sid()?;
    let sddl = wide(OsStr::new(&format!("O:{sid}D:P(A;OICI;FA;;;{sid})")))?;
    let mut value = null_mut();
    // SAFETY: NUL-terminated SDDL and valid output; result freed via LocalFree.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut value,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalMemory(value))
}

pub(super) fn protect(path: &Path) -> io::Result<()> {
    apply_acl(path, false)
}

/// Applies the private descriptor to an object created by this process. The
/// owner is written explicitly because elevated/restricted CI tokens can cause
/// Windows to choose the token's default owner instead of TokenUser.
pub(super) fn protect_new(path: &Path, directory: bool) -> io::Result<()> {
    let _ = directory;
    apply_acl(path, true)
}

fn apply_acl(path: &Path, set_owner: bool) -> io::Result<()> {
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(super::invalid("private paths must not be reparse points"));
    }
    let security = descriptor()?;
    let mut dacl = null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: valid live security descriptor and file handle; all outputs writable.
    unsafe {
        if GetSecurityDescriptorDacl(security.0, &mut present, &mut dacl, &mut defaulted) == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(io::Error::last_os_error());
        }
        if !set_owner {
            check_owner(file.as_raw_handle(), SE_FILE_OBJECT)?;
        }
        let security_flags = if set_owner {
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION
        };
        let error = SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            security_flags,
            if set_owner {
                expected_owner_from_descriptor(&security)?
            } else {
                null_mut()
            },
            null_mut(),
            dacl,
            null_mut(),
        );
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
    Ok(())
}

fn expected_owner_from_descriptor(security: &LocalMemory) -> io::Result<*mut c_void> {
    let mut owner = null_mut();
    let mut defaulted = 0;
    // SAFETY: descriptor is live and the output pointer is writable.
    if unsafe { GetSecurityDescriptorOwner(security.0, &mut owner, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(owner)
}

fn check_owner(handle: HANDLE, kind: SE_OBJECT_TYPE) -> io::Result<()> {
    let expected = descriptor()?;
    let mut expected_owner = null_mut();
    let mut owner = null_mut();
    let mut defaulted = 0;
    let mut old_security = null_mut();
    // SAFETY: live handle and descriptors; returned owner points into old_security.
    unsafe {
        if GetSecurityDescriptorOwner(expected.0, &mut expected_owner, &mut defaulted) == 0 {
            return Err(io::Error::last_os_error());
        }
        let error = GetSecurityInfo(
            handle,
            kind,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut old_security,
        );
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        let _old = LocalMemory(old_security);
        if owner.is_null() || expected_owner.is_null() || EqualSid(owner, expected_owner) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private object is not owned by current user",
            ));
        }
    }
    Ok(())
}

fn pipe_name(path: &Path) -> io::Result<Vec<u16>> {
    let parent = path
        .parent()
        .ok_or_else(|| super::invalid("admin endpoint requires parent"))?;
    let canonical = fs::canonicalize(parent)?;
    let mut hash = Sha256::new();
    hash.update(current_sid()?.as_bytes());
    // Canonicalized directory plus user SID isolates users and --data-dir values.
    // Case folding makes ordinary Windows path casing aliases address one pipe.
    for unit in canonical
        .as_os_str()
        .to_string_lossy()
        .to_lowercase()
        .encode_utf16()
    {
        hash.update(unit.to_le_bytes());
    }
    wide(OsStr::new(&format!(
        r"\\.\pipe\agentpulse-admin-{:x}",
        hash.finalize()
    )))
}

fn create_pipe(name: &[u16]) -> io::Result<OwnedHandle> {
    let security = descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    };
    // SAFETY: name and descriptor stay live throughout synchronous creation.
    owned(unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            (super::MAX_ADMIN_MESSAGE + 4) as u32,
            (super::MAX_ADMIN_MESSAGE + 4) as u32,
            0,
            &attributes,
        )
    })
}

/// Current-user-only, local-only, nonblocking Windows named-pipe listener.
pub struct AdminListener {
    pending: Arc<OwnedHandle>,
    busy: Arc<AtomicBool>,
}
impl AdminListener {
    /// Binds a unique Host endpoint; rejects an already occupied pipe name.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let name = pipe_name(path)?;
        let pending = Arc::new(create_pipe(&name)?);
        Ok(Self {
            pending,
            busy: Arc::new(AtomicBool::new(false)),
        })
    }
    /// Accepts one connection without blocking the Host health loop.
    pub fn accept(&mut self) -> io::Result<AdminStream> {
        if self.busy.load(Ordering::Acquire) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        // SAFETY: valid synchronous pipe handle, no overlapped structure required.
        let result = unsafe { ConnectNamedPipe(self.pending.as_raw_handle(), null_mut()) };
        if result == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error().map(|e| e as u32) {
                Some(ERROR_PIPE_CONNECTED) => {}
                Some(ERROR_NO_DATA) => {
                    // SAFETY: a client closed before acceptance; reset this instance.
                    unsafe {
                        DisconnectNamedPipe(self.pending.as_raw_handle());
                    }
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Some(ERROR_PIPE_LISTENING) => return Err(io::ErrorKind::WouldBlock.into()),
                _ => return Err(error),
            }
        } else {
            // In NOWAIT mode an initial successful call can mean only listening.
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.busy.store(true, Ordering::Release);
        Ok(AdminStream {
            handle: Arc::clone(&self.pending),
            busy: Some(Arc::clone(&self.busy)),
        })
    }
}

/// Bounded length-framed named-pipe connection with delivery acknowledgement.
pub struct AdminStream {
    handle: Arc<OwnedHandle>,
    busy: Option<Arc<AtomicBool>>,
}
impl Drop for AdminStream {
    fn drop(&mut self) {
        if let Some(busy) = &self.busy {
            // SAFETY: this is the uniquely accepted server connection. Disconnect
            // releases old client handles without allocating another pipe instance.
            unsafe {
                DisconnectNamedPipe(self.handle.as_raw_handle());
                // Prime the reused NOWAIT instance so a client can connect even
                // before the next Host health-loop poll calls accept.
                ConnectNamedPipe(self.handle.as_raw_handle(), null_mut());
            }
            busy.store(false, Ordering::Release);
        }
    }
}
impl AdminStream {
    /// Connects with a deadline; does not permit server impersonation of the client.
    pub fn connect(path: &Path, timeout: Duration) -> io::Result<Self> {
        let name = pipe_name(path)?;
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: valid name; no inherited handles, no asynchronous buffers.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                let handle = owned(handle)?;
                check_owner(handle.as_raw_handle(), SE_KERNEL_OBJECT)?;
                let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
                // SAFETY: connected named-pipe handle and valid mode pointer.
                if unsafe { SetNamedPipeHandleState(handle.as_raw_handle(), &mode, null(), null()) }
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
                return Ok(Self {
                    handle: Arc::new(handle),
                    busy: None,
                });
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
                return Err(error);
            }
            pause(deadline)?;
        }
    }
    /// Sends a message and waits for receipt, bounded by one total deadline.
    pub fn send(&mut self, bytes: &[u8], timeout: Duration) -> io::Result<()> {
        if bytes.len() > super::MAX_ADMIN_MESSAGE {
            return Err(super::invalid("admin message too large"));
        }
        let deadline = Instant::now() + timeout;
        self.write_all(&(bytes.len() as u32).to_le_bytes(), deadline)?;
        self.write_all(bytes, deadline)?;
        let mut ack = [0];
        self.read_exact(&mut ack, deadline)?;
        if ack != [1] {
            return Err(super::invalid("invalid admin acknowledgement"));
        }
        Ok(())
    }
    /// Reads a bounded message, then acknowledges receipt before server closure.
    pub fn receive(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut size = [0; 4];
        self.read_exact(&mut size, deadline)?;
        let size = u32::from_le_bytes(size) as usize;
        if size > super::MAX_ADMIN_MESSAGE {
            return Err(super::invalid("admin message too large"));
        }
        let mut bytes = vec![0; size];
        self.read_exact(&mut bytes, deadline)?;
        self.write_all(&[1], deadline)?;
        Ok(bytes)
    }
    fn read_exact(&self, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut count = 0;
            // SAFETY: synchronous NOWAIT handle, valid exclusive byte buffer.
            let success = unsafe {
                ReadFile(
                    self.handle.as_raw_handle(),
                    bytes.as_mut_ptr(),
                    bytes.len() as u32,
                    &mut count,
                    null_mut(),
                )
            };
            if success == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_NO_DATA as i32) {
                    return Err(error);
                }
                pause(deadline)?;
            } else if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            } else {
                bytes = &mut bytes[count as usize..];
            }
        }
        Ok(())
    }
    fn write_all(&self, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut count = 0;
            // SAFETY: synchronous NOWAIT handle, byte slice remains live until return.
            if unsafe {
                WriteFile(
                    self.handle.as_raw_handle(),
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut count,
                    null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                pause(deadline)?;
            } else {
                bytes = &bytes[count as usize..];
            }
        }
        Ok(())
    }
}

fn pause(deadline: Instant) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    thread::sleep(remaining.min(Duration::from_millis(5)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_private(handle: HANDLE, kind: SE_OBJECT_TYPE) -> io::Result<()> {
        let mut security = null_mut();
        let mut acl = null_mut();
        // SAFETY: caller supplies a live handle; allocation and ACE pointers are
        // kept live until the end of the assertions and freed exactly once.
        unsafe {
            let error = GetSecurityInfo(
                handle,
                kind,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut acl,
                null_mut(),
                &mut security,
            );
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            let _allocation = LocalMemory(security);
            assert!(!acl.is_null());
            assert_eq!((*acl).AceCount, 1);
            let mut control = 0;
            let mut revision = 0;
            if GetSecurityDescriptorControl(security, &mut control, &mut revision) == 0 {
                return Err(io::Error::last_os_error());
            }
            assert_ne!(control & SE_DACL_PROTECTED, 0);
            let mut ace = null_mut();
            if GetAce(acl, 0, &mut ace) == 0 {
                return Err(io::Error::last_os_error());
            }
            let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            assert_eq!(ace.Header.AceType, 0); // ACCESS_ALLOWED_ACE_TYPE
            assert_eq!(ace.Mask, FILE_ALL_ACCESS);
            let expected = descriptor()?;
            let mut sid = null_mut();
            let mut defaulted = 0;
            if GetSecurityDescriptorOwner(expected.0, &mut sid, &mut defaulted) == 0 {
                return Err(io::Error::last_os_error());
            }
            assert_ne!(
                EqualSid(std::ptr::addr_of!(ace.SidStart).cast_mut().cast(), sid),
                0
            );
        }
        Ok(())
    }

    #[test]
    fn actual_directory_file_and_pipe_acls_allow_only_current_user() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("ap-acl-{}", uuid::Uuid::now_v7()));
        super::super::ensure_private_dir(&root)?;
        let result = (|| {
            let directory = OpenOptions::new()
                .access_mode(READ_CONTROL)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(&root)?;
            assert_private(directory.as_raw_handle(), SE_FILE_OBJECT)?;
            let path = root.join("secret");
            super::super::atomic_write_private(&path, b"first")?;
            super::super::atomic_write_private(&path, b"replacement")?;
            let file = fs::File::open(path)?;
            assert_private(file.as_raw_handle(), SE_FILE_OBJECT)?;
            let listener = AdminListener::bind(&root.join("admin.sock"))?;
            assert_private(listener.pending.as_raw_handle(), SE_KERNEL_OBJECT)?;
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn oversized_incoming_length_is_rejected_before_body_allocation() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("ap-frame-{}", uuid::Uuid::now_v7()));
        super::super::ensure_private_dir(&root)?;
        let result = (|| {
            let path = root.join("admin.sock");
            let mut listener = AdminListener::bind(&path)?;
            let client = AdminStream::connect(&path, Duration::from_secs(1))?;
            let mut server = listener.accept()?;
            client.write_all(
                &u32::MAX.to_le_bytes(),
                Instant::now() + Duration::from_secs(1),
            )?;
            assert_eq!(
                server
                    .receive(Duration::from_secs(1))
                    .err()
                    .map(|e| e.kind()),
                Some(io::ErrorKind::InvalidData)
            );
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }
}
