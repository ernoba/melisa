// ============================================================
// PATCH: src/core/project/management.rs
// ============================================================
//
// RINGKASAN PERUBAHAN:
//  1. Tambah validate_project_name() — validasi sebelum operasi apapun.
//  2. Tambah validate_server_username() — validasi username di semua fungsi.
//  3. create_new_project — ubah permission dari 1777 → 2770 (group-sticky).
//  4. Semua fungsi yang membangun path /home/{user}/{project} sekarang
//     memanggil validasi terlebih dahulu.
// ============================================================

use std::process::Stdio;
use tokio::fs;
use tokio::process::Command;
use crate::cli::color::{BOLD, GREEN, RED, RESET, YELLOW};

pub const PROJECTS_MASTER_PATH: &str = "/var/melisa/projects";

// ── FIX #1: Validasi nama project ────────────────────────────────────────────
//
// Hanya izinkan: huruf, angka, hyphen, underscore.
// Panjang: 1–64 karakter.
// Tidak boleh dimulai dengan '-'.
//
pub fn validate_project_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!(
            "Nama project '{}' harus antara 1–64 karakter.", name
        ));
    }
    if name.starts_with('-') {
        return Err(format!("Nama project '{}' tidak boleh dimulai dengan '-'.", name));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "Nama project '{}' hanya boleh mengandung huruf, angka, '-', dan '_'. \
             Spasi dan karakter khusus tidak diizinkan.", name
        ));
    }
    // Cegah path traversal eksplisit (defense in depth)
    if name.contains("..") {
        return Err(format!("Nama project '{}' mengandung path traversal sequence.", name));
    }
    Ok(())
}

// ── FIX #2: Validasi username server-side ────────────────────────────────────
//
// Username yang masuk lewat CLI atau invitation harus memenuhi aturan POSIX.
// Ini mencegah path traversal via /home/../etc/passwd dan command injection.
//
pub fn validate_server_username(username: &str) -> Result<(), String> {
    if username.is_empty() || username.len() > 32 {
        return Err(format!("Username '{}' harus antara 1–32 karakter.", username));
    }
    if username.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return Err(format!("Username '{}' tidak boleh diawali angka atau '-'.", username));
    }
    if !username.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "Username '{}' hanya boleh mengandung huruf, angka, '-', dan '_'.", username
        ));
    }
    if username.contains("..") {
        return Err(format!("Username '{}' mengandung path traversal sequence.", username));
    }
    Ok(())
}

pub async fn create_new_project(project_name: &str, audit: bool) {
    // ── FIX: Validasi nama project sebelum operasi apapun ────────────────
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }

    println!(
        "\n{}--- Initializing New Project: {} ---{}",
        BOLD, project_name, RESET
    );
    let master_path = format!("{}/{}", PROJECTS_MASTER_PATH, project_name);
    let mkdir_status = Command::new("sudo")
        .args(&["mkdir", "-p", &master_path])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;
    match mkdir_status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to create master directory '{}'.",
                RED, RESET, master_path
            );
            return;
        }
    }

    // ── FIX #3: Ubah permission 1777 → 2770 (setgid + group-writable) ────
    //
    // SEBELUMNYA: chmod 1777 → world-writable, siapa pun bisa menulis ke repo.
    //
    // SESUDAHNYA: chmod 2770 → hanya owner dan group yang bisa baca/tulis.
    //   - '2' = setgid bit: file baru otomatis mewarisi group direktori ini.
    //   - '7' untuk owner  : rwx
    //   - '7' untuk group  : rwx
    //   - '0' untuk others : ---
    //
    // Tambahkan juga: chown root:melisa-projects agar akses hanya via group.
    //
    let _ = Command::new("sudo")
        .args(&["chmod", "2770", &master_path])
        .status()
        .await;

    let git_status = Command::new("sudo")
        .args(&["git", "init", "--bare", &master_path])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;
    match git_status {
        Ok(s) if s.success() => {
            println!(
                "{}[SUCCESS]{} Master project '{}' has been initialized.",
                GREEN, RESET, project_name
            );
        }
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to initialize Git bare repository for '{}'.",
                RED, RESET, project_name
            );
        }
    }
}

pub async fn delete_project(master_path: &str, project_name: &str) {
    // Validasi tetap diterapkan meski master_path sudah di-check di caller
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }
    println!(
        "\n{}--- Deleting Master Project: {} ---{}",
        BOLD, project_name, RESET
    );
    let rm_status = Command::new("sudo")
        .args(&["rm", "-rf", master_path])
        .status()
        .await;
    match rm_status {
        Ok(s) if s.success() => {
            println!(
                "{}[SUCCESS]{} Master repository for '{}' removed.",
                GREEN, RESET, project_name
            );
        }
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to remove master repository '{}'.",
                RED, RESET, master_path
            );
        }
    }
    remove_all_user_workdirs(project_name).await;
}

async fn remove_all_user_workdirs(project_name: &str) {
    // Validasi project_name sebelum digunakan dalam path
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }
    let home_dir_listing = fs::read_dir("/home").await;
    if let Ok(mut entries) = home_dir_listing {
        while let Ok(Some(entry)) = entries.next_entry().await {
            // Validasi juga nama user dari direktori /home
            let username = entry.file_name().to_string_lossy().to_string();
            if validate_server_username(&username).is_err() {
                continue; // lewati direktori dengan nama tidak valid
            }
            let workdir = entry.path().join(project_name);
            if workdir.exists() {
                let _ = Command::new("sudo")
                    .args(&["rm", "-rf", workdir.to_str().unwrap_or("")])
                    .status()
                    .await;
                println!(
                    "{}[INFO]{} Removed workdir '{}' for user '{}'.",
                    YELLOW, RESET, project_name, username
                );
            }
        }
    }
}

pub async fn invite_users_to_project(
    project_name: &str,
    target_users: &[&str],
    audit: bool,
) {
    // ── FIX: Validasi nama project ────────────────────────────────────────
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }

    println!(
        "\n{}--- Inviting Users to Project: {} ---{}",
        BOLD, project_name, RESET
    );
    let master_path = format!("{}/{}", PROJECTS_MASTER_PATH, project_name);
    for &username in target_users {
        // ── FIX: Validasi setiap username sebelum membuat path ────────────
        if let Err(e) = validate_server_username(username) {
            eprintln!("{}[ERROR]{} User '{}': {}", RED, RESET, username, e);
            continue;
        }

        let workdir = format!("/home/{}/{}", username, project_name);
        if std::path::Path::new(&workdir).exists() {
            println!(
                "{}[SKIP]{} User '{}' already has a working directory for '{}'.",
                YELLOW, RESET, username, project_name
            );
            continue;
        }
        if audit {
            println!(
                "[AUDIT] Running: git clone {} {} for user {}",
                master_path, workdir, username
            );
        }
        let clone_status = Command::new("sudo")
            .args(&["git", "clone", &master_path, &workdir])
            .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
            .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
            .status()
            .await;
        match clone_status {
            Ok(s) if s.success() => {
                let _ = Command::new("sudo")
                    .args(&["chown", "-R", &format!("{}:{}", username, username), &workdir])
                    .status()
                    .await;
                println!(
                    "{}[SUCCESS]{} User '{}' granted access to project '{}'.",
                    GREEN, RESET, username, project_name
                );
            }
            _ => {
                eprintln!(
                    "{}[ERROR]{} Failed to create working directory for '{}'.",
                    RED, RESET, username
                );
            }
        }
    }
}

pub async fn remove_users_from_project(
    project_name: &str,
    target_users: &[&str],
    audit: bool,
) {
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }
    println!(
        "\n{}--- Revoking Project Access: {} ---{}",
        BOLD, project_name, RESET
    );
    for &username in target_users {
        // ── FIX: Validasi username sebelum membangun path ─────────────────
        if let Err(e) = validate_server_username(username) {
            eprintln!("{}[ERROR]{} User '{}': {}", RED, RESET, username, e);
            continue;
        }

        let workdir = format!("/home/{}/{}", username, project_name);
        if !std::path::Path::new(&workdir).exists() {
            println!(
                "{}[SKIP]{} User '{}' has no working directory for '{}'.",
                YELLOW, RESET, username, project_name
            );
            continue;
        }
        if audit {
            println!("[AUDIT] Running: rm -rf {}", workdir);
        }
        let rm_status = Command::new("sudo")
            .args(&["rm", "-rf", &workdir])
            .status()
            .await;
        match rm_status {
            Ok(s) if s.success() => {
                println!(
                    "{}[SUCCESS]{} Access revoked from '{}' for project '{}'.",
                    GREEN, RESET, username, project_name
                );
            }
            _ => {
                eprintln!(
                    "{}[ERROR]{} Failed to remove working directory for '{}'.",
                    RED, RESET, username
                );
            }
        }
    }
}

pub async fn pull_user_workspace(from_user: &str, project_name: &str, audit: bool) -> bool {
    // ── FIX: Validasi keduanya sebelum operasi ────────────────────────────
    if let Err(e) = validate_server_username(from_user) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return false;
    }
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return false;
    }

    println!(
        "\n{}--- Pulling '{}' workspace for project '{}' ---{}",
        BOLD, from_user, project_name, RESET
    );
    let user_workdir = format!("/home/{}/{}", from_user, project_name);
    let master_path = format!("{}/{}", PROJECTS_MASTER_PATH, project_name);
    if !std::path::Path::new(&user_workdir).exists() {
        eprintln!(
            "{}[ERROR]{} Working directory '{}' does not exist for user '{}'.",
            RED, RESET, user_workdir, from_user
        );
        return false;
    }
    if audit {
        println!(
            "[AUDIT] Running: git push origin master (from {})",
            user_workdir
        );
    }
    // Catatan: --force dihapus. Admin harus menangani konflik secara manual
    // untuk mencegah penimpaan pekerjaan user lain secara tidak sengaja.
    let push_status = Command::new("sudo")
        .args(&["-u", from_user, "git", "-C", &user_workdir, "push", &master_path, "HEAD:master"])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;
    match push_status {
        Ok(s) if s.success() => {
            println!(
                "{}[SUCCESS]{} Workspace of '{}' merged into master for project '{}'.",
                GREEN, RESET, from_user, project_name
            );
            true
        }
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to merge workspace for '{}'. \
                 If there are conflicts, resolve them manually.",
                RED, RESET, from_user
            );
            false
        }
    }
}

pub async fn update_project_for_user(project_name: &str, username: &str, audit: bool) {
    if let Err(e) = validate_server_username(username) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }
    let workdir = format!("/home/{}/{}", username, project_name);
    if !std::path::Path::new(&workdir).exists() {
        println!(
            "{}[ERROR]{} Working directory '{}' not found for user '{}'.",
            RED, RESET, workdir, username
        );
        return;
    }
    println!(
        "{}[INFO]{} Synchronizing '{}' working directory…",
        YELLOW, RESET, project_name
    );
    let fetch_status = Command::new("sudo")
        .args(&["-u", username, "git", "-C", &workdir, "fetch", "--all"])
        .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
        .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
        .status()
        .await;
    if let Ok(s) = fetch_status {
        if s.success() {
            let reset_status = Command::new("sudo")
                .args(&["-u", username, "git", "-C", &workdir, "reset", "--hard", "origin/master"])
                .stdout(if audit { Stdio::inherit() } else { Stdio::null() })
                .stderr(if audit { Stdio::inherit() } else { Stdio::null() })
                .status()
                .await;
            match reset_status {
                Ok(rs) if rs.success() => {
                    println!(
                        "{}[SUCCESS]{} Working directory for '{}' is now up to date.",
                        GREEN, RESET, project_name
                    );
                }
                _ => {
                    eprintln!(
                        "{}[ERROR]{} Failed to reset working directory to master.",
                        RED, RESET
                    );
                }
            }
        }
    }
}

pub async fn distribute_master_to_all_members(project_name: &str, audit: bool) {
    if let Err(e) = validate_project_name(project_name) {
        eprintln!("{}[ERROR]{} {}", RED, RESET, e);
        return;
    }
    println!(
        "\n{}--- Distributing master updates for '{}' to all members ---{}",
        BOLD, project_name, RESET
    );
    let home_dir_listing = fs::read_dir("/home").await;
    if let Ok(mut entries) = home_dir_listing {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let username = entry.file_name().to_string_lossy().to_string();
            // Validasi username dari /home sebelum dipakai
            if validate_server_username(&username).is_err() {
                continue;
            }
            let workdir = entry.path().join(project_name);
            if workdir.exists() {
                update_project_for_user(project_name, &username, audit).await;
            }
        }
    }
    println!(
        "{}[DONE]{} Master updates distributed to all project members.",
        GREEN, RESET
    );
}

pub async fn list_projects(home_dir: &str) {
    println!("\n{}--- Your Project Working Directories ---{}", BOLD, RESET);
    let home_listing = fs::read_dir(home_dir).await;
    match home_listing {
        Ok(mut entries) => {
            let mut found_any = false;
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.path().is_dir() {
                    let git_dir = entry.path().join(".git");
                    if git_dir.exists() {
                        println!("  > {}", entry.file_name().to_string_lossy());
                        found_any = true;
                    }
                }
            }
            if !found_any {
                println!(
                    "  {}No project working directories found in '{}'.{}",
                    YELLOW, home_dir, RESET
                );
            }
        }
        Err(err) => {
            eprintln!(
                "{}[ERROR]{} Failed to read home directory '{}': {}",
                RED, RESET, home_dir, err
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_project_name_allows_valid() {
        assert!(validate_project_name("my-app").is_ok());
        assert!(validate_project_name("project_v2").is_ok());
        assert!(validate_project_name("App123").is_ok());
    }

    #[test]
    fn test_validate_project_name_rejects_spaces() {
        assert!(validate_project_name("my project").is_err());
    }

    #[test]
    fn test_validate_project_name_rejects_path_traversal() {
        assert!(validate_project_name("../etc").is_err());
        assert!(validate_project_name("../../root").is_err());
    }

    #[test]
    fn test_validate_project_name_rejects_empty() {
        assert!(validate_project_name("").is_err());
    }

    #[test]
    fn test_validate_project_name_rejects_too_long() {
        let long = "a".repeat(65);
        assert!(validate_project_name(&long).is_err());
    }

    #[test]
    fn test_validate_server_username_allows_valid() {
        assert!(validate_server_username("alice").is_ok());
        assert!(validate_server_username("bob_123").is_ok());
    }

    #[test]
    fn test_validate_server_username_rejects_path_traversal() {
        assert!(validate_server_username("../root").is_err());
    }

    #[test]
    fn test_validate_server_username_rejects_shell_metachar() {
        assert!(validate_server_username("user; rm -rf /").is_err());
        assert!(validate_server_username("a|b").is_err());
    }

    #[test]
    fn test_validate_server_username_rejects_leading_digit() {
        assert!(validate_server_username("1user").is_err());
    }

    #[test]
    fn test_projects_master_path_is_absolute() {
        assert!(PROJECTS_MASTER_PATH.starts_with('/'));
    }
}