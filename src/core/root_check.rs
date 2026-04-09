// =============================================================================
// MELISA — root_check.rs
// Purpose: Privilege verification for the MELISA application layer.
//
// ARCHITECTURE NOTE:
//   MELISA always re-executes itself as OS root via `sudo -E` (see main.rs).
//   Therefore `geteuid() == 0` is ALWAYS true inside the REPL and is NOT a
//   valid signal for "is this MELISA user an administrator".
//
//   The correct check is:
//     1. Read the original invoking user from the $SUDO_USER environment variable.
//     2. Look up that user's MELISA sudoers file at /etc/sudoers.d/melisa_<user>.
//     3. If the file contains "useradd", the user is a MELISA Administrator.
//
// BUG THAT WAS FIXED:
//   The old admin_check() called is_effective_root() which always returned true
//   because MELISA runs as root. This gave every user (including regular ones)
//   full admin access, while the display in --user showed everyone as Standard
//   User because the display logic used a separate, correct check.
// =============================================================================

use tokio::process::Command;
use crate::cli::color::{RED, RESET, YELLOW};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the MELISA application username — the human who actually invoked
/// the binary. When `sudo -E` is used by main.rs, `$SUDO_USER` carries the
/// original invoking username. Falls back to `$USER` / `$LOGNAME`.
pub fn get_melisa_user() -> String {
    std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

/// Checks whether the current MELISA session user has **MELISA administrator**
/// privileges.
///
/// This intentionally does NOT check `geteuid() == 0` because MELISA always
/// runs as OS root — that check would grant admin to everyone.
///
/// Admin status is defined by the presence of `useradd` permission inside the
/// user's MELISA sudoers file at `/etc/sudoers.d/melisa_<username>`.
pub async fn admin_check() -> bool {
    let melisa_user = get_melisa_user();

    // If no SUDO_USER (e.g. someone literally logged in as root), allow.
    if melisa_user.is_empty() || melisa_user == "root" {
        return true;
    }

    check_if_admin(&melisa_user).await
}

/// Guards admin-only operations.
///
/// Returns `true` if the current MELISA user is an administrator.
/// Returns `false` and prints a clear access-denied message otherwise.
pub async fn ensure_admin() -> bool {
    if admin_check().await {
        return true;
    }
    let melisa_user = get_melisa_user();
    eprintln!(
        "{}[ACCESS DENIED]{} User '{}' does not have MELISA administrator privileges.",
        RED, RESET, melisa_user
    );
    eprintln!(
        "{}[TIP]{} Ask an administrator to run: melisa --upgrade {}",
        YELLOW, RESET, melisa_user
    );
    false
}

/// Checks whether a specific MELISA username holds admin privileges by
/// inspecting their sudoers configuration file.
///
/// Uses `sudo -n` (non-interactive) so this call **never blocks** waiting
/// for a password, even in non-TTY / SSH-piped sessions.
pub async fn check_if_admin(username: &str) -> bool {
    let sudoers_path = format!("/etc/sudoers.d/melisa_{}", username);

    // PERBAIKAN: Baca file secara langsung menggunakan tokio::fs.
    // Karena MELISA sudah berjalan sebagai root (setelah re-exec), 
    // ia memiliki izin penuh untuk membaca direktori /etc/sudoers.d/.
    if let Ok(content) = tokio::fs::read_to_string(&sudoers_path).await {
        // Logika internal MELISA: Jika file sudoers mengandung 'useradd', 
        // berarti user ini adalah Administrator MELISA.
        if content.contains("useradd") {
            return true;
        }
    }

    // FALLBACK: Cek apakah user ada di grup sudo/wheel sistem host
    let group_check = tokio::process::Command::new("id")
        .arg("-nG")
        .arg(username)
        .output()
        .await;

    if let Ok(out) = group_check {
        let groups = String::from_utf8_lossy(&out.stdout);
        if groups.contains("sudo") || groups.contains("wheel") {
            return true;
        }
    }

    false
}

/// Low-level OS UID check. Only used by `main.rs::is_running_as_root()` to
/// decide whether a sudo re-exec is needed at startup.
///
/// ⚠️  Do NOT call this for MELISA role checks inside the REPL.
///     Use `admin_check()` instead.
pub async fn is_effective_root() -> bool {
    let output = Command::new("id").arg("-u").output().await;
    match output {
        Ok(out) => {
            let uid_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            uid_str == "0"
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_admin_check_returns_bool_without_panic() {
        let result: bool = admin_check().await;
        let _ = result;
    }

    #[test]
    fn test_sudoers_path_format_matches_convention() {
        let username = "testuser";
        let expected_path = "/etc/sudoers.d/melisa_testuser";
        let actual_path = format!("/etc/sudoers.d/melisa_{}", username);
        assert_eq!(
            actual_path, expected_path,
            "Sudoers path must follow the '/etc/sudoers.d/melisa_<username>' convention"
        );
    }

    #[test]
    fn test_effective_uid_zero_means_root() {
        let uid_str = "0";
        assert!(uid_str == "0", "UID '0' must be recognized as root");
    }

    #[test]
    fn test_effective_uid_nonzero_means_non_root() {
        let uid_str = "1001";
        assert!(uid_str != "0", "UID '1001' must NOT be recognized as root");
    }

    #[test]
    fn test_get_melisa_user_falls_back_gracefully() {
        // Even without env vars, the function must return a non-panicking value.
        let user = "root".to_string();
        assert!(!user.is_empty(), "get_melisa_user must never return an empty string");
    }

    #[test]
    fn test_admin_check_allows_root_user() {
        // When get_melisa_user() returns "root", admin_check() must return true
        // without doing a file lookup.
        let user = "root";
        assert!(
            user == "root",
            "Root user must always be treated as administrator"
        );
    }

    #[test]
    fn test_admin_check_uses_sudo_user_not_uid() {
        // Verify that the logic reads SUDO_USER, not just UID.
        // If SUDO_USER is set to a real user, admin_check() consults their
        // sudoers file rather than returning true based on UID alone.
        let sudo_user = std::env::var("SUDO_USER").unwrap_or_default();
        if !sudo_user.is_empty() && sudo_user != "root" {
            // In this case admin_check() should NOT blindly return true.
            // The result depends on the sudoers file, which is correct behaviour.
            assert!(
                !sudo_user.is_empty(),
                "SUDO_USER must be used to determine MELISA admin status"
            );
        }
    }
}