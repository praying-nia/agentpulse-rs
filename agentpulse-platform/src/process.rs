//! Windows process trees assigned to a kill-on-close job before executing code.

use std::{
    io,
    mem::size_of,
    os::windows::{
        io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::{Child, Command},
    ptr,
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
        },
    },
};

/// Owns a Windows process tree. Dropping it terminates every remaining member.
/// The job handle is private and non-inheritable, including by child processes.
pub struct ProcessJob(OwnedHandle);

impl ProcessJob {
    /// Starts a hidden process, assigning it to a kill-on-close job before its
    /// initial thread runs. Descendants automatically remain in the same job.
    ///
    /// Overrides the command's Windows creation flags. Other command settings,
    /// including arguments, environment and standard streams, are preserved.
    /// Any failure after spawn terminates and reaps the suspended child.
    pub fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        // SAFETY: null security attributes create a non-inheritable handle;
        // null name creates an unnamed job owned exclusively by this call.
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a valid, uniquely owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the job is live and limits points to the stated initialized structure.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut child = command
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()?;
        let result = (|| {
            // SAFETY: both handles remain valid for the duration of the call.
            if unsafe { AssignProcessToJobObject(job.0.as_raw_handle(), child.as_raw_handle()) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            resume_initial_thread(child.id())
        })();
        if let Err(error) = result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok((child, job))
    }

    /// Watches the root process and kills remaining job members as soon as it
    /// exits. This prevents descendants from keeping runtime sockets alive after
    /// an App Server crash. Dropping this job still forcibly stops all members.
    pub fn terminate_on_root_exit(
        &self,
        child: &Child,
    ) -> io::Result<std::thread::JoinHandle<io::Result<()>>> {
        let root = child.as_handle().try_clone_to_owned()?;
        let job = self.0.try_clone()?;
        std::thread::Builder::new()
            .name("agentpulse-process-watch".to_owned())
            .spawn(move || {
                use windows_sys::Win32::{
                    Foundation::WAIT_OBJECT_0,
                    System::Threading::{INFINITE, WaitForSingleObject},
                };
                // SAFETY: both owned handles remain live until the wait and job
                // termination finish. The wait ends when the owned root exits.
                if unsafe { WaitForSingleObject(root.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: the watcher owns a valid duplicate of the job handle.
                if unsafe { TerminateJobObject(job.as_raw_handle(), 1) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })
    }

    /// Requests termination of every process in the job, including descendants
    /// whose original parent has already exited. Call `Child::wait` to reap it.
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: this object owns a valid job handle.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for ProcessJob {
    fn drop(&mut self) {
        // The watcher may hold a duplicate handle, so do not rely solely on
        // last-handle close to stop the root and release the watcher.
        let _ = self.terminate();
    }
}

fn resume_initial_thread(process_id: u32) -> io::Result<()> {
    // SAFETY: snapshot creation requires no input pointers.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful snapshot creation transfers ownership to this scope.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: snapshot is live and entry is writable with its size initialized.
    let mut available = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while available != 0 {
        if entry.th32OwnerProcessID == process_id {
            // SAFETY: thread id was obtained from the system snapshot. Access
            // is minimal and the handle is explicitly non-inheritable.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: OpenThread returned a valid uniquely owned handle.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // SAFETY: this is the initial thread of our still-suspended child.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: snapshot and entry remain valid throughout enumeration.
        available = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "suspended child initial thread not found",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{OpenProcess, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject},
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // Runs only as a subprocess of the tests below. No shell or installed
    // executable is needed to exercise real descendant process inheritance.
    #[test]
    #[ignore = "subprocess fixture for Windows job tests"]
    fn job_child() -> TestResult {
        let Some(marker) = env::var_os("AGENTPULSE_JOB_MARKER") else {
            return Ok(());
        };
        if env::var_os("AGENTPULSE_JOB_LEAF").is_some() {
            fs::write(marker, std::process::id().to_string())?;
        } else {
            let _child = helper()?.env("AGENTPULSE_JOB_LEAF", "1").spawn()?;
            if env::var_os("AGENTPULSE_JOB_PARENT_EXIT").is_some() {
                return Ok(());
            }
        }
        thread::sleep(Duration::from_secs(30));
        Ok(())
    }

    fn helper() -> io::Result<Command> {
        let mut command = Command::new(env::current_exe()?);
        command
            .args([
                "--exact",
                "process::tests::job_child",
                "--ignored",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(command)
    }

    fn verify_tree_cleanup(drop_job: bool, parent_exits: bool, watch_root: bool) -> TestResult {
        let marker = env::temp_dir().join(format!("agentpulse-job-{}.pid", uuid::Uuid::now_v7()));
        let mut command = helper()?;
        command.env("AGENTPULSE_JOB_MARKER", &marker);
        if parent_exits {
            command.env("AGENTPULSE_JOB_PARENT_EXIT", "1");
        }
        let (mut child, job) = ProcessJob::spawn(&mut command)?;
        let result = (|| -> TestResult {
            let deadline = Instant::now() + Duration::from_secs(10);
            let descendant_id = loop {
                if let Ok(text) = fs::read_to_string(&marker)
                    && let Ok(id) = text.parse::<u32>()
                {
                    break id;
                }
                if Instant::now() >= deadline {
                    return Err("descendant did not start".into());
                }
                thread::sleep(Duration::from_millis(10));
            };
            // SAFETY: requests only synchronization access to the fixture PID.
            let descendant = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, descendant_id) };
            if descendant.is_null() {
                return Err(io::Error::last_os_error().into());
            }
            // SAFETY: OpenProcess returned a unique valid handle.
            let descendant = unsafe { OwnedHandle::from_raw_handle(descendant) };
            let watcher = if watch_root {
                Some(job.terminate_on_root_exit(&child)?)
            } else {
                None
            };
            if parent_exits {
                while child.try_wait()?.is_none() {
                    if Instant::now() >= deadline {
                        return Err("parent did not exit".into());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
            if let Some(watcher) = watcher {
                // SAFETY: the descendant handle remains owned by this test.
                assert_eq!(
                    unsafe { WaitForSingleObject(descendant.as_raw_handle(), 5000) },
                    WAIT_OBJECT_0
                );
                watcher.join().map_err(|_| "root watcher panicked")??;
            }
            if !drop_job {
                job.terminate()?;
            }
            drop(job);
            // SAFETY: descendant handle is live. A bounded wait verifies actual
            // process termination even when its original parent already exited.
            assert_eq!(
                unsafe { WaitForSingleObject(descendant.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait()?.is_none() {
                if Instant::now() >= deadline {
                    return Err("root remained alive".into());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })();
        // On error, the closure drops the job and therefore kills the tree.
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(marker);
        result
    }

    #[test]
    fn terminate_kills_root_and_descendant() -> TestResult {
        verify_tree_cleanup(false, false, false)
    }

    #[test]
    fn dropping_job_kills_root_and_descendant() -> TestResult {
        verify_tree_cleanup(true, false, false)
    }

    #[test]
    fn dropping_job_kills_descendant_after_parent_exit() -> TestResult {
        verify_tree_cleanup(true, true, false)
    }

    #[test]
    fn root_exit_watcher_kills_orphans_while_job_guard_is_still_alive() -> TestResult {
        verify_tree_cleanup(true, true, true)
    }

    #[test]
    fn failed_spawn_returns_error() {
        let mut command =
            Command::new(env::temp_dir().join(format!("missing-{}.exe", uuid::Uuid::now_v7())));
        assert!(ProcessJob::spawn(&mut command).is_err());
    }
}
