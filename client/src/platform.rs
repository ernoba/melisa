//! # Platform Abstraction Layer
//!
//! Detects the host operating system at runtime and resolves the paths and
//! names of external tools (SSH, SCP, SSH-keygen, Rsync / Git) that the
//! MELISA client shells out to.
//!
//! ## Platform matrix
//!
//! | Feature              | Linux         | macOS         | Windows                    |
//! |----------------------|---------------|---------------|----------------------------|
//! | SSH client           | `ssh`         | `ssh`         | `ssh.exe` (OpenSSH inbox)  |
//! | SCP                  | `scp`         | `scp`         | `scp.exe` (OpenSSH inbox)  |
//! | ssh-keygen           | `ssh-keygen`  | `ssh-keygen`  | `ssh-keygen.exe`           |
//! | ssh-copy-id          | `ssh-copy-id` | `ssh-copy-id` | **not available** → manual |
//! | Rsync                | `rsync`       | `rsync`       | not available → scp -r     |
//! | Config dir           | `~/.config/…` | `~/.config/…` | `%APPDATA%\…`              |
//! | SSH socket dir       | `~/.ssh/sockets` | `~/.ssh/sockets` | N/A (multiplexing)     |

use std::path::PathBuf;

// ── Operating system detection ────────────────────────────────────────────────

/// The host operating system, detected once at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
    Unknown,
}

/// Returns the host operating system.
pub fn detect_os() -> Os {
    if cfg!(target_os = "linux")   { return Os::Linux;   }
    if cfg!(target_os = "macos")   { return Os::Macos;   }
    if cfg!(target_os = "windows") { return Os::Windows; }
    Os::Unknown
}

// ── Tool names ────────────────────────────────────────────────────────────────

/// Returns the platform-correct executable name for the SSH client.
pub fn ssh_bin() -> &'static str {
    if cfg!(target_os = "windows") { "ssh.exe" } else { "ssh" }
}

/// Returns the platform-correct executable name for SCP.
pub fn scp_bin() -> &'static str {
    if cfg!(target_os = "windows") { "scp.exe" } else { "scp" }
}

/// Returns the platform-correct executable name for ssh-keygen.
pub fn ssh_keygen_bin() -> &'static str {
    if cfg!(target_os = "windows") { "ssh-keygen.exe" } else { "ssh-keygen" }
}

/// Returns `true` when the `ssh-copy-id` utility is expected to be available on
/// this platform.  On Windows it is absent; key deployment must use the manual
/// path implemented in [`crate::auth`].
pub fn has_ssh_copy_id() -> bool {
    !cfg!(target_os = "windows")
}

/// Returns `true` when `rsync` is expected to be available on this platform.
/// On Windows rsync is not shipped by default; file transfers fall back to
/// `scp -r`.
pub fn has_rsync() -> bool {
    !cfg!(target_os = "windows")
}

/// Returns `true` when SSH ControlMaster / ControlPath multiplexing is
/// supported on this platform (Unix socket required; not available on Windows).
pub fn has_ssh_multiplexing() -> bool {
    !cfg!(target_os = "windows")
}

// ── Directory paths ───────────────────────────────────────────────────────────

/// Returns the MELISA configuration directory for the current user.
///
/// * Linux / macOS : `~/.config/melisa`
/// * Windows       : `%APPDATA%\melisa`
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("melisa")
}

/// Returns the MELISA local data / state directory for the current user.
///
/// * Linux / macOS : `~/.local/share/melisa`
/// * Windows       : `%LOCALAPPDATA%\melisa`
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("melisa")
}

/// Returns the SSH configuration directory for the current user.
///
/// * Linux / macOS : `~/.ssh`
/// * Windows       : `%USERPROFILE%\.ssh`
pub fn ssh_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
}

/// Returns the path used to store SSH multiplexing sockets.
/// On Windows this is `None` because Unix-domain sockets are not universally
/// available.
pub fn ssh_sockets_dir() -> Option<PathBuf> {
    if has_ssh_multiplexing() {
        Some(ssh_dir().join("sockets"))
    } else {
        None
    }
}

// ── Pre-flight dependency check ───────────────────────────────────────────────

/// Verifies that the minimum required external tools are present in `PATH`.
///
/// Returns `Ok(())` when all required tools are found.  Returns `Err` with a
/// human-readable message listing every missing tool so the user can install
/// them in one pass.
pub fn verify_dependencies() -> Result<(), String> {
    let required = [ssh_bin(), scp_bin(), ssh_keygen_bin()];
    let missing: Vec<&str> = required
        .iter()
        .filter(|&&tool| which(tool).is_none())
        .copied()
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    let names = missing.join(", ");
    let hint = install_hint();
    Err(format!(
        "Required tools not found in PATH: {names}\n{hint}"
    ))
}

/// Returns a platform-appropriate installation hint for missing SSH tools.
fn install_hint() -> String {
    match detect_os() {
        Os::Linux => concat!(
            "Install OpenSSH client:\n",
            "  Debian/Ubuntu : sudo apt install openssh-client\n",
            "  Fedora/RHEL   : sudo dnf install openssh-clients\n",
            "  Arch          : sudo pacman -S openssh",
        ).to_string(),
        Os::Macos => concat!(
            "Install OpenSSH client:\n",
            "  Homebrew : brew install openssh",
        ).to_string(),
        Os::Windows => concat!(
            "Enable the built-in OpenSSH client:\n",
            "  Settings → Apps → Optional Features → Add a feature → OpenSSH Client\n",
            "  — or via PowerShell (admin):\n",
            "  Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0",
        ).to_string(),
        Os::Unknown => "Please install OpenSSH client manually.".to_string(),
    }
}

/// Resolves `tool_name` against the system `PATH` and returns its full path,
/// or `None` if not found.  This is a minimal implementation that avoids the
/// `which` crate dependency.
fn which(tool_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(tool_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        // On Windows try without the extension as well (already included in tool_name)
        #[cfg(target_os = "windows")]
        {
            let no_ext = dir.join(
                std::path::Path::new(tool_name)
                    .file_stem()
                    .unwrap_or_default()
            );
            if no_ext.is_file() {
                return Some(no_ext);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_os_returns_a_known_variant() {
        let os = detect_os();
        assert!(
            matches!(os, Os::Linux | Os::Macos | Os::Windows | Os::Unknown),
            "detect_os must always return a valid Os variant"
        );
    }

    #[test]
    fn test_config_dir_is_non_empty() {
        let dir = config_dir();
        assert!(
            !dir.as_os_str().is_empty(),
            "config_dir must return a non-empty path"
        );
        assert!(
            dir.ends_with("melisa"),
            "config_dir must end with 'melisa' subdirectory"
        );
    }

    #[test]
    fn test_data_dir_ends_with_melisa() {
        let dir = data_dir();
        assert!(
            dir.ends_with("melisa"),
            "data_dir must end with 'melisa' subdirectory"
        );
    }

    #[test]
    fn test_ssh_bin_is_not_empty() {
        assert!(!ssh_bin().is_empty(), "ssh_bin must return a non-empty string");
    }

    #[test]
    fn test_scp_bin_is_not_empty() {
        assert!(!scp_bin().is_empty(), "scp_bin must return a non-empty string");
    }

    #[test]
    fn test_verify_dependencies_returns_result() {
        // We cannot assert the exact outcome since the CI environment varies,
        // but the function must return without panicking.
        let _ = verify_dependencies();
    }
}