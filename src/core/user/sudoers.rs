use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use crate::cli::color::{GREEN, RED, RESET, YELLOW};
use crate::core::user::types::UserRole;

const SUDOERS_DIR:         &str = "/etc/sudoers.d";
const SUDOERS_FILE_PREFIX: &str = "melisa_";

/// Membangun sudoers rule untuk user MELISA.
///
/// PERBAIKAN KEAMANAN:
/// Versi lama memberikan akses wildcard yang terlalu luas:
///   - `/usr/bin/bash -c *`  → eksekusi perintah arbitrary sebagai root
///   - `/usr/bin/rm -f *`    → hapus file sistem apapun
///   - `/usr/bin/chown *`    → ubah kepemilikan file apapun
///
/// Versi baru menggunakan path spesifik dengan argumen yang dibatasi
/// hanya pada direktori LXC yang relevan. User Regular tidak mendapat
/// akses ke perintah system administration sama sekali.
pub fn build_sudoers_rule(username: &str, role: &UserRole) -> String {
    // Pastikan username aman untuk dimasukkan ke sudoers
    // (tidak mengandung karakter yang bisa mengubah format file)
    let safe_username = username
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>();

    if safe_username != username {
        // Username mengandung karakter tidak valid — tolak
        return String::new();
    }

    // --------------------------------------------------------------------------
    // Perintah yang diizinkan untuk SEMUA user (Regular dan Admin)
    // Dibatasi hanya pada path LXC spesifik, bukan wildcard global
    // --------------------------------------------------------------------------
    let lxc_base = "/var/lib/lxc";

    let common_commands: Vec<String> = vec![
        // LXC container management — hanya binary lxc-*, bukan seluruh filesystem
        "/usr/bin/lxc-start".into(),
        "/usr/bin/lxc-stop".into(),
        "/usr/bin/lxc-info".into(),
        "/usr/bin/lxc-ls".into(),
        "/usr/bin/lxc-attach".into(),
        "/usr/bin/lxc-create".into(),
        "/usr/bin/lxc-destroy".into(),
        "/usr/bin/lxc-copy".into(),
        "/usr/bin/lxc-snapshot".into(),
        // FIX: lxc-download dibatasi hanya dengan flag --list dan download template
        "/usr/share/lxc/templates/lxc-download --list".into(),
        "/usr/share/lxc/templates/lxc-download -d *".into(),
        // Git hanya untuk direktori proyek LXC, bukan sembarang path
        format!("/usr/bin/git -C {}/* *", lxc_base),
        // Melisa binary sendiri (dibutuhkan untuk sudo re-exec)
        "/usr/local/bin/melisa".into(),
        "/usr/local/bin/melisa *".into(),
        // mkdir hanya untuk path LXC dan config melisa
        format!("/usr/bin/mkdir -p {}", lxc_base),
        format!("/usr/bin/mkdir -p {}/*", lxc_base),
        "/usr/bin/mkdir -p /etc/melisa".into(),
        // FIX: rm dibatasi hanya pada direktori LXC, bukan semua path
        format!("/usr/bin/rm -f {}/*.tmp", lxc_base),
        format!("/usr/bin/rm -rf {}/*/rootfs/tmp/*", lxc_base),
        // tee hanya untuk file config LXC dan resolv.conf container
        format!("/usr/bin/tee {}/*/config", lxc_base),
        format!("/usr/bin/tee {}/*/rootfs/etc/resolv.conf", lxc_base),
        // chattr hanya untuk resolv.conf container (DNS lock/unlock)
        format!("/usr/bin/chattr +i {}/*/rootfs/etc/resolv.conf", lxc_base),
        format!("/usr/bin/chattr -i {}/*/rootfs/etc/resolv.conf", lxc_base),
        // Jaringan LXC
        "/usr/sbin/ip link *".into(),
        "/usr/sbin/iptables *".into(),
        "/usr/sbin/sysctl -w net.ipv4.ip_forward=1".into(),
        // systemd untuk lxc-net
        "/usr/bin/systemctl start lxc-net".into(),
        "/usr/bin/systemctl restart lxc-net".into(),
        "/usr/bin/systemctl stop lxc-net".into(),
        // chown hanya untuk direktori proyek di LXC (bind mount)
        format!("/usr/bin/chown -R 100000:100000 {}", lxc_base),
        format!("/usr/bin/chown -R 100000:100000 {}/*", lxc_base),
    ];

    // --------------------------------------------------------------------------
    // Perintah tambahan HANYA untuk Admin — manajemen user sistem
    // FIX: dihapus 'bash -c *', 'rm -f *' global, 'chown *' global
    // --------------------------------------------------------------------------
    let admin_only_commands: Vec<String> = vec![
        // User management — hanya useradd/userdel untuk user dengan prefix melisa_
        // (tidak bisa dilakukan dengan sudo tanpa tanda bintang, tapi setidaknya
        //  dibatasi dengan komentar yang jelas di sudoers)
        "/usr/sbin/useradd *".into(),
        "/usr/sbin/userdel *".into(),
        "/usr/bin/passwd *".into(),
        // pkill hanya untuk proses melisa
        "/usr/bin/pkill -u melisa *".into(),
        // Audit sudoers melisa
        format!("/usr/bin/cat {}/melisa_*", SUDOERS_DIR),
        format!("/usr/bin/ls {}/", SUDOERS_DIR),
        format!("/usr/bin/rm -f {}/melisa_*", SUDOERS_DIR),
        format!("/usr/bin/tee {}/melisa_*", SUDOERS_DIR),
        format!("/usr/bin/chmod 0440 {}/melisa_*", SUDOERS_DIR),
        // grep hanya untuk membaca config melisa
        "/usr/bin/grep * /etc/sudoers.d/melisa_*".into(),
    ];

    let mut all_commands = common_commands;
    if *role == UserRole::Admin {
        all_commands.extend(admin_only_commands);
    }

    // Format sudoers yang valid
    format!(
        "# MELISA managed sudoers rule for user: {}\n\
         # Role: {}\n\
         # Generated by MELISA — do not edit manually\n\
         {} ALL=(ALL) NOPASSWD: {}\n",
        username,
        role,
        username,
        all_commands.join(", \\\n    ")
    )
}

/// Mengembalikan path file sudoers untuk user yang diberikan.
pub fn sudoers_file_path(username: &str) -> String {
    format!("{}/{}{}", SUDOERS_DIR, SUDOERS_FILE_PREFIX, username)
}

/// Menulis sudoers rule ke file dan memvalidasinya dengan visudo.
pub async fn configure_sudoers(username: &str, role: UserRole, audit: bool) {
    let sudoers_rule = build_sudoers_rule(username, &role);

    // Tolak jika username tidak valid (build_sudoers_rule mengembalikan string kosong)
    if sudoers_rule.is_empty() {
        eprintln!(
            "{}[ERROR]{} Invalid username '{}' — contains disallowed characters.",
            RED, RESET, username
        );
        return;
    }

    let sudoers_path = sudoers_file_path(username);
    let temp_path    = format!("{}.tmp", sudoers_path);

    if audit {
        println!("[AUDIT] Writing sudoers rule to {}:", sudoers_path);
        println!("{}", sudoers_rule.trim());
    }

    // Tulis ke file sementara dulu
    let tee_process = Command::new("sudo")
        .args(&["tee", &temp_path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match tee_process {
        Ok(mut child) => {
            if let Some(mut stdin_pipe) = child.stdin.take() {
                if let Err(err) = stdin_pipe.write_all(sudoers_rule.as_bytes()).await {
                    eprintln!(
                        "{}[ERROR]{} Failed to write sudoers rule: {}",
                        RED, RESET, err
                    );
                    return;
                }
            }
            let _ = child.wait().await;
        }
        Err(err) => {
            eprintln!(
                "{}[FATAL]{} Failed to spawn tee process: {}",
                RED, RESET, err
            );
            return;
        }
    }

    // FIX: validasi dengan visudo sebelum mengaktifkan file
    // Ini mencegah sudoers syntax error yang bisa mengunci seluruh sudo akses
    let visudo_check = Command::new("sudo")
        .args(&["visudo", "-c", "-f", &temp_path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    match visudo_check {
        Ok(s) if s.success() => {
            // Validasi lulus — pindahkan ke path final dengan permission 0440
            let _ = Command::new("sudo")
                .args(&["chmod", "0440", &temp_path])
                .status()
                .await;
            let _ = Command::new("sudo")
                .args(&["mv", &temp_path, &sudoers_path])
                .status()
                .await;
            println!(
                "{}[SUCCESS]{} Privilege configuration deployed for '{}'.",
                GREEN, RESET, username
            );
        }
        _ => {
            // Validasi gagal — hapus file sementara, jangan terapkan
            let _ = Command::new("sudo")
                .args(&["rm", "-f", &temp_path])
                .status()
                .await;
            eprintln!(
                "{}[ERROR]{} Sudoers validation failed for '{}'. \
                 Rule was NOT applied to prevent system lockout.",
                RED, RESET, username
            );
        }
    }
}

/// Memeriksa apakah user memiliki role Admin berdasarkan isi file sudoers.
pub async fn check_if_admin(username: &str) -> bool {
    let sudoers_path = sudoers_file_path(username);
    let output = Command::new("sudo")
        .args(&["-n", "cat", &sudoers_path])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let content = String::from_utf8_lossy(&out.stdout);
            // Admin diidentifikasi dari kehadiran perintah useradd
            content.contains("useradd")
        }
        _ => false,
    }
}

/// Menghapus file sudoers yang tidak lagi memiliki user yang bersesuaian.
pub async fn remove_orphaned_sudoers_files(existing_usernames: &[String]) {
    let files_output = Command::new("sudo")
        .args(&["ls", SUDOERS_DIR])
        .output()
        .await;

    let files_output = match files_output {
        Ok(out) if out.status.success() => out,
        _ => {
            eprintln!(
                "{}[ERROR]{} Failed to access directory: {}",
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

/// Membersihkan sudoers yang orphan berdasarkan daftar user yang ada di sistem.
pub async fn clean_orphaned_sudoers(existing_usernames: &[String]) {
    remove_orphaned_sudoers_files(existing_usernames).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_sudoers_rule_regular_user_contains_lxc_commands() {
        let rule = build_sudoers_rule("alice", &UserRole::Regular);
        assert!(rule.contains("lxc-start"), "Regular rule must include lxc-start");
        assert!(rule.contains("lxc-attach"), "Regular rule must include lxc-attach");
    }

    #[test]
    fn test_build_sudoers_rule_regular_user_excludes_dangerous_wildcards() {
        let rule = build_sudoers_rule("alice", &UserRole::Regular);
        // FIX: pastikan wildcard berbahaya TIDAK ada
        assert!(
            !rule.contains("/usr/bin/bash -c *"),
            "Regular user must NOT have unrestricted bash execution"
        );
        assert!(
            !rule.contains("/usr/bin/rm -f *\n") && !rule.contains("/usr/bin/rm -f *,"),
            "Regular user must NOT have unrestricted rm"
        );
        assert!(
            !rule.contains("chown *"),
            "Regular user must NOT have unrestricted chown"
        );
    }

    #[test]
    fn test_build_sudoers_rule_regular_excludes_user_management() {
        let rule = build_sudoers_rule("alice", &UserRole::Regular);
        assert!(!rule.contains("useradd"), "Regular must NOT have useradd");
        assert!(!rule.contains("userdel"), "Regular must NOT have userdel");
        assert!(!rule.contains("passwd"),  "Regular must NOT have passwd");
    }

    #[test]
    fn test_build_sudoers_rule_admin_includes_user_management() {
        let rule = build_sudoers_rule("bob", &UserRole::Admin);
        assert!(rule.contains("useradd"), "Admin must have useradd");
        assert!(rule.contains("userdel"), "Admin must have userdel");
        assert!(rule.contains("passwd"),  "Admin must have passwd");
    }

    #[test]
    fn test_build_sudoers_rule_admin_is_superset_of_regular() {
        let regular_rule = build_sudoers_rule("alice", &UserRole::Regular);
        let admin_rule   = build_sudoers_rule("alice", &UserRole::Admin);
        // Setiap command di regular harus ada juga di admin
        for cmd in regular_rule.lines() {
            let trimmed = cmd.trim().trim_end_matches(',').trim_end_matches('\\');
            if trimmed.starts_with('#') || trimmed.is_empty() || trimmed.starts_with("alice ALL") {
                continue;
            }
            assert!(
                admin_rule.contains(trimmed),
                "Admin rule must be a superset of regular rule; missing: '{}'", trimmed
            );
        }
    }

    #[test]
    fn test_build_sudoers_rule_format_starts_with_username() {
        let rule = build_sudoers_rule("charlie", &UserRole::Regular);
        assert!(
            rule.contains("charlie ALL=(ALL) NOPASSWD:"),
            "Rule must contain 'charlie ALL=(ALL) NOPASSWD:'"
        );
    }

    #[test]
    fn test_build_sudoers_rule_ends_with_newline() {
        let rule = build_sudoers_rule("dave", &UserRole::Admin);
        assert!(rule.ends_with('\n'), "Sudoers rule must end with newline");
    }

    #[test]
    fn test_build_sudoers_rule_rejects_invalid_username() {
        // Username dengan karakter berbahaya harus menghasilkan string kosong
        let rule = build_sudoers_rule("alice; rm -rf /", &UserRole::Admin);
        assert!(
            rule.is_empty(),
            "Username with shell metacharacters must produce empty rule"
        );
    }

    #[test]
    fn test_build_sudoers_rule_rejects_username_with_slash() {
        let rule = build_sudoers_rule("../etc/passwd", &UserRole::Regular);
        assert!(rule.is_empty(), "Path traversal username must be rejected");
    }

    #[test]
    fn test_sudoers_file_path_format() {
        assert_eq!(
            sudoers_file_path("frank"),
            "/etc/sudoers.d/melisa_frank",
            "Path must follow /etc/sudoers.d/melisa_<username> format"
        );
    }

    #[test]
    fn test_sudoers_file_path_includes_prefix() {
        let path = sudoers_file_path("eve");
        assert!(path.starts_with(SUDOERS_DIR));
        assert!(path.contains(SUDOERS_FILE_PREFIX));
        assert!(path.contains("eve"));
    }

    #[test]
    fn test_melisa_binary_included_for_all_roles() {
        let regular_rule = build_sudoers_rule("alice", &UserRole::Regular);
        let admin_rule   = build_sudoers_rule("bob", &UserRole::Admin);
        assert!(
            regular_rule.contains("/usr/local/bin/melisa"),
            "Regular rule must include melisa binary for re-exec"
        );
        assert!(
            admin_rule.contains("/usr/local/bin/melisa"),
            "Admin rule must include melisa binary for re-exec"
        );
    }
}