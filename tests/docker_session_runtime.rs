#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn docker_session_script() -> PathBuf {
    repo_root().join("docker-session.sh")
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("script should be writable");

    let mut permissions = fs::metadata(path)
        .expect("script metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script should be executable");
}

fn wait_for_path(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    panic!("expected {} to exist within {timeout:?}", path.display());
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            return status;
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("expected child process to exit within {timeout:?}");
        }

        thread::sleep(Duration::from_millis(20));
    }
}

fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .expect("kill command should run");
    assert!(status.success(), "expected kill {signal} {pid} to succeed");
}

fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn read_pid(path: &Path) -> u32 {
    fs::read_to_string(path)
        .expect("pid file should be readable")
        .trim()
        .parse()
        .expect("pid file should contain a valid pid")
}

struct PidGuard(Option<u32>);

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take()
            && process_exists(pid)
        {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[test]
fn docker_session_forwards_term_to_post_self_improve_rebuild() {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let fake_bin_dir = temp_dir.path().join("bin");
    let state_dir = temp_dir.path().join("state");
    fs::create_dir(&fake_bin_dir).expect("fake bin dir should be created");
    fs::create_dir(&state_dir).expect("state dir should be created");

    write_executable(
        &fake_bin_dir.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail

state_dir="${FAKE_DOCKER_STATE_DIR:?}"

case "${1:-} ${2:-}" in
  "compose run")
    printf '%s\n' "$$" > "$state_dir/run.pid"
    exit 0
    ;;
  "compose build")
    printf '%s\n' "$$" > "$state_dir/build.pid"
    trap 'printf term > "$state_dir/build.term"; sleep 1; printf exited > "$state_dir/build.exited"; exit 143' TERM INT
    : > "$state_dir/build.started"
    while :; do :; done
    ;;
  *)
    printf '%s\n' "$*" > "$state_dir/unexpected"
    exit 99
    ;;
esac
"#,
    );

    let mut child = Command::new("bash")
        .arg(docker_session_script())
        .arg("self-improve.sh")
        .current_dir(repo_root())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("FAKE_DOCKER_STATE_DIR", &state_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("docker-session.sh should spawn");

    wait_for_path(&state_dir.join("build.started"), Duration::from_secs(5));

    send_signal(child.id(), "TERM");

    let exit_status = wait_for_child_exit(&mut child, Duration::from_secs(5));
    assert_eq!(
        exit_status.code(),
        Some(143),
        "expected docker-session.sh to exit 143 after TERM during rebuild"
    );

    let build_pid = read_pid(&state_dir.join("build.pid"));
    let _build_guard = PidGuard(Some(build_pid));

    assert!(
        state_dir.join("build.term").exists(),
        "expected fake docker build child to receive TERM before wrapper exit"
    );
    assert!(
        state_dir.join("build.exited").exists(),
        "expected wrapper to wait for the delayed build child to finish before exiting"
    );
    assert!(
        !process_exists(build_pid),
        "expected fake docker build child {build_pid} to be gone when wrapper exits"
    );
    assert!(
        !state_dir.join("unexpected").exists(),
        "expected fake docker to see only compose run/build invocations"
    );
}
