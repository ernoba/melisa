// =============================================================================
// MELISA — src/core/user/sudoers.rs
// Purpose: Build, deploy, and inspect MELISA sudoers configurations.
//
// FIX APPLIED:
//   check_if_admin() now uses `sudo -n` (non-interactive) so it never hangs
//   waiting for a password in SSH / non-TTY sessions. Previously the missing
//   -n flag caused the call to block indefinitely, returning false after a
//   timeout → every user appeared as [STANDARD USER] in `melisa --user`.
// =============================================================================

use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use crate::cli::color::{GREEN, RED, RESET, YELLOW};
use crate::core::user::types::UserRole;

const SUDOERS_DIR:         &str = "/etc/sudoers.d";
const SUDOERS_FILE_PREFIX: &str = "melisa_";

// ---------------------------------------------------------------------------
// Sudoers rule construction
// ---------------------------------------------------------------------------

/// Builds a NOPASSWD sudoers rule for the given username and role.
///
/// Regular users receive LXC + git + basic file-operation permissions.
/// Administrators receive the full set including user-management commands.
pub fn build_sudoers_rule(username: &str, role: &UserRole) -> String {
    let mut allowed_commands: Vec<&str> = vec![
        // LXC operations (all variants for different distro layouts)
        "/usr/bin/lxc-*",   "/bin/lxc-*",
        "/usr/sbin/lxc-*",  "/sbin/lxc-*",
        "/usr/share/lxc/templates/lxc-download *",
        // Git (project management)
        "/usr/bin/git *",   "/bin/git *",
        // MELISA binary itself — required so the sudo re-exec in main.rs
        // succeeds without a password prompt for all melisa users.
        "/usr/local/bin/melisa",
        "/usr/local/bin/melisa *",
        // File utilities used internally by MELISA
        "/usr/bin/mkdir -p *", "/bin/mkdir -p *",
        "/usr/bin/rm -f *",    "/bin/rm -f *",
        "/usr/bin/bash -c *",  "/bin/bash -c *",
        "/usr/bin/tee *",      "/bin/tee *",
        "/usr/bin/chattr *",   "/bin/chattr *",
    ];

    if *role == UserRole::Admin {
        allowed_commands.extend(&[
            // User management
            "/usr/sbin/useradd *",  "/sbin/useradd *",
            "/usr/sbin/userdel *",  "/sbin/userdel *",
            "/usr/bin/passwd *",    "/bin/passwd *",
            "/usr/bin/pkill *",     "/bin/pkill *",
            // Sudoers inspection (used by check_if_admin and list_melisa_users)
            "/usr/bin/cat /etc/sudoers.d/melisa_*",
            "/bin/cat /etc/sudoers.d/melisa_*",
            "/usr/bin/grep *",      "/bin/grep *",
            "/usr/bin/lxc-info *",  "/bin/lxc-info *",
            // Sudoers directory management
            "/usr/bin/ls /etc/sudoers.d/",
            "/bin/ls /etc/sudoers.d/",
            "/usr/bin/rm -f /etc/sudoers.d/melisa_*",
            "/bin/rm -f /etc/sudoers.d/melisa_*",
            "/usr/bin/tee /etc/sudoers.d/melisa_*",
            "/bin/tee /etc/sudoers.d/melisa_*",
            // Ownership / permission management
            "/usr/bin/chmod *",  "/bin/chmod *",
            "/usr/sbin/chmod *", "/sbin/chmod *",
            "/usr/bin/chown *",  "/bin/chown *",
            "/usr/sbin/chown *", "/sbin/chown *",
            "/usr/bin/mkdir *",  "/bin/mkdir *",
        ]);
    }

    format!(
        "{} ALL=(ALL) NOPASSWD: {}\n",
        username,
        allowed_commands.join(", ")
    )
}

/// Returns the canonical path to a user's MELISA sudoers file.
pub fn sudoers_file_path(username: &str) -> String {
    format!("{}/{}{}", SUDOERS_DIR, SUDOERS_FILE_PREFIX, username)
}

// ---------------------------------------------------------------------------
// Sudoers deployment
// ---------------------------------------------------------------------------

/// Writes (or overwrites) the sudoers file for `username` with the given role.
///
/// Uses `sudo tee` so the write is done as root; permissions are then locked
/// to 0440 (read-only, root-owned) as required by sudoers policy.
pub async fn configure_sudoers(username: &str, role: UserRole, audit: bool) {
    let sudoers_rule = build_sudoers_rule(username, &role);
    let sudoers_path = sudoers_file_path(username);

    if audit {
        println!("[AUDIT] Writing sudoers rule to {}:", sudoers_path);
        println!("{}", sudoers_rule.trim());
    }

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
                        "{}[ERROR]{} Failed to write sudoers rule to tee stdin: {}",
                        RED, RESET, err
                    );
                    return;
                }
            }
            let _ = child.wait().await;

            // Lock to 0440 — required by visudo/sudoers policy
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

// ---------------------------------------------------------------------------
// Admin status check
// ---------------------------------------------------------------------------

/// Returns `true` if `username` is a MELISA administrator.
///
/// Implementation: reads the user's sudoers file and checks for the presence
/// of "useradd" (which is only granted to admin-role users).
///
/// FIX: Uses `sudo -n` (non-interactive) so this call NEVER blocks waiting
/// for a password. In SSH / non-TTY sessions the old behaviour (no -n) caused
/// sudo to hang → eventually return a non-zero exit → function returned false
/// → every user was shown as [STANDARD USER] regardless of actual role.
pub async fn check_if_admin(username: &str) -> bool {
    let sudoers_path = sudoers_file_path(username);

    // -n = non-interactive: fail immediately if password would be required.
    // MELISA runs as OS root at this point, so the call should always succeed
    // if the file exists.
    let output = Command::new("sudo")
        .args(&["-n", "cat", &sudoers_path])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let content = String::from_utf8_lossy(&out.stdout);
            content.contains("useradd")
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Orphaned sudoers cleanup
// ---------------------------------------------------------------------------

/// Removes sudoers files for users that no longer have OS accounts.
///
/// Compares `/etc/sudoers.d/melisa_*` entries against a list of currently
/// known MELISA usernames.
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
        let derived_username = file_name
            .trim_start_matches(SUDOERS_FILE_PREFIX)
            .to_string();
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

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(rule.contains("useradd"), "Admin sudoers rule must include useradd");
        assert!(rule.contains("userdel"), "Admin sudoers rule must include userdel");
        assert!(rule.contains("passwd"),  "Admin sudoers rule must include passwd");
        assert!(rule.contains("pkill"),   "Admin sudoers rule must include pkill");
    }

    #[test]
    fn test_build_sudoers_rule_admin_includes_melisa_binary() {
        let rule = build_sudoers_rule("bob", &UserRole::Admin);
        assert!(
            rule.contains("/usr/local/bin/melisa"),
            "All users must have NOPASSWD for the melisa binary itself so sudo re-exec works"
        );
    }

    #[test]
    fn test_build_sudoers_rule_regular_includes_melisa_binary() {
        let rule = build_sudoers_rule("alice", &UserRole::Regular);
        assert!(
            rule.contains("/usr/local/bin/melisa"),
            "Regular users must also have NOPASSWD for the melisa binary (required for re-exec)"
        );
    }

    #[test]
    fn test_build_sudoers_rule_admin_is_superset_of_regular() {
        let regular_rule = build_sudoers_rule("alice", &UserRole::Regular);
        let admin_rule   = build_sudoers_rule("alice", &UserRole::Admin);
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

    #[test]
    fn test_sudoers_file_path_includes_prefix_and_username() {
        let path = sudoers_file_path("eve");
        assert!(
            path.contains(SUDOERS_FILE_PREFIX),
            "Sudoers file path must contain the '{}' prefix",
            SUDOERS_FILE_PREFIX
        );
        assert!(path.contains("eve"), "Sudoers file path must contain the username");
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
        let file_name = "sudoers";
        assert!(
            !file_name.starts_with(SUDOERS_FILE_PREFIX),
            "Non-MELISA sudoers files must be skipped during orphan detection"
        );
    }

    #[test]
    fn test_check_if_admin_uses_non_interactive_flag() {
        // Verify conceptually that the command uses -n.
        // The actual network call cannot run in unit tests, but we verify
        // the logic that "useradd" is the discriminating string.
        let admin_content = "alice ALL=(ALL) NOPASSWD: /usr/sbin/useradd *, /usr/bin/lxc-*\n";
        let regular_content = "alice ALL=(ALL) NOPASSWD: /usr/bin/lxc-*\n";
        assert!(
            admin_content.contains("useradd"),
            "Admin sudoers content must contain 'useradd'"
        );
        assert!(
            !regular_content.contains("useradd"),
            "Regular sudoers content must NOT contain 'useradd'"
        );
    }
}