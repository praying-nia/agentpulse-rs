//! Executes local Host commands without a Codex runtime or phone.
use std::{
    fs, io,
    path::PathBuf,
    process::{Command, Output},
};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(directory: &Directory, args: &[&str]) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_agentpulse"))
        .arg("--data-dir")
        .arg(&directory.0)
        .args(args)
        .output()
}

#[test]
fn local_cli_initializes_lists_threads_and_reports_stopped() -> io::Result<()> {
    let directory =
        Directory(std::env::temp_dir().join(format!("ap-cli-中文 空格-{}", uuid::Uuid::now_v7())));
    let initialized = run(&directory, &["init", "--name", "Windows Test"])?;
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let id = uuid::Uuid::now_v7().to_string();
    assert!(run(&directory, &["threads", "add", &id])?.status.success());
    let list = run(&directory, &["threads", "list"])?;
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains(&id));
    assert!(run(&directory, &["devices", "list"])?.status.success());
    let status = run(&directory, &["status"])?;
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("stopped"));
    assert!(!run(&directory, &["stop"])?.status.success());
    assert!(
        run(
            &directory,
            &["credentials", "rotate", "--confirm-revoke-all"]
        )?
        .status
        .success()
    );
    Ok(())
}

#[cfg(windows)]
#[test]
#[ignore = "requires installed Codex, normal user home and a private LAN address"]
fn windows_serve_status_stop_and_restart_with_real_codex() -> Result<(), Box<dyn std::error::Error>>
{
    use std::{
        process::Stdio,
        thread,
        time::{Duration, Instant},
    };
    let bind = if_addrs::get_if_addrs()?
        .into_iter()
        .find_map(|interface| match interface.ip() {
            std::net::IpAddr::V4(ip) if ip.is_private() && !ip.is_loopback() => Some(ip),
            _ => None,
        })
        .ok_or("no private IPv4 address available for Host acceptance")?;
    let directory =
        Directory(std::env::temp_dir().join(format!("ap-serve-{}", uuid::Uuid::now_v7())));
    let initialized = run(
        &directory,
        &["init", "--name", "Windows Runtime Acceptance"],
    )?;
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    for _ in 0..2 {
        let log_path = directory.0.join("acceptance-output.log");
        let log = fs::File::create(&log_path)?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_agentpulse"));
        command
            .arg("--data-dir")
            .arg(&directory.0)
            .args([
                "serve",
                "--discover-threads",
                "--bind",
                &bind.to_string(),
                "--port",
                "0",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let (mut child, job) = agentpulse_platform::ProcessJob::spawn(&mut command)?;
        let operation = (|| -> Result<(), Box<dyn std::error::Error>> {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if let Some(exit) = child.try_wait()? {
                    return Err(
                        format!("Host exited {exit}: {}", fs::read_to_string(&log_path)?).into(),
                    );
                }
                let status = run(&directory, &["status"])?;
                let text = String::from_utf8_lossy(&status.stdout);
                if text.contains("Provider: running") {
                    assert!(text.contains("Codex remote: ws://127.0.0.1:"));
                    assert!(text.contains("codex.exe"));
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "Host readiness timed out: {}",
                        fs::read_to_string(&log_path)?
                    )
                    .into());
                }
                thread::sleep(Duration::from_millis(100));
            }
            let started = Instant::now();
            assert!(run(&directory, &["stop"])?.status.success());
            loop {
                if let Some(exit) = child.try_wait()? {
                    assert!(exit.success(), "{}", fs::read_to_string(&log_path)?);
                    break;
                }
                if started.elapsed() > Duration::from_secs(8) {
                    return Err("Host failed to stop promptly".into());
                }
                thread::sleep(Duration::from_millis(50));
            }
            assert!(
                String::from_utf8_lossy(&run(&directory, &["status"])?.stdout).contains("stopped")
            );
            Ok(())
        })();
        drop(job);
        let _ = child.wait();
        operation?;
    }
    Ok(())
}
