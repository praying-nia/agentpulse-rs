//! Native private-storage and administrative IPC contracts.
use agentpulse_platform::{
    AdminListener, AdminStream, MAX_ADMIN_MESSAGE, atomic_write_private, ensure_private_dir,
};
use std::{
    fs, io,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!("ap-{}", uuid::Uuid::now_v7()));
        ensure_private_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn accept(listener: &mut AdminListener) -> io::Result<AdminStream> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok(stream) => return Ok(stream),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) => return Err(error),
        }
    }
}

#[test]
fn private_file_replaces_existing_content_without_temporary_leftovers() -> io::Result<()> {
    let root = Directory::new()?;
    let directory = root.0.join("中文 空格");
    let path = directory.join("credentials.json");
    atomic_write_private(&path, b"old")?;
    atomic_write_private(&path, b"new")?;
    assert_eq!(fs::read(&path)?, b"new");
    assert_eq!(fs::read_dir(directory)?.count(), 1);
    Ok(())
}

#[test]
fn failed_replace_preserves_destination_and_cleans_temporary_file() -> io::Result<()> {
    let root = Directory::new()?;
    let path = root.0.join("destination");
    fs::create_dir(&path)?;
    fs::write(path.join("keep"), b"old")?;
    assert!(atomic_write_private(&path, b"new").is_err());
    assert_eq!(fs::read(path.join("keep"))?, b"old");
    assert_eq!(fs::read_dir(&root.0)?.count(), 1);
    Ok(())
}

#[test]
fn admin_roundtrip_repeated_connections_and_restart() -> io::Result<()> {
    let root = Directory::new()?;
    let path = root.0.join("admin.sock");
    let mut listener = AdminListener::bind(&path)?;
    assert!(AdminListener::bind(&path).is_err());
    // Unix's occupied-endpoint probe itself opens and closes a connection.
    #[cfg(unix)]
    {
        let _ = accept(&mut listener)?;
    }
    assert_eq!(
        listener.accept().err().map(|e| e.kind()),
        Some(io::ErrorKind::WouldBlock)
    );
    let server = thread::spawn(move || -> io::Result<()> {
        for _ in 0..3 {
            let mut stream = accept(&mut listener)?;
            let message = stream.receive(Duration::from_secs(2))?;
            stream.send(&message, Duration::from_secs(2))?;
        }
        Ok(())
    });
    let mut lingering_clients = Vec::new();
    for length in [0, 512, MAX_ADMIN_MESSAGE] {
        let mut client = AdminStream::connect(&path, Duration::from_secs(2))?;
        let message = vec![42; length];
        client.send(&message, Duration::from_secs(2))?;
        assert_eq!(client.receive(Duration::from_secs(2))?, message);
        lingering_clients.push(client);
    }
    server
        .join()
        .map_err(|_| io::Error::other("server panicked"))??;
    drop(lingering_clients);
    let _restarted = AdminListener::bind(&path)?;
    Ok(())
}

#[test]
fn stalled_client_is_bounded_and_next_client_can_connect() -> io::Result<()> {
    let root = Directory::new()?;
    let path = root.0.join("admin.sock");
    let mut listener = AdminListener::bind(&path)?;
    let client = AdminStream::connect(&path, Duration::from_secs(1))?;
    let mut stream = accept(&mut listener)?;
    let started = Instant::now();
    let error = stream
        .receive(Duration::from_millis(50))
        .err()
        .ok_or_else(|| io::Error::other("idle client unexpectedly succeeded"))?;
    assert!(matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(stream);
    drop(client);
    let _next = AdminStream::connect(&path, Duration::from_secs(1))?;
    let _accepted = accept(&mut listener)?;
    Ok(())
}

#[test]
fn oversized_outgoing_message_is_rejected_without_waiting() -> io::Result<()> {
    let root = Directory::new()?;
    let path = root.0.join("admin.sock");
    let _listener = AdminListener::bind(&path)?;
    let mut client = AdminStream::connect(&path, Duration::from_secs(1))?;
    let error = client.send(&vec![0; MAX_ADMIN_MESSAGE + 1], Duration::from_secs(1));
    assert_eq!(
        error.err().map(|e| e.kind()),
        Some(io::ErrorKind::InvalidData)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_files_are_private_and_symlinks_are_rejected() -> io::Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = Directory::new()?;
    let path = root.0.join("credentials");
    atomic_write_private(&path, b"secret")?;
    assert_eq!(fs::metadata(&root.0)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    let link = root.0.join("link");
    symlink(&path, &link)?;
    assert!(atomic_write_private(&link, b"overwrite").is_err());
    assert_eq!(fs::read(path)?, b"secret");
    Ok(())
}

#[cfg(windows)]
#[test]
fn sharing_violation_keeps_old_file_and_cleans_written_temporary() -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let root = Directory::new()?;
    let path = root.0.join("credentials");
    atomic_write_private(&path, b"old")?;
    let reader = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)?;
    assert!(atomic_write_private(&path, b"new").is_err());
    assert_eq!(fs::read(&path)?, b"old");
    assert_eq!(fs::read_dir(&root.0)?.count(), 1);
    drop(reader);
    atomic_write_private(&path, b"new")?;
    assert_eq!(fs::read(&path)?, b"new");
    Ok(())
}
