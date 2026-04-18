#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod cli;
pub mod core;
pub mod deployment;
pub mod distros;

use std::env;
use std::process;

#[tokio::main]
async fn main() {
    if !is_running_as_root() {
        re_exec_as_root();
    }
    cli::melisa_cli::melisa().await;
}

fn is_running_as_root() -> bool {
    // SAFETY: geteuid() adalah syscall POSIX yang tidak memiliki precondition
    // unsafe dan tidak mengakses memori arbitrary.
    unsafe { libc::geteuid() == 0 }
}

/// Daftar environment variable yang BOLEH diwariskan ke proses sudo.
///
/// PERBAIKAN KEAMANAN: Sebelumnya menggunakan `sudo -E` yang mewariskan
/// SELURUH environment variable — termasuk LD_PRELOAD, PYTHONPATH, PATH
/// yang bisa dieksploitasi untuk privilege escalation. Sekarang hanya
/// variabel yang benar-benar dibutuhkan MELISA yang diteruskan secara eksplisit.
const ALLOWED_ENV_VARS: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_MESSAGES",
    "MELISA_DEBUG",   // flag debug opsional untuk development
];

fn re_exec_as_root() {
    let current_binary = env::current_exe().unwrap_or_else(|_| {
        eprintln!("MELISA: Failed to resolve the current executable path.");
        process::exit(1);
    });

    let args: Vec<String> = env::args().skip(1).collect();

    let mut sudo_cmd = process::Command::new("sudo");

    // FIX: Hapus '-E' (preserve-env semua), ganti dengan whitelist eksplisit.
    // Setiap variabel yang diizinkan diteruskan satu per satu menggunakan
    // '--preserve-env=VAR' jika nilainya ada di environment pemanggil.
    for var in ALLOWED_ENV_VARS {
        if let Ok(val) = env::var(var) {
            // Format: --preserve-env=VAR atau lewati jika tidak ada
            sudo_cmd.arg(format!("--preserve-env={}", var));
            // Juga set langsung untuk memastikan nilainya benar
            sudo_cmd.env(var, val);
        }
    }

    // Nonaktifkan pewarisan environment lain secara eksplisit
    // (sudo secara default sudah memfilter, tapi ini defensive)
    sudo_cmd.arg("--");
    sudo_cmd.arg(&current_binary);
    sudo_cmd.args(&args);

    let status = sudo_cmd.status().unwrap_or_else(|err| {
        eprintln!("MELISA: Failed to re-exec via sudo: {}", err);
        process::exit(1);
    });

    process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_env_vars_does_not_include_dangerous_vars() {
        let dangerous = ["LD_PRELOAD", "LD_LIBRARY_PATH", "PYTHONPATH", "RUBYLIB", "NODE_PATH"];
        for var in &dangerous {
            assert!(
                !ALLOWED_ENV_VARS.contains(var),
                "ALLOWED_ENV_VARS must not include dangerous variable: {}",
                var
            );
        }
    }

    #[test]
    fn test_allowed_env_vars_includes_required_vars() {
        assert!(ALLOWED_ENV_VARS.contains(&"HOME"), "HOME must be in allowed vars");
        assert!(ALLOWED_ENV_VARS.contains(&"TERM"), "TERM must be in allowed vars");
    }
}