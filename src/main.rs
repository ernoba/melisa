#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![warn(clippy::unwrap_used)] // Enforce robust error handling instead of panics

pub mod cli;
pub mod core;
pub mod deployment;
pub mod distros;

use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::process;

/// The entry point of the MELISA application.
/// 
/// We avoid using `#[tokio::main]` here. If the user is not root, we want to
/// re-execute via sudo immediately *before* allocating the heavy async runtime,
/// thread pools, and memory associated with Tokio.
fn main() {
    if !is_running_as_root() {
        println!("MELISA: Insufficient privileges detected. Elevating via sudo...");
        re_exec_as_root();
        
        // The process is replaced by exec() in the function above.
        // It will never reach this point unless exec() completely failed.
        unreachable!("MELISA: Process should have been replaced by sudo or exited.");
    }

    // We are confirmed to be root. Initialize the async runtime safely.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|err| {
            eprintln!("MELISA Fatal Error: Failed to initialize async runtime: {}", err);
            process::exit(1);
        });

    // Execute the main CLI logic within the Tokio runtime.
    runtime.block_on(async {
        cli::melisa_cli::melisa().await;
    });
}

/// Checks if the current process has root privileges.
fn is_running_as_root() -> bool {
    // SAFETY: `geteuid()` is a POSIX standard system call. 
    // It has no preconditions, does not mutate state, and does not access arbitrary memory.
    unsafe { libc::geteuid() == 0 }
}

/// An explicit allowlist of environment variables to preserve during sudo escalation.
///
/// SECURITY FIX:
/// Previously, passing all variables (e.g., via `sudo -E`) allowed critical vulnerabilities 
/// via `LD_PRELOAD`, `PYTHONPATH`, etc. 
/// 
/// LOGIC FIX:
/// We explicitly DO NOT preserve `HOME`, `USER`, or `LOGNAME`. If we did, MELISA would run 
/// as root but write cache/config files into the normal user's home directory with root 
/// ownership, permanently locking the user out of their own files.
const ALLOWED_ENV_VARS: &[&str] = &[
    "TERM",           // Preserves terminal color/formatting support
    "LANG",           // Preserves language/localization settings
    "LC_ALL",         // Preserves strict locale overrides
    "LC_MESSAGES",    // Preserves localized system messages
    "MELISA_DEBUG",   // Optional debug flag for internal development
];

/// Re-executes the current binary using `sudo`.
fn re_exec_as_root() {
    // 1. Resolve the absolute path to the current executable.
    let exe_path = env::current_exe().unwrap_or_else(|err| {
        eprintln!("MELISA Error: Failed to determine executable path: {}", err);
        process::exit(1);
    });

    // 2. Canonicalize the path to resolve any symlinks. This prevents spoofing
    // and ensures sudo executes the exact physical binary.
    let canonical_binary = fs::canonicalize(&exe_path).unwrap_or_else(|err| {
        eprintln!("MELISA Error: Failed to canonicalize binary path: {}", err);
        process::exit(1);
    });

    // 3. Collect original command line arguments, skipping the binary name.
    let args: Vec<String> = env::args().skip(1).collect();

    let mut sudo_cmd = process::Command::new("sudo");

    // 4. Safely inject only the whitelisted environment variables.
    for &var in ALLOWED_ENV_VARS {
        if env::var(var).is_ok() {
            sudo_cmd.arg(format!("--preserve-env={}", var));
        }
    }

    // 5. Append the '--' delimiter to stop sudo from parsing subsequent 
    // arguments as its own flags, followed by our binary and its arguments.
    sudo_cmd.arg("--");
    sudo_cmd.arg(&canonical_binary);
    sudo_cmd.args(&args);

    // 6. Use `exec()` to replace the current process entirely.
    // Unlike `spawn()` or `status()`, this does not create a child process.
    // The current PID becomes the sudo process. It only returns if it fails.
    let exec_error = sudo_cmd.exec();

    // If we reach this line, `exec()` failed (e.g., sudo is not installed).
    eprintln!("MELISA Fatal Error: Failed to escalate privileges via sudo.");
    eprintln!("Reason: {}", exec_error);
    eprintln!("Please run MELISA directly as root: sudo melisa");
    
    process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_env_vars_does_not_include_dangerous_or_conflicting_vars() {
        let dangerous_or_conflicting = [
            "LD_PRELOAD", 
            "LD_LIBRARY_PATH", 
            "PYTHONPATH", 
            "PATH", 
            "HOME", // Should not be preserved to protect user's ~/.config permissions
            "USER"
        ];
        
        for var in &dangerous_or_conflicting {
            assert!(
                !ALLOWED_ENV_VARS.contains(var),
                "ALLOWED_ENV_VARS must NOT include dangerous or conflicting variable: {}",
                var
            );
        }
    }

    #[test]
    fn test_allowed_env_vars_includes_required_vars() {
        assert!(ALLOWED_ENV_VARS.contains(&"TERM"), "TERM must be preserved for CLI UI styling");
        assert!(ALLOWED_ENV_VARS.contains(&"MELISA_DEBUG"), "MELISA_DEBUG must be preserved for debugging");
    }
}