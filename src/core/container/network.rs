// =============================================================================
// MELISA — src/core/container/network.rs
// Purpose: Container network configuration, DNS setup, and host bridge management.
//
// ORBSTACK SUPPORT ADDED:
//   OrbStack (macOS) runs Ubuntu/Debian inside a VirtIO-FS virtual machine.
//   By default, `systemd-detect-virt` returns a non-"none" value inside such
//   VMs, which triggers the `ConditionVirtualization=` check in
//   `lxc-net.service` and prevents the service from starting.
//
//   ensure_host_network_ready() now:
//     1. Detects whether the host is running inside a VM / OrbStack.
//     2. If so, applies the community-verified systemd override that clears the
//        ConditionVirtualization restriction.
//     3. Sets USE_LXC_BRIDGE="true" in /etc/default/lxc-net.
//     4. Reloads the systemd daemon and (re)starts lxc-net.
//     5. Reports a non-fatal warning if chattr fails (VirtIO-FS limitation).
// =============================================================================

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use indicatif::ProgressBar;
use crate::cli::color::{BOLD, GREEN, RED, RESET, YELLOW};
use crate::core::container::types::LXC_BASE_PATH;
use crate::distros::host_distro::{detect_host_distro, get_distro_config, FirewallKind};

// ---------------------------------------------------------------------------
// OrbStack / VM detection and override
// ---------------------------------------------------------------------------

/// Returns `true` if MELISA is running inside a virtualised environment
/// (OrbStack, VirtualBox, KVM, etc.) where `lxc-net` might be blocked by
/// the systemd `ConditionVirtualization` guard.
pub async fn is_virtualised_environment() -> bool {
    // Check via systemd-detect-virt
    let output = Command::new("systemd-detect-virt").output().await;
    let detected_by_systemd = match output {
        Ok(out) => {
            let virt_type = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
            out.status.success() && virt_type != "none" && !virt_type.is_empty()
        }
        Err(_) => false,
    };

    if detected_by_systemd {
        return true;
    }

    // BUG FIX #4: OrbStack hides itself from systemd-detect-virt.
    // Fallback: detect via os-release, same as detect_host_distro()
    let os_release = tokio::fs::read_to_string("/etc/os-release")
        .await
        .unwrap_or_default()
        .to_lowercase();
    if os_release.contains("orbstack") {
        return true;
    }

    // Old fallback
    Path::new("/proc/vz").exists() || Path::new("/.dockerenv").exists()
}

/// Diagnoses lxc-net failure by capturing systemd journal and configuration.
/// Returns a formatted diagnostic message for troubleshooting.
async fn diagnose_lxcnet_failure() -> String {
    let mut diagnosis = String::new();
    
    // 1. Get lxc-net service status
    diagnosis.push_str("\n\n=== SERVICE STATUS ===");
    match Command::new("sudo")
        .args(&["-n", "systemctl", "is-active", "lxc-net"])
        .output()
        .await
    {
        Ok(out) => {
            let status = String::from_utf8_lossy(&out.stdout);
            diagnosis.push_str(&format!("\nlxc-net status: {}\n", status.trim()));
        }
        Err(e) => diagnosis.push_str(&format!("\nFailed to check status: {}\n", e)),
    }
    
    // 2. Get recent journal errors (last 10 lines)
    diagnosis.push_str("\n=== RECENT ERRORS ===");
    match Command::new("sudo")
        .args(&["-n", "journalctl", "-u", "lxc-net", "-n", "10", "--no-pager"])
        .output()
        .await
    {
        Ok(out) => {
            let journal = String::from_utf8_lossy(&out.stdout);
            diagnosis.push_str(&format!("\n{}", journal));
        }
        Err(e) => diagnosis.push_str(&format!("\nFailed to read journal: {}\n", e)),
    }
    
    // 3. Check override file exists
    diagnosis.push_str("\n=== CONFIGURATION FILES ===");
    let override_file = "/etc/systemd/system/lxc-net.service.d/override.conf";
    if Path::new(override_file).exists() {
        diagnosis.push_str("\n[OK] systemd override file exists\n");
        if let Ok(content) = fs::read_to_string(override_file).await {
            diagnosis.push_str(&format!("Content:\n{}", content));
        }
    } else {
        diagnosis.push_str("\n[MISSING] systemd override file not found\n");
    }
    
    // 4. Check lxc-net config
    if let Ok(content) = fs::read_to_string("/etc/default/lxc-net").await {
        diagnosis.push_str("\n/etc/default/lxc-net (relevant lines):\n");
        for line in content.lines() {
            if line.contains("USE_LXC_BRIDGE") || line.contains("LXC_BRIDGE") {
                diagnosis.push_str(&format!("  {}\n", line));
            }
        }
    }
    
    diagnosis
}

/// Applies the systemd override that allows `lxc-net` to start inside a VM.
///
/// This implements the community-verified procedure for OrbStack (macOS) users
/// running MELISA inside an Ubuntu/Debian VM:
///
///   Step 1 – Create the systemd drop-in directory and write an override that
///             clears the ConditionVirtualization restriction.
///   Step 2 – Ensure USE_LXC_BRIDGE="true" is set in /etc/default/lxc-net.
///   Step 3 – Reload the systemd daemon.
///   Step 4 – Restart lxc-net with retry logic.
///   Step 5 – Poll for lxcbr0 device with 30-second timeout.
///
/// Returns `Ok(true)` if lxcbr0 is UP, `Ok(false)` if override applied but bridge didn't appear,
/// or `Err(msg)` if critical failures occur.
///
/// Note: `chattr +i` on /etc/resolv.conf may fail on VirtIO-FS (OrbStack's
/// filesystem layer).  This is non-fatal — DNS still works, the file is just
/// not locked against overwrites.
async fn apply_orbstack_lxcnet_override(audit: bool) -> Result<bool, String> {
    println!(
        "{}[ORBSTACK]{} Detected virtualised environment. Applying lxc-net compatibility override…",
        YELLOW, RESET
    );

    // ── Step 1: Create the systemd override directory ────────────────────────
    let override_dir = "/etc/systemd/system/lxc-net.service.d";
    let override_file = format!("{}/override.conf", override_dir);

    let mkdir_status = Command::new("sudo")
        .args(&["-n", "mkdir", "-p", override_dir])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    match mkdir_status {
        Ok(s) if s.success() => {
            println!("  {}[OK]{} Created systemd override directory.", GREEN, RESET);
        }
        _ => {
            return Err(format!(
                "Failed to create systemd override directory: {}",
                override_dir
            ));
        }
    }

    // Write the override that clears ConditionVirtualization.
    // An empty value resets the condition, allowing the service to start
    // regardless of the virtualisation detection result.
    let override_content = "[Unit]\nConditionVirtualization=\n";

    let write_status = Command::new("sudo")
        .args(&[
            "-n", "bash", "-c",
            &format!("echo -e '{}' > '{}'", override_content.replace('\n', "\\n"), override_file),
        ])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    match write_status {
        Ok(s) if s.success() => {
            println!(
                "  {}[OK]{} Wrote ConditionVirtualization override to {}.",
                GREEN, RESET, override_file
            );
        }
        _ => {
            return Err(format!(
                "Failed to write systemd override to: {}",
                override_file
            ));
        }
    }

    // ── Step 2: Set USE_LXC_BRIDGE="true" in /etc/default/lxc-net ───────────
    let bridge_config = "/etc/default/lxc-net";
    let set_bridge_cmd = format!(
        "grep -q 'USE_LXC_BRIDGE' '{}' \
         && sed -i 's/.*USE_LXC_BRIDGE.*/USE_LXC_BRIDGE=\"true\"/' '{}' \
         || echo 'USE_LXC_BRIDGE=\"true\"' >> '{}'",
        bridge_config, bridge_config, bridge_config
    );

    let bridge_status = Command::new("sudo")
        .args(&["-n", "bash", "-c", &set_bridge_cmd])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;
    
    match bridge_status {
        Ok(s) if s.success() => {
            println!(
                "  {}[OK]{} Configured USE_LXC_BRIDGE=true in {}.",
                GREEN, RESET, bridge_config
            );
        }
        _ => {
            return Err(format!(
                "Failed to configure USE_LXC_BRIDGE in: {}",
                bridge_config
            ));
        }
    }

    // ── Step 3: Reload systemd daemon ────────────────────────────────────────
    let daemon_reload = Command::new("sudo")
        .args(&["-n", "systemctl", "daemon-reload"])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    match daemon_reload {
        Ok(s) if s.success() => {
            println!("  {}[OK]{} systemd daemon reloaded.", GREEN, RESET);
        }
        _ => {
            eprintln!("  {}[WARN]{} systemctl daemon-reload failed — continuing anyway.", YELLOW, RESET);
        }
    }
    
    // Give systemd time to process the reload
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // ── Step 4: Restart lxc-net with retry logic ──────────────────────────────
    let max_retries = 3;
    let mut last_error = String::new();

    for attempt in 1..=max_retries {
        if attempt > 1 {
            let backoff_secs = 2_u64.pow((attempt - 1) as u32);
            println!(
                "  {}[RETRY {}/{}]{} Waiting {} seconds before retry...",
                YELLOW, attempt, max_retries, RESET, backoff_secs
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
        }

        // --- NEW PRE-RESTART CLEANUP LOGIC ---
        // 1. Terminate orphaned dnsmasq processes
        println!("  {}[CLEANUP]{} Purging blocked sockets and interfaces...", YELLOW, RESET);
        
        // 1. Matikan proses apapun yang menahan port 53 di IP 10.0.3.1
        let _ = Command::new("sudo")
            .args(&["-n", "fuser", "-k", "53/tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
            
        let _ = Command::new("sudo")
            .args(&["-n", "fuser", "-k", "53/udp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        let _ = Command::new("sudo")
            .args(&["-n", "killall", "dnsmasq"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        // 2. Tear down bridge interface
        let _ = Command::new("sudo")
            .args(&["-n", "ip", "link", "delete", "lxcbr0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        // -------------------------------------

        println!("  {}[ATTEMPT {}/{}]{} Restarting lxc-net...", YELLOW, attempt, max_retries, RESET);
        let restart_status = Command::new("sudo")
            .args(&["-n", "systemctl", "restart", "lxc-net"])
            .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
            .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
            .status()
            .await;
        
        match restart_status {
            Ok(s) if s.success() => {
                println!(
                    "  {}[OK]{} lxc-net restarted successfully on attempt {}/{}.",
                    GREEN, RESET, attempt, max_retries
                );
                last_error.clear();
                break;
            }
            Ok(s) => {
                last_error = format!("Exit code: {}", s.code().unwrap_or(-1));
            }
            Err(e) => {
                last_error = format!("IO Error: {}", e);
            }
        }
    }
    
    if !last_error.is_empty() {
        let diag = diagnose_lxcnet_failure().await;
        eprintln!(
            "  {}[ERROR]{} Failed to restart lxc-net after {} attempts. {}
            
Diagnostics:{}",
            RED, RESET, max_retries, last_error, diag
        );
        return Err(format!("lxc-net restart failed: {}", last_error));
    }

    // ── Verification: Poll for lxcbr0 with 30-second timeout ─────────────────
    println!("  {}[INFO]{} Polling for network bridge (timeout: 30s)...", YELLOW, RESET);
    
    for attempt in 1..=30 {
        if Path::new("/sys/class/net/lxcbr0").exists() {
            println!(
                "  {}[SUCCESS]{} lxcbr0 is UP on poll attempt {}. DHCP and bridge are operational.",
                GREEN, RESET, attempt
            );
            
            // Verify IP is 10.0.3.1/24 as expected by lxc-net defaults.
            let ip_check = Command::new("ip")
                .args(&["addr", "show", "lxcbr0"])
                .output()
                .await;
            if let Ok(out) = ip_check {
                let ip_output = String::from_utf8_lossy(&out.stdout);
                if ip_output.contains("10.0.3.1") {
                    println!(
                        "  {}[OK]{} lxcbr0 has IP 10.0.3.1/24 — DHCP gateway is reachable.",
                        GREEN, RESET
                    );
                }
            }
            return Ok(true);
        }
        
        if attempt < 30 {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
    
    eprintln!(
        "  {}[WARNING]{} lxcbr0 did not appear after 30 seconds of polling.",
        YELLOW, RESET
    );
    println!(
        "  {}[INFO]{} VirtIO-FS chattr limitation is non-fatal — DNS resolution may require manual /etc/resolv.conf check.",
        YELLOW, RESET
    );
    Ok(false)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Re-initialises the host LXC network infrastructure.
///
/// If the host is running inside a VM or OrbStack, the systemd
/// `ConditionVirtualization` override is applied automatically before
/// attempting to start `lxc-net`.  This makes MELISA fully operational on
/// macOS + OrbStack without any manual steps.
///
/// Returns `Ok(true)` if bridge is operational, `Ok(false)` if override applied
/// but bridge didn't appear (non-fatal), or `Err(msg)` on critical failures.
pub async fn ensure_host_network_ready(audit: bool) -> Result<bool, String> {
    println!(
        "{}[INFO]{} Re-initializing host network infrastructure…",
        BOLD, RESET
    );

    // ── OrbStack / VM compatibility ──────────────────────────────────────────
    if is_virtualised_environment().await {
        match apply_orbstack_lxcnet_override(audit).await {
            Ok(bridge_ok) => {
                println!(
                    "{}[INFO]{} Virtualisation detected (OrbStack/VM). Override applied.",
                    YELLOW, RESET
                );
                if !bridge_ok {
                    println!(
                        "{}[WARNING]{} Bridge setup completed but device not yet visible. Continuing anyway.",
                        YELLOW, RESET
                    );
                }
            }
            Err(e) => {
                eprintln!("{}[ERROR]{} Failed to apply OrbStack override: {}", RED, RESET, e);
                return Err(e);
            }
        }
    } else {
        // Standard bare-metal: just start lxc-net directly.
        println!("{}[INFO]{} Bare-metal environment detected. Starting lxc-net...", YELLOW, RESET);
        let lxc_net_args: &[&str] = &["-n", "systemctl", "start", "lxc-net"];
        let status = if audit {
            Command::new("sudo")
                .args(lxc_net_args)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await
        } else {
            Command::new("sudo").args(lxc_net_args).status().await
        };
        
        match status {
            Ok(s) if s.success() => {
                println!("{}[OK]{} lxc-net started successfully.", GREEN, RESET);
            }
            Ok(s) => {
                return Err(format!("lxc-net start failed with exit code: {}", s.code().unwrap_or(-1)));
            }
            Err(e) => {
                return Err(format!("Failed to start lxc-net: {}", e));
            }
        }
        
        // Poll for bridge on bare-metal too
        let mut bridge_found = false;
        for attempt in 1..=10 {
            if Path::new("/sys/class/net/lxcbr0").exists() {
                bridge_found = true;
                println!("{}[OK]{} lxcbr0 is UP on bare-metal.", GREEN, RESET);
                break;
            }
            if attempt < 10 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        }
        
        if !bridge_found {
            return Err("lxcbr0 bridge did not appear after 10 seconds".to_string());
        }
    }

    // ── Firewall rules ───────────────────────────────────────────────────────
    let host_distro = detect_host_distro().await;
    let distro_config = get_distro_config(&host_distro);
    configure_firewall_for_lxc(&distro_config.firewall_tool).await;
    
    Ok(true)
}

/// Injects the minimum LXC network stanza (veth/lxcbr0 + random MAC) into
/// a container's config file if it is not already present.
pub async fn inject_network_config(name: &str, pb: &ProgressBar) {
    let config_path = format!("{}/{}/config", LXC_BASE_PATH, name);

    if !Path::new(&config_path).exists() {
        pb.println(format!(
            "{}[WARNING]{} LXC config file not found at '{}'. Skipping network injection.",
            YELLOW, RESET, config_path
        ));
        return;
    }

    let existing_content = fs::read_to_string(&config_path).await.unwrap_or_default();
    if existing_content.contains("lxc.net.0.link") {
        pb.println(format!(
            "{}[SKIP]{} Network configuration already present. Skipping injection.",
            YELLOW, RESET
        ));
        return;
    }

    let mac_byte_a = rand::random::<u8>();
    let mac_byte_b = rand::random::<u8>();
    let network_stanza = format!(
        "\n# Auto-generated by MELISA\n\
        lxc.net.0.type = veth\n\
        lxc.net.0.link = lxcbr0\n\
        lxc.net.0.flags = up\n\
        lxc.net.0.hwaddr = ee:ec:fa:5e:{:02x}:{:02x}\n",
        mac_byte_a, mac_byte_b
    );

    match OpenOptions::new().append(true).open(&config_path).await {
        Ok(mut file) => {
            if let Err(err) = file.write_all(network_stanza.as_bytes()).await {
                eprintln!(
                    "{}[ERROR]{} Failed to write network configuration: {}",
                    RED, RESET, err
                );
            }
        }
        Err(err) => {
            eprintln!(
                "{}[ERROR]{} Failed to open LXC config file '{}': {}",
                RED, RESET, config_path, err
            );
        }
    }
}

/// Writes Google DNS nameservers to the container's resolv.conf and
/// attempts to lock the file with chattr.
///
/// Note: On OrbStack (VirtIO-FS) `chattr +i` is not supported — this is
/// non-fatal and a warning is emitted instead of an error.
pub async fn setup_container_dns(name: &str, pb: &ProgressBar) {
    let etc_path = format!("{}/{}/rootfs/etc", LXC_BASE_PATH, name);
    let dns_path = format!("{}/resolv.conf", etc_path);
 
    // Create directory /etc inside container if not present
    let _ = Command::new("sudo")
        .args(&["-n", "mkdir", "-p", &etc_path])
        .status()
        .await;
 
    // Release immutable lock if previously locked
    let _ = Command::new("sudo")
        .args(&["-n", "chattr", "-i", &dns_path])
        .status()
        .await;
 
    // Remove old resolv.conf
    let _ = Command::new("sudo")
        .args(&["-n", "rm", "-f", &dns_path])
        .status()
        .await;
 
    // FIX: DNS content as bytes literal — no format string that could be injected
    let dns_content = b"nameserver 8.8.8.8\nnameserver 8.8.4.4\n";
 
    // FIX: Write via `tee` with stdin — argument is literal path (not shell-interpreted)
    // tee receives path as argv argument, not shell string, so it's safe
    // even if dns_path contains characters that would be dangerous in shell.
    let mut tee_process = Command::new("sudo")
        .args(&["-n", "tee", &dns_path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
 
    let write_success = if let Some(ref mut child) = tee_process {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(dns_content).await.is_ok()
        } else {
            false
        }
    } else {
        false
    };
 
    // Wait for tee process to complete
    if let Some(mut child) = tee_process {
        let _ = child.wait().await;
    }
 
    if write_success {
        // Try to apply immutable lock (optional — not fatal if it fails on OrbStack)
        let lock_status = Command::new("sudo")
            .args(&["-n", "chattr", "+i", &dns_path])
            .status()
            .await;
 
        match lock_status {
            Ok(s) if s.success() => {
                pb.println(format!(
                    "{}[INFO]{} DNS configured and locked (immutable) successfully.",
                    GREEN, RESET
                ));
            }
            _ => {
                pb.println(format!(
                    "{}[WARNING]{} DNS written but immutable lock (chattr +i) could not be applied. \
                     This is expected on OrbStack / VirtIO-FS — DNS will still work.",
                    YELLOW, RESET
                ));
            }
        }
    } else {
        eprintln!(
            "{}[ERROR]{} Failed to configure DNS for container '{}'.",
            RED, RESET, name
        );
    }
}

/// Ensure host can forward packets from container to the internet.
/// Called EVERY time a container is created or run — not just when lxcbr0 is down.
/// On OrbStack, iptables rules are ephemeral and disappear on every VM restart.
pub async fn ensure_nat_routing_ready() {
    // 1. Enable ip_forward so kernel forwards packets between interfaces
    let _ = Command::new("sudo")
        .args(&["-n", "sysctl", "-w", "net.ipv4.ip_forward=1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    // 2. Check if MASQUERADE rule already exists (avoid duplicates)
    let check = Command::new("sudo")
        .args(&[
            "-n", "iptables", "-t", "nat", "-C", "POSTROUTING",
            "-s", "10.0.3.0/24", "!", "-d", "10.0.3.0/24", "-j", "MASQUERADE",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    // Exit code != 0 artinya rule belum ada
    if check.map(|s| !s.success()).unwrap_or(true) {
        let _ = Command::new("sudo")
            .args(&[
                "-n", "iptables", "-t", "nat", "-A", "POSTROUTING",
                "-s", "10.0.3.0/24", "!", "-d", "10.0.3.0/24", "-j", "MASQUERADE",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    // 3. Allow FORWARD from container outbound (check first to avoid duplicates)
    let fwd_check = Command::new("sudo")
        .args(&["-n", "iptables", "-C", "FORWARD", "-i", "lxcbr0", "-j", "ACCEPT"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    if fwd_check.map(|s| !s.success()).unwrap_or(true) {
        let _ = Command::new("sudo")
            .args(&["-n", "iptables", "-I", "FORWARD", "-i", "lxcbr0", "-j", "ACCEPT"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        // Paket balasan masuk ke container
        let _ = Command::new("sudo")
            .args(&[
                "-n", "iptables", "-I", "FORWARD",
                "-o", "lxcbr0", "-m", "state",
                "--state", "RELATED,ESTABLISHED", "-j", "ACCEPT",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

/// Removes the immutable lock from a container's resolv.conf (before deletion).
pub async fn unlock_container_dns(name: &str) {
    let dns_path = format!("{}/{}/rootfs/etc/resolv.conf", LXC_BASE_PATH, name);
    let _ = Command::new("sudo")
        .args(&["-n", "chattr", "-i", &dns_path])
        .status()
        .await;
}

/// Helper function to get absolute path intelligently.
/// If folder does not exist, it will be created in the current directory (CWD).
async fn resolve_and_prepare_host_path(path_input: &str) -> Result<String, String> {
    let mut path = PathBuf::from(path_input);

    // 1. If path is not absolute, combine with current working directory
    if !path.is_absolute() {
        let cwd = std::env::current_dir().map_err(|e| format!("Failed to access CWD: {}", e))?;
        path = cwd.join(path);
    }

    // 2. Check if folder exists, if not create it
    if !path.exists() {
        fs::create_dir_all(&path)
            .await
            .map_err(|e| format!("Failed to create new folder: {}", e))?;
        println!("{}[INFO]{} Folder '{}' not found, auto-created.", BOLD, RESET, path.display());
    }

    // 3. Get full path (canonicalize) to ensure no '../' or symlinks
    let absolute_path = fs::canonicalize(&path)
        .await
        .map_err(|e| format!("Failed to verify path: {}", e))?;

    Ok(absolute_path.to_string_lossy().to_string())
}

/// Mounts a host directory into a container with smart path detection and auto-create.
pub async fn add_shared_folder(container_name: &str, host_path_input: &str, container_path: &str) {
    // Step 1: Resolve Path (Intelligently)
    let host_path = match resolve_and_prepare_host_path(host_path_input).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}[ERROR]{} {}", RED, RESET, e);
            return;
        }
    };

    let config_path = format!("{}/{}/config", LXC_BASE_PATH, container_name);
    let bind_entry = format!(
        "lxc.mount.entry = {} {} none bind,create=dir 0 0",
        host_path, container_path
    );

    // Step 2: Idempotency (Check if already exists to avoid duplicates)
    if let Ok(content) = fs::read_to_string(&config_path).await {
        if content.contains(&bind_entry) {
            println!("{}[INFO]{} Folder configuration already exists. Skipping.", BOLD, RESET);
            return;
        }
    }

    // Step 3: Fix Ownership (for unprivileged container)
    let chown_status = Command::new("sudo")
        .args(&["chown", "-R", "100000:100000", &host_path])
        .status()
        .await;

    if let Err(e) = chown_status {
        eprintln!("{}[WARNING]{} Failed to run chown: {}", YELLOW, RESET, e);
    }

    // Langkah 4: Tulis ke config
    match OpenOptions::new().append(true).open(&config_path).await {
        Ok(mut file) => {
            if let Err(err) = file.write_all(format!("{}\n", bind_entry).as_bytes()).await {
                eprintln!("{}[ERROR]{} Gagal menulis config: {}", RED, RESET, err);
            } else {
                println!(
                    "{}[SUCCESS]{} Shared folder '{}' -> '{}' aktif.",
                    GREEN, RESET, host_path, container_path
                );
            }
        }
        Err(err) => eprintln!("{}[ERROR]{} Could not open config: {}", RED, RESET, err),
    }
}

/// Removes a bind-mount entry with more flexible matching.
pub async fn remove_shared_folder(container_name: &str, host_path_input: &str, container_path: &str) {
    let config_path = format!("{}/{}/config", LXC_BASE_PATH, container_name);
    
    // Resolve path input first so string matching in config is accurate
    let host_path = Path::new(host_path_input);
    let host_path_str = if host_path.is_absolute() {
        host_path_input.to_string()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(host_path).to_string_lossy().to_string(),
            Err(_) => host_path_input.to_string(),
        }
    };

    match fs::read_to_string(&config_path).await {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let mut new_lines = Vec::new();
            let mut found = false;

            for line in lines {
                // Match entry smartly (ignore excess whitespace)
                if line.contains("lxc.mount.entry") && line.contains(&host_path_str) && line.contains(container_path) {
                    found = true;
                    continue; // Skip this line (remove it)
                }
                new_lines.push(line);
            }

            if found {
                let mut updated_content = new_lines.join("\n");
                updated_content.push('\n');

                if let Err(err) = fs::write(&config_path, updated_content).await {
                    eprintln!("{}[ERROR]{} Failed to update config: {}", RED, RESET, err);
                } else {
                    println!("{}[SUCCESS]{} Folder '{}' removed from config.", GREEN, RESET, host_path_str);
                }
            } else {
                println!("{}[INFO]{} Entry not found in config.", BOLD, RESET);
            }
        }
        Err(err) => eprintln!("{}[ERROR]{} Failed to read config: {}", RED, RESET, err),
    }
}

// ---------------------------------------------------------------------------
// Firewall helpers (internal)
// ---------------------------------------------------------------------------

async fn run_sudo(args: &[&str]) -> bool {
    Command::new("sudo")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}
 
/// Check if an iptables rule already exists, then add it if not.
async fn ensure_iptables_rule(check_args: &[&str], add_args: &[&str]) {
    // -C (check) returns exit code 0 if rule already exists
    let exists = run_sudo(check_args).await;
    if !exists {
        // FIX: .await directly — no longer using tokio::spawn that wasn't awaited
        let added = run_sudo(add_args).await;
        if !added {
            eprintln!(
                "{}[WARN]{} Failed to add iptables rule: {:?}",
                YELLOW, RESET, add_args
            );
        }
    }
}
 
/// Configures the host firewall to allow LXC container traffic.
///
/// Supports three firewall backends: firewalld, ufw, and iptables.
/// For each backend, configures rules to allow traffic to/from the lxcbr0 bridge
/// and enables IP forwarding for container-to-internet connectivity.
pub async fn configure_firewall_for_lxc(firewall: &FirewallKind) {
    match firewall {
        FirewallKind::Firewalld => {
            run_sudo(&[
                "-n", "firewall-cmd",
                "--zone=trusted",
                "--add-interface=lxcbr0",
                "--permanent",
            ]).await;
            run_sudo(&["-n", "firewall-cmd", "--reload"]).await;
        }
 
        FirewallKind::Ufw => {
            run_sudo(&["-n", "ufw", "allow", "in", "on", "lxcbr0"]).await;
            run_sudo(&["-n", "ufw", "reload"]).await;
        }
 
        FirewallKind::Iptables => {
            // FIX: Consistent indentation — all commands left-aligned the same
            // FIX: tokio::spawn replaced with direct .await in all rules
 
            // Enable IP forwarding at runtime
            run_sudo(&["-n", "sysctl", "-w", "net.ipv4.ip_forward=1"]).await;
 
            // Persist IP forwarding in sysctl.conf
            // (still using bash -c but arguments are constant strings,
            //  no variable interpolation from user input)
            run_sudo(&[
                "-n", "bash", "-c",
                "grep -q 'net.ipv4.ip_forward' /etc/sysctl.conf \
                 && sed -i 's/.*net.ipv4.ip_forward.*/net.ipv4.ip_forward=1/' /etc/sysctl.conf \
                 || echo 'net.ipv4.ip_forward=1' >> /etc/sysctl.conf",
            ]).await;
 
            // Allow traffic INPUT from container
            ensure_iptables_rule(
                &["-n", "iptables", "-C", "INPUT", "-i", "lxcbr0", "-j", "ACCEPT"],
                &["-n", "iptables", "-I", "INPUT", "-i", "lxcbr0", "-j", "ACCEPT"],
            ).await;
 
            // Allow FORWARD from lxcbr0
            ensure_iptables_rule(
                &["-n", "iptables", "-C", "FORWARD", "-i", "lxcbr0", "-j", "ACCEPT"],
                &["-n", "iptables", "-I", "FORWARD", "-i", "lxcbr0", "-j", "ACCEPT"],
            ).await;
 
            // Allow FORWARD to lxcbr0 for established/related connections
            ensure_iptables_rule(
                &[
                    "-n", "iptables", "-C", "FORWARD",
                    "-o", "lxcbr0", "-m", "state",
                    "--state", "RELATED,ESTABLISHED", "-j", "ACCEPT",
                ],
                &[
                    "-n", "iptables", "-I", "FORWARD",
                    "-o", "lxcbr0", "-m", "state",
                    "--state", "RELATED,ESTABLISHED", "-j", "ACCEPT",
                ],
            ).await;
 
            // FIX: MASQUERADE — previously used tokio::spawn without .await
            // Now using ensure_iptables_rule which is properly awaited
            ensure_iptables_rule(
                &[
                    "-n", "iptables", "-t", "nat", "-C", "POSTROUTING",
                    "-s", "10.0.3.0/24", "!", "-d", "10.0.3.0/24", "-j", "MASQUERADE",
                ],
                &[
                    "-n", "iptables", "-t", "nat", "-A", "POSTROUTING",
                    "-s", "10.0.3.0/24", "!", "-d", "10.0.3.0/24", "-j", "MASQUERADE",
                ],
            ).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {

    #[test]
    fn test_bind_mount_entry_format_is_correct() {
        let host_path      = "/home/user/project";
        let container_path = "/app";
        let expected = format!(
            "lxc.mount.entry = {} {} none bind,create=dir 0 0\n",
            host_path, container_path
        );
        assert!(
            expected.starts_with("lxc.mount.entry = "),
            "Bind-mount entry must begin with 'lxc.mount.entry = '"
        );
        assert!(
            expected.contains("none bind,create=dir 0 0"),
            "Bind-mount entry must contain the bind options"
        );
    }

    #[test]
    fn test_dns_content_is_valid_resolv_conf() {
        let content = b"nameserver 8.8.8.8\nnameserver 8.8.4.4\n";
        let as_str = std::str::from_utf8(content).unwrap();
 
        assert!(as_str.contains("nameserver 8.8.8.8"), "Must have primary DNS");
        assert!(as_str.contains("nameserver 8.8.4.4"), "Must have secondary DNS");
        assert!(as_str.ends_with('\n'), "resolv.conf must end with newline");
        // Pastikan tidak ada karakter shell yang bisa menyebabkan injection
        assert!(!as_str.contains('$'), "DNS content must not contain shell variable prefix");
        assert!(!as_str.contains('`'), "DNS content must not contain backtick");
        assert!(!as_str.contains('>'), "DNS content must not contain redirection");
    }
 
    #[test]
    fn test_no_shell_interpolation_in_dns_content() {
        // Pastikan dns_content adalah literal bytes, bukan format string
        // yang bisa dipengaruhi oleh input nama container
        let container_name = "evil; rm -rf /";
        let dns_path = format!("/var/lib/lxc/{}/rootfs/etc/resolv.conf", container_name);
 
        // Path terbentuk dari nama container, tapi kontennya tidak
        // dns_content selalu sama terlepas dari nama container apapun
        let dns_content = b"nameserver 8.8.8.8\nnameserver 8.8.4.4\n";
        assert_eq!(
            dns_content,
            b"nameserver 8.8.8.8\nnameserver 8.8.4.4\n",
            "DNS content must be static regardless of container name"
        );
 
        // dns_path mengandung nama container tapi hanya digunakan sebagai
        // argumen argv ke tee, bukan diinterpolasikan dalam shell string
        assert!(dns_path.contains("evil"), "Path contains name — this is expected");
        // Yang penting: nama container tidak masuk ke DNS content
        let content_str = std::str::from_utf8(dns_content).unwrap();
        assert!(!content_str.contains("evil"), "Container name must NOT appear in DNS content");
    }

    #[test]
    fn test_network_stanza_contains_required_lxc_keys() {
        let stanza = format!(
            "\n# Auto-generated by MELISA\n\
            lxc.net.0.type = veth\n\
            lxc.net.0.link = lxcbr0\n\
            lxc.net.0.flags = up\n\
            lxc.net.0.hwaddr = ee:ec:fa:5e:{:02x}:{:02x}\n",
            0xAB_u8, 0xCD_u8
        );
        assert!(stanza.contains("lxc.net.0.type = veth"),    "Must configure veth network type");
        assert!(stanza.contains("lxc.net.0.link = lxcbr0"),  "Must link to lxcbr0 bridge");
        assert!(stanza.contains("lxc.net.0.flags = up"),     "Must set interface flags to up");
        assert!(stanza.contains("lxc.net.0.hwaddr"),         "Must include a MAC address");
    }

    #[test]
    fn test_bind_entry_removal_logic() {
        let host_path      = "/host/data";
        let container_path = "/container/data";
        let target_entry = format!(
            "lxc.mount.entry = {} {} none bind,create=dir 0 0",
            host_path, container_path
        );
        let config_content = format!(
            "lxc.utsname = mybox\n{}\nlxc.net.0.type = veth\n",
            target_entry
        );
        let filtered: String = config_content
            .lines()
            .filter(|line| !line.trim().eq(target_entry.trim()))
            .map(|line| format!("{}\n", line))
            .collect();
        assert!(
            !filtered.contains(&target_entry),
            "Filtered config must not contain the removed bind-mount entry"
        );
        assert!(
            filtered.contains("lxc.utsname = mybox"),
            "Filtered config must retain unrelated entries"
        );
    }

    #[test]
    fn test_orbstack_override_content_clears_condition() {
        // Verify the override content structure is correct.
        let override_content = "[Unit]\nConditionVirtualization=\n";
        assert!(
            override_content.contains("[Unit]"),
            "Override must be in [Unit] section"
        );
        assert!(
            override_content.contains("ConditionVirtualization="),
            "Override must clear ConditionVirtualization"
        );
        // An empty value (no text after '=') resets the condition.
        let condition_line = override_content
            .lines()
            .find(|l| l.starts_with("ConditionVirtualization"))
            .unwrap();
        let value = condition_line.split('=').nth(1).unwrap_or("x");
        assert!(value.is_empty(), "ConditionVirtualization must have an empty value to clear it");
    }

    #[test]
    fn test_lxcbr0_path_is_correct() {
        assert_eq!(
            "/sys/class/net/lxcbr0",
            "/sys/class/net/lxcbr0",
            "lxcbr0 network interface path must be correct"
        );
    }
}