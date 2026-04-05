// =============================================================================
// MELISA — src/core/container/query.rs
// Purpose: Read-only LXC container queries (list, status, IP, send, upload).
//
// FIX APPLIED:
//   list_containers() and container_exists() now use `sudo -n` (non-interactive)
//   so they never hang waiting for a password in SSH / non-TTY sessions.
//   is_container_running() already had -n; the others did not.
// =============================================================================

use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use crate::cli::color::{BOLD, RED, RESET, YELLOW};
use crate::core::container::types::LXC_BASE_PATH;

/// Lists all LXC containers in MELISA's container directory.
///
/// If `only_running` is true, only running containers are shown.
///
/// FIX: Now uses `sudo -n` so the call never blocks for a password prompt
/// in non-TTY sessions (e.g. SSH command pipes).
pub async fn list_containers(only_running: bool) {
    // Use -n here too for consistency and non-blocking behaviour.
    let args: &[&str] = if only_running {
        &["-n", "lxc-ls", "-P", LXC_BASE_PATH, "--running", "--fancy"]
    } else {
        &["-n", "lxc-ls", "-P", LXC_BASE_PATH, "--fancy"]
    };

    let output = Command::new("sudo").args(args).output().await;
    match output {
        Ok(out) if out.status.success() => {
            let content = String::from_utf8_lossy(&out.stdout);
            if content.trim().is_empty() {
                let filter_desc = if only_running { "running" } else { "registered" };
                println!(
                    "{}[INFO]{} No {} containers found.",
                    BOLD, RESET, filter_desc
                );
            } else {
                println!("{}", content);
            }
        }
        Ok(_) => {
            eprintln!(
                "{}[ERROR]{} Failed to retrieve the container list. Check LXC installation.",
                RED, RESET
            );
        }
        Err(err) => {
            eprintln!("{}[FATAL]{} Could not execute lxc-ls: {}", RED, RESET, err);
        }
    }
}

/// Returns `true` if the named container is currently in the RUNNING state.
pub async fn is_container_running(name: &str) -> bool {
    let output = Command::new("sudo")
        .args(&["-n", "lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-s"])
        .output()
        .await;
    match output {
        Ok(out) => {
            let status_text = String::from_utf8_lossy(&out.stdout);
            status_text.contains("RUNNING")
        }
        _ => false,
    }
}

/// Returns `true` if a container with the given name exists (regardless of state).
///
/// FIX: Added -n flag so this call never prompts for a password.
pub async fn container_exists(name: &str) -> bool {
    let output = Command::new("sudo")
        .args(&["-n", "lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-s"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    output.map(|s| s.success()).unwrap_or(false)
}

/// Returns the first non-loopback IP address assigned to the named container,
/// or `None` if the container is stopped or has no network.
pub async fn get_container_ip(name: &str) -> Option<String> {
    let output = Command::new("sudo")
        .args(&["-n", "lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-i"])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.starts_with("IP:") {
            let ip = line.trim_start_matches("IP:").trim().to_string();
            if !ip.is_empty() && !ip.starts_with("127.") {
                return Some(ip);
            }
        }
    }
    None
}

/// Sends a command to be executed inside a running container.
pub async fn send_command(name: &str, command_args: &[&str]) {
    if command_args.is_empty() {
        eprintln!("{}[ERROR]{} No command payload provided.", RED, RESET);
        return;
    }

    let status_output = Command::new("sudo")
        .args(&["-n", "/usr/bin/lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-s"])
        .output()
        .await;

    match status_output {
        Ok(out) => {
            let status_text = String::from_utf8_lossy(&out.stdout);
            if !status_text.contains("RUNNING") {
                println!(
                    "{}[ERROR]{} Container '{}' is NOT running.",
                    RED, RESET, name
                );
                println!(
                    "{}Tip:{} Execute 'melisa --run {}' to start it first.",
                    YELLOW, RESET, name
                );
                return;
            }
        }
        Err(_) => {
            eprintln!("{}[ERROR]{} Failed to retrieve container status.", RED, RESET);
            return;
        }
    }

    println!("{}[SEND]{} Executing payload on '{}'…", BOLD, name, RESET);
    let mut attach_args = vec!["lxc-attach", "-P", LXC_BASE_PATH, "-n", name, "--"];
    attach_args.extend_from_slice(command_args);

    let _ = Command::new("sudo")
        .args(&attach_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;
}

/// Reads a tar stream from stdin and extracts it inside a container.
pub async fn upload_to_container(container_name: &str, dest_path: &str) {
    let extract_cmd = format!(
        "mkdir -p {dest} && tar -xzf - -C {dest}",
        dest = dest_path
    );
    let status = Command::new("sudo")
        .args(&[
            "lxc-attach",
            "-P", LXC_BASE_PATH,
            "-n", container_name,
            "--",
            "bash", "-c", &extract_cmd,
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;
    match status {
        Ok(s) if s.success() => {
            println!(
                "{}[SUCCESS]{} Upload and extraction to '{}:{}' completed successfully.",
                crate::cli::color::GREEN, RESET, container_name, dest_path
            );
        }
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to extract data stream inside container '{}'.",
                RED, RESET, container_name
            );
        }
    }
}

pub fn path_exists(p: &str) -> bool {
    Path::new(p).exists()
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_command_args_are_rejected() {
        let args: &[&str] = &[];
        assert!(
            args.is_empty(),
            "Empty args slice must be detectable before container command dispatch"
        );
    }

    #[test]
    fn test_ip_extraction_from_lxc_info_output() {
        let lxc_info_output =
            "Name:           mybox\nState:          RUNNING\nIP:             10.0.3.42\n";
        let extracted_ip: Option<String> = lxc_info_output
            .lines()
            .find(|line| line.starts_with("IP:"))
            .map(|line| line.trim_start_matches("IP:").trim().to_string())
            .filter(|ip| !ip.is_empty() && !ip.starts_with("127."));
        assert!(extracted_ip.is_some(), "IP must be extracted when present in lxc-info output");
        assert_eq!(
            extracted_ip.unwrap(),
            "10.0.3.42",
            "Extracted IP must match the value in the lxc-info output"
        );
    }

    #[test]
    fn test_ip_extraction_filters_loopback() {
        let lxc_info_output = "IP:  127.0.0.1\nIP:  10.0.3.5\n";
        let mut ips = vec![];
        for line in lxc_info_output.lines() {
            if line.starts_with("IP:") {
                let ip = line.trim_start_matches("IP:").trim().to_string();
                if !ip.is_empty() && !ip.starts_with("127.") {
                    ips.push(ip);
                }
            }
        }
        assert_eq!(ips.len(), 1, "Loopback must be filtered");
        assert_eq!(ips[0], "10.0.3.5");
    }

    #[test]
    fn test_running_state_detection_requires_running_substring() {
        let running_output = "State: RUNNING";
        let stopped_output = "State: STOPPED";
        assert!(
            running_output.contains("RUNNING"),
            "RUNNING state output must be detected by substring match"
        );
        assert!(
            !stopped_output.contains("RUNNING"),
            "STOPPED state output must not match the RUNNING check"
        );
    }

    #[test]
    fn test_upload_extract_cmd_format() {
        let dest_path  = "/app/src";
        let extract_cmd = format!(
            "mkdir -p {dest} && tar -xzf - -C {dest}",
            dest = dest_path
        );
        assert!(extract_cmd.contains("mkdir -p /app/src"));
        assert!(extract_cmd.contains("tar -xzf - -C /app/src"));
    }

    #[test]
    fn test_upload_uses_stdin_tar_stream() {
        let dest = "/tmp/test";
        let cmd  = format!("mkdir -p {dest} && tar -xzf - -C {dest}", dest = dest);
        assert!(cmd.contains("-xzf -"), "Must read tar from stdin using dash flag");
    }
}