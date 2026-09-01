//! Fixed-buffer opaque duplex tunnel.

use std::{
    io::{self, Read, Write},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::{RelayError, RelayTunnelStats};

const BUFFER_CAPACITY: usize = 64 * 1024;
const CHUNK_BYTES: usize = 8 * 1024;

pub(crate) fn pump<L: Read + Write, R: Read + Write>(
    left: &mut L,
    right: &mut R,
    stop: &AtomicBool,
    idle_timeout: Duration,
    stalled_timeout: Duration,
) -> Result<RelayTunnelStats, RelayError> {
    let mut left_to_right = Vec::with_capacity(BUFFER_CAPACITY);
    let mut right_to_left = Vec::with_capacity(BUFFER_CAPACITY);
    let mut temporary = [0_u8; CHUNK_BYTES];
    let started = Instant::now();
    let mut last_activity = started;
    let mut left_stalled_since = None;
    let mut right_stalled_since = None;
    let mut to_host_bytes = 0_u64;
    let mut to_client_bytes = 0_u64;

    while !stop.load(Ordering::Acquire) {
        let mut progressed = false;
        if left_to_right.len() < BUFFER_CAPACITY {
            let available = (BUFFER_CAPACITY - left_to_right.len()).min(CHUNK_BYTES);
            match left.read(&mut temporary[..available]) {
                Ok(0) => break,
                Ok(count) => {
                    left_to_right.extend_from_slice(&temporary[..count]);
                    to_client_bytes = to_client_bytes.saturating_add(count as u64);
                    progressed = true;
                }
                Err(error) if retryable(&error) => {}
                Err(source) => return Err(RelayError::io("read tunnel Host side", source)),
            }
        }
        if right_to_left.len() < BUFFER_CAPACITY {
            let available = (BUFFER_CAPACITY - right_to_left.len()).min(CHUNK_BYTES);
            match right.read(&mut temporary[..available]) {
                Ok(0) => break,
                Ok(count) => {
                    right_to_left.extend_from_slice(&temporary[..count]);
                    to_host_bytes = to_host_bytes.saturating_add(count as u64);
                    progressed = true;
                }
                Err(error) if retryable(&error) => {}
                Err(source) => return Err(RelayError::io("read tunnel Client side", source)),
            }
        }
        if !left_to_right.is_empty() {
            match right.write(&left_to_right) {
                Ok(0) => {
                    return Err(RelayError::io(
                        "write tunnel Client side",
                        io::Error::new(io::ErrorKind::WriteZero, "zero-byte tunnel write"),
                    ));
                }
                Ok(count) => {
                    left_to_right.drain(..count);
                    progressed = true;
                    left_stalled_since = None;
                }
                Err(error) if retryable(&error) => {
                    left_stalled_since.get_or_insert_with(Instant::now);
                }
                Err(source) => return Err(RelayError::io("write tunnel Client side", source)),
            }
        } else {
            left_stalled_since = None;
        }
        if !right_to_left.is_empty() {
            match left.write(&right_to_left) {
                Ok(0) => {
                    return Err(RelayError::io(
                        "write tunnel Host side",
                        io::Error::new(io::ErrorKind::WriteZero, "zero-byte tunnel write"),
                    ));
                }
                Ok(count) => {
                    right_to_left.drain(..count);
                    progressed = true;
                    right_stalled_since = None;
                }
                Err(error) if retryable(&error) => {
                    right_stalled_since.get_or_insert_with(Instant::now);
                }
                Err(source) => return Err(RelayError::io("write tunnel Host side", source)),
            }
        } else {
            right_stalled_since = None;
        }

        let now = Instant::now();
        if progressed {
            last_activity = now;
        } else {
            thread::sleep(Duration::from_millis(5));
        }
        if now.duration_since(last_activity) >= idle_timeout {
            return Err(RelayError::Timeout {
                operation: "waiting for tunneled traffic",
            });
        }
        if left_stalled_since.is_some_and(|since| now.duration_since(since) >= stalled_timeout)
            || right_stalled_since.is_some_and(|since| now.duration_since(since) >= stalled_timeout)
        {
            return Err(RelayError::Timeout {
                operation: "draining a slow tunnel peer",
            });
        }
    }

    Ok(RelayTunnelStats {
        to_host_bytes,
        to_client_bytes,
        elapsed: started.elapsed(),
    })
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
