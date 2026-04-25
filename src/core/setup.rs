// ============================================================================
// src/core/setup.rs
//
// MELISA host environment initialization (`melisa --setup`).
//
// Provisions the host machine so it can run MELISA and manage LXC containers.
// This must be run directly on the physical console — NOT over SSH — because
// it configures network interfaces and firewall rules that could lock out a
// remote administrator.
//
// Steps:
//  1.  SSH session detection (refuse if running over SSH)
//  2.  LXC package installation
//  3.  SSH server installation
//  4.  Binary self-copy to /usr/local/bin
//  5.  Firewall configuration (SSH + lxcbr0)
//  6.  LXC network quota (lxc-usernet)
//  7.  Jail shell registration (/etc/shells)
//  8.  Sudoers access for the MELISA binary
//  9.  UID-map SUID bits (newuidmap / newgidmap)
//  10. Master projects directory
//  11. Global Git safety override
//  12. /home privacy hardening
//  13. LXC sub-ID mapping for the calling user
// ============================================================================

use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::cli::color::{BOLD, CYAN, GREEN, RED, YELLOW, RESET};
use crate::core::project::management::PROJECTS_MASTER_PATH;
use crate::distros::host_distro::{detect_host_distro, get_distro_config, FirewallKind};

use crate::core::user::{build_sudoers_rule, configure_sudoers, check_if_admin, UserRole};
use crate::core::container::network::is_virtualised_environment; // Use existing detection
// ── Entry point ───────────────────────────────────────────────────────────────

/// Runs the full host environment initialization sequence.
///
/// Detects the host Linux distribution and installs all required packages,
/// configures networking, registers the jail shell, and hardens system privacy.
pub async fn install_host_environment() {
    // Smarter logic
    if is_risky_remote_session().await {
        // Check if user provided --force-unsafe flag to bypass manual confirmation
        let args: Vec<String> = env::args().collect();
        if !args.contains(&"--force-unsafe".to_string()) {
            eprintln!("{}[BLOCKED]{} Remote SSH session detected.", RED, RESET);
            eprintln!(
                "{}[SAFETY]{} Setup stopped to prevent firewall lockout.",
                BOLD, RESET
            );
            eprintln!(
                "{}[INFO]{} If you are sure, run again with: {}melisa --setup --force-unsafe{}",
                YELLOW, RESET, BOLD, RESET
            );
            return;
        }
        println!("{}[WARNING]{} Running setup on remote session at user request.", YELLOW, RESET);
    }

    let host_distro = detect_host_distro().await;
    let mut distro_config = get_distro_config(&host_distro);

    // Override firewall ke iptables jika berjalan di VM/OrbStack
    // karena UFW umumnya tidak tersedia di environment virtual
    if is_virtualised_environment().await
        && distro_config.firewall_tool == FirewallKind::Ufw
    {
        println!(
            "{}[INFO]{} Virtualised environment detected — switching firewall to iptables.",
            YELLOW, RESET
        );
        distro_config.firewall_tool = FirewallKind::Iptables;
    }

    println!("\n{}════ MELISA HOST SETUP ════{}", BOLD, RESET);
    println!(
        "{}[INFO]{} Detected host distribution: {}{}{}",
        BOLD, RESET, CYAN, distro_config.name, RESET
    );

    install_lxc_packages(&distro_config.pkg_manager, &distro_config.lxc_packages).await;
    install_ssh_server(&distro_config.pkg_manager).await;
    copy_binary_to_system().await;
    setup_ssh_firewall(&distro_config.firewall_tool).await;
    setup_lxc_network_quota().await;
    register_melisa_shell().await;
    configure_system_sudoers_access().await;
    fix_uidmap_permissions().await;
    setup_projects_directory().await;
    configure_git_security().await;
    fix_system_privacy().await;

    // If we have a valid SUDO_USER, set up sub-ID mapping and admin privileges for them.
    if let Ok(username) = env::var("SUDO_USER") {
        if !username.is_empty() {
            setup_lxc_user_subid_mapping(&username).await;
            
            // Call setup-specific helper to configure privileges
            setup_host_user_admin_privileges(&username).await;
        }
    }

    println!("\n{}════ SETUP COMPLETE ════{}", GREEN, RESET);
    println!("{}[DONE]{} MELISA is ready. Run 'melisa' to start.", GREEN, RESET);
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Executes a command silently and returns `true` if it succeeded.
async fn execute_silent_task(
    program: &str,
    args: &[&str],
    description: &str,
    timeout_secs: u64,
) -> bool {
    let future = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match timeout(Duration::from_secs(timeout_secs), future).await {
        Ok(Ok(status)) if status.success() => {
            println!("  {:<50} [ {}OK{} ]", description, GREEN, RESET);
            true
        }
        Ok(Ok(status)) => {
            println!(
                "  {:<50} [ {}FAILED (Code: {}){} ]",
                description, RED,
                status.code().unwrap_or(-1), RESET
            );
            false
        }
        Ok(Err(err)) => {
            println!(
                "  {:<50} [ {}IO ERROR: {}{} ]",
                description, RED, err, RESET
            );
            false
        }
        Err(_) => {
            println!(
                "  {:<50} [ {}TIMEOUT{} ]",
                description, RED, RESET
            );
            false
        }
    }
}

/// Creates a backup copy of a file before modifying it.
async fn backup_file(path: &str) {
    let backup_path = format!("{}.melisa.bak", path);
    if Path::new(path).exists() && !Path::new(&backup_path).exists() {
        let _ = fs::copy(path, &backup_path).await;
    }
}

/// Installs LXC and related packages using the detected package manager.
async fn install_lxc_packages(pkg_manager: &str, lxc_packages: &[String]) {
    println!("\n{}Installing LXC Packages…{}", BOLD, RESET);
    let pkg_list: Vec<&str> = lxc_packages.iter().map(|s| s.as_str()).collect();

    let install_args: Vec<&str> = match pkg_manager {
        "apt-get" | "apt" => {
            let mut args = vec!["install", "-y"];
            args.extend(pkg_list.iter().copied());
            args
        }
        "dnf" | "yum" => {
            let mut args = vec!["install", "-y"];
            args.extend(pkg_list.iter().copied());
            args
        }
        "pacman" => {
            let mut args = vec!["-S", "--noconfirm"];
            args.extend(pkg_list.iter().copied());
            args
        }
        _ => {
            println!(
                "  {:<50} [ {}UNSUPPORTED PM{} ]",
                "Package installation", RED, RESET
            );
            return;
        }
    };

    execute_silent_task(
        pkg_manager,
        &install_args,
        "Installing LXC packages",
        300,
    )
    .await;
}

/// Installs the SSH server daemon.
async fn install_ssh_server(pkg_manager: &str) {
    println!("\n{}Installing SSH Server…{}", BOLD, RESET);
    let (pkg_name, update_cmd, install_args): (&str, &[&str], &[&str]) = match pkg_manager {
        "apt-get" | "apt" => (
            "openssh-server",
            &["update"],
            &["install", "-y", "openssh-server"],
        ),
        "dnf" | "yum" => ("openssh-server", &[], &["install", "-y", "openssh-server"]),
        "pacman" => ("openssh", &["-Sy", "--noconfirm"], &["-S", "--noconfirm", "openssh"]),
        _ => {
            println!(
                "  {:<50} [ {}UNSUPPORTED PM{} ]",
                "SSH server installation", RED, RESET
            );
            return;
        }
    };

    if !update_cmd.is_empty() {
        execute_silent_task(pkg_manager, update_cmd, "Updating package index", 120).await;
    }

    execute_silent_task(
        pkg_manager,
        install_args,
        &format!("Installing {}", pkg_name),
        180,
    )
    .await;
}

/// Copies the running MELISA binary to `/usr/local/bin/melisa`.
async fn copy_binary_to_system() {
    println!("\n{}Deploying MELISA Binary to System Path…{}", BOLD, RESET);

    let current_binary = match env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            println!(
                "  {:<50} [ {}RESOLVE ERROR: {}{} ]",
                "Locating binary", RED, err, RESET
            );
            return;
        }
    };

    let target_path = PathBuf::from("/usr/local/bin/melisa");

    // Skip if the binary is already in the system path.
    if current_binary == target_path {
        println!(
            "  {:<50} [ {}ALREADY IN PLACE{} ]",
            "Binary deployment", CYAN, RESET
        );
        return;
    }

    let copy_future = Command::new("sudo")
        .args(&[
            "cp",
            current_binary.to_str().unwrap_or(""),
            "/usr/local/bin/melisa",
        ])
        .status();

    match timeout(Duration::from_secs(30), copy_future).await {
        Ok(Ok(s)) if s.success() => {
            // Make the binary SUID so non-root MELISA users can escalate.
            execute_silent_task(
                "chmod",
                &["u+s", "/usr/local/bin/melisa"],
                "Applying SUID bit to MELISA binary",
                10,
            )
            .await;
        }
        Ok(Ok(s)) => println!(
            "  {:<50} [ {}FAILED (Code: {}){} ]",
            "Copying binary",
            RED,
            s.code().unwrap_or(-1),
            RESET
        ),
        Ok(Err(err)) => println!(
            "  {:<50} [ {}IO ERROR: {}{} ]",
            "Copying binary", RED, err, RESET
        ),
        Err(_) => println!(
            "  {:<50} [ {}TIMEOUT{} ]",
            "Binary copy timed out", RED, RESET
        ),
    }
}

// SSH session detection with smart IP filtering to allow localhost SSH but block remote sessions without console access.
async fn is_risky_remote_session() -> bool {
    // 1. Check if this is OrbStack or Local VM environment.
    // If yes, setup is always safe because operator has physical console access via Host.
    if is_virtualised_environment().await {
        return false; 
    }

    // 2. If SSH variable is not set, it's a local session (Safe).
    let ssh_conn = match env::var("SSH_CONNECTION") {
        Ok(val) => val,
        Err(_) => return false,
    };

    // 3. Analyze source IP (Smart IP Filtering).
    // SSH_CONNECTION format: "IP_CLIENT PORT_CLIENT IP_SERVER PORT_SERVER"
    let parts: Vec<&str> = ssh_conn.split_whitespace().collect();
    if let Some(client_ip) = parts.get(0) {
        // If connection is from localhost (127.0.0.1 or ::1), it's safe.
        if *client_ip == "127.0.0.1" || *client_ip == "::1" || *client_ip == "localhost" {
            return false;
        }
    }

    // 4. If we reach here and SSH_CONNECTION exists, this is a true remote session.
    true
}

async fn setup_ssh_firewall(firewall: &FirewallKind) {
    println!("\n{}Configuring Host Firewall…{}", BOLD, RESET);

    // 1. Gunakan variabel lokal untuk menghindari error async recursion.
    let fallback = FirewallKind::Iptables;
    let active_firewall: &FirewallKind = if matches!(firewall, FirewallKind::Ufw)
        && !Path::new("/usr/sbin/ufw").exists()
        && !Path::new("/sbin/ufw").exists()
    {
        println!(
            "  {:<50} [ {}SKIPPED — UFW not installed, using iptables{}  ]",
            "UFW setup", YELLOW, RESET
        );
        &fallback
    } else {
        firewall
    };

    match active_firewall {
        // Blok pertama diubah menjadi Firewalld (karena menggunakan firewall-cmd)
        FirewallKind::Firewalld => {
            let ssh_ok = execute_silent_task(
                "firewall-cmd",
                &["--add-service=ssh", "--permanent"],
                "Adding SSH service rule",
                10,
            )
            .await;
            
            let bridge_ok = execute_silent_task(
                "firewall-cmd",
                &["--zone=trusted", "--add-interface=lxcbr0", "--permanent"],
                "Assigning lxcbr0 to trusted zone",
                10,
            )
            .await;
            
            let reload_ok = execute_silent_task(
                "firewall-cmd",
                &["--reload"],
                "Reloading firewall rules",
                15,
            )
            .await;
            
            if ssh_ok && bridge_ok && reload_ok {
                println!(
                    "  {:<50} [ {}OK{} ]",
                    "Firewall: SSH and LXC bridge authorized", GREEN, RESET
                );
            }
        }
        
        FirewallKind::Ufw => {
            // Gunakan absolute path jika ditemukan untuk menghindari isu PATH
            let ufw_cmd = if Path::new("/usr/sbin/ufw").exists() {
                "/usr/sbin/ufw"
            } else {
                "ufw"
            };

            execute_silent_task(ufw_cmd, &["allow", "ssh"], "Allowing SSH via UFW", 10).await;
            execute_silent_task(ufw_cmd, &["allow", "in", "on", "lxcbr0"], "Trusting lxcbr0 in UFW", 10).await;
            execute_silent_task(ufw_cmd, &["--force", "enable"], "Enabling UFW", 10).await;
            execute_silent_task(ufw_cmd, &["reload"], "Reloading UFW", 10).await;
        }
        
        FirewallKind::Iptables => {
            // Aktifkan IP forwarding
            let sysctl_cmd = if Path::new("/usr/sbin/sysctl").exists() {
                "/usr/sbin/sysctl"
            } else {
                "sysctl"
            };
            
            execute_silent_task(
                sysctl_cmd,
                &["-w", "net.ipv4.ip_forward=1"],
                "Enabling IP forwarding",
                5,
            ).await;

            let iptables_cmd = if Path::new("/usr/sbin/iptables").exists() {
                "/usr/sbin/iptables"
            } else if Path::new("/bin/iptables").exists() {
                "/bin/iptables"
            } else {
                "iptables"
            };

            execute_silent_task(
                iptables_cmd,
                &["-A", "INPUT", "-p", "tcp", "--dport", "22", "-j", "ACCEPT"],
                "Allowing SSH via iptables",
                10,
            ).await;
            execute_silent_task(
                iptables_cmd,
                &["-A", "INPUT", "-i", "lxcbr0", "-j", "ACCEPT"],
                "Trusting lxcbr0 via iptables",
                10,
            ).await;
        }
    }
}

/// Adds the calling user to `/etc/lxc/lxc-usernet` for network quota.
async fn setup_lxc_network_quota() {
    let config_path = "/etc/lxc/lxc-usernet";
    println!("\n{}Configuring LXC Network Quota…{}", BOLD, RESET);

    let username = match env::var("SUDO_USER") {
        Ok(u) if !u.is_empty() => u,
        _ => return,
    };

    let quota_rule = format!("{} veth lxcbr0 10\n", username);
    backup_file(config_path).await;

    let existing_content = fs::read_to_string(config_path).await.unwrap_or_default();
    if existing_content.contains(&quota_rule) {
        println!(
            "  {:<50} [ {}SKIPPED{} ]",
            "Network quota already configured", CYAN, RESET
        );
        return;
    }

    match OpenOptions::new()
        .append(true)
        .create(true)
        .open(config_path)
        .await
    {
        Ok(mut file) => {
            if let Err(err) = file.write_all(quota_rule.as_bytes()).await {
                println!(
                    "  {:<50} [ {}IO ERROR{} ] {}",
                    "Network Quota", RED, RESET, err
                );
            } else {
                println!(
                    "  {:<50} [ {}OK{} ]",
                    format!("Network quota for '{}' assigned", username), GREEN, RESET
                );
            }
        }
        Err(err) => println!(
            "  {:<50} [ {}ACCESS DENIED{} ] {}",
            "Network Quota", RED, RESET, err
        ),
    }
}

/// Registers the MELISA binary as a valid login shell in `/etc/shells`.
async fn register_melisa_shell() {
    let shell_path = "/usr/local/bin/melisa";
    println!("\n{}Registering MELISA Jail Shell…{}", BOLD, RESET);
    backup_file("/etc/shells").await;

    let register_cmd = format!(
        "grep -qxF '{0}' /etc/shells || echo '{0}' >> /etc/shells",
        shell_path
    );

    execute_silent_task("sh", &["-c", &register_cmd], "Registering shell in /etc/shells", 10).await;
}

/// Writes a sudoers rule allowing any user to run `sudo melisa`.
async fn configure_system_sudoers_access() {
    println!("\n{}Configuring System Sudoers Access…{}", BOLD, RESET);
    
    // Use sudo to write the general system-wide melisa sudoers rule
    let sudo_rule = "# Allow any user to run MELISA without password\nALL ALL=(ALL) NOPASSWD: /usr/local/bin/melisa\n";
    let sudoers_file = "/etc/sudoers.d/melisa";
    let temp_file = "/tmp/melisa_system_sudoers.tmp";

    // Write to temp file
    let tee_process = Command::new("sudo")
        .args(&["tee", temp_file])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match tee_process {
        Ok(mut child) => {
            if let Some(mut stdin_pipe) = child.stdin.take() {
                if let Err(err) = stdin_pipe.write_all(sudo_rule.as_bytes()).await {
                    println!(
                        "  {:<50} [ {}IO ERROR{} ] {}",
                        "Sudoers Policy", RED, RESET, err
                    );
                    return;
                }
            }
            let _ = child.wait().await;
        }
        Err(err) => {
            println!(
                "  {:<50} [ {}FATAL{} ] {}",
                "Sudoers Policy", RED, RESET, err
            );
            return;
        }
    }

    // Validate with visudo
    let visudo_check = Command::new("sudo")
        .args(&["visudo", "-c", "-f", temp_file])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    match visudo_check {
        Ok(s) if s.success() => {
            // Move to destination and set permissions
            let mv_status = Command::new("sudo")
                .args(&["mv", temp_file, sudoers_file])
                .status()
                .await;

            if mv_status.map(|s| s.success()).unwrap_or(false) {
                let _ = Command::new("sudo")
                    .args(&["chmod", "0440", sudoers_file])
                    .status()
                    .await;
                println!("  {:<50} [ {}OK{} ]", "Sudoers rules deployed", GREEN, RESET);
            } else {
                let _ = Command::new("sudo")
                    .args(&["rm", "-f", temp_file])
                    .status()
                    .await;
                println!(
                    "  {:<50} [ {}FAILED{} ]",
                    "Applying sudoers rules", RED, RESET
                );
            }
        }
        _ => {
            let _ = Command::new("sudo")
                .args(&["rm", "-f", temp_file])
                .status()
                .await;
            println!(
                "  {:<50} [ {}VALIDATION FAILED{} ]",
                "Sudoers rules", RED, RESET
            );
        }
    }
}

/// Sets the SUID bit on `newuidmap` and `newgidmap` for unprivileged LXC containers.
async fn fix_uidmap_permissions() {
    println!("\n{}Fixing UID/GID Map Permissions…{}", BOLD, RESET);
    for path in &["/usr/bin/newuidmap", "/usr/bin/newgidmap"] {
        if Path::new(path).exists() {
            execute_silent_task(
                "chmod",
                &["u+s", path],
                &format!("Setting SUID on {}", path),
                5,
            )
            .await;
        } else {
            println!("  {:<50} [ {}MISSING{} ]", path, RED, RESET);
        }
    }
    execute_silent_task(
        "chmod",
        &["+x", "/var/lib/lxc"],
        "Enabling traversal on /var/lib/lxc",
        5,
    )
    .await;
}

/// Creates and secures the master projects directory.
async fn setup_projects_directory() {
    println!("\n{}Configuring Master Projects Infrastructure…{}", BOLD, RESET);

    match timeout(
        Duration::from_secs(10),
        Command::new("mkdir").args(&["-p", PROJECTS_MASTER_PATH]).status(),
    )
    .await
    {
        Ok(Ok(s)) if s.success() => {
            execute_silent_task(
                "chmod",
                &["1777", PROJECTS_MASTER_PATH],
                "Setting sticky bit (1777) on projects dir",
                5,
            )
            .await;
        }
        Ok(Ok(s)) => println!(
            "  {:<50} [ {}FAILED (Code: {}){} ]",
            "Projects directory creation",
            RED, s.code().unwrap_or(-1), RESET
        ),
        Ok(Err(err)) => println!(
            "  {:<50} [ {}IO ERROR: {}{} ]",
            "Projects directory", RED, err, RESET
        ),
        Err(_) => println!(
            "  {:<50} [ {}TIMEOUT{} ]",
            "Projects directory timed out", RED, RESET
        ),
    }
}

/// Sets `git config --system safe.directory '*'` to avoid ownership errors.
async fn configure_git_security() {
    println!("\n{}Configuring Global Git Security…{}", BOLD, RESET);
    execute_silent_task(
        "git",
        &["config", "--system", "--add", "safe.directory", "*"],
        "Setting global git safe.directory='*'",
        10,
    )
    .await;
}

/// Sets `chmod 711 /home` to prevent directory listing by non-owners.
async fn fix_system_privacy() {
    println!("\n{}Hardening System Privacy Boundaries…{}", BOLD, RESET);
    execute_silent_task(
        "chmod",
        &["711", "/home"],
        "Protecting /home directory indexing",
        10,
    )
    .await;
}

/// Adds sub-UID and sub-GID ranges for LXC namespace mapping.
async fn setup_lxc_user_subid_mapping(username: &str) {
    println!("\n{}Setting Up LXC Sub-ID Mapping for '{}'…{}", BOLD, username, RESET);
    execute_silent_task(
        "usermod",
        &[
            "--add-subuids", "100000-165535",
            "--add-subgids", "100000-165535",
            username,
        ],
        &format!("Mapping SubUID/SubGID for '{}'", username),
        15,
    )
    .await;
}

/// Grants the specified user MELISA administrator privileges by modifying their sudoers file.
/// Grants MELISA Administrator privileges to the host user cleanly during setup
async fn setup_host_user_admin_privileges(username: &str) {
    println!(
        "\n{}Granting MELISA Admin Privileges to Host User '{}'…{}",
        BOLD, username, RESET
    );
    if check_if_admin(username).await {
        println!(
            "  {:<50} [ {}SKIPPED{} ]",
            "Admin privileges already configured", CYAN, RESET
        );
        return;
    }
    // Delegasi sepenuhnya ke configure_sudoers() yang sudah ada visudo validation-nya
    configure_sudoers(username, UserRole::Admin, false).await;

    println!(
        "\n{}Granting MELISA Admin Privileges to Host User '{}'…{}",
        BOLD, username, RESET
    );

    // Check if user already has admin access.

    let sudoers_rule = build_sudoers_rule(username, &UserRole::Admin);
    let sudoers_path = format!("/etc/sudoers.d/melisa_{}", username);

    // Write sudoers rule directly using file system as root
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&sudoers_path)
        .await
    {
        Ok(mut file) => {
            if let Err(err) = file.write_all(sudoers_rule.as_bytes()).await {
                println!(
                    "  {:<50} [ {}IO ERROR{} ] {}",
                    "Deploying admin sudoers rule", RED, RESET, err
                );
            } else {
                // Set permissions to 0440 (required for sudoers files)
                execute_silent_task(
                    "chmod",
                    &["0440", &sudoers_path],
                    "Applying strict permissions (0440)",
                    5,
                )
                .await;

                println!(
                    "  {:<50} [ {}OK{} ]",
                    format!("Admin privileges granted to '{}'", username),
                    GREEN, RESET
                );
            }
        }
        Err(err) => println!(
            "  {:<50} [ {}ACCESS DENIED{} ] {}",
            "Deploying admin sudoers rule", RED, RESET, err
        ),
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_path_has_melisa_bak_suffix() {
        let original = "/etc/shells";
        let backup = format!("{}.melisa.bak", original);
        assert!(
            backup.ends_with(".melisa.bak"),
            "Backup path must end with '.melisa.bak'"
        );
    }

    #[test]
    fn test_quota_rule_format_is_valid() {
        let username = "testuser";
        let rule = format!("{} veth lxcbr0 10\n", username);
        assert!(rule.contains("veth"), "Quota rule must specify veth interface type");
        assert!(rule.contains("lxcbr0"), "Quota rule must target the lxcbr0 bridge");
        assert!(rule.contains("10"), "Quota rule must allow 10 veths");
        assert!(rule.ends_with('\n'), "Quota rule must end with newline");
    }

    #[test]
    fn test_projects_master_path_is_set() {
        assert!(
            !PROJECTS_MASTER_PATH.is_empty(),
            "PROJECTS_MASTER_PATH must not be empty"
        );
    }
}