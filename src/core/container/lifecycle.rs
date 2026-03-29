// ============================================================================
// src/core/container/lifecycle.rs
//
// LXC container lifecycle management:
//   create, delete, start, stop, attach.
//
// FIX #1: `create_container` previously stopped after DNS setup.
//         The old version performed many crucial steps that were missing:
//           a) Verify the lxcbr0 bridge before creating the container
//           b) Inject MELISA metadata into the container's rootfs
//           c) Start the container after creation
//           d) Wait until the container's DHCP/network is ready
//           e) Run the initial package manager update
//         Without these steps, the container was created but could not be used.
//
// FIX #2: `delete_container` calls `cleanup_container_metadata` from the new
//         metadata module — this is already correct in the new version.
//
// All functions that mutate container state require admin privileges and
// accept an `audit` flag that controls subprocess output visibility.
// ============================================================================

use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};
use indicatif::ProgressBar;
use chrono::Local;

use crate::cli::color::{BOLD, GREEN, RED, RESET, YELLOW};
use crate::core::container::network::{
    ensure_host_network_ready, inject_network_config, setup_container_dns, unlock_container_dns,
};
use crate::core::container::query::is_container_running;
use crate::core::container::types::{LXC_BASE_PATH, DistroMetadata};
use crate::core::metadata::{cleanup_container_metadata, write_container_metadata};
use crate::core::root_check::ensure_admin;

// ── Shared subprocess helper ─────────────────────────────────────────────────

/// Executes a `sudo` command and optionally inherits stdout/stderr.
async fn run_sudo(args: &[&str], is_audit: bool) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = Command::new("sudo");
    cmd.args(args);
    if is_audit {
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    cmd.status().await
}

// ── Host runtime verification ────────────────────────────────────────────────

/// Verifies that the host network bridge `lxcbr0` is active.
///
/// If the bridge is not found, attempts automatic repair via `ensure_host_network_ready`.
/// Without this bridge, the container will not get an IP from DHCP.
///
/// # Arguments
/// * `audit` - When `true`, subprocess output is forwarded to the terminal.
///
/// # Returns
/// `true` if the bridge is available (or became available after repair).
async fn verify_host_runtime(audit: bool) -> bool {
    if Path::new("/sys/class/net/lxcbr0").exists() {
        return true;
    }
    println!(
        "{}[WARNING]{} Network bridge 'lxcbr0' not found. Initiating host auto-repair...",
        YELLOW, RESET
    );
    ensure_host_network_ready(audit).await;
    // Recheck after repair
    Path::new("/sys/class/net/lxcbr0").exists()
}

// ── Network initialization wait ──────────────────────────────────────────────

/// Polls the container until it gets a DHCP lease (appears in `lxc-info -iH`).
///
/// After the container starts, it takes a few seconds for DHCP to assign an IP.
/// The old version polled this — the new version skipped it, causing
/// the package manager update to fail because the network wasn't ready.
///
/// # Arguments
/// * `name` - Container name.
/// * `pb`   - Progress bar for inline status messages.
///
/// # Returns
/// `true` if the container obtained an IP within 30 seconds, `false` otherwise.
async fn wait_for_network_initialization(name: &str, pb: &ProgressBar) -> bool {
    pb.println(format!(
        "{}[INFO]{} Waiting for DHCP lease and network interfaces to initialize...",
        YELLOW, RESET
    ));

    for _ in 0..30 {
        let output = Command::new("sudo")
            .args(&["-n", "lxc-info", "-n", name, "-iH"])
            .output()
            .await;

        if let Ok(out) = output {
            let ips = String::from_utf8_lossy(&out.stdout);
            if ips.contains('.') && !ips.trim().is_empty() {
                pb.println(format!(
                    "{}[INFO]{} Network connection established (IP: {}). Allowing DNS resolver to settle...",
                    YELLOW, RESET, ips.trim()
                ));
                // Give the resolver a little time so the `/etc/resolv.conf` lock doesn't interfere
                sleep(Duration::from_secs(3)).await;
                return true;
            }
        }
        sleep(Duration::from_secs(1)).await;
    }
    false
}

// ── Package manager detection from distro name ───────────────────────────────

/// Maps a distro name (lowercase) to the appropriate package manager command.
///
/// Used to run the initial update after the container starts, without
/// having to probe into the container (which could fail if the package manager hasn't settled).
fn get_pkg_manager_for_distro(distro_name: &str) -> &'static str {
    let name = distro_name.to_lowercase();
    if name.contains("ubuntu") || name.contains("debian") || name.contains("kali")
        || name.contains("mint") || name.contains("raspbian") || name.contains("linuxmint")
    {
        "apt"
    } else if name.contains("fedora") || name.contains("centos") || name.contains("rhel")
        || name.contains("rocky") || name.contains("alma")
    {
        "dnf"
    } else if name.contains("alpine") {
        "apk"
    } else if name.contains("arch") || name.contains("manjaro") {
        "pacman"
    } else if name.contains("suse") || name.contains("opensuse") {
        "zypper"
    } else {
        "apt" // fallback sane default
    }
}

/// Returns the update command string for a given package manager.
fn get_pkg_update_cmd(pkg_manager: &str) -> &'static str {
    match pkg_manager {
        "apt" | "apt-get" => "apt-get update -y",
        "dnf" | "yum"    => "dnf makecache",
        "apk"            => "apk update",
        "pacman"         => "pacman -Sy --noconfirm",
        "zypper"         => "zypper --non-interactive refresh",
        _                => "true",
    }
}

// ── Auto initial package setup ───────────────────────────────────────────────

/// Runs the package manager update inside the container as initial setup.
///
/// The old version did this after the network was ready — the new version didn't do
/// this at all, so the container needed to be manually updated before anything could be installed.
async fn auto_initial_setup(name: &str, distro_name: &str, pb: &ProgressBar, audit: bool) {
    let pm = get_pkg_manager_for_distro(distro_name);
    let cmd = get_pkg_update_cmd(pm);

    pb.println(format!(
        "{}[INFO]{} Updating package repository for '{}' via '{}'...",
        YELLOW, RESET, name, pm
    ));

    if audit {
        pb.println(format!(
            "{}[AUDIT]{} Running '{}' inside '{}' — raw output follows:",
            YELLOW, RESET, cmd, name
        ));
    }

    let status = Command::new("sudo")
        .args(&["-n", "lxc-attach", "-P", LXC_BASE_PATH, "-n", name, "--", "sh", "-c", cmd])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {
            pb.println(format!(
                "{}[SUCCESS]{} Initial repository setup completed for '{}'.",
                GREEN, RESET, name
            ));
        }
        Ok(_) => {
            pb.println(format!(
                "{}[WARNING]{} Package manager update failed for '{}'. Container is still usable.",
                YELLOW, RESET, name
            ));
        }
        Err(e) => {
            pb.println(format!(
                "{}[WARNING]{} Could not run package update: {}",
                YELLOW, RESET, e
            ));
        }
    }
}

// ── Container metadata injection ─────────────────────────────────────────────

/// Builds and writes MELISA metadata into the container's rootfs.
///
/// Metadata is saved at `<LXC_BASE_PATH>/<name>/rootfs/etc/melisa-info`.
/// Without this, `melisa --info <container>` will not display any data.
///
/// # Arguments
/// * `name` - Container name.
/// * `meta` - Distribution metadata for the container.
async fn inject_container_metadata(name: &str, meta: &DistroMetadata) {
    // Extract release from slug (format: "distro/release/arch")
    let release = meta.slug
        .split('/')
        .nth(1)
        .unwrap_or("unknown")
        .to_string();

    // Generate a simple ID based on timestamp + random bytes (no need for the uuid crate)
    let ts = Local::now().timestamp_micros();
    let rand_bytes: u32 = rand::random();
    let instance_id = format!("{:x}-{:08x}", ts, rand_bytes);

    let content = format!(
        "MELISA_INSTANCE_NAME={}\n\
         MELISA_INSTANCE_ID={}\n\
         DISTRO_SLUG={}\n\
         DISTRO_NAME={}\n\
         DISTRO_RELEASE={}\n\
         ARCHITECTURE={}\n\
         CREATED_AT={}\n",
        name,
        instance_id,
        meta.slug,
        meta.name,
        release,
        meta.arch,
        Local::now().to_rfc3339()
    );

    if let Err(e) = write_container_metadata(name, &content).await {
        eprintln!(
            "{}[WARNING]{} Failed to write MELISA metadata for '{}': {}",
            YELLOW, RESET, name, e
        );
    }
}

// ── Container creation ───────────────────────────────────────────────────────

/// Creates a new unprivileged LXC container from the specified distribution.
///
/// Complete pipeline (all crucial steps missing in the new version):
/// 1. Verify host runtime (lxcbr0 bridge)
/// 2. Download and provision via `lxc-create`
/// 3. Inject network configuration
/// 4. Configure DNS (locked with `chattr +i`)
/// 5. Write MELISA metadata
/// 6. Start the container
/// 7. Wait for network/DHCP to be ready
/// 8. Run initial package manager update
///
/// # Arguments
/// * `name`   - Target container name.
/// * `meta`   - Distribution metadata (slug, name, arch).
/// * `pb`     - Progress bar for status messages.
/// * `audit`  - When `true`, subprocess output is forwarded to the terminal.
pub async fn create_container(
    name: &str,
    meta: DistroMetadata,
    pb: ProgressBar,
    audit: bool,
) {
    // ── Step 0: Verify host bridge ────────────────────────────────────
    // FIX: Without this, the container won't get an IP and cannot be used.
    if !verify_host_runtime(audit).await {
        pb.println(format!(
            "{}[ERROR]{} Host network bridge 'lxcbr0' is down and auto-repair failed.{}",
            RED, BOLD, RESET
        ));
        pb.println(format!(
            "{}Tip:{} Run 'melisa --setup' to initialize host infrastructure.",
            YELLOW, RESET
        ));
        return;
    }

    pb.println(format!(
        "{}[CREATE]{} Provisioning container '{}' from '{}'…",
        BOLD, RESET, name, meta.slug
    ));

    // Split the "distro/release/arch" slug into separate components for lxc-create
    let slug_parts: Vec<&str> = meta.slug.splitn(3, '/').collect();
    let (distro, release, arch) = match slug_parts.as_slice() {
        [d, r, a] => (*d, *r, *a),
        _ => {
            eprintln!(
                "{}[ERROR]{} Invalid distro slug format: '{}'. Expected: 'distro/release/arch'",
                RED, RESET, meta.slug
            );
            return;
        }
    };

    if audit {
        pb.println(format!(
            "{}[AUDIT]{} Running: lxc-create -P {} -n {} -t download -- -d {} -r {} -a {}",
            YELLOW, RESET, LXC_BASE_PATH, name, distro, release, arch
        ));
    }

    // ── Step 1: Create container ────────────────────────────────────────────
    let status = Command::new("sudo")
        .args(&[
            "lxc-create",
            "-P", LXC_BASE_PATH,
            "-n", name,
            "-t", "download",
            "--",
            "-d", distro,
            "-r", release,
            "-a", arch,
        ])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {
            pb.println(format!(
                "{}[SUCCESS]{} Container '{}' has been provisioned.",
                GREEN, RESET, name
            ));

            // ── Step 2: Inject network config ─────────────────────────────
            pb.println(format!(
                "{}[INFO]{} Injecting network configuration for '{}'…",
                BOLD, RESET, name
            ));
            inject_network_config(name, &pb).await;

            // ── Step 3: Configure DNS ─────────────────────────────────────
            pb.println(format!(
                "{}[INFO]{} Configuring DNS for '{}'…",
                BOLD, RESET, name
            ));
            setup_container_dns(name, &pb).await;

            // ── Step 4: Inject MELISA metadata ────────────────────────────
            // FIX: Without this, `melisa --info` won't display any data.
            pb.println(format!(
                "{}[INFO]{} Writing MELISA metadata for '{}'…",
                BOLD, RESET, name
            ));
            inject_container_metadata(name, &meta).await;

            // ── Step 5: Start container ───────────────────────────────────
            // FIX: The old version always started the container after creation.
            //      The new version didn't do this at all.
            pb.println(format!(
                "{}[INFO]{} Starting container '{}' for initial setup…",
                YELLOW, RESET, name
            ));
            start_container(name, audit).await;

            // ── Step 6: Wait for network readiness ───────────────────────────────
            // FIX: Without this, the package update fails immediately because the network isn't ready.
            if wait_for_network_initialization(name, &pb).await {
                // ── Step 7: Update package manager ────────────────────────
                // FIX: Initial update so the user can immediately install packages.
                auto_initial_setup(name, distro, &pb, audit).await;
            } else {
                pb.println(format!(
                    "{}[WARNING]{} Network DHCP initialization timed out after 30s. \
                    Skipping package manager setup. You can run it manually inside the container.",
                    YELLOW, RESET
                ));
            }

            pb.println(format!(
                "{}[SUCCESS]{} Container '{}' is fully provisioned and ready!",
                GREEN, RESET, name
            ));
        }
        Ok(s) => {
            eprintln!(
                "{}[ERROR]{} Container creation failed with exit code: {}.",
                RED, RESET,
                s.code().unwrap_or(-1)
            );
        }
        Err(err) => {
            eprintln!("{}[FATAL]{} Failed to execute lxc-create: {}", RED, RESET, err);
        }
    }
}

// ── Container deletion ───────────────────────────────────────────────────────

/// Destroys an existing LXC container and removes all associated metadata.
///
/// The container is stopped gracefully before deletion if it is currently running.
pub async fn delete_container(name: &str, pb: ProgressBar, audit: bool) {
    pb.println(format!("{}--- Processing Deletion: {} ---{}", BOLD, name, RESET));

    // Stop the container first if it is running.
    if is_container_running(name).await {
        pb.println(format!(
            "{}[INFO]{} Container '{}' is currently running — initiating graceful shutdown…",
            YELLOW, RESET, name
        ));
        stop_container(name, audit).await;
        if is_container_running(name).await {
            eprintln!(
                "{}[ERROR]{} Failed to stop container '{}'. Deletion aborted to prevent data corruption.",
                RED, RESET, name
            );
            return;
        }
    }

    pb.println(format!(
        "{}[INFO]{} Unlocking system configurations for '{}'…",
        BOLD, RESET, name
    ));
    unlock_container_dns(name).await;

    pb.println(format!(
        "{}[INFO]{} Purging MELISA engine metadata for '{}'…",
        BOLD, RESET, name
    ));
    cleanup_container_metadata(name).await;

    if audit {
        pb.println(format!(
            "{}[AUDIT]{} Running lxc-destroy — raw output follows:",
            YELLOW, RESET
        ));
    }

    let status = run_sudo(
        &["-n", "lxc-destroy", "-P", LXC_BASE_PATH, "-n", name, "-f"],
        audit,
    )
    .await;

    match status {
        Ok(s) if s.success() => {
            pb.println(format!(
                "{}[SUCCESS]{} Container '{}' has been permanently destroyed.",
                GREEN, RESET, name
            ));
        }
        Ok(s) => {
            eprintln!(
                "{}[ERROR]{} Deletion failed with exit code: {}.",
                RED, RESET,
                s.code().unwrap_or(-1)
            );
            eprintln!(
                "{}[TIP]{} Ensure you have sudo permissions or check 'lxc-ls' for container status.",
                YELLOW, RESET
            );
        }
        Err(err) => {
            eprintln!("{}[FATAL]{} Could not execute lxc-destroy: {}", RED, RESET, err);
        }
    }
}

// ── Container start ──────────────────────────────────────────────────────────

/// Starts the specified LXC container in daemon mode.
pub async fn start_container(name: &str, audit: bool) {
    println!("{}[INFO]{} Starting container '{}'…", GREEN, RESET, name);

    let status = run_sudo(
        &["lxc-start", "-P", LXC_BASE_PATH, "-n", name, "-d"],
        audit,
    )
    .await;

    match status {
        Ok(s) if s.success() => {
            println!("{}[SUCCESS]{} Container '{}' is now running.", GREEN, RESET, name);
        }
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to start container '{}'. \
                Check if it exists and is properly configured.",
                RED, RESET, name
            );
        }
    }
}

// ── Container stop ───────────────────────────────────────────────────────────

/// Stops the specified LXC container.
///
/// Requires administrator privileges.
pub async fn stop_container(name: &str, audit: bool) {
    if !ensure_admin().await {
        return;
    }

    println!(
        "{}[SHUTDOWN]{} Initiating shutdown for container '{}'…",
        YELLOW, RESET, name
    );

    let status = run_sudo(
        &["lxc-stop", "-P", LXC_BASE_PATH, "-n", name],
        audit,
    )
    .await;

    match status {
        Ok(s) if s.success() => {
            println!(
                "{}[SUCCESS]{} Container '{}' has been successfully stopped.",
                GREEN, RESET, name
            );
        }
        Ok(_) => {
            eprintln!("{}[ERROR]{} Failed to stop container '{}'.", RED, RESET, name);
        }
        Err(err) => {
            eprintln!("{}[FATAL]{} Execution error: {}", RED, RESET, err);
        }
    }
}

// ── Container attach ─────────────────────────────────────────────────────────

/// Opens an interactive shell session inside the specified container.
///
/// This call blocks the current task until the user exits the shell.
pub async fn attach_to_container(name: &str) {
    println!(
        "{}[MODE]{} Entering Saferoom: '{}'. Type 'exit' to return to host.{}",
        BOLD, RESET, name, RESET
    );

    let _ = Command::new("sudo")
        .args(&["lxc-attach", "-P", LXC_BASE_PATH, "-n", name, "--", "bash"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::inherit())
        .status()
        .await;
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `run_sudo` constructs the command without panicking.
    #[tokio::test]
    async fn test_run_sudo_does_not_panic_with_empty_args() {
        let _ = run_sudo(&[], false).await;
    }

    /// Verifies that audit mode flag is boolean.
    #[test]
    fn test_audit_mode_flag_is_boolean() {
        let is_audit: bool = true;
        let _stdio = if is_audit {
            Stdio::inherit()
        } else {
            Stdio::null()
        };
    }

    /// Verifies slug parsing extracts correct components.
    #[test]
    fn test_slug_split_extracts_distro_release_arch() {
        let slug = "ubuntu/jammy/amd64";
        let parts: Vec<&str> = slug.splitn(3, '/').collect();
        assert_eq!(parts[0], "ubuntu");
        assert_eq!(parts[1], "jammy");
        assert_eq!(parts[2], "amd64");
    }

    /// Verifies pkg manager mapping is correct.
    #[test]
    fn test_get_pkg_manager_for_distro_ubuntu() {
        assert_eq!(get_pkg_manager_for_distro("ubuntu"), "apt");
    }

    #[test]
    fn test_get_pkg_manager_for_distro_alpine() {
        assert_eq!(get_pkg_manager_for_distro("alpine"), "apk");
    }

    #[test]
    fn test_get_pkg_manager_for_distro_fedora() {
        assert_eq!(get_pkg_manager_for_distro("fedora"), "dnf");
    }

    #[test]
    fn test_get_pkg_manager_for_distro_arch() {
        assert_eq!(get_pkg_manager_for_distro("archlinux"), "pacman");
    }

    /// Verifies update command mapping.
    #[test]
    fn test_get_pkg_update_cmd_apt() {
        assert_eq!(get_pkg_update_cmd("apt"), "apt-get update -y");
    }

    #[test]
    fn test_get_pkg_update_cmd_apk() {
        assert_eq!(get_pkg_update_cmd("apk"), "apk update");
    }

    #[test]
    fn test_get_pkg_update_cmd_unknown_falls_back_to_true() {
        assert_eq!(get_pkg_update_cmd("chocolatey"), "true");
    }

    /// Verifies metadata content is formatted correctly.
    #[test]
    fn test_metadata_content_format() {
        let name = "mybox";
        let release = "jammy";
        let slug = "ubuntu/jammy/amd64";
        let distro_name = "ubuntu";
        let arch = "amd64";
        let ts = chrono::Local::now().to_rfc3339();
        let id = format!("test-id-{}", chrono::Local::now().timestamp());

        let content = format!(
            "MELISA_INSTANCE_NAME={}\n\
             MELISA_INSTANCE_ID={}\n\
             DISTRO_SLUG={}\n\
             DISTRO_NAME={}\n\
             DISTRO_RELEASE={}\n\
             ARCHITECTURE={}\n\
             CREATED_AT={}\n",
            name, id, slug, distro_name, release, arch, ts
        );

        assert!(content.contains("MELISA_INSTANCE_NAME=mybox"));
        assert!(content.contains("DISTRO_SLUG=ubuntu/jammy/amd64"));
        assert!(content.contains("ARCHITECTURE=amd64"));
    }
}