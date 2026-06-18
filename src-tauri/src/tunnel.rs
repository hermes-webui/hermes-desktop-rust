//! SSH tunnel manager — port of TunnelManager.swift (150 lines).
//! Spawn args, probe timings, monitor cadence and teardown escalation are
//! carried over exactly (docs/03 § TunnelManager spec).

use crate::state::{AppState, TunnelStatus};
use crate::{health, windows};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// Build the exact argv the Swift app uses. Frozen by unit test.
pub fn ssh_args(user: &str, host: &str, local_port: u32, remote_port: u32) -> Vec<String> {
    vec![
        "-N".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-L".into(),
        // 127.0.0.1 on the remote side, never "localhost" (IPv6-first hosts).
        format!("{local_port}:127.0.0.1:{remote_port}"),
        format!("{user}@{host}"),
    ]
}

fn set_status(app: &AppHandle, status: TunnelStatus) {
    let state = app.state::<AppState>();
    *state.tunnel_status.lock().unwrap() = status;
    windows::on_tunnel_status_changed(app, status);
    // Mid-session death (monitor / termination handler): arm auto-recovery.
    // No-ops while the orchestrator is mid-run (`connecting` guard inside).
    if status == TunnelStatus::Disconnected {
        crate::conn::start_ssh_recovery(app);
    }
}

pub fn current_status(app: &AppHandle) -> TunnelStatus {
    *app.state::<AppState>().tunnel_status.lock().unwrap()
}

fn child_alive(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let mut guard = state.tunnel_child.lock().unwrap();
    match guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

/// Spawn ssh and block until the forward answers HTTP or the 5s deadline
/// passes (probing every 500ms, each HTTP probe capped at 1.5s — Swift
/// timings). Returns true when connected; monitor thread keeps watch after.
pub fn start(app: &AppHandle, user: &str, host: &str, local_port: u32, remote_port: u32) -> bool {
    set_status(app, TunnelStatus::Connecting);
    let state = app.state::<AppState>();
    state.stderr_tail.lock().unwrap().clear();

    let mut cmd = Command::new("ssh");
    cmd.args(ssh_args(user, host, local_port, remote_port))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // Windows: no console window flash per spawn (docs/09 § ssh.exe).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            log::error!("tunnel: failed to start ssh: {e}");
            *state.last_error_hint.lock().unwrap() =
                format!("Failed to start ssh: {e}. Is the OpenSSH client installed?");
            set_status(app, TunnelStatus::Disconnected);
            return false;
        }
    };
    log::info!("tunnel: ssh started (pid {})", child.id());

    // Tail stderr into state so connection failures can show a specific hint
    // (parity-plus over the Swift app, which pipes stderr but ignores it).
    if let Some(stderr) = child.stderr.take() {
        let app2 = app.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                log::warn!("ssh stderr: {line}");
                let state = app2.state::<AppState>();
                let mut tail = state.stderr_tail.lock().unwrap();
                tail.push(line);
                let drop_count = tail.len().saturating_sub(12);
                if drop_count > 0 {
                    tail.drain(0..drop_count);
                }
            }
        });
    }

    *state.tunnel_child.lock().unwrap() = Some(child);

    // Readiness: a local TCP connect only proves ssh holds the port — only an
    // HTTP round-trip proves the forward is usable end to end (Swift comment).
    // Use http_ready (not http_reachable) so a reverse proxy on the remote side
    // answering 502/503/504 while its upstream boots doesn't count as up — same
    // gateway-error gate as direct mode, so a tunneled restore waits for the
    // real server too (issue #28).
    let probe_url = format!("http://127.0.0.1:{local_port}/");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut connected = false;
    while Instant::now() < deadline {
        if !child_alive(app) {
            break;
        }
        if health::http_ready(&probe_url, Duration::from_millis(1500)) {
            connected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if connected {
        set_status(app, TunnelStatus::Connected);
        start_monitor(app);
    } else {
        let tail = state.stderr_tail.lock().unwrap().join("\n");
        *state.last_error_hint.lock().unwrap() = hint_from_stderr(&tail);
        set_status(app, TunnelStatus::Disconnected);
    }
    connected
}

/// Map common ssh failures to actionable hints (risk R7).
fn hint_from_stderr(tail: &str) -> String {
    let lower = tail.to_lowercase();
    if lower.contains("permission denied") {
        "SSH authentication failed (Permission denied). Key-based auth is required — add your key to ssh-agent or ~/.ssh/config.".into()
    } else if lower.contains("host key verification failed") {
        "Host key verification failed. Connect once from a terminal to inspect/accept the new host key.".into()
    } else if lower.contains("could not resolve") {
        "Could not resolve the SSH host name. Check the Host field in Preferences.".into()
    } else if lower.contains("connection refused") {
        "SSH connection refused. Check the host and that sshd is running.".into()
    } else if lower.contains("operation timed out") || lower.contains("timed out") {
        "SSH connection timed out. Check the host, your network, or VPN.".into()
    } else if tail.trim().is_empty() {
        "The tunnel connected but nothing answered on the forwarded port. Is hermes-webui running on the remote machine?".into()
    } else {
        format!("ssh reported: {}", tail.trim().lines().last().unwrap_or(""))
    }
}

/// Liveness monitor — every 10s, flags Disconnected if the child died.
/// Generation-tagged so stale monitors from earlier tunnels exit silently.
fn start_monitor(app: &AppHandle) {
    let state = app.state::<AppState>();
    let generation = state.monitor_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(10));
        let state = app.state::<AppState>();
        if state.monitor_gen.load(Ordering::SeqCst) != generation {
            return;
        }
        if !child_alive(&app) {
            log::warn!("tunnel: ssh process died — marking disconnected");
            set_status(&app, TunnelStatus::Disconnected);
            return;
        }
    });
}

/// Teardown: SIGTERM, then SIGKILL after 1s if still running (Swift escalation).
pub fn stop(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.monitor_gen.fetch_add(1, Ordering::SeqCst);
    let child = state.tunnel_child.lock().unwrap().take();
    let Some(mut child) = child else { return };
    let pid = child.id();
    log::info!("tunnel: stopping ssh (pid {pid})");
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    for _ in 0..10 {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The argv is API — freezing the Swift app's exact arguments.
    #[test]
    fn ssh_args_frozen() {
        assert_eq!(
            ssh_args("hermes", "example.com", 8787, 8787),
            vec![
                "-N",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ExitOnForwardFailure=yes",
                "-L",
                "8787:127.0.0.1:8787",
                "hermes@example.com",
            ]
        );
    }
}
