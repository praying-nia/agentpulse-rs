use std::{
    fs,
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Nonblocking private Unix-domain administrative listener.
pub struct AdminListener {
    listener: UnixListener,
    path: PathBuf,
}
impl AdminListener {
    /// Binds the private endpoint. Caller must hold its Host instance lock.
    pub fn bind(path: &Path) -> io::Result<Self> {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() {
                return Err(super::invalid("admin endpoint is not a socket"));
            }
            match AdminStream::connect(path, Duration::from_millis(200)) {
                Ok(_) => return Err(io::ErrorKind::AddrInUse.into()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) => {}
                Err(error) => return Err(error),
            }
            fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        let result = Self {
            listener,
            path: path.to_owned(),
        };
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        result.listener.set_nonblocking(true)?;
        Ok(result)
    }
    /// Accepts one client or returns WouldBlock.
    pub fn accept(&mut self) -> io::Result<AdminStream> {
        self.listener
            .accept()
            .map(|(stream, _)| AdminStream(stream))
    }
}
impl Drop for AdminListener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// One request/response connection, compatible with the original Unix protocol.
pub struct AdminStream(UnixStream);
impl AdminStream {
    /// Connects to the private Host endpoint.
    pub fn connect(path: &Path, timeout: Duration) -> io::Result<Self> {
        let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
        socket.connect_timeout(&socket2::SockAddr::unix(path)?, timeout)?;
        let descriptor: std::os::fd::OwnedFd = socket.into();
        Ok(Self(descriptor.into()))
    }
    /// Sends one bounded message and half-closes the write side.
    pub fn send(&mut self, bytes: &[u8], timeout: Duration) -> io::Result<()> {
        if bytes.len() > super::MAX_ADMIN_MESSAGE {
            return Err(super::invalid("admin message too large"));
        }
        let deadline = Instant::now() + timeout;
        let mut bytes = bytes;
        while !bytes.is_empty() {
            self.0.set_write_timeout(Some(remaining(deadline)?))?;
            match self.0.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        self.0.shutdown(Shutdown::Write)
    }
    /// Receives one bounded message terminated by EOF.
    pub fn receive(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            self.0.set_read_timeout(Some(remaining(deadline)?))?;
            match self.0.read(&mut buffer) {
                Ok(0) => return Ok(bytes),
                Ok(count) => {
                    if bytes.len() + count > super::MAX_ADMIN_MESSAGE {
                        return Err(super::invalid("admin message too large"));
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(remaining)
    }
}
