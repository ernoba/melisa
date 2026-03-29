// ============================================================================
// src/core/user/sudoers.rs
//
// Sudoers policy management for MELISA-managed user accounts.
//
// Responsibilities:
//   - Build per-role sudoers rules.
//   - Deploy rules via `sudo tee`.
//   - Detect and remove orphaned sudoers files.
// ============================================================================

use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::cli::color::{GREEN, RED, RESET, YELLOW};
use crate::core::user::types::UserRole;

// ── Constants ────────────────────────────────────────────────────────────────

/// Directory where per-user MELISA sudoers files are stored.
const SUDOERS_DIR: &str = "/etc/sudoers.d";

/// Prefix used for all MELISA-managed sudoers file names.
const SUDOERS_FILE_PREFIX: &str = "melisa_";

// ── Sudoers rule builder ─────────────────────────────────────────────────────

/// Builds the sudoers rule string for a given username and role.
///
/// Rules are constructed as a whitelist of allowed `sudo` commands.
/// Admins receive a superset of the standard user's allowed commands.
///
/// # Arguments
/// * `username` - The system username the rule applies to.
/// * `role`     - The access level determining which commands are permitted.
///
/// # Returns
/// A complete sudoers line ready to be written to `/etc/sudoers.d/`.
pub fn build_sudoers_rule(username: &str, role: &UserRole) -> String {
    // Base commands available to every MELISA user regardless of role.
    let mut allowed_commands: Vec<&str> = vec![
        "/usr/bin/lxc-*", "/bin/lxc-*",
        "/usr/sbin/lxc-*", "/sbin/lxc-*",
        "/usr/share/lxc/templates/lxc-download *",
        "/usr/bin/git *", "/bin/git *",
        "/usr/local/bin/melisa *",
        "/usr/bin/mkdir -p *", "/bin/mkdir -p *",
        "/usr/bin/rm -f *", "/bin/rm -f *",
        "/usr/bin/bash -c *", "/bin/bash -c *",
        "/usr/bin/tee *", "/bin/tee *",
        "/usr/bin/chattr *", "/bin/chattr *",
    ];

    // Administrator-only commands: user lifecycle, privilege management.
    if *role == UserRole::Admin {
        allowed_commands.extend(&[
            "/usr/sbin/useradd *", "/sbin/useradd *",
            "/usr/sbin/userdel *", "/sbin/userdel *",
            "/usr/bin/passwd *", "/bin/passwd *",
            "/usr/bin/pkill *", "/bin/pkill *",
            "/usr/bin/grep *", "/bin/grep *",
            "/usr/bin/lxc-info *", "/bin/lxc-info *",
            "/usr/bin/ls /etc/sudoers.d/", "/bin/ls /etc/sudoers.d/",
            "/usr/bin/rm -f /etc/sudoers.d/melisa_*",
            "/bin/rm -f /etc/sudoers.d/melisa_*",
            "/usr/bin/tee /etc/sudoers.d/melisa_*",
            "/bin/tee /etc/sudoers.d/melisa_*",
            "/usr/bin/chmod *", "/bin/chmod *",
            "/usr/sbin/chmod *", "/sbin/chmod *",
            "/usr/bin/chown *", "/bin/chown *",
            "/usr/sbin/chown *", "/sbin/chown *",
            "/usr/bin/mkdir *", "/bin/mkdir *",
        ]);
    }

    format!(
        "{} ALL=(ALL) NOPASSWD: {}\n",
        username,
        allowed_commands.join(", ")
    )
}

/// Returns the absolute path of the sudoers file for a given username.
///
/// # Arguments
/// * `username` - The system username.
pub fn sudoers_file_path(username: &str) -> String {
    format!("{}/{}{}", SUDOERS_DIR, SUDOERS_FILE_PREFIX, username)
}

// ── Sudoers deployment ───────────────────────────────────────────────────────

/// Writes a sudoers policy file for the specified user and applies 0440 permissions.
///
/// Uses `sudo tee` to write the file with elevated privileges, then `sudo chmod`
/// to enforce the required read-only permissions.
///
/// # Arguments
/// * `username` - Target system username.
/// * `role`     - Access role determining the rule content.
/// * `audit`    - When `true`, the generated rule is printed to the terminal.
pub async fn configure_sudoers(username: &str, role: UserRole, audit: bool) {
    let sudoers_rule = build_sudoers_rule(username, &role);
    let sudoers_path = sudoers_file_path(username);

    if audit {
        println!("[AUDIT] Writing sudoers rule to {}:", sudoers_path);
        println!("{}", sudoers_rule.trim());
    }

    // Write via `sudo tee` to gain elevated write access to /etc/sudoers.d/.
    let tee_process = Command::new("sudo")
        .args(&["tee", &sudoers_path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn();

    match tee_process {
        Ok(mut child) => {
            if let Some(mut stdin_pipe) = child.stdin.take() {
                if let Err(err) = stdin_pipe.write_all(sudoers_rule.as_bytes()).await {
                    eprintln!(
                        "{}[ERROR]{} Failed to write sudoers rule to stdin of tee: {}",
                        RED, RESET, err
                    );
                    return;
                }
            }

            let _ = child.wait().await;

            // Enforce strict 0440 (root-owned, read-only) permissions.
            let _ = Command::new("sudo")
                .args(&["chmod", "0440", &sudoers_path])
                .status()
                .await;

            println!(
                "{}[SUCCESS]{} Privilege configuration deployed for '{}'.",
                GREEN, RESET, username
            );
        }
        Err(err) => {
            eprintln!(
                "{}[FATAL]{} Failed to spawn tee process for sudoers deployment: {}",
                RED, RESET, err
            );
        }
    }
}

// ── Orphaned sudoers cleanup ─────────────────────────────────────────────────

/// Detects sudoers files in `/etc/sudoers.d/` whose corresponding user no
/// longer exists in `/etc/passwd`, and removes them.
///
/// A sudoers file is considered "orphaned" when it starts with `melisa_` but
/// the username extracted from the filename has no matching passwd entry.
///
/// # Arguments
/// * `existing_usernames` - A slice of usernames that are currently valid MELISA accounts.
pub async fn remove_orphaned_sudoers_files(existing_usernames: &[String]) {
    let files_output = Command::new("sudo")
        .args(&["ls", SUDOERS_DIR])
        .output()
        .await;

    let files_output = match files_output {
        Ok(out) if out.status.success() => out,
        _ => {
            println!(
                "{}[ERROR]{} Failed to access the {} directory.",
                RED, RESET, SUDOERS_DIR
            );
            return;
        }
    };

    let file_list = String::from_utf8_lossy(&files_output.stdout);

    for file_name in file_list.lines() {
        if !file_name.starts_with(SUDOERS_FILE_PREFIX) {
            continue;
        }

        let derived_username = file_name.trim_start_matches(SUDOERS_FILE_PREFIX).to_string();

        if !existing_usernames.contains(&derived_username) {
            println!(
                "{}[PURGING]{} Removing orphaned sudoers file: {}",
                YELLOW, RESET, file_name
            );
            let _ = Command::new("sudo")
                .args(&["rm", "-f", &format!("{}/{}", SUDOERS_DIR, file_name)])
                .status()
                .await;
        }
    }
}

/// Checks whether a user account has admin-level sudoers privileges by
/// inspecting the presence of admin-only commands in their sudoers file.
///
/// # Arguments
/// * `username` - The system username to check.
///
/// # Returns
/// `true` if the user's sudoers file contains admin-level permissions.
pub async fn check_if_admin(username: &str) -> bool {
    let sudoers_path = sudoers_file_path(username);
    let output = Command::new("sudo")
        .args(&["cat", &sudoers_path])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let content = String::from_utf8_lossy(&out.stdout);
            // The `useradd` command is an admin-only permission — its presence
            // is the canonical indicator that a user has the Admin role.
            content.contains("useradd")
        }
        _ => false,
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_sudoers_rule ───────────────────────────────────────────────────

    #[test]
    fn test_build_sudoers_rule_regular_user_contains_lxc_commands() {
        let rule = build_sudoers_rule("alice", &UserRole::Regular);
        assert!(
            rule.contains("/usr/bin/lxc-*"),
            "Regular user sudoers rule must include LXC commands"
        );
        assert!(
            rule.contains("/usr/bin/git *"),
            "Regular user sudoers rule must include git commands"
        );
    }

    #[test]
    fn test_build_sudoers_rule_regular_user_excludes_useradd() {
        let rule = build_sudoers_rule("alice", &UserRole::Regular);
        assert!(
            !rule.contains("useradd"),
            "Regular user sudoers rule must NOT include useradd"
        );
        assert!(
            !rule.contains("userdel"),
            "Regular user sudoers rule must NOT include userdel"
        );
        assert!(
            !rule.contains("passwd"),
            "Regular user sudoers rule must NOT include passwd"
        );
    }

    #[test]
    fn test_build_sudoers_rule_admin_includes_user_management_commands() {
        let rule = build_sudoers_rule("bob", &UserRole::Admin);
        assert!(
            rule.contains("useradd"),
            "Admin sudoers rule must include useradd"
        );
        assert!(
            rule.contains("userdel"),
            "Admin sudoers rule must include userdel"
        );
        assert!(
            rule.contains("passwd"),
            "Admin sudoers rule must include passwd"
        );
        assert!(
            rule.contains("pkill"),
            "Admin sudoers rule must include pkill"
        );
    }

    // sudoers.rs — di dalam mod tests
    #[test]
    fn test_build_sudoers_rule_admin_is_superset_of_regular() {
        let regular_rule = build_sudoers_rule("alice", &UserRole::Regular);
        let admin_rule   = build_sudoers_rule("alice", &UserRole::Admin); // sama: "alice"

        for cmd in regular_rule.split(", ") {
            let cmd_trimmed = cmd.trim().trim_end_matches('\n');
            assert!(
                admin_rule.contains(cmd_trimmed),
                "Admin rule must be a superset of the regular rule; missing: '{}'",
                cmd_trimmed
            );
        }
    }
    #[test]
    fn test_build_sudoers_rule_format_contains_username() {
        let rule = build_sudoers_rule("charlie", &UserRole::Regular);
        assert!(
            rule.starts_with("charlie ALL=(ALL) NOPASSWD:"),
            "Sudoers rule must start with '<username> ALL=(ALL) NOPASSWD:'"
        );
    }

    #[test]
    fn test_build_sudoers_rule_ends_with_newline() {
        let rule = build_sudoers_rule("dave", &UserRole::Admin);
        assert!(
            rule.ends_with('\n'),
            "Sudoers rule must end with a newline character for sudoers compatibility"
        );
    }

    // ── sudoers_file_path ────────────────────────────────────────────────────

    #[test]
    fn test_sudoers_file_path_includes_prefix_and_username() {
        let path = sudoers_file_path("eve");
        assert!(
            path.contains(SUDOERS_FILE_PREFIX),
            "Sudoers file path must contain the '{}' prefix",
            SUDOERS_FILE_PREFIX
        );
        assert!(
            path.contains("eve"),
            "Sudoers file path must contain the username"
        );
        assert!(
            path.starts_with(SUDOERS_DIR),
            "Sudoers file path must be under {}",
            SUDOERS_DIR
        );
    }

    #[test]
    fn test_sudoers_file_path_format_is_predictable() {
        assert_eq!(
            sudoers_file_path("frank"),
            "/etc/sudoers.d/melisa_frank",
            "Sudoers file path must follow the '/etc/sudoers.d/melisa_<username>' pattern"
        );
    }

    // ── remove_orphaned_sudoers_files ────────────────────────────────────────

    #[test]
    fn test_orphan_detection_identifies_non_matching_file() {
        let existing_users = vec!["alice".to_string(), "bob".to_string()];
        let file_name = "melisa_charlie";
        let derived_user = file_name.trim_start_matches(SUDOERS_FILE_PREFIX).to_string();

        assert!(
            !existing_users.contains(&derived_user),
            "User 'charlie' should be identified as orphaned (not in existing users list)"
        );
    }

    #[test]
    fn test_orphan_detection_skips_matching_file() {
        let existing_users = vec!["alice".to_string(), "bob".to_string()];
        let file_name = "melisa_alice";
        let derived_user = file_name.trim_start_matches(SUDOERS_FILE_PREFIX).to_string();

        assert!(
            existing_users.contains(&derived_user),
            "User 'alice' must NOT be identified as orphaned"
        );
    }

    #[test]
    fn test_orphan_detection_skips_non_melisa_files() {
        let file_name = "sudoers"; // default sudoers, not managed by MELISA
        assert!(
            !file_name.starts_with(SUDOERS_FILE_PREFIX),
            "Non-MELISA sudoers files must be skipped during orphan detection"
        );
    }
}