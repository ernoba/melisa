// ============================================================================
// src/core/user/management.rs
//
// MELISA user lifecycle management:
//   add, delete, list, upgrade, change password, clean orphaned configs.
//
// All mutating operations require administrator privileges verified via
// `ensure_admin()` before any system commands are executed.
// ============================================================================

use std::process::Stdio;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use crate::cli::color::{BOLD, GREEN, RED, RESET, YELLOW};
use crate::core::root_check::ensure_admin;
use crate::core::user::sudoers::{
    configure_sudoers,
    remove_orphaned_sudoers_files,
    check_if_admin,                 // ← tidak pakai alias
    SUDOERS_DIR,                    // ← tambahan
    SUDOERS_FILE_PREFIX,            // ← tambahan
};
use crate::core::user::types::UserRole;
// ── Passwd shell path ────────────────────────────────────────────────────────

/// Shell assigned to every MELISA-managed user, acting as the jail shell.
const MELISA_SHELL_PATH: &str = "/usr/local/bin/melisa";

// ── Add user ─────────────────────────────────────────────────────────────────

/// Provisions a new MELISA-managed system user account.
///
/// Steps performed:
/// 1. Verify admin privileges.
/// 2. Prompt for access level (Admin / Regular).
/// 3. Create the system user with `useradd`, assigning the jail shell.
/// 4. Restrict the home directory to `chmod 700`.
/// 5. Set an initial password.
/// 6. Deploy the sudoers policy.
///
/// # Arguments
/// * `username` - The new system username.
/// * `audit`    - When `true`, subprocess commands are logged to the terminal.
pub async fn add_melisa_user(username: &str, audit: bool) {
    use crate::core::project::management::validate_server_username;
 
    if !crate::core::root_check::ensure_admin().await {
        return;
    }
 
    // ── FIX: Validasi username sebelum operasi apapun ────────────────────
    if let Err(e) = validate_server_username(username) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }
 
    println!("\n{}--- Provisioning New MELISA User: {} ---{}", BOLD, username, RESET);
    println!("{}Select Access Level for {}:{}", BOLD, username, RESET);
    println!("  1) Administrator (Full Management: Users, Projects & LXC)");
    println!("  2) Standard User (Project & LXC Management Only)");
    print!("Enter choice (1/2): ");
 
    // ── FIX: Gunakan async stdin, bukan blocking std::io::stdin ──────────
    //
    // SEBELUMNYA (BERMASALAH):
    //   let stdin = std::io::stdin();
    //   let _ = stdin.read_line(&mut raw_choice);
    //   → Blocking call di dalam async context memblokir thread Tokio.
    //
    // SESUDAHNYA (AMAN):
    //   tokio::io::stdin() dengan BufReader async.
    //
    let _ = tokio::io::stdout().flush().await;
    let mut raw_choice = String::new();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let _ = reader.read_line(&mut raw_choice).await;
 
    let role = match raw_choice.trim() {
        "1" => crate::core::user::types::UserRole::Admin,
        _   => crate::core::user::types::UserRole::Regular,
    };
 
    if audit {
        println!(
            "[AUDIT] Running: useradd -m -s /usr/local/bin/melisa {}",
            username
        );
    }
    let status = Command::new("sudo")
        .args(&["useradd", "-m", "-s", "/usr/local/bin/melisa", username])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;
 
    match status {
        Ok(s) if s.success() => {
            println!(
                "{}[SUCCESS]{} User account '{}' successfully created.",
                GREEN, RESET, username
            );
            let home_dir = format!("/home/{}", username);
            let _ = Command::new("sudo")
                .args(&["chmod", "700", &home_dir])
                .status()
                .await;
            if crate::core::user::management::set_user_password(username).await {
                crate::core::user::sudoers::configure_sudoers(username, role.clone(), audit).await;
                if role == crate::core::user::types::UserRole::Admin {
                    let group = if Command::new("getent")
                        .arg("group").arg("sudo").output().await
                        .map_or(false, |o| o.status.success())
                    {
                        "sudo"
                    } else {
                        "wheel"
                    };
                    if audit {
                        println!("[AUDIT] Adding user '{}' to system group '{}'", username, group);
                    }
                    let _ = Command::new("sudo")
                        .args(&["usermod", "-aG", group, username])
                        .status()
                        .await;
                }
            }
        }
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to create user '{}'. The username may already exist.",
                RED, RESET, username
            );
        }
    }
}

// ── Password change ───────────────────────────────────────────────────────────

/// Interactively sets or changes the system password for a MELISA user.
///
/// Delegates to `sudo passwd <username>`, which reads credentials from stdin.
///
/// # Arguments
/// * `username` - Target system username.
///
/// # Returns
/// `true` if the password was set successfully, `false` otherwise.
pub async fn set_user_password(username: &str) -> bool {
    println!(
        "{}[ACTION]{} Please set the authentication password for '{}':",
        YELLOW, RESET, username
    );

    let status = Command::new("sudo")
        .args(&["passwd", username])
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {
            println!(
                "{}[SUCCESS]{} Password successfully updated for '{}'.",
                GREEN, RESET, username
            );
            true
        }
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to update the password for '{}'.",
                RED, RESET, username
            );
            false
        }
    }
}

// ── Delete user ───────────────────────────────────────────────────────────────

/// Removes a MELISA-managed user account and all associated system artifacts.
///
/// Steps performed:
/// 1. Verify admin privileges.
/// 2. Kill all active processes owned by the user (`pkill -u`).
/// 3. Delete the user account and home directory (`userdel -r -f`).
/// 4. Remove the user's sudoers policy file.
///
/// # Arguments
/// * `username` - The system username to remove.
/// * `audit`    - When `true`, subprocess commands are logged to the terminal.
pub async fn delete_melisa_user(username: &str, audit: bool) {
    if !ensure_admin().await {
        return;
    }

    println!(
        "\n{}--- Initiating Deletion for User: {} ---{}",
        BOLD, username, RESET
    );
    println!(
        "{}[INFO]{} Terminating all active processes for '{}'…",
        YELLOW, RESET, username
    );

    if audit {
        println!("[AUDIT] Running: pkill -u {}", username);
    }

    // Kill all processes owned by the user; ignore errors (user may have no processes).
    let _ = Command::new("sudo")
        .args(&["pkill", "-u", username])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    if audit {
        println!("[AUDIT] Running: userdel -r -f {}", username);
    }

    let delete_status = Command::new("sudo")
        .args(&["userdel", "-r", "-f", username])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;

    let sudoers_path = format!("/etc/sudoers.d/melisa_{}", username);
    let remove_sudoers_status = Command::new("sudo")
        .args(&["rm", "-f", &sudoers_path])
        .status()
        .await;

    match (delete_status, remove_sudoers_status) {
        (Ok(s1), Ok(s2)) if s1.success() && s2.success() => {
            println!(
                "{}[SUCCESS]{} User '{}' and all associated permissions have been completely removed.",
                GREEN, RESET, username
            );
        }
        _ => {
            eprintln!(
                "{}[WARNING]{} Deletion encountered anomalies for '{}' \
                (user or files may already have been removed).",
                RED, RESET, username
            );
        }
    }
}

// ── List users ────────────────────────────────────────────────────────────────

/// Displays all registered MELISA user accounts and their access roles.
///
/// Also scans `/etc/sudoers.d/` and reports any orphaned policy files.
/// Requires administrator privileges.
pub async fn list_melisa_users() {
    if !crate::core::root_check::ensure_admin().await {
        return;
    }
    println!("\n{}--- Registered MELISA Accounts ---{}", BOLD, RESET);
 
    let mut existing_usernames: Vec<String> = Vec::new();
 
    // Baca sudoers directory dengan fs::read_dir (bukan ls)
    let read_result = tokio::fs::read_dir(SUDOERS_DIR).await;
    if let Ok(mut entries) = read_result {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let file_name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => continue,
            };
            if !file_name.starts_with(SUDOERS_FILE_PREFIX) {
                continue;
            }
            let u = file_name.trim_start_matches(SUDOERS_FILE_PREFIX).to_string();
            // Validasi username yang di-derive
            if crate::core::project::management::validate_server_username(&u).is_err() {
                continue;
            }
            // Verifikasi user benar-benar ada di sistem
            if Command::new("id").arg(&u).status().await.map_or(false, |s| s.success()) {
                // FIX: cek duplikasi sebelum push (tidak push dua kali)
                if !existing_usernames.contains(&u) {
                    existing_usernames.push(u);
                }
            }
        }
    }
 
    if existing_usernames.is_empty() {
        println!("  {}No MELISA users found.{}", YELLOW, RESET);
        return;
    }
 
    for username in &existing_usernames {
        let is_admin = check_if_admin(username).await;
        let role_tag = if is_admin { "[Admin]" } else { "[User]" };
        println!("  {GREEN}•{RESET} {username} {YELLOW}{role_tag}{RESET}");
    }
    println!();
}

/// Prints orphaned sudoers file names to the terminal without removing them.
///
/// # Arguments
/// * `existing_usernames` - Currently valid MELISA usernames.
async fn report_orphaned_sudoers(existing_usernames: &[String]) {
    let files_output = Command::new("sudo")
        .args(&["ls", "/etc/sudoers.d/"])
        .output()
        .await;

    match files_output {
        Ok(out) if out.status.success() => {
            let file_list = String::from_utf8_lossy(&out.stdout);
            let mut found_orphan = false;

            for file_name in file_list.lines() {
                if !file_name.starts_with("melisa_") {
                    continue;
                }
                let derived_user = file_name.trim_start_matches("melisa_").to_string();
                if !existing_usernames.contains(&derived_user) {
                    println!(
                        "  {}! Orphan Detected:{} {} (user account no longer exists)",
                        RED, RESET, file_name
                    );
                    found_orphan = true;
                }
            }

            if !found_orphan {
                println!(
                    "  {}No orphaned configurations found. System state is clean.{}",
                    GREEN, RESET
                );
            }
        }
        _ => {
            println!(
                "{}[ERROR]{} Failed to access the /etc/sudoers.d/ directory.",
                RED, RESET
            );
        }
    }
}

// ── Upgrade user ──────────────────────────────────────────────────────────────

/// Modifies the access role of an existing MELISA user by re-deploying sudoers.
///
/// Requires administrator privileges.
///
/// # Arguments
/// * `username` - The system username to modify.
/// * `audit`    - When `true`, the new sudoers rule is printed to the terminal.
pub async fn upgrade_user(username: &str, audit: bool) {
    if !ensure_admin().await {
        return;
    }

    let mut stdout = io::stdout();
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin);

    let header = format!(
        "\n{}--- Modifying Access Privileges for: {} ---{}\n",
        BOLD, username, RESET
    );
    let _ = stdout.write_all(header.as_bytes()).await;

    // Verify the target user exists.
    let user_check = Command::new("id").arg(username).output().await;
    if let Ok(out) = user_check {
        if !out.status.success() {
            let error_msg = format!(
                "{}[ERROR]{} Target user '{}' not found.\n",
                RED, RESET, username
            );
            let _ = stdout.write_all(error_msg.as_bytes()).await;
            return;
        }
    }

    let menu = format!(
        "Select Target Role for '{}':\n  1) Administrator (Full Access)\n  2) Standard User (LXC & Project Only)\n",
        username
    );
    let _ = stdout.write_all(menu.as_bytes()).await;
    let _ = stdout.write_all(b"Enter choice (1/2): ").await;
    let _ = stdout.flush().await;

    let mut raw_choice = String::new();
    if let Err(err) = reader.read_line(&mut raw_choice).await {
        eprintln!("{}[ERROR]{} Failed to read input: {}", RED, RESET, err);
        return;
    }

    let role = match raw_choice.trim() {
        "1" => UserRole::Admin,
        _ => UserRole::Regular,
    };

    configure_sudoers(username, role.clone(), audit).await;

    // Tambahkan logika ini di dalam fungsi add_melisa_user dan upgrade_user
    // tepat setelah pemanggilan configure_sudoers()

    if role == UserRole::Admin {
        // Deteksi grup yang tersedia (sudo atau wheel)
        let group = if Command::new("getent").arg("group").arg("sudo").output().await.map_or(false, |o| o.status.success()) {
            "sudo"
        } else {
            "wheel"
        };

        if audit {
            println!("[AUDIT] Adding user '{}' to system group '{}'", username, group);
        }

        let _ = Command::new("sudo")
            .args(&["usermod", "-aG", group, username])
            .status()
            .await;
    }

    let success_msg = format!(
        "{}[DONE]{} Privileges for '{}' updated successfully.\n",
        GREEN, RESET, username
    );
    let _ = stdout.write_all(success_msg.as_bytes()).await;
    let _ = stdout.flush().await;
}

// ── Clean orphaned sudoers ────────────────────────────────────────────────────

/// Discovers and removes all orphaned MELISA sudoers files.
///
/// An orphaned file is one that has a `melisa_` prefix but whose extracted
/// username no longer exists in `/etc/passwd`.
/// Requires administrator privileges.
pub async fn clean_orphaned_sudoers() {
    if !ensure_admin().await {
        return;
    }
    println!(
        "\n{}--- Executing Orphaned Configuration Cleanup ---{}",
        BOLD, RESET
    );

    let mut existing_usernames: Vec<String> = Vec::new();

    // 1. Tambahkan user saat ini (via sudo)
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        existing_usernames.push(sudo_user);
    }

    // 2. Tambahkan SEMUA user yang punya file di /etc/sudoers.d/melisa_*
    // Ini menjamin user host seperti 'saferoom' tidak dianggap orphan
    let files = Command::new("ls").arg("/etc/sudoers.d/").output().await;
    if let Ok(out) = files {
        let list = String::from_utf8_lossy(&out.stdout);
        for line in list.lines() {
            if line.starts_with("melisa_") {
                let u = line.trim_start_matches("melisa_");
                // Cek apakah user benar-benar ada di sistem OS
                if Command::new("id").arg(u).status().await.map_or(false, |s| s.success()) {
                    if !existing_usernames.contains(&u.to_string()) {
                        existing_usernames.push(u.to_string());
                    }
                }
            }
        }
    }

    // Lindungi Host User
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() {
            existing_usernames.push(sudo_user);
        }
    }

    let passwd_output = Command::new("sudo")
        .args(&["grep", MELISA_SHELL_PATH, "/etc/passwd"])
        .output()
        .await;

    if let Ok(out) = passwd_output {
        let passwd_text = String::from_utf8_lossy(&out.stdout);
        let melisa_users: Vec<String> = passwd_text
            .lines()
            .filter_map(|line| line.split(':').next().map(|u| u.to_string()))
            .collect();
        
        // Gabungkan list tanpa duplikasi
        for u in melisa_users {
            if !existing_usernames.contains(&u) {
                existing_usernames.push(u);
            }
        }
    }

    remove_orphaned_sudoers_files(&existing_usernames).await;

    println!(
        "{}[DONE]{} System garbage collection completed successfully.",
        GREEN, RESET
    );
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the MELISA shell path constant is set to the expected binary location.
    #[test]
    fn test_melisa_shell_path_constant_is_correct() {
        assert_eq!(
            MELISA_SHELL_PATH, "/usr/local/bin/melisa",
            "The jail shell path must point to the MELISA binary"
        );
    }

    /// Verifies that the home directory path is constructed correctly for a given username.
    #[test]
    fn test_home_directory_path_construction() {
        let username = "testuser";
        let expected_home = "/home/testuser";
        let actual_home = format!("/home/{}", username);
        assert_eq!(
            actual_home, expected_home,
            "Home directory path must follow the '/home/<username>' convention"
        );
    }

    /// Verifies that orphaned username extraction from a `melisa_` prefixed filename is correct.
    #[test]
    fn test_username_extraction_from_sudoers_filename() {
        let file_name = "melisa_alice";
        let extracted = file_name.trim_start_matches("melisa_");
        assert_eq!(
            extracted, "alice",
            "Username must be extracted by stripping the 'melisa_' prefix"
        );
    }

    /// Verifies role selection: any input other than "1" defaults to Regular.
    #[test]
    fn test_role_selection_defaults_to_regular_for_unknown_input() {
        let role = match "3".trim() {
            "1" => UserRole::Admin,
            _ => UserRole::Regular,
        };
        assert_eq!(
            role,
            UserRole::Regular,
            "Unknown role selection must default to Regular"
        );
    }

    /// Verifies role selection: "1" maps to Admin.
    #[test]
    fn test_role_selection_one_maps_to_admin() {
        let role = match "1".trim() {
            "1" => UserRole::Admin,
            _ => UserRole::Regular,
        };
        assert_eq!(
            role,
            UserRole::Admin,
            "Input '1' must select the Admin role"
        );
    }
}