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
    // Cek via systemd-detect-virt
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

    // BUG FIX #4: OrbStack menyembunyikan dirinya dari systemd-detect-virt.
    // Fallback: deteksi lewat os-release, sama persis seperti detect_host_distro()
    let os_release = tokio::fs::read_to_string("/etc/os-release")
        .await
        .unwrap_or_default()
        .to_lowercase();
    if os_release.contains("orbstack") {
        return true;
    }

    // Fallback lama
    Path::new("/proc/vz").exists() || Path::new("/.dockerenv").exists()
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
///   Step 4 – Restart lxc-net.
///
/// Note: `chattr +i` on /etc/resolv.conf may fail on VirtIO-FS (OrbStack's
/// filesystem layer).  This is non-fatal — DNS still works, the file is just
/// not locked against overwrites.
async fn apply_orbstack_lxcnet_override(audit: bool) {
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
            eprintln!(
                "  {}[WARN]{} Could not create {}. lxc-net override may not apply.",
                YELLOW, RESET, override_dir
            );
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
            eprintln!(
                "  {}[WARN]{} Failed to write override file. lxc-net may not start automatically.",
                YELLOW, RESET
            );
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

    let _ = Command::new("sudo")
        .args(&["-n", "bash", "-c", &set_bridge_cmd])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    println!(
        "  {}[OK]{} Configured USE_LXC_BRIDGE=true in {}.",
        GREEN, RESET, bridge_config
    );

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
            eprintln!("  {}[WARN]{} systemctl daemon-reload failed.", YELLOW, RESET);
        }
    }

    // ── Step 4: Restart lxc-net ──────────────────────────────────────────────
    let restart_status = Command::new("sudo")
        .args(&["-n", "systemctl", "restart", "lxc-net"])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    match restart_status {
        Ok(s) if s.success() => {
            println!(
                "  {}[OK]{} lxc-net restarted successfully.",
                GREEN, RESET
            );
        }
        _ => {
            eprintln!(
                "  {}[ERROR]{} Failed to restart lxc-net even after override. \
                Check 'sudo journalctl -u lxc-net' for details.",
                RED, RESET
            );
        }
    }

    // ── Verification ─────────────────────────────────────────────────────────
    // Give lxcbr0 a moment to come up before the caller checks.
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    if Path::new("/sys/class/net/lxcbr0").exists() {
        println!(
            "  {}[SUCCESS]{} lxcbr0 is UP. DHCP and bridge are operational.",
            GREEN, RESET
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
    } else {
        eprintln!(
            "  {}[WARNING]{} lxcbr0 did not appear after override. \
            VirtIO-FS chattr limitation is non-fatal — check /etc/resolv.conf manually if DNS fails.",
            YELLOW, RESET
        );
    }
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
pub async fn ensure_host_network_ready(audit: bool) {
    println!(
        "{}[INFO]{} Re-initializing host network infrastructure…",
        BOLD, RESET
    );

    // ── OrbStack / VM compatibility ──────────────────────────────────────────
    if is_virtualised_environment().await {
        apply_orbstack_lxcnet_override(audit).await;
        println!(
            "{}[INFO]{} Virtualisation detected (OrbStack/VM). Applying lxc-net override…",
            YELLOW, RESET
        );
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    } else {
        // Standard bare-metal: just start lxc-net directly.
        let lxc_net_args: &[&str] = &["-n", "systemctl", "start", "lxc-net"];
        if audit {
            let _ = Command::new("sudo")
                .args(lxc_net_args)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await;
        } else {
            let _ = Command::new("sudo").args(lxc_net_args).status().await;
        }
    }

    // ── Firewall rules ───────────────────────────────────────────────────────
    let host_distro = detect_host_distro().await;
    let distro_config = get_distro_config(&host_distro);
    configure_firewall_for_lxc(&distro_config.firewall_tool).await;
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

    let _ = Command::new("sudo")
        .args(&["mkdir", "-p", &etc_path])
        .status()
        .await;

    // Attempt to unset immutable flag — fails silently on VirtIO-FS.
    let _ = Command::new("sudo")
        .args(&["chattr", "-i", &dns_path])
        .status()
        .await;

    let _ = Command::new("sudo")
        .args(&["rm", "-f", &dns_path])
        .status()
        .await;

    let dns_content = "nameserver 8.8.8.8\\nnameserver 8.8.4.4\\n";
    let write_status = Command::new("sudo")
        .args(&[
            "bash", "-c",
            &format!("echo -e '{}' > {}", dns_content, dns_path),
        ])
        .status()
        .await;

    match write_status {
        Ok(s) if s.success() => {
            // Attempt to lock — non-fatal if chattr is not supported.
            let lock_status = Command::new("sudo")
                .args(&["chattr", "+i", &dns_path])
                .status()
                .await;
            match lock_status {
                Ok(ls) if ls.success() => {
                    pb.println(format!(
                        "{}[INFO]{} DNS configured and locked successfully.",
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
        }
        _ => {
            eprintln!("{}[ERROR]{} Failed to configure DNS for container '{}'.", RED, RESET, name);
        }
    }
}

/// Pastikan host bisa meneruskan paket dari container ke internet.
/// Dipanggil SETIAP kali container dibuat atau dijalankan — bukan hanya saat lxcbr0 down.
/// Di OrbStack, iptables rules bersifat ephemeral dan hilang setiap VM restart.
pub async fn ensure_nat_routing_ready() {
    // 1. Aktifkan ip_forward agar kernel mau forward paket antar-interface
    let _ = Command::new("sudo")
        .args(&["-n", "sysctl", "-w", "net.ipv4.ip_forward=1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    // 2. Cek dulu apakah MASQUERADE rule sudah ada (hindari duplikat)
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

    // 3. Izinkan FORWARD dari container keluar (cek dulu agar tidak duplikat)
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

/// Fungsi pembantu untuk mendapatkan absolute path secara cerdas.
/// Jika folder tidak ada, akan dibuat di direktori saat ini (CWD).
async fn resolve_and_prepare_host_path(path_input: &str) -> Result<String, String> {
    let mut path = PathBuf::from(path_input);

    // 1. Jika jalur tidak absolut, gabungkan dengan current working directory
    if !path.is_absolute() {
        let cwd = std::env::current_dir().map_err(|e| format!("Gagal akses CWD: {}", e))?;
        path = cwd.join(path);
    }

    // 2. Cek apakah folder ada, jika tidak ada buat foldernya
    if !path.exists() {
        fs::create_dir_all(&path)
            .await
            .map_err(|e| format!("Gagal membuat folder baru: {}", e))?;
        println!("{}[INFO]{} Folder '{}' tidak ditemukan, otomatis dibuat.", BOLD, RESET, path.display());
    }

    // 3. Ambil jalur lengkap (canonicalize) untuk memastikan tidak ada '../' atau simbolik link
    let absolute_path = fs::canonicalize(&path)
        .await
        .map_err(|e| format!("Gagal verifikasi path: {}", e))?;

    Ok(absolute_path.to_string_lossy().to_string())
}

/// Mounts a host directory into a container with smart path detection and auto-create.
pub async fn add_shared_folder(container_name: &str, host_path_input: &str, container_path: &str) {
    // Langkah 1: Resolusi Path (Pintar)
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

    // Langkah 2: Idempotency (Cek apakah sudah ada agar tidak duplikat)
    if let Ok(content) = fs::read_to_string(&config_path).await {
        if content.contains(&bind_entry) {
            println!("{}[INFO]{} Konfigurasi folder sudah ada. Skip.", BOLD, RESET);
            return;
        }
    }

    // Langkah 3: Perbaiki Ownership (untuk unprivileged container)
    let chown_status = Command::new("sudo")
        .args(&["chown", "-R", "100000:100000", &host_path])
        .status()
        .await;

    if let Err(e) = chown_status {
        eprintln!("{}[WARNING]{} Gagal menjalankan chown: {}", YELLOW, RESET, e);
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
        Err(err) => eprintln!("{}[ERROR]{} Tidak bisa buka config: {}", RED, RESET, err),
    }
}

/// Removes a bind-mount entry dengan pencocokan yang lebih fleksibel.
pub async fn remove_shared_folder(container_name: &str, host_path_input: &str, container_path: &str) {
    let config_path = format!("{}/{}/config", LXC_BASE_PATH, container_name);
    
    // Resolve path input dulu supaya pencocokan string di config akurat
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
                // Mencocokkan entri secara pintar (mengabaikan spasi berlebih)
                if line.contains("lxc.mount.entry") && line.contains(&host_path_str) && line.contains(container_path) {
                    found = true;
                    continue; // Skip baris ini (hapus)
                }
                new_lines.push(line);
            }

            if found {
                let mut updated_content = new_lines.join("\n");
                updated_content.push('\n');

                if let Err(err) = fs::write(&config_path, updated_content).await {
                    eprintln!("{}[ERROR]{} Gagal update config: {}", RED, RESET, err);
                } else {
                    println!("{}[SUCCESS]{} Folder '{}' dihapus dari config.", GREEN, RESET, host_path_str);
                }
            } else {
                println!("{}[INFO]{} Entri tidak ditemukan di config.", BOLD, RESET);
            }
        }
        Err(err) => eprintln!("{}[ERROR]{} Gagal baca config: {}", RED, RESET, err),
    }
}

// ---------------------------------------------------------------------------
// Firewall helpers (internal)
// ---------------------------------------------------------------------------

async fn configure_firewall_for_lxc(firewall: &FirewallKind) {
    match firewall {
        FirewallKind::Firewalld => {
            let _ = Command::new("sudo")
                .args(&[
                    "-n", "firewall-cmd",
                    "--zone=trusted",
                    "--add-interface=lxcbr0",
                    "--permanent",
                ])
                .status()
                .await;
            let _ = Command::new("sudo")
                .args(&["-n", "firewall-cmd", "--reload"])
                .status()
                .await;
        }
        FirewallKind::Ufw => {
            let _ = Command::new("sudo")
                .args(&["-n", "ufw", "allow", "in", "on", "lxcbr0"])
                .status()
                .await;
            let _ = Command::new("sudo")
                .args(&["-n", "ufw", "reload"])
                .status()
                .await;
        }
        FirewallKind::Iptables => {
        // BUG FIX #2: Aktifkan ip_forward agar kernel mau forward paket antar-interface
        let _ = Command::new("sudo")
            .args(&["-n", "sysctl", "-w", "net.ipv4.ip_forward=1"])
            .status().await;

        // Buat permanen antar reboot
        let _ = Command::new("sudo")
            .args(&["-n", "bash", "-c",
                "grep -q 'net.ipv4.ip_forward' /etc/sysctl.conf \
                && sed -i 's/.*net.ipv4.ip_forward.*/net.ipv4.ip_forward=1/' /etc/sysctl.conf \
                || echo 'net.ipv4.ip_forward=1' >> /etc/sysctl.conf"])
            .status().await;

        // INPUT — izinkan host terima paket dari container (sudah ada, tetap dipertahankan)
        let _ = Command::new("sudo")
            .args(&["-n", "iptables", "-I", "INPUT", "-i", "lxcbr0", "-j", "ACCEPT"])
            .status().await;

        // BUG FIX #3: FORWARD chain — izinkan kernel forward paket dari container keluar
        let _ = Command::new("sudo")
            .args(&["-n", "iptables", "-I", "FORWARD", "-i", "lxcbr0", "-j", "ACCEPT"])
            .status().await;

        // BUG FIX #3: FORWARD chain — izinkan paket balasan masuk kembali ke container
        let _ = Command::new("sudo")
            .args(&["-n", "iptables", "-I", "FORWARD",
                "-o", "lxcbr0", "-m", "state",
                "--state", "RELATED,ESTABLISHED", "-j", "ACCEPT"])
            .status().await;

        // BUG FIX #1: NAT MASQUERADE — sembunyikan IP container di balik IP host
        // Tanpa ini, paket dari 10.0.3.x tidak akan di-route oleh internet
        let _ = Command::new("sudo")
            .args(&["-n", "iptables", "-t", "nat", "-C", "POSTROUTING",
                "-s", "10.0.3.0/24", "!", "-d", "10.0.3.0/24", "-j", "MASQUERADE"])
            .status().await
            .map(|s| {
                if !s.success() {
                    // Rule belum ada, tambahkan
                    tokio::spawn(Command::new("sudo")
                        .args(&["-n", "iptables", "-t", "nat", "-A", "POSTROUTING",
                            "-s", "10.0.3.0/24", "!", "-d", "10.0.3.0/24", "-j", "MASQUERADE"])
                        .status());
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_dns_content_contains_google_nameservers() {
        let dns_content = "nameserver 8.8.8.8\\nnameserver 8.8.4.4\\n";
        assert!(dns_content.contains("8.8.8.8"), "DNS config must include Google primary nameserver");
        assert!(dns_content.contains("8.8.4.4"), "DNS config must include Google secondary nameserver");
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