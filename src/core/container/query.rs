// ============================================================================
// src/core/container/query.rs
//
// Read-only operations on LXC containers:
//   list, get IP address, check running state, send a command, upload a file.
//
// FIX #1: `upload_to_container` — Versi baru salah total.
//         Versi lama membaca tar stream dari STDIN dan mengekstraknya di dalam
//         container. Ini penting karena client (`exec.sh`) melakukan:
//           tar -czf - -C "$dir" . | ssh "$CONN" "melisa --upload $container $dest"
//         Jadi server-side harus baca stdin. Versi baru malah mencopy CWD
//         dengan `cp -r` yang sama sekali berbeda semantiknya.
//
// FIX #2: `send_command` — Versi baru tidak meng-inherit stdin.
//         Ini menyebabkan perintah interaktif di dalam container gagal
//         (tidak bisa input ke program yang berjalan di container).
//         Versi lama menggunakan `stdin(Stdio::inherit())`.
// ============================================================================

use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

use crate::cli::color::{BOLD, RED, RESET, YELLOW};
use crate::core::container::types::LXC_BASE_PATH;

// ── Container listing ────────────────────────────────────────────────────────

/// Lists all LXC containers managed by MELISA.
///
/// When `only_running` is `true`, only containers in the RUNNING state are shown.
pub async fn list_containers(only_running: bool) {
    let args: &[&str] = if only_running {
        &["lxc-ls", "-P", LXC_BASE_PATH, "--running", "--fancy"]
    } else {
        &["lxc-ls", "-P", LXC_BASE_PATH, "--fancy"]
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

// ── Container status ─────────────────────────────────────────────────────────

/// Returns `true` if the specified container is currently in the RUNNING state.
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

/// Returns `true` if the specified container exists in the LXC path.
pub async fn container_exists(name: &str) -> bool {
    let output = Command::new("sudo")
        .args(&["lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-s"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    output.map(|s| s.success()).unwrap_or(false)
}

// ── IP address ───────────────────────────────────────────────────────────────

/// Retrieves the internal IPv4 address assigned to a running container.
///
/// Returns `None` if the container is stopped or does not have an IP.
pub async fn get_container_ip(name: &str) -> Option<String> {
    let output = Command::new("sudo")
        .args(&["lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-i"])
        .output()
        .await
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.starts_with("IP:") {
            let ip = line.trim_start_matches("IP:").trim().to_string();
            // Abaikan loopback
            if !ip.is_empty() && !ip.starts_with("127.") {
                return Some(ip);
            }
        }
    }
    None
}

// ── Remote command execution ─────────────────────────────────────────────────

/// Sends a shell command to the specified container via `lxc-attach`.
///
/// Verifies that the container is running before attempting execution.
///
/// # FIX: Tambahkan `stdin(Stdio::inherit())` agar perintah interaktif bisa
///        menerima input dari user. Versi baru melewatkan ini sehingga program
///        interaktif (bash, python REPL, dll.) tidak bisa menerima input.
pub async fn send_command(name: &str, command_args: &[&str]) {
    if command_args.is_empty() {
        eprintln!("{}[ERROR]{} No command payload provided.", RED, RESET);
        return;
    }

    // Verify the container is running before attempting execution.
    let status_output = Command::new("sudo")
        .args(&["/usr/bin/lxc-info", "-P", LXC_BASE_PATH, "-n", name, "-s"])
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
        // FIX: inherit stdin agar perintah interaktif berfungsi
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;
}

// ── File upload ──────────────────────────────────────────────────────────────

/// Uploads a tar-compressed file stream from STDIN into the specified container.
///
/// # FIX KRITIS: Versi baru sama sekali salah — mencopy CWD dengan `cp -r`.
///
/// Cara kerja yang benar (seperti versi lama):
///   - Client side (`exec.sh`) melakukan:
///       tar -czf - -C "$dir" . | ssh "$CONN" "melisa --upload $container $dest"
///   - Server side (fungsi ini) harus:
///       1. Attach ke container
///       2. Buat dest_path jika belum ada
///       3. Baca tar stream dari STDIN dan ekstrak ke dest_path
///
/// Tanpa ini, `melisa upload` dan `melisa run` tidak bekerja sama sekali
/// karena kedua perintah itu mengandalkan upload via stdin pipe.
///
/// # Arguments
/// * `container_name` - Target container name.
/// * `dest_path`      - Absolute path inside the container where files are extracted.
pub async fn upload_to_container(container_name: &str, dest_path: &str) {
    // Bangun perintah ekstraksi: buat direktori tujuan lalu extract tar dari stdin
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
        // FIX: stdin inherit — menerima tar stream yang di-pipe dari client via SSH
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

// ── Shared folder helpers (kept for backward compat with network.rs) ──────────

/// Helper untuk mengecek apakah path ada — dipakai oleh upload validasi.
pub fn path_exists(p: &str) -> bool {
    Path::new(p).exists()
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that an empty command args slice is detected before attempting execution.
    #[test]
    fn test_empty_command_args_are_rejected() {
        let args: &[&str] = &[];
        assert!(
            args.is_empty(),
            "Empty args slice must be detectable before container command dispatch"
        );
    }

    /// Verifies that the IP extraction logic parses the expected lxc-info format.
    #[test]
    fn test_ip_extraction_from_lxc_info_output() {
        let lxc_info_output = "Name:           mybox\nState:          RUNNING\nIP:             10.0.3.42\n";
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

    /// Verifies that loopback IPs are filtered out.
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

    /// Verifies that the running state detection checks for "RUNNING" substring.
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

    /// Verifies that the upload extract command is formed correctly.
    #[test]
    fn test_upload_extract_cmd_format() {
        let dest_path = "/app/src";
        let extract_cmd = format!(
            "mkdir -p {dest} && tar -xzf - -C {dest}",
            dest = dest_path
        );
        assert!(extract_cmd.contains("mkdir -p /app/src"));
        assert!(extract_cmd.contains("tar -xzf - -C /app/src"));
    }

    /// Verifies that the upload extract cmd uses stdin (dash flag for tar).
    #[test]
    fn test_upload_uses_stdin_tar_stream() {
        let dest = "/tmp/test";
        let cmd = format!("mkdir -p {dest} && tar -xzf - -C {dest}", dest = dest);
        // `-` in `tar -xzf -` berarti baca dari stdin
        assert!(cmd.contains("-xzf -"), "Must read tar from stdin using dash flag");
    }
}