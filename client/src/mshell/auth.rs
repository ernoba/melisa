// ============================================================
// PATCH: client/src/auth.rs
// ============================================================
//
// SUMMARY OF CHANGES:
//  1. deploy_ssh_key        — Remove pub_key interpolation to shell string.
//                             Use stdin pipe to `sh` at remote to avoid
//                             shell quoting escapes.
//  2. auth_add              — Validate melisa_user input from stdin
//                             using new validate_username function.
//  3. configure_ssh_multiplexing — Add character validation for `host`
//                             and `user` before writing to SSH config.
// ============================================================

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use crate::mshell::color::{log_error, log_info, log_success, log_warning, BOLD, CYAN, GREEN, RESET, YELLOW};
use crate::mproto::filter::{validate_profile_name, validate_user_host};
use crate::platform::{
    config_dir, has_ssh_copy_id, has_ssh_multiplexing,
    ssh_dir, ssh_keygen_bin, ssh_sockets_dir, ssh_bin,
};

fn profile_file() -> PathBuf { config_dir().join("profiles.conf") }
fn active_file()  -> PathBuf { config_dir().join("active") }

fn read_profiles() -> io::Result<String> {
    match fs::read_to_string(profile_file()) {
        Ok(s)                                     => Ok(s),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(e)                                    => Err(e),
    }
}

fn write_profiles(content: &str) -> io::Result<()> {
    let path = profile_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        set_perms_700(parent);
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, &path)?;
    #[cfg(unix)]
    set_perms_600(&path);
    Ok(())
}

#[cfg(unix)]
fn set_perms_700(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(mut p) = fs::metadata(path).map(|m| m.permissions()) {
        p.set_mode(0o700);
        let _ = fs::set_permissions(path, p);
    }
}

#[cfg(unix)]
fn set_perms_600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(mut p) = fs::metadata(path).map(|m| m.permissions()) {
        p.set_mode(0o600);
        let _ = fs::set_permissions(path, p);
    }
}

// ── FIX #2: MELISA username validation ───────────────────────────────────────
//
// BEFORE: no validation whatsoever on username input.
// AFTER: allow only letters, digits, hyphen, and underscore;
//        length limited to 1–32 characters per POSIX limits.
//
fn validate_username(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 {
        return Err(format!(
            "Username '{}' must be between 1–32 characters.", name
        ));
    }
    // POSIX: username must not start with digit or '-'
    if name.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return Err(format!(
            "Username '{}' must not start with digit or '-'.", name
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "Username '{}' can only contain letters, digits, '-', and '_'.", name
        ));
    }
    Ok(())
}

// ── FIX #3: SSH config character validation ──────────────────────────────────
//
// Check that host/user values don't contain characters that can
// inject new directives into SSH config file.
//
fn validate_ssh_config_value(value: &str, field: &str) -> io::Result<()> {
    // Newline and carriage-return can inject new directives to config file.
    // Spaces and '#' are also dangerous in SSH config values.
    let forbidden: &[char] = &['\n', '\r', '\0', '#'];
    for ch in forbidden {
        if value.contains(*ch) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                format!("SSH config {field} contains forbidden character: {ch:?}"),
            ));
        }
    }
    Ok(())
}

pub fn init_auth() -> io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    set_perms_700(&dir);
    OpenOptions::new().create(true).append(true).open(profile_file())?;
    Ok(())
}

pub fn get_active_conn() -> Option<String> {
    let active = fs::read_to_string(active_file()).ok()?.trim().to_string();
    if active.is_empty() { return None; }
    let profiles = read_profiles().ok()?;
    for line in profiles.lines() {
        if let Some(val) = line.strip_prefix(&format!("{active}=")) {
            let conn = val.splitn(2, '|').next().unwrap_or("").trim().to_string();
            if conn.is_empty() { return None; }
            return Some(conn);
        }
    }
    None
}

pub fn get_active_melisa_user() -> Option<String> {
    let active   = fs::read_to_string(active_file()).ok()?.trim().to_string();
    let profiles = read_profiles().ok()?;
    for line in profiles.lines() {
        if let Some(val) = line.strip_prefix(&format!("{active}=")) {
            let mut parts   = val.splitn(2, '|');
            let ssh_conn    = parts.next().unwrap_or("").trim().to_string();
            let mel_user    = parts.next().map(str::trim).unwrap_or("").to_string();
            return if mel_user.is_empty() || mel_user == ssh_conn {
                ssh_conn.split('@').next().map(|s| s.to_string())
            } else {
                Some(mel_user)
            };
        }
    }
    None
}

pub fn auth_add(name: &str, user_host: &str) -> io::Result<()> {
    validate_profile_name(name)
        .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e.to_string()))?;
    validate_user_host(user_host)
        .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;
    ensure_ssh_key()?;
    log_info(&format!("Deploying public SSH key to {BOLD}{user_host}{RESET}..."));
    log_info("Please prepare to enter the remote server password.");
    deploy_ssh_key(user_host)?;
    if has_ssh_multiplexing() {
        configure_ssh_multiplexing(user_host)?;
    }
    let ssh_user = user_host.split('@').next().unwrap_or("").to_string();
    print!(
        "[SETUP] Enter your MELISA username on this server \
         (leave blank to use SSH user '{ssh_user}'): "
    );
    std::io::stdout().flush()?;
    let mut melisa_user = String::new();
    std::io::stdin().read_line(&mut melisa_user)?;
    let melisa_user = melisa_user.trim().to_string();

    // ── FIX #2 applied here ──────────────────────────────────────────────
    // BEFORE: use melisa_user directly without any validation.
    // AFTER: validate before saving to profiles.conf and using
    //        to build paths on remote server.
    let melisa_user = if melisa_user.is_empty() {
        ssh_user.clone()
    } else {
        validate_username(&melisa_user)
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;
        melisa_user
    };

    let existing = read_profiles()?;
    let filtered: String = existing
        .lines()
        .filter(|l| !l.starts_with(&format!("{name}=")))
        .map(|l| format!("{l}\n"))
        .collect();
    write_profiles(&format!("{filtered}{name}={user_host}|{melisa_user}\n"))?;
    fs::write(active_file(), name)?;
    log_success(&format!(
        "Server profile '{name}' registered. Remote MELISA user: {melisa_user}"
    ));
    Ok(())
}

pub fn auth_remove(name: &str) -> io::Result<()> {
    if name.is_empty() {
        log_error("Usage: melisa auth remove <profile_name>");
        return Ok(());
    }
    let existing = read_profiles()?;
    if !existing.lines().any(|l| l.starts_with(&format!("{name}="))) {
        log_error(&format!("Server profile '{name}' was not found in the registry."));
        return Ok(());
    }
    print!("{YELLOW}Are you sure you want to permanently remove the profile '{name}'? (y/N): {RESET}");
    std::io::stdout().flush()?;
    let mut confirm = String::new();
    std::io::stdin().read_line(&mut confirm)?;
    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
        log_info("Profile deletion aborted by user.");
        return Ok(());
    }
    let new_content: String = existing
        .lines()
        .filter(|l| !l.starts_with(&format!("{name}=")))
        .map(|l| format!("{l}\n"))
        .collect();
    write_profiles(&new_content)?;
    if let Ok(active) = fs::read_to_string(active_file()) {
        if active.trim() == name {
            let _ = fs::remove_file(active_file());
            log_info("The active profile was deleted. Use 'melisa auth switch' to select a new server.");
        }
    }
    log_success(&format!("Server profile '{name}' has been successfully purged from the registry."));
    Ok(())
}

pub fn auth_switch(name: &str) -> io::Result<()> {
    if name.is_empty() {
        log_error("Usage: melisa auth switch <profile_name>");
        return Ok(());
    }
    let profiles = read_profiles()?;
    if profiles.lines().any(|l| l.starts_with(&format!("{name}="))) {
        fs::write(active_file(), name)?;
        log_success(&format!("Successfully switched active connection to server: {BOLD}{name}{RESET}"));
    } else {
        log_error(&format!(
            "Server profile '{name}' not found! Execute 'melisa auth list' to view available profiles."
        ));
    }
    Ok(())
}

pub fn auth_list() -> io::Result<()> {
    let active = fs::read_to_string(active_file())
        .unwrap_or_default()
        .trim()
        .to_string();
    println!("\n{BOLD}{CYAN}=== MELISA REMOTE SERVER REGISTRY ==={RESET}");
    let profiles = read_profiles()?;
    if profiles.trim().is_empty() {
        println!("No servers are currently registered. Add one using 'melisa auth add <n> <user@host>'.");
        return Ok(());
    }
    for line in profiles.lines() {
        let parts: Vec<&str> = line.splitn(2, '=').collect();
        if parts.len() != 2 || parts[0].is_empty() { continue; }
        let (name, val) = (parts[0], parts[1]);
        let mut val_parts = val.splitn(2, '|');
        let conn     = val_parts.next().unwrap_or("").trim();
        let mel_user = val_parts.next().map(str::trim).unwrap_or("");
        let mel_tag  = if mel_user.is_empty() || mel_user == conn {
            String::new()
        } else {
            format!(" [melisa: {mel_user}]")
        };
        if name == active {
            println!("  {GREEN}* {name}{RESET} \t({conn}){mel_tag} {YELLOW}<- [ACTIVE]{RESET}");
        } else {
            println!("    {name} \t({conn}){mel_tag}");
        }
    }
    println!();
    Ok(())
}

fn ensure_ssh_key() -> io::Result<()> {
    let key_path = ssh_dir().join("id_ed25519");
    if key_path.exists() || ssh_dir().join("id_rsa").exists() {
        return Ok(());
    }
    log_info("No local SSH identity found. Generating a high-security Ed25519 keypair...");
    let ssh_d = ssh_dir();
    fs::create_dir_all(&ssh_d)?;
    #[cfg(unix)]
    set_perms_700(&ssh_d);
    let status = Command::new(ssh_keygen_bin())
        .args(["-t", "ed25519", "-f", key_path.to_str().unwrap_or(""), "-N", "", "-q"])
        .status()?;
    if status.success() {
        log_success("Cryptographic identity (Ed25519) successfully generated.");
        Ok(())
    } else {
        log_error("Failed to generate SSH keypair. Check local directory permissions.");
        Err(io::Error::new(ErrorKind::Other, "ssh-keygen failed"))
    }
}

// ── FIX #1: deploy_ssh_key — Remove pub_key interpolation to shell string ──────
//
// BEFORE (DANGEROUS):
//   let remote_cmd = format!("echo '{pub_key}' >> ~/.ssh/authorized_keys");
//   If pub_key contains ' (e.g., in comment section), attacker can inject
//   command to remote server.
//
// AFTER (SAFE):
//   Send public key content via STDIN ssh, not as string argument.
//   Remote script is entirely hardcoded — no user data is
//   interpolated into shell command.
//   This pattern is identical to how ssh-copy-id works.
//
fn deploy_ssh_key(user_host: &str) -> io::Result<()> {
    if has_ssh_copy_id() {
        let status = Command::new("ssh-copy-id").arg(user_host).status()?;
        if status.success() { return Ok(()); }
        log_warning("ssh-copy-id failed; attempting manual key deployment as fallback...");
    }

    let key_path = if ssh_dir().join("id_ed25519.pub").exists() {
        ssh_dir().join("id_ed25519.pub")
    } else {
        ssh_dir().join("id_rsa.pub")
    };
    if !key_path.exists() {
        return Err(io::Error::new(ErrorKind::NotFound, "No SSH public key found."));
    }

    let pub_key = fs::read_to_string(&key_path)?.trim().to_string();

    // Validate pub_key: must be single line with no newlines
    // (valid public keys are always single line).
    if pub_key.contains('\n') || pub_key.contains('\r') {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Public key file contains multiple lines — invalid format.",
        ));
    }

    // Remote script is entirely hardcoded; data sent via stdin, not
    // interpolated into command string.
    // `cat >> authorized_keys` reads from stdin that we pipe.
    let remote_script = "\
        mkdir -p ~/.ssh && \
        chmod 700 ~/.ssh && \
        cat >> ~/.ssh/authorized_keys && \
        chmod 600 ~/.ssh/authorized_keys";

    let mut child = Command::new(ssh_bin())
        .args([user_host, remote_script])
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    // Send public key + newline to stdin of remote script
    if let Some(mut stdin) = child.stdin.take() {
        // Write key + newline so new line is added correctly
        stdin.write_all(format!("{pub_key}\n").as_bytes())?;
        // stdin dropped automatically → remote `cat` finishes reading
    }

    let status = child.wait()?;
    if status.success() {
        log_success("SSH public key deployed to remote server.");
        Ok(())
    } else {
        log_error("Failed to establish a connection to the remote server.");
        Err(io::Error::new(ErrorKind::ConnectionRefused, "SSH key deployment failed"))
    }
}

// ── FIX #3: configure_ssh_multiplexing — Validate host & user ────────────────
//
// BEFORE: host and user interpolated directly to SSH config string
//         without checking for dangerous characters.
// AFTER: validate both values before writing to config file.
//
fn configure_ssh_multiplexing(user_host: &str) -> io::Result<()> {
    let Some(sockets_dir) = ssh_sockets_dir() else { return Ok(()); };
    let host = user_host.split('@').nth(1).unwrap_or(user_host);
    let user = user_host.split('@').next().unwrap_or("");

    // FIX: validate before writing to SSH config file.
    validate_ssh_config_value(host, "host")?;
    validate_ssh_config_value(user, "user")?;

    let ssh_config_path = ssh_dir().join("config");
    let existing_cfg    = fs::read_to_string(&ssh_config_path).unwrap_or_default();
    if existing_cfg.contains(&format!("Host {host}")) { return Ok(()); }
    fs::create_dir_all(&sockets_dir)?;
    #[cfg(unix)]
    {
        set_perms_700(&sockets_dir);
        set_perms_700(&ssh_dir());
    }
    let stanza = format!(
        "\nHost {host}\n    User {user}\n    ControlMaster auto\n    \
         ControlPath {}/%r@%h:%p\n    ControlPersist 10m\n",
        sockets_dir.display()
    );
    let mut file = OpenOptions::new().create(true).append(true).open(&ssh_config_path)?;
    file.write_all(stanza.as_bytes())?;
    #[cfg(unix)]
    set_perms_600(&ssh_config_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_username_allows_valid_names() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("alice_123").is_ok());
        assert!(validate_username("dev-user").is_ok());
    }

    #[test]
    fn test_validate_username_rejects_empty() {
        assert!(validate_username("").is_err());
    }

    #[test]
    fn test_validate_username_rejects_path_traversal() {
        assert!(validate_username("../etc/passwd").is_err());
        assert!(validate_username("../../root").is_err());
    }

    #[test]
    fn test_validate_username_rejects_shell_metachar() {
        assert!(validate_username("user; rm -rf /").is_err());
        assert!(validate_username("$(whoami)").is_err());
        assert!(validate_username("user|cat").is_err());
    }

    #[test]
    fn test_validate_username_rejects_too_long() {
        let long = "a".repeat(33);
        assert!(validate_username(&long).is_err());
    }

    #[test]
    fn test_validate_username_rejects_leading_digit() {
        assert!(validate_username("1user").is_err());
    }

    #[test]
    fn test_validate_ssh_config_value_rejects_newline() {
        assert!(validate_ssh_config_value("host\nHost evil", "host").is_err());
    }

    #[test]
    fn test_validate_ssh_config_value_rejects_carriage_return() {
        assert!(validate_ssh_config_value("host\rProxyJump attacker", "host").is_err());
    }

    #[test]
    fn test_validate_ssh_config_value_allows_normal_hostname() {
        assert!(validate_ssh_config_value("192.168.1.10", "host").is_ok());
        assert!(validate_ssh_config_value("my-server.local", "host").is_ok());
    }

    #[test]
    fn test_get_active_conn_does_not_panic() {
        let _ = get_active_conn();
    }

    #[test]
    fn test_auth_remove_empty_name_returns_ok() {
        let result = auth_remove("");
        assert!(result.is_ok());
    }

    #[test]
    fn test_auth_switch_empty_name_returns_ok() {
        let result = auth_switch("");
        assert!(result.is_ok());
    }
}