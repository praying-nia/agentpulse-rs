# agentpulse-platform

Private local storage and administrative IPC shared by Host and pairing.
Session/Event storage remains in memory; this crate introduces no database.

## Storage boundary

Use these functions only on dedicated, application-owned directories. Unix
directories/files use 0700/0600. Windows uses a protected DACL with one full-access
ACE for the current process user SID; existing objects must have that owner.
Windows security updates use handles opened without following reparse points.
Final-component symlinks/reparse points are rejected. This is not a sandbox
against another process running as the same user or an administrator.

`atomic_write_private` creates a unique temporary file in the private destination
directory, protects it before writing, syncs and closes it, then renames it over
the destination. Unix additionally syncs the parent directory. Windows does not
try to open and fsync a directory as an ordinary file. If writing or replacement
fails before replacement (including Windows sharing violations), the old content
is retained and the temporary file is removed. A Unix directory-sync failure
after replacement returns an error although the new content is already visible.
A process crash can leave an orphan temporary
file; automatic recursive cleanup of unknown files is deliberately avoided.
These operations do not promise power-loss durability on every filesystem.

## Administrative IPC

The Host holds its instance lock before binding. Unix preserves the existing
JSON followed by write-half-close protocol. Windows uses a local-only named
pipe, named by the user SID and canonical data directory hash. Its protected
DACL admits only that user. The first-instance flag prevents silently binding
an occupied name. Clients check server object ownership and use identification
level SQOS to prevent server impersonation of the client.

Windows messages are a four-byte little-endian length followed by payload, then
a one-byte receipt acknowledgement (1) in the reverse direction. This avoids
discarding unread responses when the server disconnects, without an unbounded
`FlushFileBuffers` call. Each send/receive has a total deadline and a 64 KiB
payload limit. The pipe uses synchronous NOWAIT operations with bounded 5 ms
polling for this low-volume management channel, not for Provider event traffic.
The listener reuses one instance after disconnect so lingering client handles
cannot exhaust newly allocated instances. Invalid/idle clients do not stop Host.

Only Windows FFI in `src/windows.rs` and `src/process.rs` permits unsafe, with handle ownership and
buffer-lifetime safety comments. The crate otherwise denies unsafe; existing
business crates retain the workspace-wide forbid.

## Windows process trees

`ProcessJob::spawn(&mut Command)` returns a standard `Child` and an owning job
guard. Keep the guard for the entire managed runtime lifetime, including after
the original child exits. The process starts suspended and hidden, is assigned
to an unnamed, non-inheritable kill-on-close Job Object, and only then resumes.
This prevents descendants from starting before containment. Assignment or resume
failure kills and reaps the suspended child; unsupported job nesting fails closed.
The function replaces Windows creation flags while retaining other command settings.

`ProcessJob::terminate()` forcibly terminates all members, including descendants
whose parent already exited. Dropping the guard closes the job handle and also
terminates remaining members. Reap the root with `Child::wait()`; the API does not
provide an application-level graceful shutdown handshake. Windows owns job cleanup
when the owning process exits. No inherited handle keeps this private job alive.
`terminate_on_root_exit` adds a watcher that kills remaining descendants when the
root exits. Explicit guard drop also terminates the tree while the watcher owns
a duplicate job handle.

Tests launch real descendant processes and verify termination via process handles
for explicit termination, guard drop, and an already-exited root, plus failed spawn.
The Codex Provider uses this primitive for its Windows runtime and version probe.

## Verification

```text
cargo test -p agentpulse-platform -p agentpulse-pairing -p agentpulse-host --locked -- --test-threads=1
cargo clippy -p agentpulse-platform -p agentpulse-pairing -p agentpulse-host --all-targets --no-deps --locked -- -D warnings
```

The platform workflow runs these contracts on Windows and Linux. Windows tests
inspect actual directory/file/pipe DACLs, malformed framing, timeouts, pipe reuse,
sharing-violation recovery, credential rotation, and real Host CLI commands.
ACL inspection is not a substitute for a separate-user interactive acceptance
test. Provider and Host real-runtime acceptance are separate installed-Codex
tests; storage/IPC contracts alone do not certify desktop/mobile integration.
